//! HTML → denoised Markdown units.
//!
//! [`extract`] is the whole parsing story: parse once with `tl`, index the
//! arena, pick the content root, and write Markdown while dropping boilerplate.
//! It never touches the network and never allocates a second copy of the
//! document, so it is cheap enough to run over a whole result set in parallel
//! via [`extract_many`].

pub(crate) mod dom;
mod markdown;
mod meta;

use rayon::prelude::*;
use url::Url;

use crate::denoise::{DenoiseConfig, DenoiseStats};
use crate::error::Result;
use crate::text;

pub use markdown::RenderOptions;
pub use meta::Meta;

/// What kind of block a [`Unit`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    /// `<h1>` … `<h6>`.
    Heading,
    /// A paragraph or other prose block.
    Paragraph,
    /// One `<li>`.
    ListItem,
    /// A fenced code block.
    Code,
    /// A `<blockquote>`.
    Quote,
    /// A Markdown pipe table.
    Table,
}

impl UnitKind {
    /// Weight applied when ranking, before relevance.
    ///
    /// Headings are short but carry the document's structure, so they punch
    /// above their token count; boilerplate-prone list items punch below.
    pub(crate) fn prior(self) -> f32 {
        match self {
            UnitKind::Heading => 1.25,
            UnitKind::Paragraph => 1.0,
            UnitKind::Code => 0.95,
            UnitKind::Table => 0.9,
            UnitKind::Quote => 0.85,
            UnitKind::ListItem => 0.8,
        }
    }
}

/// One addressable block of an article.
///
/// This is the unit the context slimmer ranks, selects and emits, so it carries
/// both its rendered Markdown and the plain text used for scoring.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Unit {
    /// Block type.
    pub kind: UnitKind,
    /// Heading level (1–6), or list nesting depth.
    pub level: u8,
    /// Plain text, with Markdown syntax removed.
    pub text: String,
    /// Rendered Markdown for this block.
    pub markdown: String,
    /// Enclosing headings, outermost first — the breadcrumb a model needs to
    /// make sense of a block lifted out of its document.
    pub heading_path: Vec<String>,
    /// Index in document order.
    pub position: usize,
    /// Estimated LLM tokens for [`Unit::markdown`].
    pub tokens: usize,
}

/// A cleaned document.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Article {
    /// Source URL, if one was supplied.
    pub url: Option<String>,
    /// Metadata read from `<head>`.
    pub meta: Meta,
    /// The full cleaned document as Markdown.
    pub markdown: String,
    /// The full cleaned document as plain text.
    pub text: String,
    /// The Markdown split into rankable blocks.
    pub units: Vec<Unit>,
    /// What the denoiser dropped.
    pub stats: DenoiseStats,
}

impl Article {
    /// Estimated LLM tokens for the whole article.
    pub fn tokens(&self) -> usize {
        self.units.iter().map(|u| u.tokens).sum()
    }

    /// Title, falling back to the first heading.
    pub fn title(&self) -> Option<&str> {
        self.meta
            .title
            .as_deref()
            .or_else(|| {
                self.units.iter().find(|u| u.kind == UnitKind::Heading).map(|u| u.text.as_str())
            })
            .filter(|t| !t.is_empty())
    }
}

/// Knobs for [`extract`].
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Denoiser tuning.
    pub denoise: DenoiseConfig,
    /// Markdown rendering tuning.
    pub render: RenderOptions,
    /// If the chosen content root yields fewer than this many characters, redo
    /// the extraction from `<body>`. Guards against a scoring miss on pages
    /// with unusual markup.
    pub min_content_chars: usize,
}

impl ExtractOptions {
    /// Defaults tuned for article-shaped pages.
    pub fn new() -> Self {
        ExtractOptions {
            denoise: DenoiseConfig::default(),
            render: RenderOptions::default(),
            min_content_chars: 200,
        }
    }
}

/// Turn a raw HTML document into denoised Markdown units.
///
/// `url` is used to absolutise relative links and is echoed back on the
/// [`Article`]; pass `None` for standalone fragments.
pub fn extract(html: &str, url: Option<&str>) -> Result<Article> {
    extract_with(html, url, &ExtractOptions::new())
}

