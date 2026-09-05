//! A thin index over `tl`'s arena.
//!
//! `tl` is fast because it hands back a flat arena of nodes with no parent
//! pointers and no cached metrics. Every heuristic in [`crate::denoise`] needs
//! exactly three things per node — its parent, its text length and how much of
//! that text sits inside anchors — so we compute all of them in a single
//! pre-order pass plus one reverse sweep, and then never touch the tree
//! structure again.

use std::borrow::Cow;

use crate::error::{Error, Result};
use crate::text;

/// Decode HTML entities, but only pay for it when there is an `&` to decode.
///
/// `memchr` compiles to a runtime-dispatched SIMD scan on stable, and the vast
/// majority of text nodes contain no entity at all, so this skips the decoder
/// outright for most of the document.
#[inline]
pub(crate) fn decode_entities(s: &str) -> Cow<'_, str> {
    if memchr::memchr(b'&', s.as_bytes()).is_none() {
        return Cow::Borrowed(s);
    }
    Cow::Owned(html_escape::decode_html_entities(s).into_owned())
}

/// Most bytes a single attribute value may contribute to an element's markup
/// weight.
///
/// Generous next to real attributes -- a long `class` or a `srcset` fits --
/// and far below the serialised JSON that annotation-heavy generators attach.
const ATTR_VALUE_WEIGHT_CAP: u32 = 128;

/// Elements whose content HTML defines as raw text, and which `tl` does not.
const RAW_TEXT_ELEMENTS: &[&str] = &["script", "style"];

/// Neutralise raw-text element bodies before the parser sees them.
///
/// HTML says the content of `<script>` and `<style>` is raw text: nothing
/// inside starts a tag until the matching end tag. `tl` does not implement
/// that, so a `<` followed by a letter opens an element -- and minified
/// JavaScript is full of them. `for(i=0;i<n;i++)` is enough: the phantom
/// `<n…>` consumes the real `</script>`, the script stays open, and it
/// swallows the rest of the document. Every node after it then sits inside a
/// `<script>`, which the denoiser drops on sight, so the page extracts to
/// nothing. WordPress ships such a script on every page it renders.
///
/// The body is deleted rather than escaped, because nothing here reads it --
/// with one exception. `<script type="application/ld+json">` carries the
/// metadata that `Doc::raw_text` parses, so its body is kept with `<` escaped
/// instead; `raw_text` decodes entities, so the JSON arrives intact.
pub(crate) fn neutralise_raw_text(html: &str) -> Cow<'_, str> {
    let bytes = html.as_bytes();
    if !RAW_TEXT_ELEMENTS.iter().any(|t| find_tag(bytes, t, 0).is_some()) {
        return Cow::Borrowed(html);
    }
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    while cursor < html.len() {
        let Some((start, tag)) = RAW_TEXT_ELEMENTS
            .iter()
            .filter_map(|t| find_tag(bytes, t, cursor).map(|i| (i, *t)))
            .min_by_key(|(i, _)| *i)
        else {
            break;
        };
        let Some(open_end) = end_of_open_tag(bytes, start) else { break };
        out.push_str(&html[cursor..=open_end]);

        let close = format!("</{tag}");
        let body_end = find_ci(bytes, close.as_bytes(), open_end + 1).unwrap_or(html.len());
        let keeps_body = html[start..=open_end].to_ascii_lowercase().contains("json");
        if keeps_body {
            out.push_str(&html[open_end + 1..body_end].replace('<', "&lt;"));
        }
        cursor = body_end;
    }
    out.push_str(&html[cursor.min(html.len())..]);
    Cow::Owned(out)
}

/// Position of `<name` at or after `from`, where the name is followed by a
/// delimiter -- so `<style` matches but `<styles` does not.
fn find_tag(bytes: &[u8], name: &str, from: usize) -> Option<usize> {
    let needle = format!("<{name}");
    let mut at = from;
    while let Some(i) = find_ci(bytes, needle.as_bytes(), at) {
        match bytes.get(i + needle.len()) {
            Some(b' ' | b'>' | b'/' | b'\t' | b'\n' | b'\r') => return Some(i),
            _ => at = i + 1,
        }
    }
    None
}

