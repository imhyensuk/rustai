# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`Article.chunks()` and `rustai.chunk_many()`, for putting this in a vector
  store.** A `Unit` is one block, which is the grain ranking wants and the
  wrong one for embedding: on MDN's `Promise` reference the median unit is 37
  tokens and 36% are under 30, so embedding units directly gives 125 vectors
  that each know almost nothing. Chunking groups them to a target size,
  starting a new chunk at each heading so a chunk opens with the thing that
  says what it is about, and repeating whole units across the boundary so an
  answer that straddles one stays retrievable from either side. The same page
  becomes 26 chunks with a median of 409 tokens. A unit longer than the target
  is emitted whole rather than cut mid-sentence, and an article that would
  otherwise produce nothing gets one chunk anyway — a short page is still a
  source. `chunk_many` releases the GIL and spreads across the rayon pool.
- **`benches/tokens.py` — what a question costs, and whether the answer
  survives.** Five stages, each where somebody's pipeline actually stops.
  Averaged over seven queries: raw HTML 233,184 tokens, a parser's text dump
  65,071, a real extractor 18,968, every unit this library kept 26,058, and
  the ranked context 1,884 — 0.8% of the raw, and the answer present in all
  seven at every stage. Token counts alone would reward throwing everything
  away, so each stage is checked for patterns any correct source would
  contain.

- **`notebooks/colab_slm_chat.ipynb` — the library used the way it is meant to
  be.** One Colab cell on a T4: it caches a 2–3B Korean-capable model to Google
  Drive, loads it, and runs a grounded conversation where the context comes from
  pages fetched moments earlier. Every question is answered twice by the same
  model, with and without that context, because the fourth goal of this project
  — that the output is useful to a small local model — had never been measured,
  only asserted. It now prices every question as it goes: raw HTML, extracted,
  and the context actually sent, against the model's own window. For "딥러닝이
  뭐야?" that reads 593,439 → 77,957 → 1,627, and the raw pages do not fit in
  a 32k window at all, which is the difference between compression as a
  convenience and as the thing that makes the question answerable.
- **`Context.selected` now says its scores are not comparable across queries.**
  They rank units within one query; BM25 scales with how rare the query's terms
  are. On two live queries an off-topic encyclopedia article scored 0.22 while
  every correct source on the other scored below 0.10, so a fixed cutoff drops
  the good set and keeps the bad one. The field invited exactly that mistake by
  not saying so.
### Added

- **`scripts/check_notebooks.py`, because I shipped this bug twice.** The
  notebooks are for Colab, Colab installs from PyPI, and twice a notebook went
  out calling an argument that existed only on `main` — `max_results`, then
  `respect_crawl_delay` — so the cell died on the first line that touched the
  library. The script reads every `rustai.<name>(...)` out of every notebook
  and checks the name and its keywords against whatever rustai is installed;
  CI now runs it against the published package rather than the checkout. Calls
  guarded by `try` / `except TypeError` are skipped, which is how a notebook
  should reach for something newer than the floor it supports.
- **`respect_crawl_delay` is now settable from Python.** It existed in the Rust
  config and nowhere else, so a Python caller could neither turn it off nor
  discover why a fetch had stalled. A site may publish a `Crawl-delay` in its
  `robots.txt` and this client honours it: arXiv asks for fifteen seconds, so a
  second arXiv URL through the same client waits that long with nothing on the
  network. A new client has not visited anything and does not wait, which is
  how this looks like "reusing a client is slow" until you find the directive.
  `per_host_delay` does not override it — the larger of the two wins.
- **`notebooks/colab_speed_benchmark.ipynb` times this library against five other
  extractors and three HTTP clients.** One cell, real pages, and it separates
  parsers from extractors with output sizes alongside, since comparing the speed
  of two things that are not doing the same work is how scraping benchmarks
  mislead. On eight cores it does not flatter us: `resiliparse` extracts 2.4×
  faster serially and 2.7× faster across cores. Against `trafilatura` the ratio
  runs the other way, 8× serially and 29× across cores, because `trafilatura`
  holds the GIL and gains nothing from threads. The network stage runs three
  rounds and reports a median, since a single measurement lies there — whoever
  runs first pays for cold DNS and TLS — and it lists rustai twice, obeying and
  ignoring `Crawl-delay`, because only rustai reads that line at all. Ignoring
  it, the library fetches fifteen hosts faster than httpx or aiohttp while also
  fetching every robots.txt. Each row reports one round whole rather than a
  median per column, since mixing them produced a line reading 0.3s at 5.4 MB/s
  for a transfer of 5.4 MB. And the extraction ranking is stated as
  hardware-dependent rather than settled: `resiliparse` is 2.4× faster than
  this library on an M1 and slightly slower on Colab's x86 pair.
