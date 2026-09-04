//! Public SearXNG instances.
//!
//! SearXNG aggregates other engines and, on instances that allow it, will hand
//! back JSON. Many public instances disable that to discourage scraping, so we
//! ask for JSON first and fall back to parsing the HTML result list.
//!
//! No instance is hard-coded as a default. Public instances are volunteer-run
//! and go up and down weekly; baking a list into a library means shipping a
//! broken default and a load problem for whoever is on it. Pass the instance
//! you want — ideally your own.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::error::{Error, Result};
use crate::http::Fetcher;
use crate::parse::dom::{Doc, Id};
use crate::search::RawHit;
use crate::text;

/// Query an instance, preferring its JSON API.
pub(crate) async fn search(
    fetcher: &Fetcher,
    base: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<RawHit>> {
    let base = base.trim_end_matches('/');
    crate::http::normalize(base)?;
    let q = utf8_percent_encode(query, NON_ALPHANUMERIC);

    if let Ok(page) = fetcher.fetch_api(&format!("{base}/search?q={q}&format=json")).await
        && let Ok(hits) = parse_json(&page.body, limit)
        && !hits.is_empty()
    {
        return Ok(hits);
    }

    let page = fetcher.fetch_api(&format!("{base}/search?q={q}")).await?;
    let hits = parse_html(&page.body, limit);
    if hits.is_empty() {
        return Err(Error::provider(
            "searxng",
            format!("{base} returned neither JSON nor a parseable result list"),
        ));
    }
    Ok(hits)
}

/// Parse the `format=json` response.
pub(crate) fn parse_json(body: &str, limit: usize) -> Result<Vec<RawHit>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| Error::provider("searxng", format!("bad json: {e}")))?;
    let results = value
        .get("results")
        .and_then(|r| r.as_array())
        .ok_or_else(|| Error::provider("searxng", "response had no results array"))?;
    Ok(results
        .iter()
        .filter_map(|r| {
            let url = r.get("url")?.as_str()?.to_string();
            let title = r.get("title").and_then(|t| t.as_str()).unwrap_or_default().to_string();
            let snippet = r.get("content").and_then(|c| c.as_str()).unwrap_or_default().to_string();
            Some(RawHit { title, url, snippet })
        })
        .take(limit)
        .collect())
}

/// Parse the rendered result list, for instances with the API disabled.
pub(crate) fn parse_html(html: &str, limit: usize) -> Vec<RawHit> {
    let Ok(doc) = Doc::parse(html) else { return Vec::new() };
    let mut out = Vec::new();

    for &id in &doc.preorder {
        if doc.tag_name(id) != "article" || !has_class(&doc, id, "result") {
            continue;
        }
        let link = doc.descendants(id).into_iter().find(|&d| {
            doc.tag_name(d) == "a"
                && has_class(&doc, d, "url_wrapper")
                && doc.attr(d, "href").is_some()
        });
        let heading = doc
            .descendants(id)
            .into_iter()
            .find(|&d| doc.tag_name(d) == "h3")
            .and_then(|h| doc.descendants(h).into_iter().find(|&d| doc.tag_name(d) == "a"));

        let anchor = heading.or(link);
        let Some(anchor) = anchor else { continue };
        let Some(url) = doc.attr(anchor, "href").filter(|h| h.starts_with("http")) else {
            continue;
        };
        let title = text::normalize_ws(&doc.inner_text(anchor));
        let snippet = doc
            .descendants(id)
            .into_iter()
            .find(|&d| has_class(&doc, d, "content"))
            .map(|d| text::normalize_ws(&doc.inner_text(d)))
            .unwrap_or_default();
        out.push(RawHit { title, url, snippet });
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn has_class(doc: &Doc<'_>, id: Id, needle: &str) -> bool {
    doc.attr(id, "class").is_some_and(|c| c.split_whitespace().any(|c| c == needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_json_api() {
        let body = r#"{"results":[
            {"url":"https://a.dev/1","title":"A","content":"first"},
            {"url":"https://b.dev/2","title":"B","content":"second"}]}"#;
        let hits = parse_json(body, 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://a.dev/1");
        assert_eq!(hits[1].snippet, "second");
        assert_eq!(parse_json(body, 1).unwrap().len(), 1);
    }

    #[test]
    fn rejects_a_non_searxng_json_body() {
        assert!(parse_json("{\"oops\":1}", 5).is_err());
        assert!(parse_json("<html>", 5).is_err());
    }

    #[test]
    fn parses_the_rendered_result_list() {
        let html = r#"<html><body>
          <article class="result result-default">
            <a href="https://a.dev/1" class="url_wrapper"><span>a.dev</span></a>
            <h3><a href="https://a.dev/1">First result</a></h3>
            <p class="content">Snippet text here.</p>
          </article>
          <article class="result"><h3><a href="/local">skip me</a></h3></article>
        </body></html>"#;
        let hits = parse_html(html, 10);
        assert_eq!(hits.len(), 1, "{hits:#?}");
        assert_eq!(hits[0].url, "https://a.dev/1");
        assert_eq!(hits[0].title, "First result");
        assert_eq!(hits[0].snippet, "Snippet text here.");
    }
}