/// ASCII-case-insensitive substring search.
fn find_ci(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= haystack.len() {
        return None;
    }
    let first = needle[0].to_ascii_lowercase();
    let mut at = from;
    while at + needle.len() <= haystack.len() {
        let i = memchr::memchr2(first, first.to_ascii_uppercase(), &haystack[at..])? + at;
        if i + needle.len() > haystack.len() {
            return None;
        }
        if haystack[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(i);
        }
        at = i + 1;
    }
    None
}

/// End of an opening tag, respecting quoted attribute values.
fn end_of_open_tag(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, &b) in bytes[start..].iter().enumerate() {
        match (quote, b) {
            (Some(q), c) if c == q => quote = None,
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b'>') => return Some(start + offset),
            _ => {}
        }
    }
    None
}

/// Index into the node arena.
pub(crate) type Id = usize;

/// Tags whose text content is never document content.
pub(crate) const INVISIBLE_TAGS: &[&str] =
    &["script", "style", "noscript", "template", "svg", "canvas"];

/// Block-level elements. A node containing one of these is a container, not a
/// paragraph, which is the distinction the content scorer depends on.
pub(crate) const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "div",
    "dl",
    "dd",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
];

/// Parsed document plus the derived metrics the heuristics need.
pub(crate) struct Doc<'a> {
    pub(crate) dom: tl::VDom<'a>,
    parent: Vec<Option<Id>>,
    /// Lowercase tag name per node, interned once. Empty for text and comments.
    names: Vec<Box<str>>,
    bytes: Vec<u32>,
    text_len: Vec<u32>,
    link_len: Vec<u32>,
    /// Ids in document order.
    pub(crate) preorder: Vec<Id>,
    document_text: u32,
}

