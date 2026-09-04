//! Index-page extraction: when the link list *is* the content.
//!
//! A front page, a feed, a blog archive and a forum listing all defeat article
//! extraction for the same reason — the link density that marks navigation as
//! boilerplate is, on these pages, the point. Running the article heuristics
//! over one correctly yields nothing.
//!
//! What such a page is actually worth is an inventory: which stories exist and
//! where they live, so the pipeline can go read them. That is what this module
//! produces, and it is why an index page doubles as a collection source — see
//! [`crate::search::Provider::Index`].
//!
//! Detection only runs after article extraction has already come up empty, so
//! ordinary pages never take this path.

use url::Url;

use crate::denoise::{DenoiseConfig, is_noise};
use crate::parse::dom::{BLOCK_TAGS, Doc, Id};
use crate::search::canonical_key;
use crate::text;

/// Minimum links before a page is worth calling an index.
const MIN_INDEX_LINKS: usize = 5;

/// A link must be at least this share of its block to be an entry title.
///
/// An entry headline dominates its container; a link inside a sentence is a
/// few percent of it. That gap is what separates an inventory from prose, and
/// it holds regardless of whether article extraction happened to succeed.
const TITULAR_PERCENT: u32 = 35;

/// Share of the page's prose that titular links must account for.
const INDEX_COVERAGE_PERCENT: usize = 25;

/// Absolute floor for anchor text. Below this it is a control, not a title.
const MIN_HEADLINE_CHARS: usize = 8;

/// A single-word anchor has to be this long to count, since one word is far
/// more likely to be a control ("Subscribe", "Comments") than a headline.
const MIN_SINGLE_WORD_CHARS: usize = 12;

/// Ceiling on anchor text. No headline runs this long; an anchor that does has
/// swallowed its siblings because the parser never closed it, and its text is
/// not to be trusted.
const MAX_HEADLINE_CHARS: usize = 200;

/// Descriptive text kept alongside a link.
const MAX_SNIPPET_CHARS: usize = 200;

/// How far up to look for a boilerplate container. Deep enough to clear any
/// realistic nesting, bounded so a pathological tree cannot stall the walk.
const MAX_ANCESTOR_WALK: usize = 32;

/// Anchor text that is navigation whatever the page is.
const CONTROL_WORDS: &[&str] = &[
    "home",
    "about",
    "contact",
    "login",
    "log in",
    "sign in",
    "sign up",
    "register",
    "search",
    "menu",
    "more",
    "read more",
    "next",
    "previous",
    "prev",
    "back",
    "top",
    "share",
    "subscribe",
    "newsletter",
    "privacy",
    "terms",
    "cookies",
    "settings",
    "help",
    "support",
    "faq",
    "rss",
    "skip to content",
    "advertisement",
    "댓글",
    "로그인",
    "회원가입",
    "더보기",
    "구독",
];

/// One harvested link.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Link {
    /// Anchor text.
    pub text: String,
    /// Absolute URL.
    pub url: String,
    /// Nearby descriptive text, if the page offers any.
    pub snippet: String,
    /// Enclosing headings, outermost first.
    pub heading_path: Vec<String>,
}

