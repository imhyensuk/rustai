//! Providers whose subject is a conversation rather than a document.
//!
//! A question with three answers under it, a thread arguing about a release,
//! a repository's own description of itself: none of these are articles, and
//! all of them answer questions that articles do not. Each is a keyless JSON
//! API, so the cost of asking is one request.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::http::Fetcher;
use crate::search::RawHit;
use crate::text;

/// Longest snippet kept from any of these providers.
const MAX_SNIPPET_CHARS: usize = 900;

/// Hacker News, through the Algolia index that powers its own search.
///
/// Two kinds of hit come back and they need different treatment: a story
/// links somewhere else, and its `url` is the thing worth reading; a `Show
/// HN` or `Ask HN` post has no `url` and its text *is* the content, so the
/// discussion page is the destination.
pub(crate) async fn hacker_news(
    fetcher: &Fetcher,
    query: &str,
    limit: usize,
) -> Result<Vec<RawHit>> {
    let q = utf8_percent_encode(query, NON_ALPHANUMERIC);
    let url = format!("https://hn.algolia.com/api/v1/search?query={q}&hitsPerPage={limit}");
    let page = fetcher.fetch_api(&url).await?;
    parse_hacker_news(&page.body, limit)
}

pub(crate) fn parse_hacker_news(body: &str, limit: usize) -> Result<Vec<RawHit>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| Error::provider("hackernews", format!("bad json: {e}")))?;
    let hits = value
        .get("hits")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::provider("hackernews", "response had no hits array"))?;

    Ok(hits
        .iter()
        .filter_map(|hit| {
            let title = string(hit, "title").or_else(|| string(hit, "story_title"))?;
            let id = string(hit, "objectID")?;
            let discussion = format!("https://news.ycombinator.com/item?id={id}");
            let url = string(hit, "url").unwrap_or_else(|| discussion.clone());
            let mut parts = Vec::new();
            if let Some(points) = hit.get("points").and_then(Value::as_u64) {
                let comments = hit.get("num_comments").and_then(Value::as_u64).unwrap_or(0);
                parts.push(format!("{points} points, {comments} comments"));
            }
            if let Some(text) = string(hit, "story_text").or_else(|| string(hit, "comment_text")) {
                parts.push(text::normalize_ws(&strip_tags(&text)));
            }
            if url != discussion {
                parts.push(format!("Discussion: {discussion}"));
            }
            Some(RawHit { title, url, snippet: clamp(&parts.join(" — ")) })
        })
        .take(limit)
        .collect())
}

/// Stack Exchange search, over one site — `stackoverflow` by default.
///
/// The `withbody` filter is what makes this worth querying: without it the
/// API returns titles and scores, which rank a result but do not answer
/// anything.
pub(crate) async fn stack_exchange(
    fetcher: &Fetcher,
    site: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<RawHit>> {
    let q = utf8_percent_encode(query, NON_ALPHANUMERIC);
    let site = utf8_percent_encode(site, NON_ALPHANUMERIC);
    let url = format!(
        "https://api.stackexchange.com/2.3/search/advanced?order=desc&sort=relevance\
         &q={q}&site={site}&pagesize={limit}&filter=withbody"
    );
    let page = fetcher.fetch_api(&url).await?;
    parse_stack_exchange(&page.body, limit)
}

pub(crate) fn parse_stack_exchange(body: &str, limit: usize) -> Result<Vec<RawHit>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| Error::provider("stackexchange", format!("bad json: {e}")))?;
    if let Some(message) = value.get("error_message").and_then(Value::as_str) {
        return Err(Error::provider("stackexchange", message));
    }
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::provider("stackexchange", "response had no items array"))?;

    Ok(items
        .iter()
        .filter_map(|item| {
            let title = string(item, "title")?;
            let url = string(item, "link")?;
            let mut parts = Vec::new();
            let answers = item.get("answer_count").and_then(Value::as_u64).unwrap_or(0);
            let answered = item.get("is_answered").and_then(Value::as_bool).unwrap_or(false);
            parts.push(format!(
                "{answers} answer{}{}",
                if answers == 1 { "" } else { "s" },
                if answered { ", accepted" } else { "" }
            ));
            if let Some(tags) = item.get("tags").and_then(Value::as_array) {
                let tags: Vec<_> = tags.iter().filter_map(Value::as_str).take(5).collect();
                if !tags.is_empty() {
                    parts.push(tags.join(", "));
                }
            }
            if let Some(text) = string(item, "body") {
                parts.push(text::normalize_ws(&strip_tags(&text)));
            }
            Some(RawHit { title: strip_tags(&title), url, snippet: clamp(&parts.join(" — ")) })
        })
        .take(limit)
        .collect())
}

