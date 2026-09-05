//! Semantic denoising: find the article, drop everything else.
//!
//! Three signals, applied in this order, do nearly all the work:
//!
//! 1. **Structural priors** — `<nav>`, `<footer>`, `<aside>`, `role="banner"`
//!    and a class/id vocabulary that has been stable across the web for a
//!    decade.
//! 2. **Link density** — anchor text over total text. Navigation and "related
//!    posts" rails sit near 1.0; prose sits near 0.
//! 3. **Text-to-HTML ratio** — visible characters over serialised markup
//!    bytes. Ad slots and JS widgets are almost pure markup.
//!
//! Only then do we run a Readability-style content score to pick the container
//! that actually holds the article, because scoring alone happily selects a
//! comment thread on a thin page.

use std::sync::LazyLock;

use regex::Regex;

use crate::parse::dom::{Doc, Id};

/// Tags removed outright, whatever they contain.
pub const DROP_TAGS: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "canvas", "iframe", "object", "embed",
    "form", "input", "select", "textarea", "button", "label", "fieldset", "dialog", "audio",
    "video", "map", "area", "link", "meta", "base",
];

/// Tags that are boilerplate by definition in the HTML5 outline.
pub const CHROME_TAGS: &[&str] = &["nav", "footer", "aside", "menu"];

/// Elements exempt from the two prose-shaped heuristics.
///
/// Link density and text-to-markup ratio both ask "does this container read
/// like prose?", and a data table answers no however good it is: cells hold a
/// word or a number, and a header row that cites its sources is *mostly*
/// links. Judging a table by those thresholds drops the GDP of every nation
/// for looking insufficiently like a paragraph. What a table is still gets
/// decided -- by the vocabulary above, which catches an ad wherever it sits,
/// and by the writer's own structural test for layout tables.
///
/// Code blocks fail the same tests for the same reason, and a highlighted one
/// fails them badly: `<span class="line"><span>` around every token, with the
/// theme inlined as `style`.
const TABLE_TAGS: &[&str] = &[
    "table", "thead", "tbody", "tfoot", "tr", "th", "td", "caption", "colgroup", "col",
    // Code blocks answer those two questions the same way a table does. A
    // syntax highlighter emits a `<span>` per token and a theme's worth of
    // inline style, so the markup dwarfs the text; the text itself is source,
    // which does not read like prose and never will.
    "pre", "code", "samp", "kbd",
];

/// Class/id tokens that are never content, and that no positive marker
/// outranks.
///
/// Split out from [`CONCLUSIVE`] because "positive wins ties" is the wrong
/// rule here: `class="sponsored-content"` matches both vocabularies, and it is
/// an advertisement. Commercial markers are unambiguous in a way that
/// structural ones ("comments", "related") are not.
static ABSOLUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|[-_\s])(ads?|adbox|adslot|adunit|advert|adverts|advertising|advertisement|advertisements|sponsors?|sponsored|sponsorship|promos?|promoted|promotions?|banners?|adsense|adsbygoogle|doubleclick|googlead|googleads|taboola|outbrain|revcontent|zergnet|mgid|criteo|popups?|interstitials?|paywall|cookies?|consent|gdpr)([-_\s]|$)",
    )
    .expect("static absolute regex")
});

/// Class/id tokens that justify dropping a subtree **at any size**.
///
/// The vocabulary below is high-precision: nothing here is ever the article. A
/// reference list or a comment thread can run to thousands of characters, so
/// the size guard that protects [`NEGATIVE`] from false positives would let
/// them straight through — and a Wikipedia citation list will happily eat a
/// whole context budget.
static CONCLUSIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|[-_\s])(ads?|adbox|advert|advertisement|sponsors?|sponsored|promos?|promotions?|banners?|popups?|modals?|overlays?|interstitials?|cookies?|consent|gdpr|newsletters?|subscribe|signup|paywall|shares?|sharing|socials?|comments?|disqus|livefyre|related|recommend|recommended|recirc|outbrain|taboola|trending|breadcrumbs?|pagination|pagers?|navbars?|navigation|navbox|navboxes|masthead|footers?|toolbars?|skip|sr-only|screen-reader|visually-hidden|noprint|references?|reference-list|reflist|refbegin|refend|citations?|footnotes?|catlinks|authority-control|mw-editsection|mw-references|printfooter|sitesub|jump-link|mw-jump-link)([-_\s]|$)",
    )
    .expect("static conclusive regex")
});

static NEGATIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|[-_\s])(ad|ads|adbox|advert|advertisement|sponsor|sponsored|promo|promotion|banner|popup|modal|overlay|interstitial|cookie|consent|gdpr|newsletter|subscribe|signup|paywall|share|sharing|social|follow|comment|comments|disqus|livefyre|reply|sidebar|side-bar|widget|related|recommend|recirc|outbrain|taboola|trending|popular|breadcrumb|pagination|pager|paging|nav|navbar|navigation|menu|masthead|footer|header|topbar|toolbar|utility|skip|hidden|invisible|screen-reader|sr-only|visually-hidden|meta|byline|tags|tag-list|author-box|bio|cta|newsl|toc|table-of-contents|infobox|navbox|metadata|mw-editsection|reference|citation|footnote)([-_\s]|$)",
    )
    .expect("static negative regex")
});

/// `header` and `footer` qualified by a word that scopes them to a component
/// rather than to the page.
///
/// The chrome vocabulary matches tokens, not whole class names, so `header`
/// fires inside `sticky-header-multi` exactly as it does inside `site-header`.
/// The first is a data table describing its own sticky column headings — the
/// markup Wikipedia puts on every sortable table — and dropping it costs the
/// reader the table. Compounds listed here are stripped from a signature
/// before the vocabulary sees it; unqualified `header`, and page-scoped
/// compounds like `site-header`, are left alone.
static COMPONENT_SCOPED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|[-_\s])(sticky|table|col|column|row|grid|data|sort|sortable|cell|group|thead|tfoot)[-_](headers?|footers?)([-_\s]|$)",
    )
    .expect("static component-scoped regex")
});

static POSITIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|[-_\s])(article|articlebody|post|postbody|post-content|entry|entry-content|content|contents|main|maincontent|story|storybody|body|bodytext|text|prose|markdown|readme|blog|page-content|rich-text|document|paragraph|section-content|mw-parser-output|mw-content-text)([-_\s]|$)",
    )
    .expect("static positive regex")
});

/// Tuning knobs for the denoiser.
#[derive(Debug, Clone)]
pub struct DenoiseConfig {
    /// A block shorter than this (in grapheme clusters) is dropped unless it is
    /// a heading, list item or table cell.
    pub min_block_len: u32,
    /// Blocks above this anchor-text fraction are treated as navigation.
    pub max_link_density: f32,
    /// Blocks below this text-to-markup ratio are treated as widgets.
    pub min_text_ratio: f32,
    /// Drop `<nav>`, `<footer>`, `<aside>` and friends outright.
    pub drop_chrome: bool,
    /// Also drop nodes whose class/id matches the boilerplate vocabulary.
    pub drop_by_class: bool,
    /// Keep `<table>` content as Markdown tables rather than dropping them.
    pub keep_tables: bool,
}

