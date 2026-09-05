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

/// How much more text the container must hold than the chosen root before
/// the second walk is worth doing, as a percentage.
///
/// When root scoring worked, the root already covers nearly all of the
/// container and there is nothing to win — so this gate is what keeps the
/// comparison off the hot path. Measured over the same 34 pages, 120 keeps
/// every bit of the 20% gain that comparing unconditionally gives, and costs
/// nothing: 1,808 docs/s against 1,820 before, where comparing at 102 dropped
/// throughput to 1,250.
const COMPARE_GAP_PERCENT: u64 = 120;

use crate::denoise::{DenoiseConfig, DenoiseStats};

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
    // Two things `tl` gets wrong badly enough to lose the document, both of
    // which have to be repaired before parsing. Script and style bodies are
    // not treated as raw text, so a `<` in a comparison opens a phantom
    // element; and a `<` followed by punctuation opens one too, where HTML
    // says it is a bogus comment or not markup at all. Each pass borrows when
    // it finds nothing, so a clean document is never copied.
    let unraw = dom::neutralise_raw_text(html);
    let repaired = dom::neutralise_bogus_markup(&unraw);
    let html = repaired.as_ref();
    let doc = dom::Doc::parse(html)?;
    let meta = meta::extract(&doc);

    let base = url
        .and_then(|u| Url::parse(u).ok())
        .or_else(|| meta.canonical.as_deref().and_then(|c| Url::parse(c).ok()));

    // The container to fall back to when root scoring does badly. Normally
    // `<body>`, but a page whose markup strands its text outside `<body>` --
    // an unclosed tag earlier in the document -- needs the parse root instead,
    // or the fallback has nothing to offer.
    let body = {
        let doc_root = doc.preorder.first().copied();
        let body_tag = doc.preorder.iter().copied().find(|&id| doc.tag_name(id) == "body");
        match (body_tag, doc_root) {
            (Some(b), Some(r)) if doc.text_len(r) > doc.text_len(b).saturating_mul(2) => Some(r),
            (Some(b), _) => Some(b),
            (None, r) => r,
        }
    };

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
    writer.walk_root(root);

    // Content-root scoring can land in the wrong subtree — on a sidebar, on a
    // reference list, or inside a maintenance banner that a lenient parser
    // nested the whole article into. Rather than guess from thresholds whether
    // that happened, extract the container too and keep whichever yielded more.
    //
    // The old test asked whether the root was *starved* — under five percent
    // of the document's text. That catches a root that collapsed to nothing
    // and misses every root that merely stopped early, which is the common
    // failure: an encyclopaedia article split into sibling `<section>`s gives
    // a root holding its lead and nothing else, comfortably over the
    // threshold and comfortably wrong. Both walks run the same denoiser, so
    // the larger result is not the noisier one — measured over 34 pages this
    // gained 20% more text with nothing regressing.
    let extracted = |w: &markdown::Writer<'_, '_>| -> usize {
        w.units.iter().map(|u| text::visible_len(&u.text)).sum()
    };
    let current_len = extracted(&writer);

    // Nothing to gain when the root already covers the container's text, and
    // this is the common case — skipping it keeps the second walk off the hot
    // path for pages where root scoring did its job.
    let worth_comparing = body.is_some_and(|b| {
        b != root && doc.text_len(b) as u64 * 100 > doc.text_len(root) as u64 * COMPARE_GAP_PERCENT
    });

    if (current_len < opts.min_content_chars || worth_comparing)
        && let Some(body) = body
        && body != root
    {
        let mut fallback = markdown::Writer::new(&doc, &opts.denoise, &opts.render, base.clone());
        fallback.walk_root(body);
        let fallback_len = extracted(&fallback);
        if fallback_len > current_len {
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

#[cfg(test)]
mod superscript_tests {
    use super::*;

    fn md(fragment: &str) -> String {
        let html = format!("<article><p>{} {fragment}</p></article>", "Prose. ".repeat(12));
        extract(&html, None).unwrap().markdown
    }

    #[test]
    fn exponents_and_units_survive() {
        assert!(md("6.02 × 10<sup>23</sup> mol<sup>-1</sup>").contains("10^23 mol^-1"));
        assert!(md("area is 5 m<sup>2</sup>").contains("m^2"));
    }

    #[test]
    fn ordinals_read_naturally() {
        assert!(md("the 1<sup>st</sup> release").contains("1st release"));
    }

    #[test]
    fn citation_markers_are_dropped() {
        for marker in [
            r#"<sup class="reference">[1]</sup>"#,
            r##"<sup><a href="#cite_note-1">[12]</a></sup>"##,
            "<sup>[3]</sup>",
        ] {
            let out = md(&format!("A claim{marker} follows."));
            assert!(out.contains("A claim follows.") || out.contains("A claim  follows."), "{out}");
            assert!(!out.contains('['), "marker survived: {out}");
        }
    }

    #[test]
    fn attribute_entities_are_decoded() {
        let html = r#"<html><head><meta property="og:title" content="Steve Irwin&#x27;s family &amp; friends"></head>
            <body><article><p>Body text long enough to keep around here.</p></article></body></html>"#;
        let art = extract(html, None).unwrap();
        assert_eq!(art.meta.title.as_deref(), Some("Steve Irwin's family & friends"));
    }
}

#[cfg(test)]
mod table_regressions {
    use super::*;

    /// Wrap a table in enough prose that it is not the whole document, so the
    /// dominance rule cannot rescue it and the heuristics under test are the
    /// ones that decide.
    fn page(table: &str) -> String {
        format!(
            "<html><body><article><h1>Figures</h1><p>{}</p>{table}</article></body></html>",
            "A sentence of ordinary prose, with commas, to anchor the page. ".repeat(6)
        )
    }

    fn table_rows(html: &str) -> usize {
        let art = extract(html, Some("https://example.com/x")).expect("extract");
        art.markdown.lines().filter(|l| l.starts_with('|')).count()
    }

    const ROWS: &str = "<tr><th>Country</th><th>GDP</th></tr>\
                        <tr><td>Korea</td><td>1,870,000</td></tr>\
                        <tr><td>Japan</td><td>4,230,000</td></tr>";

    /// `header` is a chrome token, but `sticky-header-multi` is a data table
    /// describing its own headings — the class Wikipedia puts on every
    /// sortable table.
    #[test]
    fn component_scoped_header_is_not_page_chrome() {
        for cls in [
            "wikitable sortable sticky-header-multi static-row-numbers",
            "sticky-header-multi",
            "static-row-header",
            "table-footer",
            "column-header",
        ] {
            let html = page(&format!("<table class=\"{cls}\">{ROWS}</table>"));
            assert!(table_rows(&html) >= 4, "class={cls} lost the table");
        }
    }

    /// The page-scoped compounds must still read as boilerplate.
    #[test]
    fn page_scoped_header_is_still_chrome() {
        for cls in ["site-header", "page-header", "global-header", "header", "site-footer"] {
            let html = format!(
                "<html><body><article><h1>T</h1><p>{}</p>\
                 <div class=\"{cls}\"><a href=\"/a\">Menu one</a><a href=\"/b\">Menu two</a></div>\
                 </article></body></html>",
                "Ordinary prose, with commas, at length. ".repeat(6)
            );
            let art = extract(&html, Some("https://example.com/x")).expect("extract");
            assert!(!art.text.contains("Menu one"), "class={cls} survived");
        }
    }

    /// One enormous attribute value must not read as "almost pure markup".
    /// Wikipedia's parser hangs serialised JSON off every element.
    #[test]
    fn giant_attribute_does_not_bury_a_table() {
        let blob = "y".repeat(3000);
        let table = format!(
            "<table><tr><th>Country</th><th>GDP</th></tr>\
             <tr><td data-mw='{{\"parts\":\"{blob}\"}}'>Korea</td><td>1,870,000</td></tr></table>"
        );
        assert!(table_rows(&page(&table)) >= 3, "attribute payload sank the table");
    }

    /// A header row that cites each column's source is mostly links, and a
    /// data cell holds one word. Neither makes a table a link farm.
    #[test]
    fn link_heavy_cells_survive() {
        let table = "<table>\
            <tr><th><a href=\"/imf\">IMF</a></th><th><a href=\"/wb\">World Bank</a></th></tr>\
            <tr><td><a href=\"/kr\">Korea</a></td><td>1,870,000</td></tr></table>";
        assert!(table_rows(&page(table)) >= 3, "link density dropped the table");
    }

    /// A real data table runs to thousands of nodes. Rejecting it does not
    /// degrade the output, it deletes it — walked as blocks, cells are too
    /// short to survive as paragraphs.
    #[test]
    fn large_data_table_is_still_tabulated() {
        let mut t = String::from("<table><tr><th>Country</th><th>GDP</th></tr>");
        for i in 0..400 {
            t.push_str(&format!(
                "<tr><td><a href=\"/c{i}\" title=\"country {i}\">Country {i}</a></td>\
                 <td>{i},000</td></tr>"
            ));
        }
        t.push_str("</table>");
        assert!(table_rows(&page(&t)) > 300, "large table was not tabulated");
    }
}

#[cfg(test)]
mod root_fallback {
    use super::*;

    /// A document split into sibling sections under one wrapper, with a
    /// reference list long enough to outscore any single section.
    ///
    /// The article stays the larger half deliberately: a reference list that
    /// held most of a page's text would be protected by the dominance rule,
    /// and rightly — that shape is a parse accident, not an encyclopaedia.
    fn sectioned() -> String {
        let mut s = String::from("<html><body><div class=\"mw-parser-output\">");
        for i in 0..12 {
            s.push_str(&format!(
                "<section><h2>Section {i}</h2><p>This section argues its point at \
                 length, with commas, clauses and several sentences. It runs on \
                 for a while so that it reads as prose rather than a label. \
                 Marker{i} appears here, and the paragraph continues past it with \
                 further discussion, more commas, and a closing sentence.</p>\
                 <p>A second paragraph follows, also of a reasonable length, so \
                 that the section carries real weight when the scorer looks at \
                 it. It too has commas and sentences.</p></section>"
            ));
        }
        s.push_str("<ol class=\"references\">");
        for i in 0..40 {
            s.push_str(&format!(
                "<li>Author {i}, <a href=\"/r{i}\">a cited work</a>, somewhere.</li>"
            ));
        }
        s.push_str("</ol></div></body></html>");
        s
    }

    fn as_article(html: &str) -> Article {
        let opts = ExtractOptions { index_mode: IndexMode::Never, ..ExtractOptions::new() };
        extract_with(html, Some("https://example.com/x"), &opts).expect("extract")
    }

    #[test]
    fn partial_root_loses_to_the_fuller_container() {
        let art = as_article(&sectioned());
        for i in 0..6 {
            assert!(
                art.text.contains(&format!("Marker{i}")),
                "section {i} missing; extraction stopped early:\n{}",
                &art.markdown[..art.markdown.len().min(400)]
            );
        }
    }

    /// The comparison must not become an excuse to sweep in boilerplate: the
    /// reference list is dropped either way.
    #[test]
    fn the_fuller_container_still_gets_denoised() {
        let art = as_article(&sectioned());
        assert!(!art.text.contains("a cited work"), "reference list survived");
    }
}