/// [`extract`] with explicit options.
pub fn extract_with(html: &str, url: Option<&str>, opts: &ExtractOptions) -> Result<Article> {
    let doc = dom::Doc::parse(html)?;
    let meta = meta::extract(&doc);

    let base = url
        .and_then(|u| Url::parse(u).ok())
        .or_else(|| meta.canonical.as_deref().and_then(|c| Url::parse(c).ok()));

    let body = doc
        .preorder
        .iter()
        .copied()
        .find(|&id| doc.tag_name(id) == "body")
        .or_else(|| doc.preorder.first().copied());

    let root = crate::denoise::find_content_root(&doc, &opts.denoise).or(body);
    let Some(root) = root else {
        return Ok(Article {
            url: url.map(str::to_string),
            meta,
            markdown: String::new(),
            text: String::new(),
            units: Vec::new(),
            stats: DenoiseStats { html_bytes: html.len(), ..Default::default() },
        });
    };

    let mut writer = markdown::Writer::new(&doc, &opts.denoise, &opts.render, base.clone());
    writer.walk(root);

    // Scoring can land on a sidebar when the real body is unusually structured.
    // Retrying from `<body>` costs one more walk and rescues those pages.
    let thin = writer.units.iter().map(|u| text::visible_len(&u.text)).sum::<usize>()
        < opts.min_content_chars;
    if thin
        && let Some(body) = body
        && body != root
    {
        let mut fallback = markdown::Writer::new(&doc, &opts.denoise, &opts.render, base);
        fallback.walk(body);
        let fallback_len: usize = fallback.units.iter().map(|u| text::visible_len(&u.text)).sum();
        let current_len: usize = writer.units.iter().map(|u| text::visible_len(&u.text)).sum();
        if fallback_len > current_len {
            writer = fallback;
        }
    }

    let mut units = writer.units;
    for (i, u) in units.iter_mut().enumerate() {
        u.position = i;
    }
    let markdown = units.iter().map(|u| u.markdown.as_str()).collect::<Vec<_>>().join("\n\n");
    let text = units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join("\n");

    let mut stats = writer.stats;
    // Everything outside the chosen content root was removed just as surely as
    // a node the noise rules rejected, and it is usually the larger share.
    let total_nodes = doc.preorder.len();
    let outside_root = total_nodes.saturating_sub(doc.descendants(root).len());
    stats.nodes_visited = total_nodes;
    stats.nodes_dropped = (stats.nodes_dropped + outside_root).min(total_nodes);
    stats.html_bytes = html.len();
    stats.markdown_bytes = markdown.len();

    Ok(Article { url: url.map(str::to_string), meta, markdown, text, units, stats })
}

/// Extract many documents in parallel across the rayon pool.
///
/// Parsing is CPU-bound and per-document independent, which is exactly the
/// shape rayon is for: a 20-result search collapses to roughly one document's
/// worth of wall time on a multi-core machine.
pub fn extract_many<'a, I>(docs: I, opts: &ExtractOptions) -> Vec<Result<Article>>
where
    I: IntoIterator<Item = (&'a str, Option<&'a str>)>,
    I::IntoIter: Send,
{
    let docs: Vec<_> = docs.into_iter().collect();
    docs.into_par_iter().map(|(html, url)| extract_with(html, url, opts)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"<html lang="en"><head><title>Zero-cost crawling</title></head><body>
      <nav><a href="/a">A</a><a href="/b">B</a><a href="/c">C</a><a href="/d">D</a></nav>
      <main><article class="post">
        <h1>Zero-cost crawling</h1>
        <p>Python crawlers pay for a DOM they never asked for, and it shows in RSS, in latency, and in bills.</p>
        <h2>Why Rust</h2>
        <p>An arena parser, a work-stealing pool, and no garbage collector at all: the memory ceiling is a design choice.</p>
        <ul><li>Low memory</li><li>Fast parsing</li></ul>
        <pre><code class="language-rust">let x = 1;</code></pre>
        <table><tr><th>Tool</th><th>RSS</th></tr><tr><td>rustai</td><td>28 MB</td></tr></table>
      </article></main>
      <footer class="footer">© 2026 Example, all rights reserved, contact us</footer>
    </body></html>"##;

    #[test]
    fn extracts_structure_and_drops_chrome() {
        let art = extract(PAGE, Some("https://example.com/post")).unwrap();
        assert_eq!(art.title(), Some("Zero-cost crawling"));
        assert!(art.markdown.contains("# Zero-cost crawling"));
        assert!(art.markdown.contains("## Why Rust"));
        assert!(art.markdown.contains("- Low memory"));
        assert!(art.markdown.contains("```rust\nlet x = 1;\n```"));
        assert!(art.markdown.contains("| Tool | RSS |"));
        assert!(!art.text.contains("all rights reserved"), "footer survived:\n{}", art.markdown);
        assert!(!art.markdown.contains("](/a)"), "nav survived");
    }

    #[test]
    fn units_carry_their_heading_path() {
        let art = extract(PAGE, None).unwrap();
        let para = art
            .units
            .iter()
            .find(|u| u.text.starts_with("An arena parser"))
            .expect("second paragraph");
        assert_eq!(para.heading_path, ["Zero-cost crawling", "Why Rust"]);
        assert!(para.tokens > 0);
    }

    #[test]
    fn relative_links_are_absolutised() {
        let html = r#"<article><p>Read the <a href="/docs/guide">guide</a> for the full walkthrough of the pipeline.</p></article>"#;
        let art = extract(html, Some("https://example.com/blog/post")).unwrap();
        assert!(art.markdown.contains("(https://example.com/docs/guide)"), "{}", art.markdown);
    }

    #[test]
    fn compression_is_reported() {
        let art = extract(PAGE, None).unwrap();
        assert!(art.stats.compression() > 0.3, "only {}", art.stats.compression());
        assert!(art.tokens() > 0);
    }

    #[test]
    fn dropped_nodes_include_everything_outside_the_content_root() {
        let art = extract(PAGE, None).unwrap();
        // The nav and footer live outside the article and must still be counted.
        assert!(art.stats.nodes_dropped > 0, "{:?}", art.stats);
        assert!(art.stats.nodes_dropped < art.stats.nodes_visited);
    }

    #[test]
    fn empty_input_is_not_an_error() {
        let art = extract("", None).unwrap();
        assert!(art.units.is_empty());
    }

    #[test]
    fn many_documents_extract_in_parallel() {
        let inputs: Vec<(&str, Option<&str>)> = (0..8).map(|_| (PAGE, None)).collect();
        let out = extract_many(inputs, &ExtractOptions::new());
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|r| r.as_ref().is_ok_and(|a| !a.units.is_empty())));
    }
}
