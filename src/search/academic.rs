//! Scholarly sources: arXiv, OpenAlex and Crossref.
//!
//! All three are free, keyless, documented and stable — which is why they are
//! here and a general web search is not a substitute. A query for a paper on a
//! web search engine returns blog posts about the paper; these return the paper.
//!
//! OpenAlex and Crossref both run a "polite pool" that is faster and more
//! reliable for callers who identify themselves. Set
//! [`SearchConfig::contact_email`](crate::search::SearchConfig::contact_email)
//! and they will use it.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use crate::error::{Error, Result};
use crate::http::Fetcher;
use crate::search::RawHit;
use crate::text;

/// Snippets longer than this are truncated: an abstract is context, not content.
const MAX_SNIPPET_CHARS: usize = 1200;

// ------------------------------------------------------------------- arXiv

/// Search arXiv's Atom API.
pub(crate) async fn arxiv(fetcher: &Fetcher, query: &str, limit: usize) -> Result<Vec<RawHit>> {
    let q = utf8_percent_encode(query, NON_ALPHANUMERIC);
    // `all:` searches title, abstract, authors and comments at once, which is
    // what a natural-language query actually wants.
    let url = format!(
        "https://export.arxiv.org/api/query?search_query=all:{q}&start=0&max_results={limit}\
         &sortBy=relevance&sortOrder=descending"
    );
    let page = fetcher.fetch_api(&url).await?;
    let hits = parse_arxiv(&page.body);
    if hits.is_empty() {
        return Err(Error::provider("arxiv", "no entries in the Atom response"));
    }
    Ok(hits)
}

