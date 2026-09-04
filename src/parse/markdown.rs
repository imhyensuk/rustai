//! Markdown writer.
//!
//! The output is deliberately plain — ATX headings, `-` bullets, fenced code,
//! pipe tables — because it is fed to a small local model, not rendered. Every
//! block becomes a [`Unit`], which is the granularity the context slimmer
//! later ranks and selects.

use url::Url;

use crate::denoise::{DenoiseConfig, DenoiseStats, is_noise};
use crate::parse::dom::{Doc, Id, decode_entities};
use crate::parse::{Unit, UnitKind};
use crate::text;

const INLINE_TAGS: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "big", "cite", "code", "data", "del", "dfn", "em", "font", "i",
    "img", "ins", "kbd", "mark", "q", "s", "samp", "small", "span", "strike", "strong", "sub",
    "sup", "time", "tt", "u", "var", "wbr", "br", "ruby", "rt", "rp",
];

const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

/// Above this serialised size, an "inline" element is checked for block-level
/// content before being believed.
///
/// Inline elements are small by nature. A `<span>` carrying 50 KB of markup is
/// not emphasis — it is an unclosed tag that swallowed the rest of the page,
/// and flattening it would turn the whole document into one paragraph. The
/// size gate keeps the check off the hot path: `html_len` is O(1) and the vast
/// majority of inline nodes never pay for the descendant walk.
const INLINE_INSPECT_BYTES: u32 = 2048;

/// Language names accepted from a bare `class="rust"`-style attribute.
///
/// A whitelist rather than a pattern, because that attribute is shared with
/// styling hooks: `hljs`, `notranslate` and `prettyprint` all look exactly like
/// a language name to anything less strict.
const KNOWN_LANGUAGES: &[&str] = &[
    "bash",
    "c",
    "clojure",
    "cpp",
    "cs",
    "csharp",
    "css",
    "dart",
    "diff",
    "dockerfile",
    "elixir",
    "elm",
    "erlang",
    "fsharp",
    "go",
    "graphql",
    "groovy",
    "haskell",
    "hcl",
    "html",
    "ini",
    "java",
    "javascript",
    "js",
    "json",
    "json5",
    "jsx",
    "julia",
    "kotlin",
    "latex",
    "less",
    "lisp",
    "lua",
    "makefile",
    "markdown",
    "matlab",
    "nginx",
    "nim",
    "objectivec",
    "ocaml",
    "perl",
    "php",
    "powershell",
    "protobuf",
    "python",
    "r",
    "ruby",
    "rust",
    "sass",
    "scala",
    "scss",
    "shell",
    "sh",
    "sql",
    "svelte",
    "swift",
    "terraform",
    "toml",
    "ts",
    "tsx",
    "typescript",
    "vim",
    "vue",
    "xml",
    "yaml",
    "zig",
    "zsh",
];

/// Options that change what ends up in the Markdown.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Keep `[text](href)` links instead of flattening to text.
    pub include_links: bool,
    /// Keep `![alt](src)` images.
    pub include_images: bool,
    /// Render `<table>` as a pipe table.
    pub include_tables: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { include_links: true, include_images: false, include_tables: true }
    }
}

pub(crate) struct Writer<'d, 'a> {
    doc: &'d Doc<'a>,
    cfg: &'d DenoiseConfig,
    opts: &'d RenderOptions,
    base: Option<Url>,
    heading_stack: Vec<(u8, String)>,
    list_depth: usize,
    pub(crate) units: Vec<Unit>,
    pub(crate) stats: DenoiseStats,
}

impl<'d, 'a> Writer<'d, 'a> {
    pub(crate) fn new(
        doc: &'d Doc<'a>,
        cfg: &'d DenoiseConfig,
        opts: &'d RenderOptions,
        base: Option<Url>,
    ) -> Self {
        Writer {
            doc,
            cfg,
            opts,
            base,
            heading_stack: Vec::new(),
            list_depth: 0,
            units: Vec::new(),
            stats: DenoiseStats::default(),
        }
    }

