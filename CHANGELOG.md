# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Scholarly providers**: `arxiv`, `openalex` and `crossref`, all keyless and
  free. OpenAlex abstracts are stored as an inverted index and are reconstructed
  into readable text; arXiv results point at abstract pages rather than PDFs.
  `contact_email` opts into the OpenAlex/Crossref polite pool.
- `extract_many` in the Python API: batch denoising across the rayon pool with
  the GIL released. ~4x a loop over `extract`, ~30x `trafilatura`.

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
- The boilerplate vocabulary missed plurals: `reference` did not match
  `references`, so Wikipedia citation lists survived and ate context budgets.
  High-precision terms are now conclusive at any size, since a reference list or
  comment thread runs far past the length guard that keeps the rest honest.
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
