//! HTML → denoised Markdown units.
//!
//! [`extract`] is the whole parsing story: parse once with `tl`, index the
//! arena, pick the content root, and write Markdown while dropping boilerplate.
//! It never touches the network and never allocates a second copy of the
//! document, so it is cheap enough to run over a whole result set in parallel
//! via [`extract_many`].

pub(crate) mod dom;
mod index;
mod markdown;
mod meta;

use rayon::prelude::*;
use url::Url;

use crate::denoise::{DenoiseConfig, DenoiseStats};

/// Below this much body text, the ratio check is noise — short pages routinely
/// extract a small absolute number of characters and are perfectly fine.
const MIN_BODY_FOR_RATIO_CHECK: usize = 2_000;

/// Extracting less than this share of the document's visible text means the
/// content root is almost certainly wrong.
const STARVED_PERCENT: usize = 5;

use crate::error::Result;
use crate::text;

pub use index::Link;
pub use markdown::RenderOptions;
pub use meta::Meta;

/// Whether a page turned out to be an article or a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArticleKind {
    /// Prose: the usual case.
    Article,
    /// A front page, feed or archive, where the link list is the content.
    Index,
}

/// When to fall back to index-page extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexMode {
    /// Try it only when article extraction came up empty. The default.
    #[default]
    Auto,
    /// Never; a page with no prose extracts to nothing.
    Never,
    /// Always harvest links, even from a page that did yield an article.
    Always,
}

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
    /// Whether this page read as prose or as a listing.
    pub kind: ArticleKind,
    /// Metadata read from `<head>`.
    pub meta: Meta,
    /// The full cleaned document as Markdown.
    pub markdown: String,
    /// The full cleaned document as plain text.
    pub text: String,
    /// The Markdown split into rankable blocks.
    pub units: Vec<Unit>,
    /// Links harvested from a listing page. Empty for ordinary articles unless
    /// [`IndexMode::Always`] was requested.
    pub links: Vec<Link>,
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
    /// When to fall back to harvesting links instead of prose.
    pub index_mode: IndexMode,
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
            index_mode: IndexMode::default(),
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
            kind: ArticleKind::Article,
            meta,
            markdown: String::new(),
            text: String::new(),
            units: Vec::new(),
            links: Vec::new(),
            stats: DenoiseStats { html_bytes: html.len(), ..Default::default() },
        });
    };

    let mut writer = markdown::Writer::new(&doc, &opts.denoise, &opts.render, base.clone());
    writer.walk(root);

    // Content-root scoring can land in the wrong subtree — on a sidebar, or
    // inside a maintenance banner that a lenient parser accidentally nested the
    // whole article into. Two signals say we got it wrong: almost nothing came
    // out at all, or what came out is a rounding error next to the text the
    // document actually contains.
    let extracted = |w: &markdown::Writer<'_, '_>| -> usize {
        w.units.iter().map(|u| text::visible_len(&u.text)).sum()
    };
    let current_len = extracted(&writer);
    let body_text = body.map(|b| doc.text_len(b)).unwrap_or(0) as usize;
    let starved =
        body_text > MIN_BODY_FOR_RATIO_CHECK && current_len * 100 < body_text * STARVED_PERCENT;

    if (current_len < opts.min_content_chars || starved)
        && let Some(body) = body
        && body != root
    {
        let mut fallback = markdown::Writer::new(&doc, &opts.denoise, &opts.render, base.clone());
        fallback.walk(body);
        let fallback_len = extracted(&fallback);
        // Walking `<body>` always sweeps up more boilerplate, so it only wins
        // when it is dramatically better — not merely bigger.
        let decisively_better = fallback_len > current_len.saturating_mul(3);
        if fallback_len > current_len && (current_len < opts.min_content_chars || decisively_better)
        {
            writer = fallback;
        }
    }

    // A page with no prose may still be a listing, where the links are the
    // content rather than the boilerplate. Harvesting only runs once article
    // extraction has already failed, so ordinary pages never take this path.
    let mut kind = ArticleKind::Article;
    let mut links = Vec::new();
    let article_chars = extracted(&writer);
    if opts.index_mode != IndexMode::Never && index::could_be_index(&doc, article_chars) {
        let harvested = index::harvest(&doc, base.as_ref(), &opts.denoise);
        if index::looks_like_index(&harvested, article_chars) {
            kind = ArticleKind::Index;
            writer.units = index::units_from_links(&harvested);
            links = harvested;
        } else if opts.index_mode == IndexMode::Always {
            links = harvested;
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

    Ok(Article { url: url.map(str::to_string), kind, meta, markdown, text, units, links, stats })
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
    fn an_unclosed_paragraph_does_not_swallow_the_document() {
        // HTML implies `</p>` before a block-level start tag. Lenient parsers
        // do not insert it, so without a guard the whole page becomes one
        // paragraph. This is the shape real pages hit it with.
        let html = "<article><p>Intro sentence that runs on                    <h2>A real heading</h2>                    <p>A second paragraph with enough words in it to be kept.</p>                    <h2>Another heading</h2>                    <p>A third paragraph, also long enough to survive the filter.</p>                    </article>";
        let art = extract(html, None).unwrap();
        assert!(
            art.units.len() >= 4,
            "collapsed into {} units:\n{}",
            art.units.len(),
            art.markdown
        );
        assert!(art.markdown.contains("## A real heading"));
        assert!(art.markdown.contains("## Another heading"));
        let biggest = art.units.iter().map(|u| u.tokens).max().unwrap();
        assert!(biggest < 60, "one unit swallowed the rest: {biggest} tokens");
    }

    #[test]
    fn an_inline_element_holding_blocks_does_not_flatten_the_page() {
        // The shape an unclosed `<span>` produces: an inline tag that has taken
        // ownership of the rest of the document. Flattening it would emit the
        // whole page as a single paragraph with no headings at all.
        //
        // The filler is sized past `INLINE_INSPECT_BYTES` on purpose. Below
        // that the guard deliberately does not run: a sub-2 KB span that
        // flattens is a cosmetic wrinkle, while the descendant walk on every
        // small `<a>` and `<strong>` would be a real cost on every document.
        let filler = "Real prose that carries the paragraph past the length filter. ".repeat(20);
        let html = format!(
            "<article><div class=\"notice\"><span>Notice text.\
             <h2>First section</h2><p>{filler}</p>\
             <h2>Second section</h2><p>{filler}</p></span></div></article>"
        );
        let art = extract(&html, None).unwrap();
        assert!(art.markdown.contains("## First section"), "headings lost:\n{}", art.markdown);
        assert!(art.markdown.contains("## Second section"));
        // Two headings and two paragraphs, not one merged blob.
        assert!(
            art.units.len() >= 4,
            "collapsed into {} units:\n{}",
            art.units.len(),
            art.markdown
        );
    }

    #[test]
    fn a_list_item_holding_blocks_keeps_their_structure() {
        let prose = "Real prose inside a list item, long enough to survive the filter. ";
        let html = format!(
            "<article><ul><li>Bullet text\
             <h2>A heading inside the item</h2><p>{prose}</p>\
             <ul><li>Nested bullet</li></ul></li></ul></article>"
        );
        let art = extract(&html, None).unwrap();
        assert!(art.markdown.contains("- Bullet text"));
        assert!(art.markdown.contains("## A heading inside the item"), "{}", art.markdown);
        assert!(art.markdown.contains("- Nested bullet"));
        let biggest = art.units.iter().map(|u| u.tokens).max().unwrap();
        assert!(biggest < 40, "the item swallowed its blocks: {biggest} tokens");
    }

    #[test]
    fn small_inline_elements_stay_inline() {
        let html = "<article><p>A sentence with <strong>bold</strong> and                     <em>italic</em> and <code>code</code> in it, long enough to keep.</p></article>";
        let art = extract(html, None).unwrap();
        assert_eq!(art.units.len(), 1, "inline runs were split:\n{}", art.markdown);
        assert!(art.markdown.contains("**bold**"));
        assert!(art.markdown.contains("*italic*"));
        assert!(art.markdown.contains("`code`"));
    }

    #[test]
    fn a_layout_table_is_walked_not_tabulated() {
        // Navboxes, page shells and unclosed tables all look like this: block
        // content inside cells. Rendering it as a pipe table would produce one
        // giant unreadable row.
        let html = "<table><tr><td>                    <h1>Page title</h1>                    <p>A paragraph of real prose that is long enough to keep around.</p>                    <h2>Section</h2>                    <p>Another paragraph of real prose, also long enough to keep.</p>                    </td></tr></table>";
        let art = extract(html, None).unwrap();
        assert!(!art.units.iter().any(|u| u.kind == UnitKind::Table), "{}", art.markdown);
        assert!(art.markdown.contains("# Page title"));
        assert!(art.markdown.contains("## Section"));
    }

    #[test]
    fn a_real_data_table_is_still_tabulated() {
        let html = "<article><p>Some prose introducing the numbers below it.</p>                    <table><tr><th>Engine</th><th>RSS</th></tr>                    <tr><td>arena</td><td>4.8 MiB</td></tr></table></article>";
        let art = extract(html, None).unwrap();
        assert!(art.markdown.contains("| Engine | RSS |"), "{}", art.markdown);
        assert!(art.units.iter().any(|u| u.kind == UnitKind::Table));
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