/// Collect the outbound links a listing page exists to offer.
///
/// Links inside boilerplate, duplicate targets and navigation controls are all
/// dropped, so what comes back is the page's actual inventory rather than every
/// `<a>` in the document.
pub(crate) fn harvest(doc: &Doc<'_>, base: Option<&Url>, cfg: &DenoiseConfig) -> Vec<Link> {
    // The article denoiser drops containers for being link-dense and
    // markup-heavy. On a listing page those are not defects — they are the
    // format. Structural and vocabulary rules still apply, so links inside a
    // `<nav>`, a footer or a cookie banner are still rejected.
    let cfg = &DenoiseConfig { max_link_density: 1.0, min_text_ratio: 0.0, ..cfg.clone() };

    let mut links: Vec<Link> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Heading ids are kept so a link can exclude the heading it sits inside:
    // a headline is not its own section.
    let mut headings: Vec<(u8, String, Id)> = Vec::new();
    let self_key = base.and_then(|b| canonical_key(b.as_str()));

    for &id in &doc.preorder {
        let name = doc.tag_name(id);

        if let Some(level) = heading_level(&name) {
            let title = text::normalize_ws(&doc.inner_text(id));
            if !title.is_empty() {
                while headings.last().is_some_and(|(l, _, _)| *l >= level) {
                    headings.pop();
                }
                headings.push((level, title, id));
            }
            continue;
        }
        if name != "a" {
            continue;
        }

        let Some(text_raw) = Some(text::normalize_ws(&doc.inner_text(id))) else { continue };
        if !is_headline(&text_raw) {
            continue;
        }
        let Some(url) = resolve(doc, id, base) else { continue };
        let Some(key) = canonical_key(&url) else { continue };
        // A link back to the page itself carries no new information.
        if self_key.as_ref() == Some(&key) || !seen.insert(key) {
            continue;
        }
        if in_boilerplate(doc, id, cfg) {
            continue;
        }
        if !is_titular(doc, id) {
            continue;
        }

        links.push(Link {
            snippet: snippet_for(doc, id, &text_raw),
            text: text_raw,
            url,
            heading_path: headings
                .iter()
                .filter(|(_, _, hid)| !is_descendant_of(doc, id, *hid))
                .map(|(_, t, _)| t.clone())
                .collect(),
        });
    }
    links
}

/// Does this page look like a listing rather than an article?
///
/// The test is coverage, not absence: how much of the prose the article path
/// found is attached to a link. On a listing every headline and standfirst
/// belongs to an entry, so coverage approaches one; on an article with a
/// "related reading" rail at the end it is a rounding error. Asking instead
/// whether article extraction *failed* would miss every listing page whose
/// cards carry a summary — which is most of them.
pub(crate) fn looks_like_index(links: &[Link], article_chars: usize) -> bool {
    if links.len() < MIN_INDEX_LINKS {
        return false;
    }
    if article_chars == 0 {
        return true;
    }
    // Titles only. Snippets are lifted from the same prose the article path
    // measured, so counting them would compare the page against itself.
    let inventory: usize = links.iter().map(|l| text::visible_len(&l.text)).sum();
    inventory * 100 > article_chars * INDEX_COVERAGE_PERCENT
}

/// Anchor text long enough, and specific enough, to be a headline.
fn is_headline(text: &str) -> bool {
    let trimmed = text.trim();
    let chars = text::visible_len(trimmed);
    if !(MIN_HEADLINE_CHARS..=MAX_HEADLINE_CHARS).contains(&chars) {
        return false;
    }
    // Aggregator titles are short — "GPT-6 Astra" is eleven characters — so the
    // length bar drops as soon as there is more than one word to look at.
    if chars < MIN_SINGLE_WORD_CHARS && trimmed.split_whitespace().count() < 2 {
        return false;
    }
    if !trimmed.chars().any(char::is_alphabetic) {
        return false;
    }
    let lower = trimmed.to_lowercase();
    if CONTROL_WORDS.iter().any(|w| lower == *w) {
        return false;
    }
    !is_entry_metadata(trimmed)
}

/// Timestamps, comment counts, scores and bare domains.
///
/// Aggregators surround every headline with these, and they clear a length
/// threshold easily — "1501 comments" is longer than plenty of real titles.
fn is_entry_metadata(text: &str) -> bool {
    let lower = text.trim().to_lowercase();

    // A bare domain or filename: one token, has a dot, no spaces.
    if !lower.contains(' ') && lower.contains('.') {
        return true;
    }

    // The pattern has to describe the *whole* string. "13 hours ago" is
    // metadata; "10 years of Rust in production" is a headline that happens to
    // start the same way, and only length tells them apart.
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.len() > 3 {
        return false;
    }
    let first = words.first().copied().unwrap_or_default();
    let leads_with_number = first
        .trim_matches(|c: char| !c.is_alphanumeric())
        .chars()
        .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
        && first.chars().any(|c| c.is_ascii_digit());
    if !leads_with_number {
        return false;
    }

    // "1501 comments", "342 points", "13 hours ago", "3분 전"
    const UNITS: &[&str] = &[
        "comment", "comments", "point", "points", "vote", "votes", "reply", "replies", "view",
        "views", "second", "seconds", "minute", "minutes", "hour", "hours", "day", "days", "week",
        "weeks", "month", "months", "year", "years", "ago", "분", "시간", "일", "개",
    ];
    words[1..].iter().any(|w| UNITS.contains(&w.trim_matches(|c: char| !c.is_alphanumeric())))
}