impl<'a> Doc<'a> {
    /// Parse `html` and precompute per-node metrics.
    pub(crate) fn parse(html: &'a str) -> Result<Doc<'a>> {
        let dom = tl::parse(html, tl::ParserOptions::default())
            .map_err(|e| Error::Parse(e.to_string()))?;
        let n = dom.nodes().len();

        let mut parent = vec![None; n];
        let mut names: Vec<Box<str>> = vec![Box::from(""); n];
        let mut seen = vec![false; n];
        let mut preorder = Vec::with_capacity(n);

        // Pre-order walk with an explicit stack: real-world markup nests deeply
        // enough that recursion is a genuine stack-overflow risk.
        let mut stack: Vec<(Id, Option<Id>)> =
            dom.children().iter().rev().map(|h| (h.get_inner() as Id, None)).collect();
        while let Some((id, par)) = stack.pop() {
            if id >= n || seen[id] {
                continue; // defensive: malformed markup can repeat a handle
            }
            seen[id] = true;
            parent[id] = par;
            if let Some(tag) = dom.nodes().get(id).and_then(|node| node.as_tag()) {
                names[id] = Box::from(tag.name().as_utf8_str().to_ascii_lowercase().as_str());
            }
            preorder.push(id);
            if let Some(node) = dom.nodes().get(id)
                && let Some(children) = node.children()
            {
                for h in children.top().as_slice().iter().rev() {
                    stack.push((h.get_inner() as Id, Some(id)));
                }
            }
        }

        // Reverse pre-order is a valid post-order for accumulation: every
        // descendant has a strictly larger pre-order index than its ancestor.
        let mut text_len = vec![0u32; n];
        let mut link_len = vec![0u32; n];
        let mut bytes = vec![0u32; n];
        for &id in preorder.iter().rev() {
            let node = match dom.nodes().get(id) {
                Some(node) => node,
                None => continue,
            };
            match node {
                tl::Node::Raw(raw_bytes) => {
                    let raw = raw_bytes.as_utf8_str();
                    let decoded = decode_entities(&raw);
                    text_len[id] = text::visible_len(decoded.trim()) as u32;
                    bytes[id] = bytes[id].saturating_add(raw.len() as u32);
                }
                tl::Node::Tag(tag) => {
                    let name = tag.name().as_utf8_str();
                    // Markup weight of this element's own tags, computed rather
                    // than measured: `tl` sets a tag's raw slice to just the
                    // opening tag when it cannot find the matching close, which
                    // is exactly the malformed-page case the ratio must survive.
                    let mut own = 2 * name.len() as u32 + 4;
                    for (key, value) in tag.attributes().iter() {
                        own = own.saturating_add(key.len() as u32 + 4);
                        // Attribute payload counts, but only up to a point. The
                        // ratio this feeds asks "is this scaffolding or prose?",
                        // and scaffolding shows up as many elements, not as one
                        // enormous value. Wikipedia's parser hangs a `data-mw`
                        // JSON blob off every element of a sortable table --
                        // 334 KB of markup around 7.5 KB of text -- and counting
                        // those in full reads a table of national GDP figures as
                        // an ad slot.
                        let len = value.map(|v| v.len() as u32).unwrap_or(0);
                        own = own.saturating_add(len.min(ATTR_VALUE_WEIGHT_CAP));
                    }
                    bytes[id] = bytes[id].saturating_add(own);
                    if INVISIBLE_TAGS.contains(&name.as_ref()) {
                        text_len[id] = 0;
                        link_len[id] = 0;
                    } else if name == "a" {
                        link_len[id] = text_len[id];
                    }
                }
                tl::Node::Comment(c) => {
                    text_len[id] = 0;
                    bytes[id] = c.as_bytes().len() as u32 + 7;
                }
            }
            if let Some(p) = parent[id] {
                text_len[p] = text_len[p].saturating_add(text_len[id]);
                link_len[p] = link_len[p].saturating_add(link_len[id]);
                bytes[p] = bytes[p].saturating_add(bytes[id]);
            }
        }

        // Total visible text, taken from `<body>` when there is one. This is
        // the denominator for "is this node most of the document?", which is
        // what keeps a mis-nested page from being classified as boilerplate.
        let document_text = preorder
            .iter()
            .copied()
            .find(|&id| {
                dom.nodes()
                    .get(id)
                    .and_then(|n| n.as_tag())
                    .is_some_and(|t| t.name().as_utf8_str().eq_ignore_ascii_case("body"))
            })
            .map(|body| text_len[body])
            .unwrap_or_else(|| preorder.first().map(|&r| text_len[r]).unwrap_or(0));

        Ok(Doc { dom, parent, names, bytes, text_len, link_len, preorder, document_text })
    }

    /// Does this node hold most of the document's visible text?
    ///
    /// Boilerplate is, by definition, a minority of a page. A node that holds
    /// the majority is the article — even when its class or `role` says
    /// otherwise, which happens whenever a lenient parser fails to close a tag
    /// and nests the whole document inside a navigation element.
    pub(crate) fn is_dominant(&self, id: Id) -> bool {
        self.document_text > 0 && self.text_len(id) as u64 * 2 > self.document_text as u64
    }

    #[inline]
    pub(crate) fn node(&self, id: Id) -> Option<&tl::Node<'a>> {
        self.dom.nodes().get(id)
    }

