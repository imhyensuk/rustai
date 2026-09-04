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

static NEGATIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|[-_\s])(ad|ads|adbox|advert|advertisement|sponsor|sponsored|promo|promotion|banner|popup|modal|overlay|interstitial|cookie|consent|gdpr|newsletter|subscribe|signup|paywall|share|sharing|social|follow|comment|comments|disqus|livefyre|reply|sidebar|side-bar|widget|related|recommend|recirc|outbrain|taboola|trending|popular|breadcrumb|pagination|pager|paging|nav|navbar|navigation|menu|masthead|footer|header|topbar|toolbar|utility|skip|hidden|invisible|screen-reader|sr-only|visually-hidden|meta|byline|tags|tag-list|author-box|bio|cta|newsl|toc|table-of-contents|infobox|navbox|metadata|mw-editsection|reference|citation|footnote)([-_\s]|$)",
    )
    .expect("static negative regex")
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
    fn default() -> Self {
        DenoiseConfig {
            min_block_len: 25,
            max_link_density: 0.5,
            min_text_ratio: 0.06,
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
pub(crate) fn class_weight(doc: &Doc<'_>, id: Id) -> f32 {
    let sig = doc.signature(id);
    if sig.is_empty() {
        return 0.0;
    }
    let mut w = 0.0;
    if NEGATIVE.is_match(&sig) {
        w -= 25.0;
    }
    if POSITIVE.is_match(&sig) {
        w += 25.0;
    }
    w
}

/// Is this node boilerplate that should never reach the Markdown writer?
pub(crate) fn is_noise(doc: &Doc<'_>, id: Id, cfg: &DenoiseConfig) -> bool {
    let name = doc.tag_name(id);
    if name.is_empty() {
        return false; // raw text; judged through its parent
    }
    if DROP_TAGS.contains(&name.as_str()) {
        return true;
    }
    if cfg.drop_chrome && CHROME_TAGS.contains(&name.as_str()) {
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

    // A class/id match is suggestive, not conclusive: `<div id="main-content">`
    // and `<div class="content-ad">` both match something, so require the node
    // to also look thin before dropping it on the vocabulary alone.
    if cfg.drop_by_class && class_weight(doc, id) < 0.0 && text < 400 {
        return true;
    }

    // Link farms. Headings and list items legitimately run link-heavy, so only
    // apply this to containers that carry real text.
    if text >= cfg.min_block_len
        && doc.link_density(id) > cfg.max_link_density
        && !matches!(name.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
    {
        return true;
    }

    // Markup-heavy, text-poor: a widget, a player shell, an ad slot.
    if doc.html_len(id) > 512 && doc.text_ratio(id) < cfg.min_text_ratio {
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
            name.as_str(),
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
    fn class_weight_signs_are_right() {
        let doc = Doc::parse(PAGE).unwrap();
        assert!(class_weight(&doc, id_of(&doc, "post-content")) > 0.0);
        assert!(class_weight(&doc, id_of(&doc, "ad-slot")) < 0.0);
    }
}
