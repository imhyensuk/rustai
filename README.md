# rustai

**Local, zero-cost research for local models.** Search the open web, strip it down
to the text that matters, and hand a small model a context window it can
actually use — with no API key, no per-call billing, and no heavyweight Python
crawler in the middle.

```python
import rustai

result = rustai.research("how does BM25 handle document length", max_sources=5, max_tokens=2048)
print(result.markdown)   # cited, deduplicated, ranked Markdown — ready to prompt
```

[![CI](https://github.com/imhyeonsuk/rustai/actions/workflows/ci.yml/badge.svg)](https://github.com/imhyeonsuk/rustai/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/rustai.svg)](https://pypi.org/project/rustai/)
[![crates.io](https://img.shields.io/crates/v/rustai-core.svg)](https://crates.io/crates/rustai-core)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

---

## Why this exists

Retrieval for a local model is a data-cleaning problem wearing a search costume.
The search itself is the easy part; what actually decides whether a 7B model
answers well is how much of its 4k window you spent on cookie banners.

Hosted APIs solve this for a fee. Python crawler stacks solve it by pulling in a
browser-grade DOM, which is where the memory goes. `rustai` does it locally in
Rust:

| | Hosted search API | Python crawler stack | `rustai` |
|---|---|---|---|
| Cost per query | metered | free | free |
| API key | required | — | none |
| Extraction | vendor's | yours to build | built in |
| Context compression | rarely | yours to build | built in |
| Scholarly sources | rarely | yours to build | built in |
| Peak RSS, 200 docs | n/a | +15.5 MB | +4.4 MB |

See [Benchmarks](#benchmarks) for how those numbers were produced.

## Try it in Colab

[`notebooks/colab_quickstart.ipynb`](notebooks/colab_quickstart.ipynb) runs the
whole pipeline in a browser — offline denoising, live collection, the search
providers, a benchmark against `trafilatura`, and a look at the TLS fingerprints.

[`notebooks/colab_oneshot.ipynb`](notebooks/colab_oneshot.ipynb) is the same
thing as a single cell: build, install, and a report covering denoising
accuracy, throughput, live fetches, every provider, and the pipeline end to end.

Until wheels are on PyPI, Colab has to build from source, and the BoringSSL
dependency makes that a 10–20 minute first run. Both notebooks cache the wheel
to Drive, so every session after the first installs in seconds. They install
`clang` and `libclang-dev` before building: BoringSSL's bindings run `bindgen`,
which needs `libclang`, and the Colab base image does not ship it.

## Install

```bash
pip install rustai
```

Wheels are published for Linux, macOS and Windows on CPython 3.9+ (a single
`abi3` wheel per platform). No Rust toolchain needed to install.

For the Rust crate (published as `rustai-core`, since `rustai` was taken on crates.io):

```bash
cargo add rustai-core
```

## What it does, stage by stage

```
query ──▶ search router ──▶ fetcher ──▶ denoiser ──▶ slimmer ──▶ context
          concurrent        impersonated  DOM heuristics  BM25 + density
          free providers    + polite      + Markdown      + MMR dedupe
```

### 1. Search router — free providers, fused

| Kind | Providers |
|---|---|
| Web search | `duckduckgo`, `searxng:<instance>` |
| Reference | `wikipedia`, `wikipedia:ko` (any language edition) |
| Scholarly | `arxiv`, `openalex`, `crossref` |
| Sites you trust | `rss:<feed>`, `sitemap:<sitemap.xml>`, `index:<front page>` |

All keyless, all free, all queried concurrently; a provider that fails or
rate-limits degrades the result set instead of failing the call.

The scholarly providers are not a nicety. A web search for a paper returns blog
posts *about* the paper; arXiv, OpenAlex and Crossref return the paper. OpenAlex
stores abstracts as an inverted index for licensing reasons, and `rustai`
reconstructs them, which makes its snippets the most informative of any provider
here. arXiv results always point at the abstract page, never the PDF, because a
PDF is not something this pipeline can read.

`index:` harvests an HTML listing page — a front page, an archive, a forum — for
the links it offers. A site with no feed and no sitemap is still collectable.

OpenAlex and Crossref run a faster "polite pool" for callers who identify
themselves — pass `contact_email` to use it.

Rankings are combined with **reciprocal rank fusion**, because provider scores
are not comparable to each other but ranks always are. URLs are canonicalised
first — `www.`, tracking parameters, fragments and trailing slashes stripped —
so a page found by three providers counts once and ranks higher for it.

### 2. Fetcher — looks like a browser, behaves like a guest

Bot walls read your TLS ClientHello (JA3/JA4) and HTTP/2 SETTINGS frame, not
your User-Agent. `rustai` replays a real Chrome's via
[`wreq`](https://crates.io/crates/wreq), so ordinary pages return ordinary
`200`s.

Being able to get in is not a licence to be rude, so `robots.txt`, `Crawl-delay`,
per-host spacing, a concurrency ceiling, capped bodies and backoff-on-retry are
all on by default. Legacy encodings (EUC-KR, Shift_JIS) are decoded properly
rather than assumed to be UTF-8.

When a site answers with `Retry-After`, that is honoured in place of the
backoff curve — a server's own number beats any we could invent — up to
`max_retry_after`, past which a delay is really a refusal.

For IP-reputation blocking, which fingerprinting cannot touch, pass `proxies`
and requests rotate round-robin across them. `cookie_file` persists the jar, so
a session — clearance cookies included — survives the process.

HTTP redirects are followed, and so are HTML-level ones: a stub page that
redirects through `<meta refresh>` or `location.replace` returns a perfectly
good `200` containing no content, which is how sites that canonicalise URLs in
the browser silently produce empty extractions elsewhere.

For the minority of pages that ship an empty shell and build the DOM in
JavaScript, an opt-in headless Chrome fallback fires — but only after a cheap
static fetch has demonstrably failed, since rendering costs ~100× a fetch.

### 3. Denoiser — find the article, drop the furniture

Three signals, in order:

1. **Structural priors** — `<nav>`, `<footer>`, `<aside>`, `role="banner"`, and a
   class/id vocabulary that has been stable for a decade.
2. **Link density** — anchor text over total text. Navigation and "related posts"
   rails approach 1.0; prose approaches 0.
3. **Text-to-HTML ratio** — visible characters over serialised markup bytes. Ad
   slots and widgets are almost pure markup.

Then a Readability-style content score picks the container holding the article,
and the survivors are written as clean Markdown — headings, lists, fenced code,
pipe tables, absolutised links.

The output is not one blob. It is a list of **units**, each carrying its own text,
its Markdown, its heading breadcrumb and its token cost. That granularity is what
makes the next stage possible.

**Listing pages** take a different path. A front page defeats article extraction
for the same reason navigation is boilerplate everywhere else — except here the
link density *is* the content. `rustai` detects that by measuring how much of the
page's prose belongs to a link, and returns an inventory instead: titles, URLs,
standfirsts and section headings, ready to fetch. `article.kind` tells you which
you got. On thirteen real pages spanning news front pages, aggregators, encyclopaedia
articles, papers, READMEs and specs, the classifier is 12 for 12; it costs about
1% of extraction time and `index_mode="never"` turns it off.

### 4. Slimmer — spend the context window deliberately

Given a question and a pile of articles, choose the units that best fill a
budget:

- **BM25** relevance at unit granularity, so you get the two paragraphs that
  answer the question rather than the page that contains them.
- **Information density** — content-word ratio, numeric ratio, type-token ratio,
  minus an explicit boilerplate penalty. This is what separates a dense factual
  paragraph from *"In this article we will explore some of the things you need to
  know"*, which matches query terms perfectly and says nothing.
- **MMR selection** with an overlap-coefficient redundancy penalty, because three
  copies of the same syndicated paragraph is the most common way to waste a
  window.

Output is Markdown with source headers, URLs, heading breadcrumbs and `[…]`
elision markers, under the token budget you set.

## Usage

### One call

```python
import rustai

r = rustai.research("what changed in HTTP/3", max_sources=5, max_tokens=2048)
print(r.markdown)          # the context
print(r.context.tokens)    # what it actually cost
for src in r.context.sources:
    print(src["url"], src["tokens"])
for stage, message in r.failures:
    print("skipped:", stage, message)   # nothing is swallowed silently
```

### A reusable client

Sharing a client shares its connection pool, its `robots.txt` cache and its
per-host pacing state, so it is meaningfully faster than repeated one-shot calls.

```python
client = rustai.Client(
    providers=["duckduckgo", "wikipedia:en", "arxiv", "openalex"],
    contact_email="you@example.com",   # OpenAlex/Crossref polite pool
    max_tokens=4096,
    concurrency=24,
    impersonate="chrome",       # or "firefox", "safari", "random", "chrome_143", "none"
    respect_robots=True,
    proxies=["socks5://user:pass@host:1080"],   # rotated round-robin
    cookie_file="~/.cache/rustai/jar.json",     # session survives the process
)
...
client.save_cookies()       # write the jar back out

hits = client.search("rust async runtime")           # list[SearchResult]
pages = client.fetch([h.url for h in hits[:5]])      # list[Page]   — raw HTML
articles = client.read([h.url for h in hits[:5]])    # list[Article] — cleaned
result = client.research("rust async runtime")       # the whole pipeline
```

### Offline: clean HTML you already have

No network, no client, nothing to configure:

```python
article = rustai.extract(html, url="https://example.com/post")

article.title            # str | None
article.markdown         # cleaned Markdown
article.text             # plain text
article.tokens           # estimated LLM tokens
article.stats.compression  # e.g. 0.94 — how much was dropped
for unit in article.units:
    print(unit.kind, unit.tokens, unit.heading_path, unit.text[:60])
```

For a batch, `extract_many` runs across every core and releases the GIL, so it
is roughly 4× the throughput of a loop over `extract` — and about 30× that of
`trafilatura`:

```python
articles = rustai.extract_many(list_of_html)                 # 1:1 with the input
articles = rustai.extract_many(list_of_html, list_of_urls)   # positional urls
```

### Compress a set you assembled yourself

```python
articles = client.read(my_urls)
ctx = rustai.slim("my question", articles, max_tokens=1024, max_tokens_per_source=400)
print(ctx.markdown)
```

`max_tokens_per_source` caps how much any single page can contribute, so one long
article cannot crowd out corroborating sources.

### Listing pages

```python
front = rustai.extract(html, "https://news.example/")
if front.kind == "index":
    for link in front.links:
        print(link.text, link.url, link.heading_path)
    urls = [l.url for l in front.links]
    articles = client.read(urls)      # now go read them
```

### Scholarly search

```python
client = rustai.Client(
    providers=["arxiv", "openalex", "crossref"],
    contact_email="you@example.com",
)
for hit in client.search("sparse attention long context"):
    print(hit.title)
    print(" ", hit.url)
    print(" ", hit.snippet[:120])   # authors (year). abstract…
```

### Your own sites, without a search engine

```python
client = rustai.Client(providers=[
    "rss:https://blog.rust-lang.org/feed.xml",
    "sitemap:https://doc.rust-lang.org/sitemap.xml",
])
```

Feeds and sitemaps are the highest-signal sources here — complete, ordered, and
free of anyone else's ranking.

### Utilities

```python
rustai.count_tokens("some text")     # budget estimate, CJK-aware
rustai.density("some text")          # 0.0–1.0 information density
rustai.tokenize("한국어 텍스트")       # the ranker's own tokens
rustai.canonical_url("https://www.x.com/a/?utm_source=b")   # 'x.com/a'
rustai.parse_feed(xml)               # list[dict]
rustai.parse_sitemap(xml)            # {"urls": [...], "sitemaps": [...]}
```

## Errors

Everything derives from `rustai.RustaiError`:

| Exception | Raised when |
|---|---|
| `NetworkError` | transport failure, timeout, oversized body |
| `HttpStatusError` | non-2xx response |
| `RobotsError` | `robots.txt` disallows the URL |
| `ExtractError` | the document could not be parsed |
| `ProviderError` | a search provider failed |
| `BrowserError` | headless fallback unavailable |

Batch calls (`fetch`, `read`) skip failures by default; pass
`raise_on_error=True` to get the first one instead. `search` returns partial
results by default; pass `strict=True` to raise when every provider fails.

## Rust API

```rust
use rustai_core::pipeline::Pipeline;

# async fn run() -> rustai_core::Result<()> {
let pipeline = Pipeline::new()?;
let research = pipeline.research("what is BM25", 5).await;
println!("{}", research.context.markdown);
# Ok(()) }
```

Every stage is public and usable alone: `rustai_core::search::Router`,
`rustai_core::http::Fetcher`, `rustai_core::parse::extract`, `rustai_core::rank::slim`.

### Cargo features

| Feature | Default | What it does |
|---|---|---|
| `impersonate` | ✅ | Chrome TLS/JA3 + HTTP/2 fingerprint emulation |
| `python` | — | PyO3 bindings (enabled by maturin) |
| `browser` | — | headless-Chrome fallback via `chromiumoxide` |
| `nightly-simd` | — | SIMD tokenisation inside `tl`; **requires a nightly toolchain** |

Text scanning in this crate is SIMD-accelerated on stable regardless, via
`memchr`. The `nightly-simd` feature only affects `tl`'s own HTML tokeniser,
which is gated on `#![feature(portable_simd)]` upstream.

## Being a good citizen

This library makes it easy to hit other people's servers quickly, so the
defaults lean conservative:

- `robots.txt` is honoured for every content fetch, with `Crawl-delay` respected.
- Requests to one host are spaced 250 ms apart; total concurrency is capped.
- Response bodies are capped at 8 MB and aborted mid-stream past that.
- Retries back off exponentially rather than hammering a rate limit.

Search-provider endpoints you name explicitly (DuckDuckGo's HTML endpoint, a feed
URL) skip the `robots.txt` check, because a query is not a crawl and several of
those endpoints disallow the very path they exist to serve. Pages discovered
*through* them go through the normal checked path.

Impersonation exists so ordinary reading is not misclassified as abuse. It is not
a licence to ignore a site's terms, and you are responsible for what you point
this at.

Be clear-eyed about what it does. Measured against ten sites, the profiles
produce genuinely distinct JA3, JA3N and Akamai HTTP/2 fingerprints — but on
sites with real bot defences (Cloudflare Enterprise, PerimeterX) turning
impersonation on changed the outcome on **none** of them. Those blocks key on
datacenter IP reputation and behaviour, not on the ClientHello. A fingerprint is
necessary, not sufficient; `proxies` is the knob that addresses the rest, and
some sites you simply should not be scraping.

## Benchmarks

Reproduce with:

```bash
python benches/corpus.py /tmp/rustai-corpus
cargo run --release --example bench -- /tmp/rustai-corpus   # Rust only
python benches/benchmark.py                                  # batch, vs bs4+lxml
RUSTAI_BENCH_STREAM=1 python benches/benchmark.py            # streaming
```

The corpus is 200 synthetic article pages, 9.43 MB of HTML, ~46 KB each, shaped
like real ones: heavy chrome, nested wrappers, ad slots, a sidebar, a script
blob. Synthetic so the benchmark is deterministic and redistributable — the
chrome-to-content ratio is what an extractor is tested on, not raw size.

**Rust alone**, streaming one document at a time (Apple M1, macOS 26.6, release
build):

```
documents      200
input          9.43 MB
output         2.27 MB in 12000 units
compression    75.9%
throughput     638 docs/s, 30.1 MB/s
peak RSS       4.9 MiB
```

**From Python** (CPython 3.14):

| Engine | Throughput | vs `trafilatura` |
|---|---|---|
| `rustai.extract_many` | **3,545 docs/s** | **16.1×** |
| `rustai.extract` (loop) | 840 docs/s | 3.8× |
| `trafilatura` | 220 docs/s | 1× |

Median of three runs. `trafilatura` is the fair comparison — it is the closest
equivalent, since it also produces structured Markdown — and both emit the same
2.27 MB of output from the same input. `extract_many` runs across the rayon pool
with the GIL released, which is where the further 4.2× comes from.

Peak process RSS, streaming one document at a time: `rustai` 39.3 MB against
`bs4`+`lxml`'s 50.4 MB — **+4.4 MB** versus **+15.5 MB** over the 34.9 MB floor
of a CPython 3.14 interpreter holding the corpus.

Concurrent fetching, measured on 20 live URLs across many hosts: 1.7 pages/s
serial versus 15.1 concurrent, an **8.7× speedup**. Beyond that the bottleneck
is the remote server, not this library.

"Marginal RSS" subtracts the 34.9 MB floor of a CPython 3.14 interpreter holding
the corpus, which both engines pay identically. Importing `rustai` itself costs
1.0 MB over a bare interpreter.

Caveats worth stating plainly. The `bs4` baseline produces a flat string while
`rustai` produces structured Markdown units with metadata, so that row
understates `rustai`'s work per document; the `trafilatura` row is the honest
comparison. And these are single-machine numbers on one synthetic corpus — run
the benchmark on your own pages before trusting them.

## Development

```bash
cargo test                    # Rust unit tests, no network
cargo test --features browser # includes the headless fallback build
maturin develop --release     # build and install into the active venv
pytest                        # Python tests
pytest -m network             # the live-internet tests, off by default
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.