    #[inline]
    pub(crate) fn tag(&self, id: Id) -> Option<&tl::HTMLTag<'a>> {
        self.node(id)?.as_tag()
    }

    /// Lowercase tag name, or `""` for text and comment nodes.
    /// Lowercase tag name, or `""` for text and comment nodes.
    ///
    /// Interned during parsing: this is on nearly every hot path in the crate,
    /// and lowercasing on each call allocated millions of short strings per
    /// document set.
    #[inline]
    pub(crate) fn tag_name(&self, id: Id) -> &str {
        self.names.get(id).map(|s| &**s).unwrap_or("")
    }

    #[inline]
    pub(crate) fn parent(&self, id: Id) -> Option<Id> {
        self.parent.get(id).copied().flatten()
    }

    /// Length of the node's visible text, in grapheme clusters.
    #[inline]
    pub(crate) fn text_len(&self, id: Id) -> u32 {
        self.text_len.get(id).copied().unwrap_or(0)
    }

    /// Fraction of the node's text that sits inside `<a>` elements.
    ///
    /// The single most reliable boilerplate signal there is: navigation, link
    /// farms and "related posts" rails approach 1.0, prose approaches 0.
    pub(crate) fn link_density(&self, id: Id) -> f32 {
        let text = self.text_len(id);
        if text == 0 {
            return 0.0;
        }
        (self.link_len.get(id).copied().unwrap_or(0) as f32 / text as f32).min(1.0)
    }

    /// Markup weight of a node's whole subtree, in bytes.
    ///
    /// Accumulated in the same reverse sweep as the text metrics rather than
    /// read off `tl`'s per-tag source slice, because that slice collapses to
    /// the opening tag alone whenever the parser cannot match a closing tag —
    /// silently reporting 28 bytes for an element holding 180 KB.
    #[inline]
    pub(crate) fn html_len(&self, id: Id) -> u32 {
        self.bytes.get(id).copied().unwrap_or(0)
    }

    /// Ratio of visible text to markup weight.
    ///
    /// Prose sits well above 0.2; ad slots, share bars and script-driven
    /// widgets are mostly attributes and nesting, and fall near zero.
    pub(crate) fn text_ratio(&self, id: Id) -> f32 {
        let html = self.html_len(id);
        if html == 0 {
            return 0.0;
        }
        (self.text_len(id) as f32 / html as f32).min(1.0)
    }

    /// Direct children of `id`.
    pub(crate) fn children(&self, id: Id) -> Vec<Id> {
        self.node(id)
            .and_then(|n| n.children())
            .map(|c| c.top().as_slice().iter().map(|h| h.get_inner() as Id).collect())
            .unwrap_or_default()
    }

    /// Every node in document order, starting at `id` and including it.
    pub(crate) fn descendants(&self, id: Id) -> Vec<Id> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            out.push(cur);
            let kids = self.children(cur);
            for k in kids.into_iter().rev() {
                stack.push(k);
            }
        }
        out
    }

    /// Value of an attribute, with HTML entities decoded.
    ///
    /// Attribute values carry entities as readily as text does — `og:title` and
    /// `alt` especially — and leaving them raw puts `&#x27;` in front of a
    /// reader.
    pub(crate) fn attr(&self, id: Id, key: &str) -> Option<String> {
        let tag = self.tag(id)?;
        let value = tag.attributes().get(key)??;
        let raw = value.as_utf8_str();
        Some(decode_entities(&raw).into_owned())
    }

    /// `class` and `id` joined, lowercased — the string the noise regexes match.
    pub(crate) fn signature(&self, id: Id) -> String {
        let mut sig = String::new();
        if let Some(tag) = self.tag(id) {
            if let Some(c) = tag.attributes().class() {
                sig.push_str(&c.as_utf8_str().to_ascii_lowercase());
                sig.push(' ');
            }
            if let Some(i) = tag.attributes().id() {
                sig.push_str(&i.as_utf8_str().to_ascii_lowercase());
                sig.push(' ');
            }
            for key in ["role", "data-testid", "aria-label", "itemprop"] {
                if let Some(Some(v)) = tag.attributes().get(key) {
                    sig.push_str(&v.as_utf8_str().to_ascii_lowercase());
                    sig.push(' ');
                }
            }
            // Ad slots carry their purpose in attribute *names* — `data-ad-unit`,
            // `data-ad-client`, `data-google-query-id` — far more reliably than
            // in any class. Surfacing that as a token lets the same vocabulary
            // catch them.
            for (key, _) in tag.attributes().iter() {
                let key = key.to_ascii_lowercase();
                if key.starts_with("data-ad") || key.starts_with("data-google-ad") {
                    sig.push_str("advertisement ");
                    break;
                }
            }
        }
        sig
    }

    /// Decoded, whitespace-normalised text of a subtree.
    pub(crate) fn inner_text(&self, id: Id) -> String {
        let mut buf = String::new();
        for d in self.descendants(id) {
            match self.node(d) {
                Some(tl::Node::Raw(bytes)) => {
                    // Skip text belonging to an invisible ancestor.
                    if self.has_invisible_ancestor(d, id) {
                        continue;
                    }
                    let raw = bytes.as_utf8_str();
                    buf.push_str(&decode_entities(&raw));
                    buf.push(' ');
                }
                Some(tl::Node::Tag(tag)) => {
                    if matches!(tag.name().as_utf8_str().as_ref(), "br" | "p" | "div" | "li") {
                        buf.push('\n');
                    }
                }
                _ => {}
            }
        }
        text::normalize_ws(&buf)
    }

    /// Text of a subtree with entity decoding but no whitespace normalisation,
    /// and without the visibility filter — for `<pre>`, `<title>` and
    /// `<script type="application/ld+json">`, where the literal bytes matter.
    pub(crate) fn raw_text(&self, id: Id) -> String {
        let mut buf = String::new();
        for d in self.descendants(id) {
            if let Some(tl::Node::Raw(bytes)) = self.node(d) {
                let raw = bytes.as_utf8_str();
                buf.push_str(&decode_entities(&raw));
            }
        }
        buf
    }

    /// True when the node holds text but no block-level element.
    ///
    /// Only such nodes may *contribute* a content score; containers merely
    /// *receive* one. Without this a wrapper `<div>` accumulates both its own
    /// text score and its children's, and reliably outscores the article it
    /// wraps.
    pub(crate) fn is_leaf_block(&self, id: Id) -> bool {
        self.descendants(id).into_iter().skip(1).all(|d| !BLOCK_TAGS.contains(&self.tag_name(d)))
    }

    fn has_invisible_ancestor(&self, mut id: Id, stop: Id) -> bool {
        while let Some(p) = self.parent(id) {
            if INVISIBLE_TAGS.contains(&self.tag_name(p)) {
                return true;
            }
            if p == stop {
                return false;
            }
            id = p;
        }
        false
    }

    /// Walk up to `levels` ancestors, nearest first.
    pub(crate) fn ancestors(&self, id: Id, levels: usize) -> Vec<Id> {
        let mut out = Vec::with_capacity(levels);
        let mut cur = id;
        for _ in 0..levels {
            match self.parent(cur) {
                Some(p) => {
                    out.push(p);
                    cur = p;
                }
                None => break,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r#"<html><body>
        <nav><a href="/a">A</a><a href="/b">B</a><a href="/c">C</a></nav>
        <article><p>Real prose with several words in it.</p></article>
        <script>var noise = "should not count";</script>
    </body></html>"#;

    fn find<'d>(doc: &Doc<'d>, name: &str) -> Id {
        doc.preorder.iter().copied().find(|&id| doc.tag_name(id) == name).expect("tag present")
    }

    #[test]
    fn link_density_separates_nav_from_prose() {
        let doc = Doc::parse(HTML).unwrap();
        let nav = find(&doc, "nav");
        let article = find(&doc, "article");
        assert!(doc.link_density(nav) > 0.99, "nav was {}", doc.link_density(nav));
        assert_eq!(doc.link_density(article), 0.0);
    }

    #[test]
    fn script_text_is_not_counted() {
        let doc = Doc::parse(HTML).unwrap();
        let script = find(&doc, "script");
        assert_eq!(doc.text_len(script), 0);
        assert!(!doc.inner_text(find(&doc, "body")).contains("should not count"));
    }

    #[test]
    fn subtree_bytes_reflect_the_whole_subtree() {
        let doc = Doc::parse(HTML).unwrap();
        let body = find(&doc, "body");
        let article = find(&doc, "article");
        let p = find(&doc, "p");
        // Strictly decreasing from ancestor to descendant, and large enough to
        // account for the text it contains.
        assert!(doc.html_len(body) > doc.html_len(article));
        assert!(doc.html_len(article) > doc.html_len(p));
        assert!(doc.html_len(p) >= doc.text_len(p));
        // Prose sits far above the widget threshold; a link farm does not.
        assert!(doc.text_ratio(p) > 0.3, "{}", doc.text_ratio(p));
    }

    #[test]
    fn parents_and_document_order_are_consistent() {
        let doc = Doc::parse(HTML).unwrap();
        let article = find(&doc, "article");
        let p = find(&doc, "p");
        assert_eq!(doc.parent(p), Some(article));
        let pos = |id: Id| doc.preorder.iter().position(|&x| x == id).unwrap();
        assert!(pos(article) < pos(p), "ancestors must precede descendants");
    }
}

#[cfg(test)]
mod raw_text_tests {
    use super::neutralise_raw_text;

    /// The bug this exists for: a comparison operator in minified JavaScript
    /// reads as the start of a tag, and the phantom element eats the document.
    #[test]
    fn a_comparison_in_a_script_no_longer_opens_a_tag() {
        for js in ["for(i=0;i<n;i++){}", "if(a<1){}", "x()<e.timestamp+1", "a<b&&c<d"] {
            let html = format!("<html><body><script>{js}</script><div>KEEP</div></body></html>");
            let out = neutralise_raw_text(&html);
            assert!(!out.contains(js), "script body survived: {out}");
            assert!(out.contains("<div>KEEP</div>"), "document truncated: {out}");
        }
    }

    /// JSON-LD is the one body that is read, so it is escaped rather than
    /// dropped. `raw_text` decodes entities, so the reader still sees `<`.
    #[test]
    fn json_ld_survives_escaped() {
        let html = r#"<script type="application/ld+json">{"name":"a<b"}</script>"#;
        let out = neutralise_raw_text(html);
        assert!(out.contains(r#"{"name":"a&lt;b"}"#), "{out}");
    }

    #[test]
    fn style_bodies_go_too() {
        let html = "<style>.a{color:red}</style><p>x</p>";
        let out = neutralise_raw_text(html);
        assert!(!out.contains("color:red"));
        assert!(out.contains("<p>x</p>"));
    }

    /// Case, attributes with `>` inside quotes, and a missing close tag.
    #[test]
    fn handles_the_awkward_shapes() {
        let out = neutralise_raw_text(r#"<SCRIPT TYPE="text/javascript">a<b</SCRIPT><p>x</p>"#);
        assert!(out.contains("<p>x</p>"), "{out}");
        let out = neutralise_raw_text(r#"<script data-x="a>b">i<n</script><p>y</p>"#);
        assert!(out.contains("<p>y</p>"), "{out}");
        let out = neutralise_raw_text("<script>unterminated i<n");
        assert!(!out.contains("i<n"), "{out}");
    }

    /// A document with neither element is handed back untouched, not copied.
    #[test]
    fn no_raw_text_elements_means_no_allocation() {
        let html = "<html><body><p>plain</p></body></html>";
        assert!(matches!(neutralise_raw_text(html), std::borrow::Cow::Borrowed(_)));
    }

    /// `<styles>` is not `<style>`.
    #[test]
    fn only_matches_whole_tag_names() {
        let html = "<styleguide>a<b</styleguide><p>z</p>";
        assert!(matches!(neutralise_raw_text(html), std::borrow::Cow::Borrowed(_)));
    }
}