/// Is this link an entry title rather than a link inside a sentence?
fn is_titular(doc: &Doc<'_>, id: Id) -> bool {
    let mut cur = id;
    while let Some(parent) = doc.parent(cur) {
        let name = doc.tag_name(parent);
        if matches!(name.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            return true;
        }
        if BLOCK_TAGS.contains(&name.as_str()) {
            let block = doc.text_len(parent);
            return block == 0 || doc.text_len(id) * 100 >= block * TITULAR_PERCENT;
        }
        cur = parent;
    }
    true
}

/// Is `id` inside `ancestor`?
fn is_descendant_of(doc: &Doc<'_>, mut id: Id, ancestor: Id) -> bool {
    while let Some(p) = doc.parent(id) {
        if p == ancestor {
            return true;
        }
        id = p;
    }
    false
}

/// Is this link inside something the denoiser would drop?
///
/// The walk goes all the way up, not a few levels: a documentation sidebar
/// nests its `<nav>` half a dozen lists deep, and stopping early harvests the
/// page's own table of contents as if it were an inventory.
fn in_boilerplate(doc: &Doc<'_>, id: Id, cfg: &DenoiseConfig) -> bool {
    doc.ancestors(id, MAX_ANCESTOR_WALK).into_iter().any(|a| is_noise(doc, a, cfg))
}

/// Descriptive text sitting next to the link, if the page provides any.
///
/// Listing pages wrap each entry in a card; the card's text minus the headline
/// is the standfirst. It is what makes these links rankable against a query.
fn snippet_for(doc: &Doc<'_>, link: Id, anchor_text: &str) -> String {
    // The nearest ancestor that adds text beyond the headline is the card. An
    // ancestor holding many times the headline is a whole page section, and
    // borrowing its text would attach every other story's standfirst to this one.
    let anchor_len = doc.text_len(link);
    let card = doc.ancestors(link, 4).into_iter().find(|&a| {
        let len = doc.text_len(a);
        len > anchor_len + 20 && len < anchor_len.saturating_mul(10).max(anchor_len + 600)
    });
    let Some(card) = card else { return String::new() };

    let full = doc.inner_text(card);
    let rest = full.replace(anchor_text, " ");
    let rest = text::normalize_ws(&rest);
    if text::visible_len(&rest) < 20 {
        return String::new();
    }
    rest.chars().take(MAX_SNIPPET_CHARS).collect()
}

fn resolve(doc: &Doc<'_>, id: Id, base: Option<&Url>) -> Option<String> {
    let href = doc.attr(id, "href")?;
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
        return None;
    }
    let absolute = match base {
        Some(base) => base.join(href).ok()?,
        None => Url::parse(href).ok()?,
    };
    matches!(absolute.scheme(), "http" | "https").then(|| absolute.to_string())
}

fn heading_level(name: &str) -> Option<u8> {
    ["h1", "h2", "h3", "h4", "h5", "h6"].iter().position(|h| *h == name).map(|i| i as u8 + 1)
}

