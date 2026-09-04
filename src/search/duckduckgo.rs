//! DuckDuckGo via its no-JavaScript HTML endpoint.
//!
//! No key, no quota, and it answers a plain `GET`. The catch is that result
//! links are wrapped in a redirector, so the real URL has to be unwrapped out
//! of the `uddg` parameter before anything downstream can use it.

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::error::{Error, Result};
use crate::http::Fetcher;
use crate::parse::dom::{Doc, Id};
use crate::search::RawHit;
use crate::text;

const ENDPOINT: &str = "https://html.duckduckgo.com/html/";

/// Run a query and return hits in DuckDuckGo's own order.
pub(crate) async fn search(fetcher: &Fetcher, query: &str, limit: usize) -> Result<Vec<RawHit>> {
    let q = utf8_percent_encode(query, NON_ALPHANUMERIC);
    let page = fetcher.fetch_api(&format!("{ENDPOINT}?q={q}")).await?;
    let hits = parse(&page.body, limit);
    if hits.is_empty() {
        // An empty page here almost always means a challenge or a rate limit
        // rather than a genuinely empty result set, and silently returning
        // nothing would hide that from the caller.
        return Err(Error::provider("duckduckgo", "no results parsed from response"));
    }
    Ok(hits)
}

/// Parse the result list out of a DuckDuckGo HTML page.
pub(crate) fn parse(html: &str, limit: usize) -> Vec<RawHit> {
    let Ok(doc) = Doc::parse(html) else { return Vec::new() };
    let mut out = Vec::new();

    for &id in &doc.preorder {
        if doc.tag_name(id) != "a" {
            continue;
        }
        if !class_contains(&doc, id, "result__a") {
            continue;
        }
        let Some(href) = doc.attr(id, "href") else { continue };
        let Some(url) = unwrap_redirect(&href) else { continue };
        let title = text::normalize_ws(&doc.inner_text(id));
        if title.is_empty() {
            continue;
        }
        let snippet = snippet_for(&doc, id);
        out.push(RawHit { title, url, snippet });
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn class_contains(doc: &Doc<'_>, id: Id, needle: &str) -> bool {
    doc.attr(id, "class").is_some_and(|c| c.split_whitespace().any(|c| c == needle))
}

/// Find the snippet that belongs to a result link, by walking up to the result
/// container and back down.
fn snippet_for(doc: &Doc<'_>, link: Id) -> String {
    let container = doc
        .ancestors(link, 4)
        .into_iter()
        .find(|&a| class_contains(doc, a, "result__body") || class_contains(doc, a, "result"));
    let Some(container) = container else { return String::new() };
    doc.descendants(container)
        .into_iter()
        .find(|&d| class_contains(doc, d, "result__snippet"))
        .map(|d| text::normalize_ws(&doc.inner_text(d)))
        .unwrap_or_default()
}

/// Unwrap `//duckduckgo.com/l/?uddg=<encoded>&rut=…` to the real target.
fn unwrap_redirect(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    if let Some(query) = href.split_once('?').map(|(_, q)| q)
        && (href.contains("duckduckgo.com/l/") || href.starts_with("/l/"))
    {
        for pair in query.split('&') {
            if let Some(v) = pair.strip_prefix("uddg=") {
                let decoded = percent_decode_str(v).decode_utf8().ok()?.into_owned();
                return decoded.starts_with("http").then_some(decoded);
            }
        }
        return None;
    }
    // Protocol-relative links are common in this markup.
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    href.starts_with("http").then(|| href.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"<html><body>
      <div class="result results_links">
        <div class="result__body">
          <h2><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ftokio.rs%2Ftutorial&amp;rut=ab">Tokio tutorial</a></h2>
          <a class="result__snippet">An asynchronous runtime for the Rust programming language.</a>
        </div>
      </div>
      <div class="result results_links">
        <div class="result__body">
          <h2><a class="result__a" href="https://docs.rs/rayon">Rayon docs</a></h2>
          <a class="result__snippet">Data parallelism library.</a>
        </div>
      </div>
      <div class="result result--ad"><a class="result__a" href="#">Ad</a></div>
    </body></html>"##;

    #[test]
    fn parses_titles_urls_and_snippets() {
        let hits = parse(PAGE, 10);
        assert_eq!(hits.len(), 2, "{hits:#?}");
        assert_eq!(hits[0].url, "https://tokio.rs/tutorial");
        assert_eq!(hits[0].title, "Tokio tutorial");
        assert!(hits[0].snippet.starts_with("An asynchronous runtime"));
        assert_eq!(hits[1].url, "https://docs.rs/rayon");
    }

    #[test]
    fn honours_the_limit() {
        assert_eq!(parse(PAGE, 1).len(), 1);
    }

    #[test]
    fn unwraps_and_rejects_hrefs() {
        assert_eq!(
            unwrap_redirect("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.dev%2Fb").as_deref(),
            Some("https://a.dev/b")
        );
        assert_eq!(unwrap_redirect("//a.dev/b").as_deref(), Some("https://a.dev/b"));
        assert_eq!(unwrap_redirect("#"), None);
        assert_eq!(unwrap_redirect(""), None);
    }

    #[test]
    fn empty_html_parses_to_nothing() {
        assert!(parse("", 10).is_empty());
        assert!(parse("<html><body></body></html>", 10).is_empty());
    }
}