impl Default for DenoiseConfig {
    /// Tuned against `benches/quality.py`, which scores extraction on 23 real
    /// pages two ways: five-word shingle overlap with the article container
    /// (recall and precision), and a count of site furniture that leaked in.
    /// Both are needed. Judged on overlap alone the right answer is to switch
    /// every threshold off -- it scores best -- but that quadruples the
    /// furniture, because the article container the score compares against
    /// never contained the navigation in the first place.
    ///
    /// From the defaults these replace, F1 goes 59.1% to 63.1% with furniture
    /// unchanged at 4 occurrences over 23 pages.
    fn default() -> Self {
        DenoiseConfig {
            // Holds the line at 25. This is the one threshold that guards
            // against furniture rather than against widgets: "Jump to
            // content" is 17 characters and "Skip to main" is 12, so
            // lowering it to 15 takes leaked furniture from 4 to 16.
            min_block_len: 25,
            // 0.5 was costing recall for nothing measurable. Between 0.5 and
            // 0.7 the link share of the output moves 6.7% to 7.1% while F1
            // gains a point; 0.6 takes most of that for half the loosening.
            max_link_density: 0.6,
            // 0.06 was the single most expensive default: it drops legitimate
            // markup-heavy content -- highlighted code, annotated tables --
            // and buys almost no precision. Anything at or below 0.02 scores
            // the same, so this keeps a floor rather than removing the test.
            min_text_ratio: 0.02,
            drop_chrome: true,
            drop_by_class: true,
            keep_tables: true,
        }
    }
}

/// What the denoiser threw away, for observability and tuning.
///
/// Counts are over DOM *nodes*, and a rejected node counts its whole subtree —
/// dropping one `<nav>` removes everything under it, and a figure that said
/// "1 node dropped" would be useless for judging whether the heuristics fired.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct DenoiseStats {
    /// Nodes in the document.
    pub nodes_visited: usize,
    /// Nodes removed, counting whole rejected subtrees and everything outside
    /// the chosen content root.
    pub nodes_dropped: usize,
    /// Bytes of HTML handed in.
    pub html_bytes: usize,
    /// Bytes of Markdown handed back.
    pub markdown_bytes: usize,
}

impl DenoiseStats {
    /// How much smaller the output is than the input, in `[0, 1]`.
    pub fn compression(&self) -> f32 {
        if self.html_bytes == 0 {
            return 0.0;
        }
        1.0 - (self.markdown_bytes as f32 / self.html_bytes as f32).min(1.0)
    }
}

/// Class/id weighting, mirroring Readability's `getClassWeight`.
/// A node's class/id signature with component-scoped `header`/`footer`
/// compounds removed, ready to match against the vocabularies.
///
/// See [`COMPONENT_SCOPED`] for why they are removed rather than matched.
fn signature_for_match(doc: &Doc<'_>, id: Id) -> String {
    let raw = doc.signature(id);
    match COMPONENT_SCOPED.replace_all(&raw, " ") {
        std::borrow::Cow::Borrowed(_) => raw,
        std::borrow::Cow::Owned(cleaned) => cleaned,
    }
}

pub(crate) fn class_weight(doc: &Doc<'_>, id: Id) -> f32 {
    let sig = signature_for_match(doc, id);
    if sig.is_empty() {
        return 0.0;
    }
    let mut w = 0.0;
    if NEGATIVE.is_match(&sig) || CONCLUSIVE.is_match(&sig) || ABSOLUTE.is_match(&sig) {
        w -= 25.0;
    }
    if POSITIVE.is_match(&sig) {
        w += 25.0;
    }
    w
}

/// Is this node's text mostly inside a code block?
///
/// A syntax highlighter turns ten lines of source into a `<span>` per token
/// with a theme inlined as `style`, which reads as pure scaffolding to the
/// text-to-markup ratio. The code is the content; the scaffolding is what
/// displaying code costs.
fn holds_mostly_code(doc: &Doc<'_>, id: Id) -> bool {
    let text = doc.text_len(id);
    if text == 0 {
        return false;
    }
    let mut in_code = 0u32;
    let mut stack = vec![id];
    while let Some(node) = stack.pop() {
        if matches!(doc.tag_name(node), "pre" | "code") {
            in_code = in_code.saturating_add(doc.text_len(node));
            continue; // 중첩된 code 를 두 번 세지 않는다
        }
        stack.extend(doc.children(node));
    }
    in_code * 2 >= text
}