/// Parse an arXiv Atom feed.
///
/// Written by hand rather than reusing the generic feed reader because arXiv's
/// entries carry authors and a `rel="alternate"` abstract link that the generic
/// path would flatten away — and the author list is most of what makes a
/// scholarly snippet worth reading.
pub(crate) fn parse_arxiv(xml: &str) -> Vec<RawHit> {
    let mut reader = Reader::from_str(xml);
    let config = reader.config_mut();
    config.trim_text(false);
    config.check_end_names = false;
    config.allow_dangling_amp = true;

    let mut hits = Vec::new();
    let mut in_entry = false;
    let mut in_author = false;
    let mut field: Option<&'static str> = None;
    let mut buf = String::new();
    let (mut title, mut link, mut summary, mut published) =
        (String::new(), String::new(), String::new(), String::new());
    let mut authors: Vec<String> = Vec::new();

    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let name = local(e.name().as_ref());
                match name.as_str() {
                    "entry" => {
                        in_entry = true;
                        title.clear();
                        link.clear();
                        summary.clear();
                        published.clear();
                        authors.clear();
                    }
                    "author" if in_entry => in_author = true,
                    "link" if in_entry => {
                        // The alternate link is the abstract page; `related` is
                        // the PDF, which this pipeline cannot read.
                        let rel = attr(&e, "rel").unwrap_or_default();
                        if rel == "alternate"
                            && let Some(href) = attr(&e, "href")
                        {
                            link = href;
                        }
                    }
                    "id" | "title" | "summary" | "published" | "name" if in_entry => {
                        field = Some(match name.as_str() {
                            "id" => "id",
                            "title" => "title",
                            "summary" => "summary",
                            "published" => "published",
                            _ => "name",
                        });
                        buf.clear();
                    }
                    _ => {}
                }
            }
            Event::Text(t) if field.is_some() => {
                buf.push_str(&t.xml_content(XmlVersion::Implicit1_0))
            }
            Event::CData(c) if field.is_some() => buf.push_str(c.as_ref()),
            Event::GeneralRef(r) if field.is_some() => buf.push_str(&resolve_ref(&r)),
            Event::End(e) => {
                let name = local(e.name().as_ref());
                if let Some(key) = field.take() {
                    let value = text::normalize_ws(&buf);
                    match key {
                        "title" if title.is_empty() => title = value,
                        "summary" if summary.is_empty() => summary = value,
                        "published" if published.is_empty() => published = value,
                        "name" if in_author => authors.push(value),
                        "id" if link.is_empty() && value.starts_with("http") => {
                            link = value.replace("http://", "https://");
                        }
                        _ => {}
                    }
                    buf.clear();
                }
                match name.as_str() {
                    "author" => in_author = false,
                    "entry" => {
                        in_entry = false;
                        if !link.is_empty() && !title.is_empty() {
                            hits.push(RawHit {
                                title: title.clone(),
                                url: link.replace("http://", "https://"),
                                snippet: scholarly_snippet(&authors, &published, &summary),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    hits
}

// ---------------------------------------------------------------- OpenAlex

/// Search OpenAlex, the open index of ~250M scholarly works.
/// Strip the characters OpenAlex reads as wildcard operators.
///
/// `?` and `*` are wildcards in a `search=` value, and supplying either
/// without an exact-match qualifier is rejected outright:
///
/// ```text
/// 400 {"error":"Invalid query parameters error.",
///      "message":"Wildcards (* or ?) require exact ..."}
/// ```
///
/// Which means a question — "how does BM25 normalise for document length?" —
/// fails on its question mark, and a research query is usually a question.
/// Removing them costs nothing: neither character carries meaning for a
/// relevance search, and the provider offers no way to escape them.
fn openalex_search_term(query: &str) -> String {
    let cleaned: String =
        query.chars().map(|c| if c == '?' || c == '*' { ' ' } else { c }).collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) async fn openalex(
    fetcher: &Fetcher,
    query: &str,
    limit: usize,
    contact: Option<&str>,
) -> Result<Vec<RawHit>> {
    let term = openalex_search_term(query);
    let q = utf8_percent_encode(&term, NON_ALPHANUMERIC);
    let mut url = format!("https://api.openalex.org/works?search={q}&per-page={limit}");
    if let Some(email) = contact {
        url.push_str(&format!("&mailto={}", utf8_percent_encode(email, NON_ALPHANUMERIC)));
    }
    let page = fetcher.fetch_api(&url).await?;
    parse_openalex(&page.body, limit)
}

/// Parse an OpenAlex `works` response.
pub(crate) fn parse_openalex(body: &str, limit: usize) -> Result<Vec<RawHit>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| Error::provider("openalex", format!("bad json: {e}")))?;
    let results = value
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| Error::provider("openalex", "response had no results array"))?;

    Ok(results
        .iter()
        .filter_map(|w| {
            let title = w.get("display_name")?.as_str()?.to_string();
            // Prefer somewhere the full text can actually be read; fall back to
            // the landing page, then to the DOI resolver.
            let url = w
                .get("best_oa_location")
                .and_then(|l| l.get("landing_page_url"))
                .and_then(|u| u.as_str())
                .or_else(|| w.get("open_access")?.get("oa_url")?.as_str())
                .or_else(|| w.get("primary_location")?.get("landing_page_url")?.as_str())
                .or_else(|| w.get("doi")?.as_str())?
                .to_string();

            let authors: Vec<String> = w
                .get("authorships")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            Some(x.get("author")?.get("display_name")?.as_str()?.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();
            let year = w
                .get("publication_year")
                .and_then(|y| y.as_i64())
                .map(|y| y.to_string())
                .unwrap_or_default();
            let abstract_text = w
                .get("abstract_inverted_index")
                .and_then(|i| i.as_object())
                .map(reconstruct_abstract)
                .unwrap_or_default();

            Some(RawHit { title, url, snippet: scholarly_snippet(&authors, &year, &abstract_text) })
        })
        .take(limit)
        .collect())
}

/// Rebuild a readable abstract from OpenAlex's inverted index.
///
/// OpenAlex stores abstracts as `{word: [positions]}` for licensing reasons.
/// Inverting it back is a few lines and turns an unusable field into the single
/// most informative snippet any provider here returns.
fn reconstruct_abstract(index: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut positioned: Vec<(u64, &str)> = Vec::new();
    for (word, slots) in index {
        let Some(slots) = slots.as_array() else { continue };
        for slot in slots {
            if let Some(p) = slot.as_u64() {
                positioned.push((p, word.as_str()));
            }
        }
    }
    positioned.sort_unstable_by_key(|(p, _)| *p);
    let mut out = String::new();
    for (_, word) in positioned {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.len() > MAX_SNIPPET_CHARS {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------- Crossref

/// Search Crossref, the DOI registration agency's metadata index.
pub(crate) async fn crossref(
    fetcher: &Fetcher,
    query: &str,
    limit: usize,
    contact: Option<&str>,
) -> Result<Vec<RawHit>> {
    let q = utf8_percent_encode(query, NON_ALPHANUMERIC);
    let mut url = format!("https://api.crossref.org/works?query={q}&rows={limit}");
    if let Some(email) = contact {
        url.push_str(&format!("&mailto={}", utf8_percent_encode(email, NON_ALPHANUMERIC)));
    }
    let page = fetcher.fetch_api(&url).await?;
    parse_crossref(&page.body, limit)
}

/// Parse a Crossref `works` response.
pub(crate) fn parse_crossref(body: &str, limit: usize) -> Result<Vec<RawHit>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| Error::provider("crossref", format!("bad json: {e}")))?;
    let items = value
        .get("message")
        .and_then(|m| m.get("items"))
        .and_then(|i| i.as_array())
        .ok_or_else(|| Error::provider("crossref", "response had no message.items array"))?;

    Ok(items
        .iter()
        .filter_map(|w| {
            let title = w
                .get("title")
                .and_then(|t| t.as_array())
                .and_then(|t| t.first())
                .and_then(|t| t.as_str())
                .map(text::normalize_ws)
                .filter(|t| !t.is_empty())?;
            let url = w
                .get("resource")
                .and_then(|r| r.get("primary"))
                .and_then(|p| p.get("URL"))
                .and_then(|u| u.as_str())
                .or_else(|| w.get("URL")?.as_str())?
                .to_string();

            let authors: Vec<String> = w
                .get("author")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            let family = x.get("family")?.as_str()?;
                            Some(match x.get("given").and_then(|g| g.as_str()) {
                                Some(given) => format!("{given} {family}"),
                                None => family.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let year = w
                .get("issued")
                .and_then(|i| i.get("date-parts"))
                .and_then(|d| d.as_array())
                .and_then(|d| d.first())
                .and_then(|d| d.as_array())
                .and_then(|d| d.first())
                .and_then(|y| y.as_i64())
                .map(|y| y.to_string())
                .unwrap_or_default();
            // Crossref abstracts, when present, are JATS XML fragments.
            let summary =
                w.get("abstract").and_then(|a| a.as_str()).map(strip_tags).unwrap_or_default();

            Some(RawHit { title, url, snippet: scholarly_snippet(&authors, &year, &summary) })
        })
        .take(limit)
        .collect())
}

// ------------------------------------------------------------------ shared

/// `Author, Author et al. (2024). Abstract…` — the shape a reader expects.
fn scholarly_snippet(authors: &[String], date: &str, body: &str) -> String {
    let mut out = String::new();
    if !authors.is_empty() {
        let shown = authors.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        out.push_str(&shown);
        if authors.len() > 3 {
            out.push_str(" et al.");
        }
    }
    let year: String = date.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !year.is_empty() {
        out.push_str(&format!(" ({year})"));
    }
    if !out.is_empty() && !body.is_empty() {
        out.push_str(". ");
    }
    out.push_str(body);
    truncate(&text::normalize_ws(&out), MAX_SNIPPET_CHARS)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
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
    text::normalize_ws(&html_escape::decode_html_entities(&out))
}

fn local(raw: &str) -> String {
    raw.rsplit(':').next().unwrap_or(raw).to_ascii_lowercase()
}

fn attr(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| local(a.key.as_ref()) == key)
        .map(|a| a.value.as_ref().trim().to_string())
        .filter(|v| !v.is_empty())
}

fn resolve_ref(r: &quick_xml::events::BytesRef<'_>) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    html_escape::decode_html_entities(&format!("&{};", r.as_ref())).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARXIV: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
    <feed xmlns="http://www.w3.org/2005/Atom">
      <entry>
        <id>http://arxiv.org/abs/1706.03762v7</id>
        <title>Attention Is All You
  Need</title>
        <summary>The dominant sequence transduction models are based on complex recurrent
  networks &amp; attention.</summary>
        <published>2017-06-12T17:57:34Z</published>
        <author><name>Ashish Vaswani</name></author>
        <author><name>Noam Shazeer</name></author>
        <author><name>Niki Parmar</name></author>
        <author><name>Jakob Uszkoreit</name></author>
        <link href="https://arxiv.org/abs/1706.03762v7" rel="alternate" type="text/html"/>
        <link href="https://arxiv.org/pdf/1706.03762v7" rel="related" type="application/pdf"/>
      </entry>
    </feed>"#;

    #[test]
    fn arxiv_entries_parse_with_authors_and_abstract_links() {
        let hits = parse_arxiv(ARXIV);
        assert_eq!(hits.len(), 1, "{hits:#?}");
        assert_eq!(hits[0].title, "Attention Is All You Need", "newlines were not normalised");
        // The abstract page, never the PDF: this pipeline cannot read a PDF.
        assert_eq!(hits[0].url, "https://arxiv.org/abs/1706.03762v7");
        assert!(
            hits[0].snippet.starts_with("Ashish Vaswani, Noam Shazeer, Niki Parmar et al. (2017).")
        );
        assert!(hits[0].snippet.contains("recurrent networks & attention"));
    }

    #[test]
    fn arxiv_empty_feed_yields_nothing() {
        assert!(parse_arxiv("<feed></feed>").is_empty());
        assert!(parse_arxiv("").is_empty());
    }

    #[test]
    fn openalex_reconstructs_the_inverted_abstract() {
        let body = r#"{"results":[{
            "display_name":"Attention Is All You Need",
            "publication_year":2017,
            "doi":"https://doi.org/10.5555/x",
            "best_oa_location":{"landing_page_url":"https://arxiv.org/abs/1706.03762"},
            "authorships":[{"author":{"display_name":"Ashish Vaswani"}}],
            "abstract_inverted_index":{"The":[0],"dominant":[1],"models":[2],"are":[3],"complex":[4]}
        }]}"#;
        let hits = parse_openalex(body, 10).unwrap();
        assert_eq!(hits[0].url, "https://arxiv.org/abs/1706.03762", "OA link should win");
        assert!(hits[0].snippet.contains("The dominant models are complex"), "{}", hits[0].snippet);
        assert!(hits[0].snippet.starts_with("Ashish Vaswani (2017)."));
    }

    #[test]
    fn openalex_falls_back_to_the_doi() {
        let body = r#"{"results":[{"display_name":"T","doi":"https://doi.org/10.1/x"}]}"#;
        assert_eq!(parse_openalex(body, 5).unwrap()[0].url, "https://doi.org/10.1/x");
        assert!(parse_openalex("{}", 5).is_err());
        assert!(parse_openalex("not json", 5).is_err());
    }

    #[test]
    fn crossref_items_parse() {
        let body = r#"{"message":{"items":[{
            "title":["A Study of Things"],
            "URL":"https://doi.org/10.1/abc",
            "author":[{"given":"Ada","family":"Lovelace"},{"family":"Babbage"}],
            "issued":{"date-parts":[[1843,7]]},
            "abstract":"<jats:p>We show <jats:italic>things</jats:italic>.</jats:p>"
        }]}}"#;
        let hits = parse_crossref(body, 10).unwrap();
        assert_eq!(hits[0].title, "A Study of Things");
        assert_eq!(hits[0].url, "https://doi.org/10.1/abc");
        assert_eq!(hits[0].snippet, "Ada Lovelace, Babbage (1843). We show things.");
    }

    #[test]
    fn crossref_rejects_a_foreign_body() {
        assert!(parse_crossref(r#"{"results":[]}"#, 5).is_err());
    }

    #[test]
    fn snippets_are_truncated() {
        let long = "word ".repeat(600);
        let s = scholarly_snippet(&[], "", &long);
        assert!(s.chars().count() <= MAX_SNIPPET_CHARS + 1);
        assert!(s.ends_with('…'));
    }
}

#[cfg(test)]
mod openalex_query_tests {
    use super::openalex_search_term;

    /// A research query is usually a question, and OpenAlex rejects the
    /// question mark as an unqualified wildcard.
    #[test]
    fn strips_wildcards_openalex_rejects() {
        assert_eq!(
            openalex_search_term("How does BM25 normalise for document length?"),
            "How does BM25 normalise for document length"
        );
        assert_eq!(
            openalex_search_term("BM25는 문서 길이를 정규화하는가?"),
            "BM25는 문서 길이를 정규화하는가"
        );
        assert_eq!(openalex_search_term("wild*card"), "wild card");
        assert_eq!(openalex_search_term("what? why? how?"), "what why how");
    }

    /// Everything else is left alone, including punctuation the API accepts.
    #[test]
    fn leaves_ordinary_queries_untouched() {
        assert_eq!(openalex_search_term("BM25 ranking"), "BM25 ranking");
        assert_eq!(openalex_search_term("Müller & Sons: a study!"), "Müller & Sons: a study!");
    }
}

/// Europe PMC — the life-sciences literature, in one request.
///
/// Preferred over PubMed's E-utilities, which need two round trips (search
/// for identifiers, then fetch their summaries) to reach the same place.
/// Europe PMC indexes PubMed and PMC alongside preprints and patents, and
/// `resultType=core` returns the abstract with the metadata.
pub(crate) async fn europe_pmc(
    fetcher: &Fetcher,
    query: &str,
    limit: usize,
) -> Result<Vec<RawHit>> {
    let q = utf8_percent_encode(query, NON_ALPHANUMERIC);
    let url = format!(
        "https://www.ebi.ac.uk/europepmc/webservices/rest/search\
         ?query={q}&format=json&pageSize={limit}&resultType=core"
    );
    let page = fetcher.fetch_api(&url).await?;
    parse_europe_pmc(&page.body, limit)
}

/// Parse a Europe PMC `search` response.
pub(crate) fn parse_europe_pmc(body: &str, limit: usize) -> Result<Vec<RawHit>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| Error::provider("europepmc", format!("bad json: {e}")))?;
    let results = value
        .get("resultList")
        .and_then(|l| l.get("result"))
        .and_then(|r| r.as_array())
        .ok_or_else(|| Error::provider("europepmc", "response had no resultList.result"))?;

    Ok(results
        .iter()
        .filter_map(|item| {
            let field = |key: &str| {
                item.get(key).and_then(|v| v.as_str()).map(str::to_string).filter(|s| !s.is_empty())
            };
            let title = field("title")?;
            // Every record has a source and an id; a DOI is not guaranteed,
            // and a PMID even less so for preprints.
            let url = match (field("source"), field("id")) {
                (Some(source), Some(id)) => format!("https://europepmc.org/article/{source}/{id}"),
                _ => format!("https://doi.org/{}", field("doi")?),
            };
            // `authorString` is one comma-separated line, where the shared
            // snippet builder wants the names apart so it can decide where
            // to stop and add "et al.".
            let authors: Vec<String> = field("authorString")
                .unwrap_or_default()
                .split(',')
                .map(|a| a.trim().trim_end_matches('.').to_string())
                .filter(|a| !a.is_empty())
                .collect();
            let year = field("pubYear").unwrap_or_default();
            // Abstracts arrive as JATS, the same markup Crossref sends.
            let body = field("abstractText").map(|a| strip_tags(&a)).unwrap_or_default();
            Some(RawHit {
                title: strip_tags(&title),
                url,
                snippet: scholarly_snippet(&authors, &year, &body),
            })
        })
        .take(limit)
        .collect())
}

