//! Wikipedia through the MediaWiki search API.
//!
//! The one provider here that is a real, documented, rate-limit-friendly API.
//! It is worth querying alongside a general web search because it reliably
//! supplies the definitional paragraph that web results assume you already have.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::error::{Error, Result};
use crate::http::Fetcher;
use crate::search::RawHit;
use crate::text;

/// Search one language edition.
pub(crate) async fn search(
    fetcher: &Fetcher,
    query: &str,
    lang: &str,
    limit: usize,
) -> Result<Vec<RawHit>> {
    let lang = sanitize_lang(lang)?;
    let q = utf8_percent_encode(query, NON_ALPHANUMERIC);
    let url = format!(
        "https://{lang}.wikipedia.org/w/api.php?action=query&list=search&srsearch={q}\
         &srlimit={limit}&format=json&formatversion=2&srprop=snippet"
    );
    let page = fetcher.fetch_api(&url).await?;
    parse(&page.body, &lang)
}

/// Parse a `list=search` response.
pub(crate) fn parse(body: &str, lang: &str) -> Result<Vec<RawHit>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| Error::provider("wikipedia", format!("bad json: {e}")))?;

    if let Some(err) = value.get("error").and_then(|e| e.get("info")).and_then(|i| i.as_str()) {
        return Err(Error::provider("wikipedia", err));
    }

    let items = value
        .get("query")
        .and_then(|q| q.get("search"))
        .and_then(|s| s.as_array())
        .ok_or_else(|| Error::provider("wikipedia", "response had no query.search array"))?;

    Ok(items
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?.to_string();
            let snippet =
                item.get("snippet").and_then(|s| s.as_str()).map(strip_tags).unwrap_or_default();
            Some(RawHit { url: article_url(lang, &title), title, snippet })
        })
        .collect())
}

/// Build the canonical article URL for a title.
fn article_url(lang: &str, title: &str) -> String {
    let slug = utf8_percent_encode(&title.replace(' ', "_"), NON_ALPHANUMERIC).to_string();
    // Underscores and the handful of characters MediaWiki leaves literal in
    // article paths should stay readable rather than be escaped.
    let slug = slug
        .replace("%2F", "/")
        .replace("%5F", "_")
        .replace("%3A", ":")
        .replace("%2D", "-")
        .replace("%2E", ".");
    format!("https://{lang}.wikipedia.org/wiki/{slug}")
}

/// Snippets come back with `<span class="searchmatch">` highlighting.
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

/// Language codes go straight into a hostname, so they get validated.
fn sanitize_lang(lang: &str) -> Result<String> {
    let lang = lang.trim().to_ascii_lowercase();
    let ok = !lang.is_empty()
        && lang.len() <= 12
        && lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if ok {
        Ok(lang)
    } else {
        Err(Error::Config(format!("invalid wikipedia language code `{lang}`")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"{"batchcomplete":true,"query":{"search":[
        {"title":"Rust (programming language)","snippet":"Rust is a <span class=\"searchmatch\">systems</span> language &amp; more"},
        {"title":"Tokio","snippet":"async runtime"}
    ]}}"#;

    #[test]
    fn parses_results_into_article_urls() {
        let hits = parse(BODY, "en").unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://en.wikipedia.org/wiki/Rust_%28programming_language%29");
        assert_eq!(hits[0].snippet, "Rust is a systems language & more");
        assert_eq!(hits[1].url, "https://en.wikipedia.org/wiki/Tokio");
    }

    #[test]
    fn korean_titles_survive_encoding() {
        let hits =
            parse(r#"{"query":{"search":[{"title":"러스트","snippet":""}]}}"#, "ko").unwrap();
        assert!(hits[0].url.starts_with("https://ko.wikipedia.org/wiki/%"));
        assert!(crate::http::normalize(&hits[0].url).is_ok());
    }

    #[test]
    fn api_errors_surface() {
        let err = parse(r#"{"error":{"info":"Invalid parameter"}}"#, "en").unwrap_err();
        assert!(err.to_string().contains("Invalid parameter"));
        assert!(parse("not json", "en").is_err());
        assert!(parse("{}", "en").is_err());
    }

    #[test]
    fn language_codes_are_validated() {
        assert!(sanitize_lang("ko").is_ok());
        assert!(sanitize_lang("zh-yue").is_ok());
        assert!(sanitize_lang("en.evil.com/").is_err());
        assert!(sanitize_lang("").is_err());
    }
}