    /// Walk a subtree, emitting units.
    pub(crate) fn walk(&mut self, id: Id) {
        self.stats.nodes_visited += 1;
        if is_noise(self.doc, id, self.cfg) {
            self.stats.nodes_dropped += self.doc.descendants(id).len();
            return;
        }
        let name = self.doc.tag_name(id);

        if let Some(level) = heading_level(name) {
            let inline = self.render_children_inline(id);
            let t = text::normalize_ws(&inline);
            if !t.is_empty() {
                self.push_heading(level, &t);
                let md = format!("{} {}", "#".repeat(level as usize), t);
                self.emit(UnitKind::Heading, level, t, md);
            }
            return;
        }

        match name {
            "" => {
                // Bare text directly under a container.
                if let Some(tl::Node::Raw(_)) = self.doc.node(id) {
                    let t = text::normalize_ws(&self.render_inline(id));
                    if text::visible_len(&t) >= self.cfg.min_block_len as usize {
                        self.emit(UnitKind::Paragraph, 0, t.clone(), t);
                    }
                }
            }
            // A paragraph is only a paragraph if it holds no block-level
            // element. HTML forbids that nesting, but lenient parsers -- this
            // one included -- do not insert the implied `</p>` that a browser
            // would, so an unclosed `<p>` can end up owning the rest of the
            // document. Recursing costs one check and turns that failure into
            // ordinary output.
            "p" | "dd" | "dt" | "figcaption" | "summary" | "address" => {
                if self.doc.is_leaf_block(id) {
                    let t = text::normalize_ws(&self.render_children_inline(id));
                    if !t.is_empty() {
                        let plain = strip_markdown(&t);
                        self.emit(UnitKind::Paragraph, 0, plain, t);
                    }
                } else {
                    self.walk_children(id);
                }
            }
            "pre" => self.emit_code(id),
            "blockquote" if !self.doc.is_leaf_block(id) => self.walk_children(id),
            "blockquote" => {
                let inner = self.doc.inner_text(id);
                if !inner.is_empty() {
                    let md = inner.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
                    self.emit(UnitKind::Quote, 0, inner, md);
                }
            }
            "ul" | "ol" | "dl" => {
                self.list_depth += 1;
                self.walk_children(id);
                self.list_depth = self.list_depth.saturating_sub(1);
            }
            "li" => self.emit_list_item(id),
            // A table holding headings, sections or other tables is laying out
            // a page, not tabulating data. Rendering one as a pipe table turns
            // a whole document into a single unreadable row — and lenient HTML
            // parsers, this one included, will happily put an entire page
            // inside a `<table>` whose close tag they failed to match.
            "table" if self.is_data_table(id) => {
                if self.opts.include_tables {
                    self.emit_table(id);
                }
            }
            "table" => self.walk_children(id),
            "hr" | "br" => {}
            _ => self.walk_children(id),
        }
    }

    fn walk_children(&mut self, id: Id) {
        // Group runs of inline children into implicit paragraphs so that text
        // sitting loose inside a `<div>` is not lost.
        let mut run: Vec<Id> = Vec::new();
        for child in self.doc.children(id) {
            if self.is_inline(child) {
                run.push(child);
            } else {
                self.flush_inline_run(&mut run);
                self.walk(child);
            }
        }
        self.flush_inline_run(&mut run);
    }

    fn flush_inline_run(&mut self, run: &mut Vec<Id>) {
        if run.is_empty() {
            return;
        }
        // An image is a block in its own right, so a run that carries one is
        // kept whatever its alt text weighs; the length gate exists to drop
        // stray words, not figures the caller explicitly asked for.
        let has_image =
            self.opts.include_images && run.iter().any(|&id| self.doc.tag_name(id) == "img");
        let mut buf = String::new();
        for id in run.drain(..) {
            buf.push_str(&self.render_inline(id));
        }
        let t = text::normalize_ws(&buf);
        let plain = strip_markdown(&t);
        if has_image || text::visible_len(&plain) >= self.cfg.min_block_len as usize {
            self.emit(UnitKind::Paragraph, 0, plain, t);
        }
    }

    fn is_inline(&self, id: Id) -> bool {
        match self.doc.node(id) {
            Some(tl::Node::Raw(_)) => true,
            Some(tl::Node::Tag(tag)) => {
                if !INLINE_TAGS.contains(&tag.name().as_utf8_str().to_ascii_lowercase().as_str()) {
                    return false;
                }
                self.doc.html_len(id) <= INLINE_INSPECT_BYTES || self.doc.is_leaf_block(id)
            }
            _ => false,
        }
    }

