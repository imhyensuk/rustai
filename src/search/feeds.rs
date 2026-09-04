//! RSS, Atom and XML sitemaps.
//!
//! These are the highest-signal sources in the router and the only ones that
//! are unambiguously meant to be read by machines. When you already know which
//! sites matter, pointing at their feeds beats any general web search: the
//! results are complete, ordered, and free of an intermediary's ranking.

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use crate::error::{Error, Result};
use crate::http::Fetcher;
use crate::rank::Bm25;
use crate::search::RawHit;
use crate::text;

/// Child sitemaps followed from a sitemap index. Bounded on purpose: a large
/// site's index can list hundreds, and fetching them all is a crawl, not a
/// query.
const MAX_CHILD_SITEMAPS: usize = 3;

/// One entry from a feed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FeedItem {
    /// Entry title.
    pub title: String,
    /// Entry URL.
    pub link: String,
    /// Description, summary or content, as plain text.
    pub summary: String,
    /// Publication date, verbatim from the feed.
    pub published: Option<String>,
}

/// A parsed sitemap: either a list of pages or a list of other sitemaps.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Sitemap {
    /// Page URLs from a `<urlset>`.
    pub urls: Vec<String>,
    /// Child sitemap URLs from a `<sitemapindex>`.
    pub sitemaps: Vec<String>,
}

