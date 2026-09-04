# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