/// Is this node boilerplate that should never reach the Markdown writer?
pub(crate) fn is_noise(doc: &Doc<'_>, id: Id, cfg: &DenoiseConfig) -> bool {
    let name = doc.tag_name(id);
    if name.is_empty() {
        return false; // raw text; judged through its parent
    }
    if DROP_TAGS.contains(&name) {
        return true;
    }
    if cfg.drop_chrome && CHROME_TAGS.contains(&name) {
        return true;
    }
    if !cfg.keep_tables && name == "table" {
        return true;
    }
    if doc.attr(id, "hidden").is_some()
        || doc.attr(id, "aria-hidden").as_deref() == Some("true")
        || doc.attr(id, "style").is_some_and(|s| {
            let s = s.replace(' ', "").to_ascii_lowercase();
            s.contains("display:none") || s.contains("visibility:hidden")
        })
    {
        return true;
    }
    // Past this point every rule is a heuristic about *where* content usually
    // is not. None of them may overrule the arithmetic fact that this node
    // holds most of the page.
    if doc.is_dominant(id) {
        return false;
    }

    if matches!(
        doc.attr(id, "role").as_deref(),
        Some(
            "navigation"
                | "banner"
                | "complementary"
                | "search"
                | "contentinfo"
                | "alert"
                | "dialog"
        )
    ) {
        return true;
    }

    let text = doc.text_len(id);

    if cfg.drop_by_class {
        let sig = signature_for_match(doc, id);
        // Commercial markers outrank everything, including a positive one.
        if ABSOLUTE.is_match(&sig) {
            return true;
        }
        // High-precision vocabulary: never the article, whatever its size.
        if CONCLUSIVE.is_match(&sig) && !POSITIVE.is_match(&sig) {
            return true;
        }
        // The rest is suggestive, not conclusive: `<div id="main-content">` and
        // `<div class="content-ad">` both match something, so require the node
        // to also look thin before dropping it on the vocabulary alone.
        if class_weight(doc, id) < 0.0 && text < 400 {
            return true;
        }
    }

    if TABLE_TAGS.contains(&name) {
        return false;
    }

    // Link farms. Headings and list items legitimately run link-heavy, so only
    // apply this to containers that carry real text.
    if text >= cfg.min_block_len
        && doc.link_density(id) > cfg.max_link_density
        && !matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
    {
        return true;
    }

    // Markup-heavy, text-poor: a widget, a player shell, an ad slot -- unless
    // the markup is a highlighter's and the text is source code. Exempting
    // `<pre>` itself is not enough: a wrapper `<div>` around it is judged on
    // its own, fails, and takes the code with it. Checked only on the way to
    // rejecting, so the walk stays off the common path.
    if doc.html_len(id) > 512
        && doc.text_ratio(id) < cfg.min_text_ratio
        && !holds_mostly_code(doc, id)
    {
        return true;
    }

    false
}