    fn emit_list_item(&mut self, id: Id) {
        // Only the inline run becomes the bullet. Block children — nested
        // lists, but also the paragraphs and tables that `<li>` is allowed to
        // contain — are walked so they keep their own structure. Flattening
        // them with `inner_text` is how a single mis-nested `<li>` ends up
        // holding an entire page.
        let mut inline_run: Vec<Id> = Vec::new();
        let mut blocks: Vec<Id> = Vec::new();
        for child in self.doc.children(id) {
            if self.is_inline(child) {
                inline_run.push(child);
            } else {
                blocks.push(child);
            }
        }

        let mut buf = String::new();
        for child in inline_run {
            buf.push_str(&self.render_inline(child));
        }
        let t = text::normalize_ws(&buf);
        if !t.is_empty() {
            let depth = self.list_depth.saturating_sub(1).min(6);
            let md = format!("{}- {}", "  ".repeat(depth), t);
            self.emit(UnitKind::ListItem, depth as u8, strip_markdown(&t), md);
        }
        for child in blocks {
            self.walk(child);
        }
    }

    fn emit_code(&mut self, id: Id) {
        let raw = self.doc.raw_text(id);
        let body = raw.trim_end_matches('\n');
        if body.trim().is_empty() {
            return;
        }
        let lang = self.detect_language(id);
        let md = format!("```{lang}\n{body}\n```");
        self.emit(UnitKind::Code, 0, body.to_string(), md);
    }

    /// Work out what language a code block is in.
    ///
    /// There is no single convention. Highlighters write `language-rust`,
    /// `lang-rust` or a bare `rust`; GitHub writes `highlight-source-rust` on a
    /// wrapper *above* the `<pre>`; others use `data-lang`. A fenced block
    /// without its language is markedly less useful to a model reading it, so
    /// this checks all of them — ancestors included — rather than the first
    /// class attribute it happens to find.
    fn detect_language(&self, id: Id) -> String {
        let mut candidates: Vec<Id> = vec![id];
        candidates.extend(self.doc.descendants(id).into_iter().skip(1).take(8));
        candidates.extend(self.doc.ancestors(id, 2));

        for node in candidates {
            for key in ["class", "data-lang", "data-language", "data-code-language"] {
                let Some(value) = self.doc.attr(node, key) else { continue };
                if key != "class" {
                    if let Some(lang) = normalize_language(&value) {
                        return lang;
                    }
                    continue;
                }
                for token in value.split_whitespace() {
                    let stripped = token
                        .strip_prefix("language-")
                        .or_else(|| token.strip_prefix("lang-"))
                        .or_else(|| token.strip_prefix("highlight-source-"))
                        .or_else(|| token.strip_prefix("highlight-text-"))
                        .or_else(|| token.strip_prefix("sourceCode-"));
                    if let Some(lang) = stripped.and_then(normalize_language) {
                        return lang;
                    }
                    // A bare class name, but only if it names a language we
                    // recognise — `hljs`, `prettyprint` and `notranslate` all
                    // sit in the same attribute.
                    if let Some(lang) = normalize_language(token)
                        && KNOWN_LANGUAGES.contains(&lang.as_str())
                    {
                        return lang;
                    }
                }
            }
        }
        String::new()
    }

    /// Is this a table of data, or a table used for layout?
    ///
    /// Structural content inside cells is the giveaway. The caps are a second
    /// line of defence against a `<table>` whose close tag the parser never
    /// found, which swallows the rest of the document.
    ///
    /// Only the text cap speaks to context budget, and it is the tighter of
    /// the two by far. The node cap has to stay well clear of what a real
    /// table costs: a 223-row table of national GDP figures, six columns of
    /// linked and footnoted cells, comes to 4,522 nodes around 6.4 KB of
    /// text. Rejecting that emits nothing at all -- walking a data table as
    /// ordinary blocks yields cells too short to survive as paragraphs -- so a
    /// cap set near real tables does not degrade the output, it deletes it.
    fn is_data_table(&self, id: Id) -> bool {
        const MAX_TABLE_TEXT: u32 = 20_000;
        const MAX_TABLE_NODES: usize = 30_000;

        if self.doc.text_len(id) > MAX_TABLE_TEXT {
            return false;
        }
        let descendants = self.doc.descendants(id);
        if descendants.len() > MAX_TABLE_NODES {
            return false;
        }
        !descendants.iter().skip(1).any(|&d| {
            matches!(
                self.doc.tag_name(d),
                "table"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "section"
                    | "article"
                    | "aside"
                    | "nav"
                    | "footer"
                    | "header"
                    | "figure"
            )
        })
    }