#[cfg(test)]
mod europe_pmc_tests {
    use super::parse_europe_pmc;

    #[test]
    fn builds_a_url_from_source_and_id() {
        let body = r#"{"resultList":{"result":[{"id":"42281096","source":"MED",
            "title":"Rare-disease diagnosis","authorString":"Islam MS, Jamal A, Alkhathlan A.",
            "pubYear":"2026","abstractText":"<title>Abstract</title><p>Diagnosis is hard.</p>"}]}}"#;
        let hits = parse_europe_pmc(body, 10).unwrap();
        assert_eq!(hits[0].url, "https://europepmc.org/article/MED/42281096");
        assert!(hits[0].snippet.contains("Islam MS, Jamal A, Alkhathlan A (2026)"));
        assert!(hits[0].snippet.contains("Diagnosis is hard."), "JATS survived");
        assert!(!hits[0].snippet.contains("<p>"));
    }

    /// A preprint has no PMID and often no DOI; source and id always exist.
    #[test]
    fn a_preprint_still_resolves() {
        let body = r#"{"resultList":{"result":[{"id":"PPR1302687","source":"PPR",
            "title":"BM25 and Dense Retrieval Are Complementary","pubYear":"2026"}]}}"#;
        let hits = parse_europe_pmc(body, 10).unwrap();
        assert_eq!(hits[0].url, "https://europepmc.org/article/PPR/PPR1302687");
    }

    #[test]
    fn an_empty_result_list_is_not_an_error() {
        let body = r#"{"resultList":{"result":[]}}"#;
        assert!(parse_europe_pmc(body, 10).unwrap().is_empty());
    }
}
