# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Data tables were being dropped, five different ways.** A table of national
  GDP figures survived none of them, and each cause hid the next:
  - `header` is a chrome token, and it matches inside `sticky-header-multi` —
    the class Wikipedia puts on every sortable table — exactly as it does
    inside `site-header`. Compounds scoped to a component (`sticky-`, `row-`,
    `column-`, `table-`) are now stripped before the vocabulary sees them;
    page-scoped ones still read as boilerplate.
  - Markup weight counted attribute payload in full, so one serialised JSON
    blob per element (334 KB of markup around 7.5 KB of text) made a data
    table look like an ad slot. An attribute value now contributes at most 128
    bytes.
  - Link density and text-to-markup ratio ask whether a container reads like
    prose. A data table answers no however good it is: cells hold a word, and
    a header row that cites its sources is mostly links. Table elements are
    exempt from both; the vocabulary still catches an ad wherever it sits.
  - `is_data_table` rejected anything over 4,000 nodes, and a 223-row table
    comes to 4,522. The fallback for a rejected table is to walk it as
    ordinary blocks, which emits nothing at all — cells are too short to
    survive as paragraphs — so a cap set near real tables deleted the output
    rather than degrading it.
  - Index detection read a linked cell as a listing entry, classifying data
    tables as front pages. Anchors inside a table with header cells are no
    longer counted; tables *without* header cells still are, because that is
    how Hacker News lays out its front page.

### Added

- **Linux wheel builds needed `libclang` and nobody said so.** BoringSSL's
  binding crate build-depends on `bindgen` as well as `cmake`, so a container
  without clang fails with "Unable to find libclang" — a message that names
  neither the crate nor the dependency that wanted it. The release workflow now
  installs it, and both Linux targets build on manylinux 2_28 because bindgen
  needs libclang 9+, newer than the CentOS 7 image can offer.

### Added

- **Scholarly providers**: `arxiv`, `openalex` and `crossref`, all keyless and
  free. OpenAlex abstracts are stored as an inverted index and are reconstructed
  into readable text; arXiv results point at abstract pages rather than PDFs.
  `contact_email` opts into the OpenAlex/Crossref polite pool.
- **Index-page extraction.** A front page, archive or aggregator defeats
  article extraction for the same reason navigation is boilerplate everywhere
  else — except there the link density is the point. Such pages now return an
  inventory instead: titles, URLs, standfirsts and section headings, on
  `Article.links`, with `Article.kind` saying which you got. Detection measures
  how much of the page's prose belongs to a link rather than whether article
  extraction failed, so a listing whose cards carry summaries is still
  recognised. 12 of 12 correct across real news front pages, aggregators,
  encyclopaedia articles, papers, READMEs and specs; ~1% of extraction time,
  and `index_mode="never"` disables it.
- `index:<url>` as a search provider: a site with no feed and no sitemap is
  still collectable.
- `extract_many` in the Python API: batch denoising across the rayon pool with
  the GIL released. ~4x a loop over `extract`, ~30x `trafilatura`.

- **Proxy rotation** (`proxies`): one client per proxy, rotated round-robin.
  A TLS fingerprint says what a client is; an address says who it is, and
  IP-reputation blocking is what fingerprinting cannot touch.
- **`Retry-After` is honoured** in place of the backoff curve, capped by
  `max_retry_after` — past which a delay is a refusal, not a wait.
- **Cookie persistence** (`cookie_file`, `Client.save_cookies()`): clearance
  cookies are the expensive part of getting past a bot wall, and throwing them
  away at process exit means paying for them again.

### Fixed

- **Extraction on pages that defeat the HTML parser.** `tl` does not insert the
  implied end tags a browser would, so an unclosed `<p>`, `<span>`, `<li>` or
  `<table>` can end up owning the rest of the document — and on one real
  Wikipedia article it did, producing a single 54,000-token "paragraph"
  containing the whole page. Four guards now hold, each of which is also just
  correct on well-formed markup:
  - an element is only inline if it carries no block-level content;
  - a `<p>`, `<li>` or `<blockquote>` holding blocks is walked, not flattened;
  - a `<table>` holding headings or sections is laid out, not tabulated;
  - a node holding the majority of a document's text is never boilerplate,
    whatever its class or `role` says.
- **Subtree size was misreported.** `text_ratio`, one of the three headline
  denoising signals, read `tl`'s per-tag source slice — which collapses to the
  opening tag alone when the parser cannot match a close, reporting 28 bytes for
  an element holding 180 KB. Markup weight is now accumulated directly.
- `Unit.text` disagreed with `Unit.markdown` for tables — it carried the source
  subtree's text rather than what was written — so a mis-nested table reported
  3,870 characters of content behind 53 tokens of output, and every measurement
  downstream inherited the error.
- The boilerplate vocabulary missed plurals: `reference` did not match
  `references`, so Wikipedia citation lists survived and ate context budgets.
  High-precision terms are now conclusive at any size, since a reference list or
  comment thread runs far past the length guard that keeps the rest honest.
- JS-gate detection used an absolute text threshold, so a 350 KB front page
  carrying 510 characters of text — plainly an app shell — was never escalated
  to the headless renderer. The test is now the text-to-markup ratio.
- HTML-level redirects (`<meta http-equiv="refresh">` and `location.replace`)
  are now followed, bounded to two hops and gated on a zero delay and a small
  body. Sites that canonicalise URLs in the browser previously extracted to
  nothing — they returned a valid `200` whose body was a redirect stub.

## [0.1.0] — 2026-09-04

First release.

### Added

- **Search router** over free providers — DuckDuckGo, the Wikipedia API, any
  SearXNG instance, RSS/Atom feeds and XML sitemaps — queried concurrently and
  combined with reciprocal rank fusion. URLs are canonicalised before fusion, so
  the same page found by three providers counts once and ranks higher for it.
- **Fetcher** with Chrome TLS/JA3 and HTTP/2 fingerprint emulation via `wreq`,
  plus `robots.txt` enforcement, `Crawl-delay`, per-host pacing, a concurrency
  ceiling, streamed body caps, exponential backoff, and charset detection that
  handles legacy encodings such as EUC-KR.
- **Denoiser** combining structural priors, link density and text-to-HTML ratio
  with a Readability-style content score, emitting clean Markdown units that
  each carry their heading breadcrumb and token cost.
- **Context slimmer** — BM25 at unit granularity, an information-density score,
  and greedy MMR selection with an overlap-coefficient redundancy penalty, under
  a token budget that covers the rendered output rather than just the units.
- **Python bindings** (PyO3, `abi3-py39`): `Client`, `research`, `extract`,
  `slim`, and helpers, all releasing the GIL for the duration of the call.
- Optional headless-Chrome fallback (`browser` feature) for JS-gated pages,
  triggered only after a static fetch has demonstrably returned an empty shell.

### Notes

- `reqwest-impersonate` and its successor `rquest` are both fully yanked on
  crates.io; this project uses their maintained continuation, `wreq`.
- `tl`'s `simd` feature requires a nightly toolchain and is exposed as the
  opt-in `nightly-simd` feature. Text scanning in this crate is SIMD-accelerated
  on stable regardless, via `memchr`.

[Unreleased]: https://github.com/hyeonseok-im/rustai/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/hyeonseok-im/rustai/releases/tag/v0.1.0