    fn emit_table(&mut self, id: Id) {
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut header: Option<Vec<String>> = None;
        for d in self.doc.descendants(id) {
            if self.doc.tag_name(d) != "tr" || is_noise(self.doc, d, self.cfg) {
                continue;
            }
            let mut cells = Vec::new();
            let mut is_header = false;
            for c in self.doc.children(d) {
                let name = self.doc.tag_name(c);
                if name == "th" {
                    is_header = true;
                } else if name != "td" {
                    continue;
                }
                cells.push(text::normalize_ws(&self.render_children_inline(c)).replace('|', "\\|"));
            }
            if cells.is_empty() {
                continue;
            }
            if is_header && header.is_none() {
                header = Some(cells);
            } else {
                rows.push(cells);
            }
        }
        if rows.is_empty() && header.is_none() {
            return;
        }
        let width = header
            .as_ref()
            .map(Vec::len)
            .into_iter()
            .chain(rows.iter().map(Vec::len))
            .max()
            .unwrap_or(0);
        if width == 0 {
            return;
        }
        let pad = |mut r: Vec<String>| {
            r.resize(width, String::new());
            format!("| {} |", r.join(" | "))
        };
        let mut md = String::new();
        let head = header.unwrap_or_else(|| vec![String::new(); width]);
        md.push_str(&pad(head));
        md.push('\n');
        md.push_str(&format!("|{}", " --- |".repeat(width)));
        for r in &rows {
            md.push('\n');
            md.push_str(&pad(r.clone()));
        }
        // Derived from what was actually written, not from the source subtree.
        // A mis-nested table can hold far more text than it renders, and a unit
        // whose `text` disagrees with its `markdown` corrupts every downstream
        // measurement — token budgets, density, and the "did we find an
        // article?" test alike.
        let plain = strip_markdown(&md);
        self.emit(UnitKind::Table, 0, plain, md);
    }

    fn render_children_inline(&self, id: Id) -> String {
        let mut buf = String::new();
        for child in self.doc.children(id) {
            buf.push_str(&self.render_inline(child));
        }
        buf
    }

    /// Render a subtree as inline Markdown.
    fn render_inline(&self, id: Id) -> String {
        match self.doc.node(id) {
            Some(tl::Node::Raw(bytes)) => {
                let raw = bytes.as_utf8_str();
                escape_inline(&decode_entities(&raw))
            }
            Some(tl::Node::Comment(_)) | None => String::new(),
            Some(tl::Node::Tag(tag)) => {
                let name = tag.name().as_utf8_str().to_ascii_lowercase();
                if crate::denoise::DROP_TAGS.contains(&name.as_str()) {
                    return String::new();
                }
                match name.as_str() {
                    "br" => "\n".to_string(),
                    "img" => {
                        if !self.opts.include_images {
                            return String::new();
                        }
                        let alt = self.doc.attr(id, "alt").unwrap_or_default();
                        match self.resolve(id, "src").or_else(|| self.resolve(id, "data-src")) {
                            Some(src) => format!("![{}]({})", escape_inline(&alt), src),
                            None => String::new(),
                        }
                    }
                    "a" => {
                        let inner = self.render_children_inline(id);
                        if !self.opts.include_links || inner.trim().is_empty() {
                            return inner;
                        }
                        match self.resolve(id, "href") {
                            Some(href) if !href.starts_with("javascript:") => {
                                format!("[{}]({})", inner.trim(), href)
                            }
                            _ => inner,
                        }
                    }
                    "strong" | "b" => wrap(&self.render_children_inline(id), "**"),
                    "em" | "i" | "dfn" | "cite" | "var" => {
                        wrap(&self.render_children_inline(id), "*")
                    }
                    "del" | "s" | "strike" => wrap(&self.render_children_inline(id), "~~"),
                    "code" | "kbd" | "samp" | "tt" => {
                        let inner = text::normalize_ws(&self.doc.inner_text(id));
                        if inner.is_empty() { inner } else { format!("`{inner}`") }
                    }
                    "sup" => self.render_superscript(id),
                    _ => self.render_children_inline(id),
                }
            }
        }
    }

    /// `<sup>` is two unrelated things wearing one tag.
    ///
    /// A citation marker is noise an LLM should never see. An exponent is data:
    /// dropping it silently turns 10^23 into 10, which is the kind of error
    /// that survives every downstream check. Telling them apart is the point.
    fn render_superscript(&self, id: Id) -> String {
        let inner = text::normalize_ws(&self.doc.inner_text(id));
        if inner.is_empty() {
            return String::new();
        }
        if self.is_citation_marker(id, &inner) {
            return String::new();
        }
        // `^` only where it disambiguates: `10^23` and `mol^-1` need it,
        // `1st` reads worse as `1^st`.
        let numeric = inner.chars().all(|c| c.is_ascii_digit() || "+-.,()".contains(c))
            && inner.chars().any(|c| c.is_ascii_digit());
        if numeric { format!("^{inner}") } else { inner }
    }