- **`benches/quality.py --vs-trafilatura` scores a second extractor on the same
  pages.** A number is not good or bad on its own. trafilatura wins on this
  corpus, 75.1% F1 against 71.7%, and two thirds of the difference is Wikipedia
  reference lists that it keeps and this library drops — right for recall
  against a page's own container, wrong for a token budget being spent on an
  answer. The other third is prose, and that part is a real deficit. rustai
  extracts the same twenty pages in 327ms against 3,146ms.
- **`benches/retrieval.py` — does the answer reach the context window?** The
  existing benchmarks measure throughput and extraction; neither says whether
  the library did its actual job for a question. This one does, with no model
  and no prompt in the loop, so the number describes rustai rather than
  whoever wrote the prompt. Each query carries the patterns any correct source
  would contain. Over nine queries: answer present 96%, 90% of the budget spent
  on sources that mention the subject, median 3.2s.
- **`benches/fetch_corpus.py`, so the quality benchmark can actually be run.**
  `quality.py` scores real saved pages, and nothing in the repository fetched
  them — `corpus.py` is the synthetic generator for the throughput benchmark,
  which the docstring pointed at by mistake. The page list is checked in and
  the pages are not: they are other people's copyright, and a frozen copy would
  rot against the live sites the extractor has to survive. An empty directory
  now names the fetcher instead of reporting "no scoreable pages".

### Changed

- **`Client`'s `limit` is now `max_results`, which says what it caps.** The
  constructor sat next to `research(max_sources=…)` and the two look like one
  quantity under two names. They are not: `max_results` is search breadth, how
  many fused hits `search` returns, and `max_sources` is read depth, how many
  of those are fetched and extracted. `Client(max_results=20).research(q,
  max_sources=5)` casts a wide net and reads the best five. `limit` is still
  accepted; passing both with different values is an error rather than a
  silent winner.
- **crates.io publishing no longer uses a stored token.** The release job now
  exchanges its GitHub OIDC identity for a crates.io token that lives for the
  length of the job and is revoked by the action's post step, which is what the
  PyPI job has always done. A token in repository secrets is a credential that
  outlives the release it was needed for — this one was set to expire in three
  months, which is a deadline to forget rather than a safeguard. Trusted
  publishing needs a crate to already exist, so it could not have been used for
  the first publish; 0.2.0 established the crate, so it can be used from here.

  A manual dispatch now rehearses the handshake and stops before uploading, so
  a wrong trust configuration surfaces in two minutes rather than at the last
  job of a real release, after every wheel has been built. That is how both
  previous crates.io attempts failed.

## [0.2.0] — 2026-09-06

### Added

- **Four more search providers, taking the router from nine kinds to thirteen.**
  All keyless, all verified against the live APIs before being written:
  - `europepmc` — the life sciences, covering PubMed, PMC and preprints in one
    request where PubMed's own API needs two. Abstracts arrive as JATS, the
    same markup Crossref sends, and are stripped the same way.
  - `hackernews` — via the Algolia index behind the site's own search. A story
    points at what it links to; a `Show HN` has no link of its own, so the
    thread is the destination.
  - `stackexchange` — Stack Overflow by default, any site by key
    (`stackexchange:serverfault`). Requested with the `withbody` filter,
    without which the API returns titles and scores that rank a result but
    answer nothing.
  - `github` — repository search, with the language and star count that decide
    whether a result is worth opening.

  Semantic Scholar was tried and dropped: it rate-limits unauthenticated
  callers hard enough that a provider fired concurrently with the others
  fails more often than it answers.

- **`max_tokens_per_source` is now accepted by `research()` and `Client`,** not
  only by `slim`. One long page taking most of the window is a real failure —
  in one measured query an article took 86% of a 1,500-token budget and starved
  the encyclopedia entry on the exact term asked about — but it is not the
  common case, so the default stays `None`. Over six queries at budgets of
  1,000, 2,048 and 4,096 tokens, capping either changed nothing or traded
  relevance for coverage, and coverage rises on its own with the budget (84% of
  fetched sources represented at 1,000 tokens, 97% at 4,096). The class
  docstring says when to reach for it; PyO3 discards a doc comment on `#[new]`,
  so `Client`'s parameters are documented on the class rather than `__init__`.

### Changed

- **Denoising thresholds tuned against a measured corpus rather than judgement.**
  `benches/quality.py` scores extraction on real pages two ways — five-word
  shingle overlap with the page's own article container, and a count of site
  furniture that reached the output — because either alone points the wrong
  way. Judged on overlap, the best setting is to switch every threshold off;
  that quadruples the furniture, since the container being compared against
  never held the navigation. `min_text_ratio` drops 0.06 → 0.02 and
  `max_link_density` 0.5 → 0.6; `min_block_len` stays at 25, being the one
  threshold that guards against furniture rather than widgets. F1 over 23 pages
  goes 59.1% → 63.1% with furniture unchanged, and 7% more text is kept.

### Fixed

