//! Document metadata from `<head>`, Open Graph, Twitter cards and JSON-LD.

use crate::parse::dom::{Doc, Id};
use crate::text;

/// Everything we can learn about a page without reading its body.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Meta {
    /// Best available title.
    pub title: Option<String>,
    /// Meta description or `og:description`.
    pub description: Option<String>,
    /// Author, if the page declares one.
    pub byline: Option<String>,
    /// Publication timestamp as the page states it (usually ISO 8601).
    pub published: Option<String>,
    /// BCP-47 language tag from `<html lang>` or `og:locale`.
    pub language: Option<String>,
    /// `<link rel="canonical">`.
    pub canonical: Option<String>,
    /// `og:site_name`.
    pub site_name: Option<String>,
}

/// Read metadata out of a parsed document.
pub(crate) fn extract(doc: &Doc<'_>) -> Meta {
    let mut meta = Meta::default();

    for &id in &doc.preorder {
        match doc.tag_name(id).as_str() {
            "html" => {
                meta.language = meta.language.take().or_else(|| clean(doc.attr(id, "lang")));
            }
            "title" if meta.title.is_none() => {
                meta.title = clean(Some(doc.raw_text(id)));
            }
            "link" => {
                if doc.attr(id, "rel").as_deref().map(str::trim) == Some("canonical") {
                    meta.canonical = meta.canonical.take().or_else(|| clean(doc.attr(id, "href")));
                }
            }
            "meta" => apply_meta_tag(doc, id, &mut meta),
            "script" if doc.attr(id, "type").as_deref() == Some("application/ld+json") => {
                apply_json_ld(&doc.raw_text(id), &mut meta);
            }
            _ => {}
        }
    }

    // A `<title>` is usually "Headline — Site Name"; prefer og:title, which
    // normally is not decorated, and otherwise trim the site suffix.
    if let (Some(title), Some(site)) = (meta.title.clone(), meta.site_name.clone())
        && let Some(stripped) = strip_site_suffix(&title, &site)
    {
        meta.title = Some(stripped);
    }
    meta
}

fn apply_meta_tag(doc: &Doc<'_>, id: Id, meta: &mut Meta) {
    let key = doc
        .attr(id, "property")
        .or_else(|| doc.attr(id, "name"))
        .or_else(|| doc.attr(id, "itemprop"))
        .map(|k| k.trim().to_ascii_lowercase());
    let Some(key) = key else { return };
    let Some(value) = clean(doc.attr(id, "content")) else { return };

    match key.as_str() {
        "og:title" | "twitter:title" => meta.title = Some(value),
        "og:description" | "twitter:description" | "description" => {
            meta.description.get_or_insert(value);
        }
        "og:site_name" => meta.site_name = Some(value),
        "og:locale" => {
            meta.language.get_or_insert(value.replace('_', "-"));
        }
        "og:url" => {
            meta.canonical.get_or_insert(value);
        }
        "author" | "article:author" | "dc.creator" | "citation_author" | "byl" => {
            meta.byline.get_or_insert(value);
        }
        "article:published_time"
        | "datepublished"
        | "dc.date"
        | "date"
        | "citation_publication_date"
        | "pubdate" => {
            meta.published.get_or_insert(value);
        }
        _ => {}
    }
}

/// Pull the handful of fields we care about out of schema.org JSON-LD.
fn apply_json_ld(raw: &str, meta: &mut Meta) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw.trim()) else { return };
    let mut queue = vec![value];
    while let Some(node) = queue.pop() {
        match node {
            serde_json::Value::Array(items) => queue.extend(items),
            serde_json::Value::Object(map) => {
                if let Some(graph) = map.get("@graph") {
                    queue.push(graph.clone());
                }
                if let Some(v) = map.get("headline").and_then(|v| v.as_str()) {
                    meta.title.get_or_insert_with(|| v.to_string());
                }
                if let Some(v) = map.get("datePublished").and_then(|v| v.as_str()) {
                    meta.published.get_or_insert_with(|| v.to_string());
                }
                if let Some(v) = map.get("description").and_then(|v| v.as_str()) {
                    meta.description.get_or_insert_with(|| v.to_string());
                }
                match map.get("author") {
                    Some(serde_json::Value::String(s)) => {
                        meta.byline.get_or_insert_with(|| s.clone());
                    }
                    Some(serde_json::Value::Object(a)) => {
                        if let Some(n) = a.get("name").and_then(|v| v.as_str()) {
                            meta.byline.get_or_insert_with(|| n.to_string());
                        }
                    }
                    Some(serde_json::Value::Array(a)) => {
                        let names: Vec<String> = a
                            .iter()
                            .filter_map(|v| {
                                v.as_str()
                                    .map(str::to_string)
                                    .or_else(|| v.get("name")?.as_str().map(str::to_string))
                            })
                            .collect();
                        if !names.is_empty() {
                            meta.byline.get_or_insert_with(|| names.join(", "));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// `"Headline - Example News"` with site `"Example News"` becomes `"Headline"`.
fn strip_site_suffix(title: &str, site: &str) -> Option<String> {
    let site = site.trim();
    if site.is_empty() || title.len() <= site.len() {
        return None;
    }
    for sep in [" - ", " | ", " — ", " – ", " :: ", " · "] {
        // Two grapheme clusters, not two words: a Korean or Chinese headline is
        // routinely shorter than any sensible Latin-script minimum.
        if let Some(head) = title.strip_suffix(&format!("{sep}{site}"))
            && text::visible_len(head.trim()) >= 2
        {
            return Some(head.trim().to_string());
        }
    }
    None
}

fn clean(v: Option<String>) -> Option<String> {
    let v = text::normalize_ws(&v?);
    if v.is_empty() { None } else { Some(v) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_head_and_open_graph() {
        let html = r#"<html lang="ko"><head>
            <title>제목 - 예시 뉴스</title>
            <meta property="og:site_name" content="예시 뉴스">
            <meta name="description" content="설명입니다">
            <meta property="article:published_time" content="2026-01-02T03:04:05Z">
            <link rel="canonical" href="https://example.com/a">
        </head><body><p>본문</p></body></html>"#;
        let doc = Doc::parse(html).unwrap();
        let meta = extract(&doc);
        assert_eq!(meta.title.as_deref(), Some("제목"));
        assert_eq!(meta.language.as_deref(), Some("ko"));
        assert_eq!(meta.description.as_deref(), Some("설명입니다"));
        assert_eq!(meta.published.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert_eq!(meta.canonical.as_deref(), Some("https://example.com/a"));
    }

    #[test]
    fn reads_json_ld_author() {
        let html = r#"<html><head><script type="application/ld+json">
            {"@type":"Article","headline":"H","author":{"name":"Ada Lovelace"}}
        </script></head><body></body></html>"#;
        let doc = Doc::parse(html).unwrap();
        let meta = extract(&doc);
        assert_eq!(meta.byline.as_deref(), Some("Ada Lovelace"));
    }
}