    /// Is this `<sup>` a footnote or reference marker?
    fn is_citation_marker(&self, id: Id, inner: &str) -> bool {
        // Wikipedia and most CMSes wrap the marker in a link to the note.
        if self.doc.descendants(id).into_iter().skip(1).any(|d| self.doc.tag_name(d) == "a") {
            return true;
        }
        let sig = self.doc.signature(id);
        if !sig.is_empty()
            && ["reference", "citation", "footnote", "cite"].iter().any(|w| sig.contains(w))
        {
            return true;
        }
        // A bracketed number standing alone: `[1]`, `(12)`.
        let trimmed = inner.trim();
        let bracketed = (trimmed.starts_with('[') && trimmed.ends_with(']'))
            || (trimmed.starts_with('(') && trimmed.ends_with(')'));
        bracketed && trimmed.chars().any(|c| c.is_ascii_digit())
    }

    /// Absolutise an attribute against the document's base URL.
    fn resolve(&self, id: Id, key: &str) -> Option<String> {
        let raw = self.doc.attr(id, key)?;
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        match &self.base {
            Some(base) => base.join(raw).ok().map(|u| u.to_string()),
            None => Some(raw.to_string()),
        }
    }

    fn push_heading(&mut self, level: u8, title: &str) {
        while self.heading_stack.last().is_some_and(|(l, _)| *l >= level) {
            self.heading_stack.pop();
        }
        self.heading_stack.push((level, title.to_string()));
    }

    fn emit(&mut self, kind: UnitKind, level: u8, plain: String, markdown: String) {
        let plain = plain.trim().to_string();
        let markdown = markdown.trim().to_string();
        if markdown.is_empty() {
            return;
        }
        let path: Vec<String> = if kind == UnitKind::Heading {
            self.heading_stack.iter().rev().skip(1).rev().map(|(_, t)| t.clone()).collect()
        } else {
            self.heading_stack.iter().map(|(_, t)| t.clone()).collect()
        };
        self.stats.markdown_bytes += markdown.len() + 2;
        self.units.push(Unit {
            kind,
            level,
            tokens: text::estimate_tokens(&markdown),
            position: self.units.len(),
            text: plain,
            markdown,
            heading_path: path,
        });
    }
}

/// Lowercase and sanity-check a language token.
fn normalize_language(raw: &str) -> Option<String> {
    let lang = raw.trim().trim_matches('"').to_ascii_lowercase();
    let ok = !lang.is_empty()
        && lang.len() <= 16
        && lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '#' || c == '-');
    ok.then_some(lang)
}

fn heading_level(name: &str) -> Option<u8> {
    HEADINGS.iter().position(|h| *h == name).map(|i| i as u8 + 1)
}

fn wrap(inner: &str, marker: &str) -> String {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Preserve the surrounding spacing the original markup implied.
    let lead = if inner.starts_with(char::is_whitespace) { " " } else { "" };
    let trail = if inner.ends_with(char::is_whitespace) { " " } else { "" };
    format!("{lead}{marker}{trimmed}{marker}{trail}")
}

/// Escape only what would otherwise change Markdown block structure.
///
/// Aggressive escaping makes the text harder for a small model to read, so we
/// leave `*` and `_` alone inside words and only defuse link syntax.
fn escape_inline(s: &str) -> String {
    if !s.contains(['[', ']']) {
        return s.to_string();
    }
    s.replace('[', "\\[").replace(']', "\\]")
}

/// Strip the Markdown we just added, to recover plain text for ranking.
pub(crate) fn strip_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_link_text = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '[' => in_link_text = true,
            ']' => {
                in_link_text = false;
                // Drop the following `(...)` target.
                if chars.peek() == Some(&'(') {
                    chars.next();
                    let mut depth = 1;
                    for c in chars.by_ref() {
                        match c {
                            '(' => depth += 1,
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            '*' | '`' | '~' | '#' | '>' if !in_link_text => {}
            _ => out.push(c),
        }
    }
    text::normalize_ws(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_links_and_emphasis() {
        assert_eq!(strip_markdown("**bold** and [a link](http://x.dev/y)"), "bold and a link");
        assert_eq!(strip_markdown("`code` ~~gone~~"), "code gone");
    }

    #[test]
    fn wrap_keeps_outer_spacing() {
        assert_eq!(wrap(" hi ", "**"), " **hi** ");
        assert_eq!(wrap("   ", "**"), "");
    }

    #[test]
    fn escape_only_touches_brackets() {
        assert_eq!(escape_inline("a_b*c"), "a_b*c");
        assert_eq!(escape_inline("see [1]"), "see \\[1\\]");
    }
}