- **A heading's plain text kept the Markdown link wrapping it.** `Unit.text`
  is documented as plain text and a paragraph's is, but a heading's was the
  rendered Markdown — and modern documentation wraps every heading in its own
  anchor, so `[Description](https://…#description)` went into the text that
  BM25 scores, into every heading breadcrumb, and now into everything an
  embedding model would read. Extraction F1 over twenty pages goes 71.7% →
  72.6% and precision 83.9% → 85.5%, which is what happens when a URL stops
  counting as extracted text.
- **A `<` followed by punctuation opened a phantom element, losing the rest of
  the document.** After `<`, HTML opens an element only for a letter, an end
  tag for `/` and a declaration for `!`; `?` starts a bogus comment, discarded
  at the next `>`, and every other byte is not markup at all. `tl` opens an
  element for any of them, and since that element is never closed, everything
  after it nests inside and the denoiser drops the lot — twenty-seven
  punctuation bytes each costing the whole page. Two occur in the wild: MDN
  emits a bare `<?>` where its build tool left a template hole, and a
  misconfigured server leaking `<%= %>` or `<?php ?>` loses the page entirely.
  Bogus comments are now removed before parsing and every other `<` is escaped
  to the character it means. MDN's `Promise` reference goes from 16,498
  characters extracted to 27,975, taking its F1 from 66.6% to 86.0%; the
  corpus mean rises 70.7% → 71.7% with precision and furniture unchanged.
- **`tl` does not treat script bodies as raw text, so a comparison operator in
  minified JavaScript swallowed the page.** HTML says nothing inside `<script>`
  starts a tag until the end tag; `tl` opens one anyway, so `for(i=0;i<n;i++)`
  produces a phantom `<n…>` that consumes the real `</script>`. The script then
  stays open and every node after it becomes its descendant — and the denoiser
  drops scripts on sight, so the page extracts to almost nothing. Script and
  style bodies are now removed before parsing, and `application/ld+json` is kept
  with `<` escaped so the metadata reader still sees it. Measured over 46 real
  pages, 7% carry such a script; one of them went from 5.9% of its text
  extracted to 76.7%.
- **A link that wraps a heading is titular.** Modern listings make the whole
  card an anchor — `<a><h2>Title</h2><span>Read More</span></a>` — which is the
  classic `<h2><a>…</a></h2>` with the nesting inverted. Index harvesting looked
  only upwards and so found nothing at all on those pages.
- Code blocks join tables in being exempt from the link-density and
  text-to-markup tests, and so does a container holding mostly code. A syntax
  highlighter wraps every token in a `<span>` and inlines a theme, so
  highlighted code fails both tests for the same reason a data table does —
  and exempting `<pre>` alone does not help, because the `<div>` around it is
  judged on its own, fails, and takes the code with it. On one Korean blog post
  this recovered every code block (4 of 11 to 11 of 11) and three paragraphs.

## [0.1.0] — 2026-09-04

First release.

### Added

- **Linux wheel builds needed `libclang` and nobody said so.** BoringSSL's
  binding crate build-depends on `bindgen` as well as `cmake`, so a container
  without clang fails with "Unable to find libclang" — a message that names
  neither the crate nor the dependency that wanted it. The release workflow now
  installs it, and both Linux targets build on manylinux 2_28 because bindgen
  needs libclang 9+, newer than the CentOS 7 image can offer.

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

### Fixed

- **Extraction stopped early on pages whose content root scores onto a
  fragment.** The guard against a bad root asked whether it was *starved* —
  under five percent of the document's text — which catches a root that
  collapsed to nothing and misses every root that merely stopped early. That
  is the common failure: a docs page split into sibling sections gives a root
  holding one of them, comfortably above the threshold and comfortably wrong.
  The container is now extracted too and the larger result kept, both having
  been through the same denoiser. Measured over 34 cached pages: 20% more text
  overall, nothing regressed, no page reclassified. `python.org`'s asyncio
  reference goes from 19.5% of its text to 90.6%, `doc.rust-lang.org`'s `Vec`
  from 48% to 87.5%. A gate skips the second walk when the root already covers
  the container, which keeps throughput where it was (1,796 docs/s against
  1,820).
- The chosen content root is no longer re-judged by the boilerplate rules
  before being walked. It arrives already decided, and an article container
  that still holds the navigation it is about to drop reads as a link farm by
  raw link density — rejecting it discarded the document to save the part that
  was leaving anyway.


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

### Notes

- `reqwest-impersonate` and its successor `rquest` are both fully yanked on
  crates.io; this project uses their maintained continuation, `wreq`.
- `tl`'s `simd` feature requires a nightly toolchain and is exposed as the
  opt-in `nightly-simd` feature. Text scanning in this crate is SIMD-accelerated
  on stable regardless, via `memchr`.

[Unreleased]: https://github.com/imhyensuk/rustai/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/imhyensuk/rustai/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/imhyensuk/rustai/releases/tag/v0.1.0