/// Parse an RSS 2.0 or Atom feed.
///
/// Both formats are handled by one pass because they differ only in element
/// names, and real feeds mix conventions freely enough that being strict about
/// which is which loses entries for no benefit.
pub fn parse_rss(xml: &str) -> Result<Vec<FeedItem>> {
    let mut reader = reader_for(xml);
    let mut items = Vec::new();
    let mut current: Option<FeedItem> = None;
    let mut field: Option<&'static str> = None;
    let mut buf = String::new();

    loop {
        match reader.read_event().map_err(|e| Error::Parse(e.to_string()))? {
            Event::Start(e) => {
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    "item" | "entry" => {
                        current = Some(FeedItem {
                            title: String::new(),
                            link: String::new(),
                            summary: String::new(),
                            published: None,
                        });
                    }
                    "title" | "link" | "description" | "summary" | "content" | "pubdate"
                    | "published" | "updated" | "id"
                        if current.is_some() =>
                    {
                        field = field_key(&name);
                        buf.clear();
                    }
                    _ => {}
                }
                // Atom puts the URL in an attribute rather than in the text.
                if name == "link"
                    && let Some(item) = current.as_mut()
                    && let Some(href) = attr(&e, "href")
                    && item.link.is_empty()
                {
                    item.link = href;
                }
            }
            Event::Empty(e) => {
                if local_name(e.name().as_ref()) == "link"
                    && let Some(item) = current.as_mut()
                    && let Some(href) = attr(&e, "href")
                    && item.link.is_empty()
                {
                    item.link = href;
                }
            }
            Event::Text(t) => {
                if field.is_some() {
                    buf.push_str(&t.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::CData(c) => {
                if field.is_some() {
                    buf.push_str(c.as_ref());
                }
            }
            Event::GeneralRef(r) => {
                if field.is_some() {
                    buf.push_str(&resolve_ref(&r));
                }
            }
            Event::End(e) => {
                let name = local_name(e.name().as_ref());
                if let (Some(key), Some(item)) = (field, current.as_mut()) {
                    let value = text::normalize_ws(&buf);
                    match key {
                        "title" if item.title.is_empty() => item.title = value,
                        "link" if item.link.is_empty() && value.starts_with("http") => {
                            item.link = value;
                        }
                        "summary" if item.summary.is_empty() => {
                            item.summary = strip_tags(&value);
                        }
                        "published" if item.published.is_none() => {
                            item.published = Some(value);
                        }
                        _ => {}
                    }
                    buf.clear();
                    field = None;
                }
                if matches!(name.as_str(), "item" | "entry")
                    && let Some(item) = current.take()
                    && !item.link.is_empty()
                {
                    items.push(item);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(items)
}

/// Parse a `<urlset>` or `<sitemapindex>` document.
pub fn parse_sitemap(xml: &str) -> Result<Sitemap> {
    let mut reader = reader_for(xml);
    let mut out = Sitemap::default();
    let mut in_index = false;
    let mut in_loc = false;
    let mut buf = String::new();

    loop {
        match reader.read_event().map_err(|e| Error::Parse(e.to_string()))? {
            Event::Start(e) => match local_name(e.name().as_ref()).as_str() {
                "sitemapindex" => in_index = true,
                "loc" => {
                    in_loc = true;
                    buf.clear();
                }
                _ => {}
            },
            Event::Text(t) if in_loc => {
                buf.push_str(&t.xml_content(XmlVersion::Implicit1_0));
            }
            Event::CData(c) if in_loc => buf.push_str(c.as_ref()),
            Event::GeneralRef(r) if in_loc => buf.push_str(&resolve_ref(&r)),
            Event::End(e) => {
                if local_name(e.name().as_ref()) == "loc" {
                    let url = buf.trim().to_string();
                    if url.starts_with("http") {
                        if in_index {
                            out.sitemaps.push(url);
                        } else {
                            out.urls.push(url);
                        }
                    }
                    in_loc = false;
                    buf.clear();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Fetch a feed and return its most relevant entries.
pub(crate) async fn search_rss(
    fetcher: &Fetcher,
    feed_url: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<RawHit>> {
    let page = fetcher.fetch_api(feed_url).await?;
    let items = parse_rss(&page.body)?;
    if items.is_empty() {
        return Err(Error::provider("rss", format!("{feed_url} had no entries")));
    }

    let hits: Vec<RawHit> = items
        .into_iter()
        .map(|i| RawHit { title: i.title, url: i.link, snippet: i.summary })
        .collect();
    Ok(rank_hits(hits, query, limit))
}

/// Fetch an HTML listing page and return its links, ranked against the query.
///
/// A front page is a feed without the XML: the same inventory, marked up for
/// people instead of machines. Treating it as a provider means a site with no
/// feed and no sitemap is still collectable.
pub(crate) async fn search_index(
    fetcher: &Fetcher,
    page_url: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<RawHit>> {
    let page = fetcher.fetch_api(page_url).await?;
    let opts = crate::parse::ExtractOptions {
        index_mode: crate::parse::IndexMode::Always,
        ..crate::parse::ExtractOptions::new()
    };
    let article = crate::parse::extract_with(&page.body, Some(&page.final_url), &opts)?;
    if article.links.is_empty() {
        return Err(Error::provider("index", format!("{page_url} listed no links")));
    }
    let hits: Vec<RawHit> = article
        .links
        .into_iter()
        .map(|l| RawHit { title: l.text, url: l.url, snippet: l.snippet })
        .collect();
    Ok(rank_hits(hits, query, limit))
}

/// Fetch a sitemap and return the URLs whose slugs best match the query.
pub(crate) async fn search_sitemap(
    fetcher: &Fetcher,
    sitemap_url: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<RawHit>> {
    let page = fetcher.fetch_api(sitemap_url).await?;
    let mut map = parse_sitemap(&page.body)?;

    if map.urls.is_empty() && !map.sitemaps.is_empty() {
        let children = map.sitemaps.iter().take(MAX_CHILD_SITEMAPS).cloned().collect::<Vec<_>>();
        let fetched =
            futures_util::future::join_all(children.iter().map(|u| fetcher.fetch_api(u))).await;
        for child in fetched.into_iter().flatten() {
            if let Ok(sub) = parse_sitemap(&child.body) {
                map.urls.extend(sub.urls);
            }
        }
    }
    if map.urls.is_empty() {
        return Err(Error::provider("sitemap", format!("{sitemap_url} listed no URLs")));
    }

    // A sitemap carries no titles, so the URL slug is all the signal there is.
    let hits: Vec<RawHit> = map
        .urls
        .into_iter()
        .map(|u| {
            let title = slug_words(&u);
            RawHit { title, url: u, snippet: String::new() }
        })
        .collect();
    Ok(rank_hits(hits, query, limit))
}

/// Order hits by BM25 against the query, or keep feed order if there is none.
fn rank_hits(hits: Vec<RawHit>, query: &str, limit: usize) -> Vec<RawHit> {
    let q = text::tokenize(query);
    if q.is_empty() {
        return hits.into_iter().take(limit).collect();
    }
    let corpus: Vec<String> =
        hits.iter().map(|h| format!("{} {} {}", h.title, h.snippet, slug_words(&h.url))).collect();
    let index = Bm25::from_texts(&corpus);
    let scores = index.score_all(&q);

    let mut order: Vec<usize> = (0..hits.len()).collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    order.into_iter().filter(|&i| scores[i] > 0.0).take(limit).map(|i| hits[i].clone()).collect()
}

/// Turn a URL path into words, so slugs are searchable text.
fn slug_words(url: &str) -> String {
    let path = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path = path.split_once('/').map(|(_, r)| r).unwrap_or("");
    path.split(['/', '-', '_', '.', '?', '&', '='])
        .filter(|s| s.len() > 1 && !s.chars().all(|c| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn reader_for(xml: &str) -> Reader<&[u8]> {
    let mut reader = Reader::from_str(xml);
    let config = reader.config_mut();
    // Text is *not* trimmed here. quick-xml delivers `&amp;` as its own event,
    // so trimming each fragment would silently eat the spaces around every
    // entity; the accumulated buffer is normalised in one go instead.
    config.trim_text(false);
    config.check_end_names = false;
    // Real feeds contain bare `&` characters. Refusing to parse the whole
    // document over one is not a trade any caller wants.
    config.allow_dangling_amp = true;
    reader
}

/// Drop the namespace prefix and lowercase, so `dc:date` and `DATE` agree.
fn local_name(raw: &str) -> String {
    raw.rsplit(':').next().unwrap_or(raw).to_ascii_lowercase()
}

/// Resolve an entity or character reference to its text.
///
/// `quick-xml` resolves numeric references but leaves named ones to the caller,
/// since XML itself only predefines five. Feeds in the wild use the HTML set
/// freely, so we hand named references to the HTML decoder and keep anything
/// still unresolved verbatim rather than dropping it.
fn resolve_ref(r: &quick_xml::events::BytesRef<'_>) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    let name: &str = r.as_ref();
    let literal = format!("&{name};");
    let decoded = html_escape::decode_html_entities(&literal);
    decoded.into_owned()
}

fn field_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "title" => "title",
        "link" | "id" => "link",
        "description" | "summary" | "content" => "summary",
        "pubdate" | "published" | "updated" => "published",
        _ => return None,
    })
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes().flatten().find(|a| local_name(a.key.as_ref()) == key).and_then(|a| {
        let v = a.value.as_ref().trim().to_string();
        (!v.is_empty()).then_some(v)
    })
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    text::normalize_ws(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?>
    <rss version="2.0"><channel>
      <title>Channel</title>
      <item>
        <title>Tokio 2.0 released</title>
        <link>https://blog.dev/tokio-2</link>
        <description><![CDATA[<p>The async runtime gets a new scheduler.</p>]]></description>
        <pubDate>Tue, 01 Sep 2026 00:00:00 GMT</pubDate>
      </item>
      <item>
        <title>Bread &amp; butter</title>
        <link>https://blog.dev/bread</link>
        <description>Baking notes.</description>
      </item>
    </channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0"?>
    <feed xmlns="http://www.w3.org/2005/Atom">
      <entry>
        <title>Rayon internals</title>
        <link href="https://atom.dev/rayon" rel="alternate"/>
        <summary>Work stealing explained.</summary>
        <updated>2026-08-01T00:00:00Z</updated>
      </entry>
    </feed>"#;

    #[test]
    fn parses_rss_items() {
        let items = parse_rss(RSS).unwrap();
        assert_eq!(items.len(), 2, "{items:#?}");
        assert_eq!(items[0].title, "Tokio 2.0 released");
        assert_eq!(items[0].link, "https://blog.dev/tokio-2");
        assert_eq!(items[0].summary, "The async runtime gets a new scheduler.");
        assert!(items[0].published.as_deref().unwrap().contains("2026"));
        assert_eq!(items[1].title, "Bread & butter", "entity was not resolved");
        assert_eq!(items[1].summary, "Baking notes.");
    }

    #[test]
    fn parses_atom_entries_with_href_links() {
        let items = parse_rss(ATOM).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].link, "https://atom.dev/rayon");
        assert_eq!(items[0].summary, "Work stealing explained.");
    }

    #[test]
    fn parses_a_urlset() {
        let xml = r#"<urlset><url><loc>https://a.dev/1</loc></url><url><loc>https://a.dev/2</loc></url></urlset>"#;
        let map = parse_sitemap(xml).unwrap();
        assert_eq!(map.urls, ["https://a.dev/1", "https://a.dev/2"]);
        assert!(map.sitemaps.is_empty());
    }

    #[test]
    fn parses_a_sitemap_index() {
        let xml =
            r#"<sitemapindex><sitemap><loc>https://a.dev/s1.xml</loc></sitemap></sitemapindex>"#;
        let map = parse_sitemap(xml).unwrap();
        assert!(map.urls.is_empty());
        assert_eq!(map.sitemaps, ["https://a.dev/s1.xml"]);
    }

    #[test]
    fn ranking_puts_the_matching_entry_first() {
        let items = parse_rss(RSS).unwrap();
        let hits: Vec<RawHit> = items
            .into_iter()
            .map(|i| RawHit { title: i.title, url: i.link, snippet: i.summary })
            .collect();
        let ranked = rank_hits(hits.clone(), "async scheduler runtime", 5);
        assert_eq!(ranked[0].url, "https://blog.dev/tokio-2");
        // With no query, feed order is preserved.
        assert_eq!(rank_hits(hits, "", 5)[0].url, "https://blog.dev/tokio-2");
    }

    #[test]
    fn slugs_become_searchable_words() {
        assert_eq!(
            slug_words("https://a.dev/blog/zero-cost_crawling.html"),
            "blog zero cost crawling html"
        );
    }

    #[test]
    fn malformed_xml_does_not_panic() {
        assert!(
            parse_rss("<rss><item><title>x").is_ok() || parse_rss("<rss><item><title>x").is_err()
        );
        assert!(parse_sitemap("").unwrap().urls.is_empty());
    }
}