/// Pick the element that most likely holds the article body.
///
/// Readability's scoring, with two changes: the ratio and link-density signals
/// above gate which paragraphs are allowed to contribute at all, and the climb
/// out of the winning candidate stops at the first ancestor that does not
/// improve, which keeps multi-section articles whole without swallowing the
/// page furniture around them.
pub(crate) fn find_content_root(doc: &Doc<'_>, cfg: &DenoiseConfig) -> Option<Id> {
    let n = doc.dom.nodes().len();
    let mut score = vec![0.0f32; n];
    let mut scored = vec![false; n];

    for &id in &doc.preorder {
        let name = doc.tag_name(id);
        if !matches!(
            name,
            "p" | "pre" | "td" | "blockquote" | "article" | "section" | "div" | "li" | "dd"
        ) {
            continue;
        }
        let text = doc.text_len(id);
        if text < cfg.min_block_len {
            continue;
        }
        if doc.link_density(id) > cfg.max_link_density {
            continue;
        }
        if !doc.is_leaf_block(id) {
            continue;
        }

        let inner = doc.inner_text(id);
        let commas = inner.chars().filter(|c| matches!(c, ',' | '，' | '、' | ';')).count() as f32;
        // Sentence-ish punctuation is the cheapest proxy for "this is prose,
        // not a list of link labels".
        let stops =
            inner.chars().filter(|c| matches!(c, '.' | '。' | '?' | '!' | '？' | '！')).count()
                as f32;
        let base = 1.0 + commas + stops * 0.5 + (text as f32 / 100.0).min(3.0);

        for (level, anc) in doc.ancestors(id, 3).into_iter().enumerate() {
            let divisor = match level {
                0 => 1.0,
                1 => 2.0,
                _ => 3.0,
            };
            score[anc] += base / divisor;
            scored[anc] = true;
        }
    }

    let mut best: Option<(Id, f32)> = None;
    for id in 0..n {
        if !scored[id] {
            continue;
        }
        let name = doc.tag_name(id);
        if name == "body" || name == "html" || name.is_empty() {
            continue;
        }
        let final_score = (score[id] + class_weight(doc, id)) * (1.0 - doc.link_density(id));
        if final_score <= 0.0 {
            continue;
        }
        if best.is_none_or(|(_, b)| final_score > b) {
            best = Some((id, final_score));
        }
    }

    let (mut root, mut root_score) = best?;
    // Climb while the parent genuinely scores better — that means the sibling
    // sections around `root` are content too.
    while let Some(p) = doc.parent(root) {
        let name = doc.tag_name(p);
        if name == "body" || name == "html" || name.is_empty() {
            break;
        }
        let parent_score = (score[p] + class_weight(doc, p)) * (1.0 - doc.link_density(p));
        if parent_score > root_score {
            root = p;
            root_score = parent_score;
        } else {
            break;
        }
    }
    Some(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::dom::Doc;

    const PAGE: &str = r##"<html><body>
      <header class="site-header"><a href="/">Home</a><a href="/about">About</a></header>
      <nav class="main-nav"><a href="/x">X</a><a href="/y">Y</a><a href="/z">Z</a></nav>
      <div class="ad-slot"><div><div><span></span></div></div></div>
      <div id="content">
        <article class="post-content">
          <h1>Rust makes crawling cheap</h1>
          <p>The first paragraph explains, at some length, why memory footprint matters, and it contains commas.</p>
          <p>The second paragraph continues the argument with more prose, more commas, and several sentences. It ends here.</p>
        </article>
      </div>
      <aside class="related-posts"><a href="/1">One</a><a href="/2">Two</a></aside>
      <footer class="site-footer">© 2026</footer>
    </body></html>"##;

    fn id_of(doc: &Doc<'_>, sig: &str) -> Id {
        doc.preorder
            .iter()
            .copied()
            .find(|&i| doc.signature(i).contains(sig))
            .unwrap_or_else(|| panic!("no node with signature {sig}"))
    }

    #[test]
    fn content_root_is_the_article() {
        let doc = Doc::parse(PAGE).unwrap();
        let root = find_content_root(&doc, &DenoiseConfig::default()).expect("a root");
        assert_eq!(doc.tag_name(root), "article", "picked {:?}", doc.signature(root));
    }

    #[test]
    fn chrome_and_ads_are_noise() {
        let doc = Doc::parse(PAGE).unwrap();
        let cfg = DenoiseConfig::default();
        for sig in ["main-nav", "ad-slot", "related-posts", "site-footer"] {
            assert!(is_noise(&doc, id_of(&doc, sig), &cfg), "{sig} should be noise");
        }
    }

    #[test]
    fn article_body_is_not_noise() {
        let doc = Doc::parse(PAGE).unwrap();
        let cfg = DenoiseConfig::default();
        assert!(!is_noise(&doc, id_of(&doc, "post-content"), &cfg));
    }

    #[test]
    fn large_reference_and_comment_blocks_are_dropped() {
        // Wikipedia's citation list is `<ol class="mw-references references">` and
        // runs to thousands of characters — far past the size guard that keeps
        // the suggestive vocabulary honest.
        let long = "Author, A. (2020). Some paper title here. Journal of Things. ".repeat(40);
        let html = format!(
            r#"<html><body><div id="content">
                 <p>{long}</p>
                 <ol class="mw-references references"><li>{long}</li></ol>
                 <div class="comments"><p>{long}</p></div>
                 <section class="related-articles"><p>{long}</p></section>
               </div></body></html>"#
        );
        let doc = Doc::parse(&html).unwrap();
        let cfg = DenoiseConfig::default();
        for sig in ["mw-references", "comments", "related-articles"] {
            assert!(is_noise(&doc, id_of(&doc, sig), &cfg), "{sig} survived at full size");
        }
    }

    #[test]
    fn plural_class_names_are_matched() {
        // The article has to be present and larger, or the dominance rule
        // correctly refuses to call the only content on the page boilerplate.
        let prose = "Real article prose that runs well past any length filter here. ".repeat(30);
        for sig in ["references", "citations", "footnotes", "comments", "ads", "related"] {
            let html = format!(
                r#"<html><body><article class="post"><p>{prose}</p></article>
                   <div class="{sig}"><p>some boilerplate text</p></div></body></html>"#
            );
            let doc = Doc::parse(&html).unwrap();
            assert!(
                is_noise(&doc, id_of(&doc, sig), &DenoiseConfig::default()),
                "class {sig:?} was not recognised"
            );
        }
    }

    #[test]
    fn commercial_markers_outrank_a_positive_class() {
        // `sponsored-content` matches both vocabularies. It is an advert.
        let prose = "Real article prose that runs past any length filter here. ".repeat(30);
        for sig in ["sponsored-content", "ad-content", "promoted-story", "adsbygoogle"] {
            let html = format!(
                r#"<html><body><article class="post"><p>{prose}</p></article>
                   <div class="{sig}"><p>{prose}</p></div></body></html>"#
            );
            let doc = Doc::parse(&html).unwrap();
            assert!(
                is_noise(&doc, id_of(&doc, sig), &DenoiseConfig::default()),
                "{sig:?} survived"
            );
        }
    }

    #[test]
    fn ad_tech_attributes_are_recognised() {
        let prose = "Real article prose that runs past any length filter here. ".repeat(30);
        let html = format!(
            r#"<html><body><article class="post"><p>{prose}</p></article>
               <div data-ad-unit="/1234/banner"><p>banner copy</p></div>
               <div data-ad-client="ca-pub-1"><p>more banner copy</p></div>
               </body></html>"#
        );
        let doc = Doc::parse(&html).unwrap();
        let cfg = DenoiseConfig::default();
        let ads: Vec<Id> = doc
            .preorder
            .iter()
            .copied()
            .filter(|&i| {
                doc.attr(i, "data-ad-unit").is_some() || doc.attr(i, "data-ad-client").is_some()
            })
            .collect();
        assert_eq!(ads.len(), 2);
        for ad in ads {
            assert!(is_noise(&doc, ad, &cfg), "ad-tech attribute not recognised");
        }
    }

    #[test]
    fn a_positive_class_overrides_the_conclusive_list() {
        // `article-share-content` should not be dropped just because it says
        // "share": an explicit content marker wins.
        let html = r#"<html><body><div class="share entry-content"><p>Real prose that is long enough to matter here.</p></div></body></html>"#;
        let doc = Doc::parse(html).unwrap();
        assert!(!is_noise(&doc, id_of(&doc, "entry-content"), &DenoiseConfig::default()));
    }

    #[test]
    fn a_node_holding_the_whole_page_is_never_boilerplate() {
        // The shape a lenient parser produces when it cannot close a tag: the
        // entire article ends up inside a navigation element. Dropping it on
        // the class name would discard the page.
        let prose = "Real article prose that carries the paragraph well past any length filter. "
            .repeat(30);
        let html = format!(
            r#"<html><body>
                 <table class="sidebar nomobile" role="navigation"><tr><td>
                   <h1>The article</h1><p>{prose}</p>
                 </td></tr></table>
               </body></html>"#
        );
        let doc = Doc::parse(&html).unwrap();
        let sidebar = id_of(&doc, "sidebar");
        assert!(doc.is_dominant(sidebar));
        assert!(!is_noise(&doc, sidebar, &DenoiseConfig::default()), "the page was discarded");
    }

    #[test]
    fn a_genuine_sidebar_is_still_dropped() {
        let prose =
            "Real article prose that carries the paragraph past any length filter. ".repeat(30);
        let html = format!(
            r#"<html><body>
                 <article class="post"><h1>T</h1><p>{prose}</p></article>
                 <aside class="sidebar" role="navigation"><a href="/1">One</a><a href="/2">Two</a></aside>
               </body></html>"#
        );
        let doc = Doc::parse(&html).unwrap();
        let aside = id_of(&doc, "sidebar");
        assert!(!doc.is_dominant(aside));
        assert!(is_noise(&doc, aside, &DenoiseConfig::default()));
    }

    #[test]
    fn class_weight_signs_are_right() {
        let doc = Doc::parse(PAGE).unwrap();
        assert!(class_weight(&doc, id_of(&doc, "post-content")) > 0.0);
        assert!(class_weight(&doc, id_of(&doc, "ad-slot")) < 0.0);
    }
}

#[cfg(test)]
mod code_wrapper_tests {
    use super::*;
    use crate::parse::dom::Doc;

    /// A highlighter's output is mostly markup by weight, and the `<div>` that
    /// wraps it is judged on its own: it fails the ratio test, and the code
    /// goes with it. Exempting `<pre>` alone does not save the wrapper.
    #[test]
    fn a_wrapper_around_code_survives_the_ratio_test() {
        // Shaped like the real thing: a `<span>` per token, each carrying a
        // slice of the theme, wrapped in a `<pre>` carrying the rest of it.
        const TOKEN: &str =
            "<span class=\"tok\" style=\"color:#E06C75;font-weight:400;font-style:normal\">";
        let mut code = String::new();
        for i in 0..40 {
            code.push_str(
                "<span class=\"line\" style=\"display:block;min-height:1lh;white-space:pre\">",
            );
            for tok in ["l", "x", "=", "1", ";", " "] {
                code.push_str(TOKEN);
                code.push_str(tok);
                code.push_str("</span>");
            }
            code.push_str("</span>\n");
            let _ = i;
        }
        let html = format!(
            "<html><body><article><p>{}</p>\
             <div class=\"code-block-wrapper\"><pre class=\"astro-code\" \
             style=\"--shiki-light:#abb2bf;--shiki-dark:#383A42;--shiki-light-bg:#282c34;\
             --shiki-dark-bg:#FAFAFA;overflow-x:auto;white-space:pre-wrap\">\
             <code>{code}</code></pre></div></article></body></html>",
            "Prose with commas, at some length. ".repeat(6)
        );
        let doc = Doc::parse(&html).unwrap();
        let cfg = DenoiseConfig::default();
        let wrapper = doc
            .preorder
            .iter()
            .copied()
            .find(|&i| doc.signature(i).contains("code-block-wrapper"))
            .expect("wrapper");
        assert!(doc.text_ratio(wrapper) < cfg.min_text_ratio, "fixture is not markup-heavy");
        assert!(!is_noise(&doc, wrapper, &cfg), "code wrapper was dropped");
    }

    /// The exemption is for code, not for every markup-heavy container.
    #[test]
    fn an_ordinary_widget_shell_is_still_dropped() {
        let filler = "<div><span></span></div>".repeat(80);
        let html = format!(
            "<html><body><article><p>{}</p>\
             <div class=\"player\">{filler}<i>x</i></div></article></body></html>",
            "Prose with commas, at some length. ".repeat(20)
        );
        let doc = Doc::parse(&html).unwrap();
        let cfg = DenoiseConfig::default();
        let shell = doc
            .preorder
            .iter()
            .copied()
            .find(|&i| doc.signature(i).contains("player"))
            .expect("shell");
        assert!(is_noise(&doc, shell, &cfg), "widget shell survived");
    }
}