/// Render harvested links as rankable Markdown units.
///
/// One unit per link, with the section heading emitted whenever it changes.
/// Keeping the snippet in the unit text is what lets the slimmer rank a listing
/// against a query rather than just truncating it.
pub(crate) fn units_from_links(links: &[Link]) -> Vec<crate::parse::Unit> {
    use crate::parse::{Unit, UnitKind};

    let mut units = Vec::with_capacity(links.len() + 4);
    let mut last_path: Vec<String> = Vec::new();

    for link in links {
        if link.heading_path != last_path
            && let Some(section) = link.heading_path.last()
        {
            let markdown = format!("## {section}");
            units.push(Unit {
                kind: UnitKind::Heading,
                level: 2,
                tokens: text::estimate_tokens(&markdown),
                position: units.len(),
                text: section.clone(),
                markdown,
                heading_path: link
                    .heading_path
                    .iter()
                    .take(link.heading_path.len().saturating_sub(1))
                    .cloned()
                    .collect(),
            });
            last_path = link.heading_path.clone();
        }

        let markdown = if link.snippet.is_empty() {
            format!("- [{}]({})", link.text, link.url)
        } else {
            format!("- [{}]({}) — {}", link.text, link.url, link.snippet)
        };
        let plain = if link.snippet.is_empty() {
            link.text.clone()
        } else {
            format!("{} — {}", link.text, link.snippet)
        };
        units.push(Unit {
            kind: UnitKind::ListItem,
            level: 0,
            tokens: text::estimate_tokens(&markdown),
            position: units.len(),
            text: plain,
            markdown,
            heading_path: link.heading_path.clone(),
        });
    }
    units
}