/// GitHub repository search.
///
/// Unauthenticated search is capped at ten requests a minute, which one query
/// per search comfortably fits. A repository is not prose, so the snippet is
/// the description plus the two facts that decide whether it is worth
/// opening: what it is written in, and whether anyone uses it.
pub(crate) async fn github(fetcher: &Fetcher, query: &str, limit: usize) -> Result<Vec<RawHit>> {
    let q = utf8_percent_encode(query, NON_ALPHANUMERIC);
    let url = format!("https://api.github.com/search/repositories?q={q}&per_page={limit}");
    let page = fetcher.fetch_api(&url).await?;
    parse_github(&page.body, limit)
}

pub(crate) fn parse_github(body: &str, limit: usize) -> Result<Vec<RawHit>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| Error::provider("github", format!("bad json: {e}")))?;
    if value.get("items").is_none()
        && let Some(message) = value.get("message").and_then(Value::as_str)
    {
        return Err(Error::provider("github", message));
    }
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::provider("github", "response had no items array"))?;

    Ok(items
        .iter()
        .filter_map(|item| {
            let title = string(item, "full_name")?;
            let url = string(item, "html_url")?;
            let mut parts = Vec::new();
            if let Some(description) = string(item, "description") {
                parts.push(description);
            }
            let stars = item.get("stargazers_count").and_then(Value::as_u64).unwrap_or(0);
            let language = string(item, "language").unwrap_or_else(|| "unknown".into());
            parts.push(format!("{language}, {stars} stars"));
            Some(RawHit { title, url, snippet: clamp(&parts.join(" — ")) })
        })
        .take(limit)
        .collect())
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty())
}

fn clamp(s: &str) -> String {
    s.chars().take(MAX_SNIPPET_CHARS).collect()
}

/// Strip HTML from a snippet these APIs return as rendered markup.
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
    crate::parse::dom::decode_entities(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A story links elsewhere; a `Show HN` has no `url` of its own and the
    /// discussion page is the only place to read it.
    #[test]
    fn hacker_news_falls_back_to_the_discussion() {
        let body = r#"{"hits":[
            {"title":"A search engine in 80 lines","url":"https://example.com/se",
             "objectID":"39301940","points":412,"num_comments":93},
            {"title":"Show HN: my ranker","objectID":"45684443","points":13,
             "num_comments":2,"story_text":"<p>I built <b>this</b> &amp; more</p>"}
        ]}"#;
        let hits = parse_hacker_news(body, 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/se");
        assert!(hits[0].snippet.contains("412 points, 93 comments"));
        assert!(hits[0].snippet.contains("item?id=39301940"), "no route to the thread");

        assert_eq!(hits[1].url, "https://news.ycombinator.com/item?id=45684443");
        assert!(hits[1].snippet.contains("I built this & more"), "markup or entities survived");
        assert!(!hits[1].snippet.contains("Discussion:"), "pointed at itself");
    }

    #[test]
    fn stack_exchange_keeps_what_decides_whether_to_open_it() {
        let body = r#"{"items":[{"tags":["python","ranking"],"answer_count":2,
            "is_answered":true,"title":"How to use BM25 &amp; friends",
            "link":"https://stackoverflow.com/questions/1","body":"<p>I found <code>gensim</code></p>"}]}"#;
        let hits = parse_stack_exchange(body, 10).unwrap();
        assert_eq!(hits[0].title, "How to use BM25 & friends");
        assert!(hits[0].snippet.starts_with("2 answers, accepted"));
        assert!(hits[0].snippet.contains("python, ranking"));
        assert!(hits[0].snippet.contains("I found gensim"));
    }

    #[test]
    fn stack_exchange_reports_its_own_error() {
        let body = r#"{"error_id":400,"error_message":"site is required"}"#;
        assert!(parse_stack_exchange(body, 10).is_err());
    }

    #[test]
    fn github_snippet_answers_is_this_worth_opening() {
        let body = r#"{"items":[{"full_name":"dorianbrown/rank_bm25",
            "html_url":"https://github.com/dorianbrown/rank_bm25",
            "description":"A Collection of BM25 Algorithms in Python",
            "stargazers_count":1382,"language":"Python"}]}"#;
        let hits = parse_github(body, 10).unwrap();
        assert_eq!(hits[0].title, "dorianbrown/rank_bm25");
        assert!(hits[0].snippet.contains("Python, 1382 stars"));
    }

    /// Rate limiting arrives as a message with no items, not as a status the
    /// fetcher would have rejected.
    #[test]
    fn github_surfaces_a_rate_limit_as_an_error() {
        let body = r#"{"message":"API rate limit exceeded","documentation_url":"https://..."}"#;
        let err = parse_github(body, 10).unwrap_err().to_string();
        assert!(err.contains("rate limit"), "{err}");
    }

    #[test]
    fn a_hit_without_a_title_is_skipped_rather_than_faked() {
        let body = r#"{"hits":[{"url":"https://example.com","objectID":"1"}]}"#;
        assert!(parse_hacker_news(body, 10).unwrap().is_empty());
    }
}