/// Could this page possibly be an index? A cheap upper bound.
///
/// The real harvest resolves URLs, decodes text and gathers snippets — worth
/// paying for on a listing, pure waste on the article pages that are the
/// common case. This pass touches only precomputed lengths and the tag name,
/// and it over-estimates, so a page it rejects could not have qualified.
pub(crate) fn could_be_index(doc: &Doc<'_>, article_chars: usize) -> bool {
    let mut count = 0usize;
    let mut chars = 0usize;
    for &id in &doc.preorder {
        if doc.tag_name(id) != "a" {
            continue;
        }
        let len = doc.text_len(id) as usize;
        if !(MIN_HEADLINE_CHARS..=MAX_HEADLINE_CHARS).contains(&len) {
            continue;
        }
        if !is_titular(doc, id) {
            continue;
        }
        count += 1;
        chars += len;
    }
    count >= MIN_INDEX_LINKS
        && (article_chars == 0 || chars * 100 > article_chars * INDEX_COVERAGE_PERCENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r##"<html><body>
      <header><nav><a href="/">Home</a><a href="/about">About us</a></nav></header>
      <main>
        <h2>Top stories</h2>
        <div class="card"><h3><a href="/story/rust-memory">Rust cuts crawler memory by an order of magnitude</a></h3>
          <p>A new pipeline holds under five megabytes while parsing six hundred documents a second.</p></div>
        <div class="card"><h3><a href="/story/http3">What actually changed in HTTP/3</a></h3>
          <p>The transport moved to UDP, and head-of-line blocking went with it.</p></div>
        <div class="card"><h3><a href="/story/bm25">BM25 is still the baseline to beat</a></h3>
          <p>Twenty years on, the ranking function remains hard to improve upon.</p></div>
        <h2>Opinion</h2>
        <div class="card"><h3><a href="/opinion/agents">Agents are not a product category</a></h3>
          <p>A short argument about naming things.</p></div>
        <div class="card"><h3><a href="/opinion/slm">The case for small local models</a></h3>
          <p>Latency, privacy and cost all point the same way.</p></div>
        <a href="/page/2">Next</a>
        <a href="/story/rust-memory?utm_source=footer">Rust cuts crawler memory by an order of magnitude</a>
      </main>
      <footer><a href="/privacy">Privacy policy</a><a href="/terms">Terms of service</a></footer>
    </body></html>"##;

    fn harvested() -> Vec<Link> {
        let doc = Doc::parse(LISTING).unwrap();
        let base = Url::parse("https://wire.example/").unwrap();
        harvest(&doc, Some(&base), &DenoiseConfig::default())
    }

    #[test]
    fn collects_the_stories_and_nothing_else() {
        let links = harvested();
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(links.len(), 5, "{urls:#?}");
        assert!(urls.contains(&"https://wire.example/story/bm25"));
        // Navigation, pagination and footer controls are not inventory.
        assert!(!urls.iter().any(|u| u.contains("/privacy")));
        assert!(!urls.iter().any(|u| u.contains("/about")));
        assert!(!urls.iter().any(|u| u.contains("/page/2")));
    }

    #[test]
    fn duplicate_targets_collapse_across_decorations() {
        let links = harvested();
        let dupes = links.iter().filter(|l| l.url.contains("rust-memory")).count();
        assert_eq!(dupes, 1, "the utm-tagged repeat survived");
    }

    #[test]
    fn section_headings_become_the_breadcrumb() {
        let links = harvested();
        let opinion: Vec<&Link> = links.iter().filter(|l| l.url.contains("/opinion/")).collect();
        assert_eq!(opinion.len(), 2);
        assert!(opinion.iter().all(|l| l.heading_path == ["Opinion"]), "{opinion:#?}");
        let top = links.iter().find(|l| l.url.contains("/story/bm25")).unwrap();
        assert_eq!(top.heading_path, ["Top stories"]);
    }

    #[test]
    fn standfirsts_are_kept_as_snippets() {
        let links = harvested();
        let http3 = links.iter().find(|l| l.url.contains("http3")).unwrap();
        assert!(http3.snippet.contains("head-of-line blocking"), "{:?}", http3.snippet);
    }

    #[test]
    fn detection_measures_how_much_prose_belongs_to_links() {
        let links = harvested();
        // Nothing but links: unambiguous.
        assert!(looks_like_index(&links, 0));
        // A listing whose cards carry standfirsts: the headlines still account
        // for a large share of the prose, so it reads as an index even though
        // article extraction succeeded.
        let titles: usize = links.iter().map(|l| text::visible_len(&l.text)).sum();
        assert!(looks_like_index(&links, titles * 2));
        // An article that merely ends with a reading list is still an article.
        assert!(!looks_like_index(&links, titles * 20));
        // And an inventory too small to be one.
        assert!(!looks_like_index(&links[..2], 0));
    }

    #[test]
    fn links_inside_sentences_are_not_inventory() {
        let html = r#"<html><body><article>
            <p>A paragraph of real prose that runs on for a while and happens to mention
               <a href="/guide">the full guide to everything</a> somewhere in the middle of it,
               plus <a href="/other">another long link title here</a> for good measure.</p>
            <p>A second paragraph, similarly long, that also links to
               <a href="/third">a third destination with a title</a> inside a sentence.</p>
        </article></body></html>"#;
        let doc = Doc::parse(html).unwrap();
        let base = Url::parse("https://example.com/post").unwrap();
        let links = harvest(&doc, Some(&base), &DenoiseConfig::default());
        assert!(links.is_empty(), "prose links were harvested: {links:#?}");
    }

    #[test]
    fn short_and_control_anchors_are_rejected() {
        assert!(!is_headline("Next"));
        assert!(!is_headline("Read more"));
        assert!(!is_headline("2026"));
        assert!(!is_headline("   "));
        assert!(is_headline("Rust cuts crawler memory"));
        assert!(is_headline("GPT-6 Astra"), "short aggregator titles must survive");
        assert!(!is_headline("Subscribe"));
        assert!(!is_headline("upvote"));
        // An anchor that swallowed the rest of the menu is not a headline.
        assert!(!is_headline(&"Some navigation label ".repeat(30)));
        assert!(is_headline("작은 로컬 모델을 위한 근거"));
    }

    #[test]
    fn aggregator_metadata_is_rejected() {
        // All of these clear the length threshold and are not stories.
        for junk in [
            "1501 comments",
            "342 points",
            "13 hours ago",
            "2 days ago",
            "neil.fraser.name",
            "docs.rs/tokio",
        ] {
            assert!(!is_headline(junk), "{junk:?} was taken for a headline");
        }
        // Headlines that merely begin with a number must survive.
        for real in [
            "3 things we learned about small models",
            "1.76.0 is out, and it changes the defaults",
            "10 years of Rust in production",
        ] {
            assert!(is_headline(real), "{real:?} was rejected");
        }
    }
}
