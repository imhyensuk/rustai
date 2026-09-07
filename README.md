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

[![CI](https://github.com/imhyensuk/rustai/actions/workflows/ci.yml/badge.svg)](https://github.com/imhyensuk/rustai/actions/workflows/ci.yml)
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
| 200 docs | n/a | 0.78s, 55.6 MB | 0.11s, 51.9 MB |

See [Benchmarks](#benchmarks) for how those numbers were produced.

## How it compares

The honest version, because every number below is one this repository measures
and you can re-run.

**Against extraction libraries** — `trafilatura`, `resiliparse`,
`readability-lxml`, `justext`. They do one of the five things this does, and
two of them do it well:

| | rustai | trafilatura | resiliparse |
|---|---|---|---|
| Extraction F1, 20 real pages | 72.6% | **75.1%** | not scored |
| Serial extraction | 64 docs/s | 5 docs/s | **103 docs/s** |
| Across cores | 77 docs/s | 4 docs/s | **116 docs/s** |
| Search routing | 13 provider kinds | — | — |
| Ranking to a token budget | built in | — | — |
| Tokens to answer a question | **1,884** | 18,968 | 18,968 |

`trafilatura` extracts better. Two thirds of its 2.5-point lead is Wikipedia
reference lists that it keeps and this library drops — right for recall against
a page's own container, wrong for a token budget being spent on an answer — but
the remaining third is prose, and that part is a genuine deficit.

`resiliparse` extracts faster, by 1.6× here. It is C++ and extraction is its
only job. The ranking flips with the hardware: on an Apple M1 it was 2.4× ahead,
on Colab's x86 pair it has come out both ahead and 5% behind on different runs.
Do not trust either number without running it on your machine.

Neither closes the gap that matters. Feeding a question's pages to a model costs
18,968 tokens through the best extractor and 1,884 through this one, with the
answer present either way — a tenth, because extraction is not the last stage
here. **If you want the best article text, use `trafilatura`. If you want the
fewest tokens that still answer the question, that is this library.**

**Against crawler frameworks** — Scrapy, Crawl4AI. They are frameworks: you
bring the pipeline and they schedule it. This is a library with one opinion,
which is the shape of the output. Not benchmarked here, so no numbers claimed.

**Against hosted search and scrape APIs** — Tavily, Exa, Firecrawl, Jina
Reader. They will out-recall a keyless router because they run their own index.
They are also metered, keyed, and see every query you make. This runs on your
machine, costs nothing per call, and works offline for the extraction half.

**Against HTTP clients** — `httpx`, `aiohttp`, `requests`. Fetching fifteen
distinct hosts, this library is within noise of the async clients while also
reading every `robots.txt` and honouring the `Crawl-delay` they ignore. On the
same corpus arXiv asks for fifteen seconds between requests; only this client
waits.

## Try it in Colab

[`notebooks/colab_extreme.ipynb`](notebooks/colab_extreme.ipynb) is the one to
open. Nine sections, each measuring something rather than claiming it: a listing
page turned into a crawl frontier, concurrency held against itself, thirty pages
fetched and extracted with the memory measured, eight provider kinds fused, the
token budget checked at six sizes, a benchmark against `trafilatura`, and the
whole pipeline down to a prompt. Installing is one `pip` line, so the notebook
spends its length on the library rather than on a build.

[`colab_slm_chat.ipynb`](notebooks/colab_slm_chat.ipynb) is the one that answers
the question this library exists for: does any of it help a small model? One cell
on a T4 — installs, caches a 2–3B Korean-capable model to Google Drive, loads it,
and holds a conversation where every answer is grounded in pages fetched seconds
earlier. Each question is answered twice by the same model, once from memory and
once from retrieved context, because a retrieval library that is never compared
against not retrieving is a library nobody has measured.

[`colab_speed_benchmark.ipynb`](notebooks/colab_speed_benchmark.ipynb) times this
library against five other extractors and three HTTP clients on the same pages, in
one cell. It does not flatter us: `resiliparse`, a C++ extraction library, extracts
2.4× faster on an M1 — and 5% slower on Colab's x86 pair, which is why the notebook
tells you to run it on your own machine rather than trust either number. It also
separates parsers from extractors and prints output sizes, because comparing the
speed of two things that are not doing the same work is how most scraping
benchmarks mislead, and it lists rustai twice on the network stage, obeying and
ignoring `Crawl-delay`, since it is the only client there that reads that line.

[`colab_quickstart.ipynb`](notebooks/colab_quickstart.ipynb) walks the same
ground more slowly, and [`colab_oneshot.ipynb`](notebooks/colab_oneshot.ipynb)
is a single cell that installs and prints a report. Both predate the PyPI
release and still know how to build from source, which is only useful now if you
want the `browser` feature.

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

## Five minutes

Each step below runs on its own and answers the question the previous one
leaves you with. Nothing here needs configuring first.

**1 — Ask a question.** Search, fetch, clean and compress, in one call.

```python
import rustai

r = rustai.research("how does BM25 handle document length", max_tokens=1024)
print(r.markdown)          # cited Markdown, ready to paste into a prompt
```

**2 — See what it cost, and what it skipped.** A retrieval step you cannot
account for is one you cannot debug.

```python
print(r.context.tokens)                    # 1017 — under the 1024 you asked for
for src in r.context.sources:
    print(src["tokens"], src["url"])       # who contributed what
for stage, why in r.failures:
    print("skipped:", stage, why)          # dead links say so
```

The budget is a guarantee, not an estimate: the slimmer renders, measures, drops
the weakest unit and repeats until the real total fits.

**3 — Clean HTML you already have.** No network, no client, no keys — useful for
seeing what the denoiser does before you trust it with anything.

```python
article = rustai.extract(html, url="https://example.com/post")

article.markdown              # nav, footer, ads and share widgets gone
article.units                 # the same thing as rankable blocks
article.stats.compression     # how much was dropped
```

If you pass a URL here by mistake it will tell you so — `extract` does not
fetch. Use step 4 for that.

**4 — Bring your own URLs.** Read pages, then compress them against a query of
your choosing.

```python
client = rustai.Client(contact_email="you@example.com")
articles = client.read([
    "https://en.wikipedia.org/wiki/Okapi_BM25",
    "https://en.wikipedia.org/wiki/Tf%E2%80%93idf",
])

ctx = rustai.slim("document length normalisation", articles, max_tokens=800)
print(ctx.markdown)
```

`read` fetches concurrently and drops URLs that failed, so the list you get back
may be shorter than the one you passed. Match on `Article.url`, or pass
`raise_on_error=True`.

**5 — Follow a front page.** Listing pages come back as link inventories rather
than prose, which makes them usable as crawl seeds.

```python
front = client.read(["https://blog.rust-lang.org/"])[0]
front.kind                    # "index"
for link in front.links:
    print(link.text, link.url, link.snippet)
```

**Where to go next:** [Usage](#usage) for the full surface, [Being a good
citizen](#being-a-good-citizen) before you point it at anyone else's server, and
the Colab notebook above if you would rather run than read.

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
| Scholarly | `arxiv`, `openalex`, `crossref`, `europepmc` |
| Community | `hackernews`, `stackexchange`, `stackexchange:<site>`, `github` |
| Sites you trust | `rss:<feed>`, `sitemap:<sitemap.xml>`, `index:<front page>` |

All keyless, all free, all queried concurrently; a provider that fails or
rate-limits degrades the result set instead of failing the call.

The scholarly providers are not a nicety. A web search for a paper returns blog
posts *about* the paper; arXiv, OpenAlex and Crossref return the paper. OpenAlex
stores abstracts as an inverted index for licensing reasons, and `rustai`
reconstructs them, which makes its snippets the most informative of any provider
here. arXiv results always point at the abstract page, never the PDF, because a
PDF is not something this pipeline can read.

`europepmc` covers the life sciences — PubMed, PMC and preprints — in one
request, where PubMed's own API needs two.

The community providers answer a different kind of question. A paper explains
what a method is; a Stack Exchange thread explains why it did not work for
somebody, a Hacker News thread explains what practitioners argued about it, and
a GitHub result says whether anyone implemented it. `stackexchange` defaults to
Stack Overflow and takes any site key — `stackexchange:serverfault`,
`stackexchange:stats`. GitHub search is capped at ten requests a minute without
a token, which one query per search fits inside.

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

BM25 is lexical, so it cannot see that "딥러닝" and "deep learning" name one
subject, or that a paragraph explains a term without using it. Pass vectors from
a model you already run and cosine similarity folds into the same selection —
this library bundles no embedding model, because that would trade a keyless
install and a fast build for a few hundred megabytes of weights. Swept over
nine questions with a multilingual MiniLM, the blend beats both ends: pure BM25
answers 96%, pure cosine 96%, half and half 100%.

### 5. Or: chunks, if the destination is a vector store

A unit is one block, which is the right grain to *rank* and the wrong one to
*embed* — on MDN's `Promise` reference the median unit is 37 tokens and a third
are under 30, so embedding units directly gives 125 vectors that each know
almost nothing. `article.chunks()` groups them to a target size, starting a new
chunk at each heading so a chunk opens with the thing that says what it is
about, and repeating whole units across the boundary so an answer that straddles
one stays retrievable from either side. The same page becomes 26 chunks with a
median of 409 tokens, each carrying its URL, title and heading path.

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
article cannot crowd out corroborating sources. It is also accepted by `research()`
and `Client`, so you do not have to drop down to `slim` to reach it:

```python
r = rustai.research("what is BM25 term saturation", max_tokens_per_source=500)
```

Leave it off for most questions. Measured over six queries at budgets of 1,000,
2,048 and 4,096 tokens, capping either changed nothing or bought source coverage
by admitting less relevant text — and coverage climbs on its own as the budget
grows, from 84% of fetched sources represented at 1,000 tokens to 97% at 4,096.
Reach for it when a question wants corroboration rather than depth: two of those
six queries had one long page take 86% and 90% of the window, in one case
starving the encyclopedia article on the exact term asked about.

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

### Hybrid ranking with your own embeddings

```python
articles = client.read(urls)
units = [u.text for a in articles for u in a.units]     # this exact order

ctx = rustai.slim(
    "왜 위치 인코딩이 필요한가", articles,
    query_vector=model.encode("왜 위치 인코딩이 필요한가").tolist(),
    unit_vectors=model.encode(units).tolist(),
    semantic_weight=0.5,        # 0 is pure BM25, 1 is pure cosine
)
```

One vector per unit, flattened over articles in order. Short units are dropped
before scoring, so the alignment is against *every* unit rather than the
survivors — a wrong-sized list is refused with the expected length rather than
quietly mis-ranked. Vectors need not be normalised.

### Chunks for a vector store

```python
for chunk in article.chunks(target_tokens=512, overlap_tokens=64):
    store.add(chunk["text"], metadata=chunk)

chunks = rustai.chunk_many(articles)      # parallel across every core
```

Each chunk is a dict with `text`, `markdown`, `tokens`, `url`, `title`,
`heading_path`, `index` and `units` — enough for a retrieved chunk to cite
itself without a second lookup.

### Your own sites, without a search engine

```python
client = rustai.Client(providers=[
    "rss:https://blog.rust-lang.org/feed.xml",
    "sitemap:https://doc.rust-lang.org/sitemap.xml",
])
```

Feeds and sitemaps are the highest-signal sources here — complete, ordered, and
free of anyone else's ranking.

## API reference

Every name below is exported from `rustai`; the stub in
`python/rustai/_rustai.pyi` carries the same signatures with full docstrings,
so an editor will complete them.

### Entry points

| Call | Returns | Use it when |
|---|---|---|
| `research(query, **kw)` | `Research` | One question, one call, throwaway client |
| `Client(**kw)` | `Client` | Many questions — the connection pool, robots cache and per-host pacing live here |
| `extract(html, url=None, **kw)` | `Article` | You already have the HTML |
| `extract_many(documents, urls=None, **kw)` | `list[Article]` | A batch — parallel across every core, GIL released |
| `slim(query, articles, **kw)` | `Context` | You assembled the articles yourself |
| `chunk_many(articles, **kw)` | `list[dict]` | Feeding a vector store |

```python
research(
    query: str, *,
    max_sources: int = 5,              # pages actually fetched and read
    max_tokens: int = 2048,            # context budget
    max_tokens_per_source: int | None = None,
    providers: Sequence[str] | None = None,
    impersonate: str = "chrome",
    respect_robots: bool = True,
    contact_email: str | None = None,  # OpenAlex/Crossref polite pool
) -> Research
```

```python
Client(*,
    # search
    providers: Sequence[str] | None = None,
    max_results: int | None = None,    # hits search returns (was `limit`)
    contact_email: str | None = None,
    # fetching
    concurrency: int = 16,
    timeout: float = 20.0,
    impersonate: str = "chrome",       # "chrome", "firefox", "safari", "none"
    respect_robots: bool = True,
    respect_crawl_delay: bool = True,  # arXiv asks 15s; this is why a fetch waits
    per_host_delay: float = 0.25,
    retries: int = 2,
    max_retry_after: float = 60.0,
    max_body_bytes: int = 8 * 1024 * 1024,
    accept_language: str = "en-US,en;q=0.9",
    browser_fallback: bool = False,    # headless Chrome, only after a static fetch fails
    proxies: Sequence[str] | None = None,
    cookie_file: str | None = None,
    # extraction
    include_links: bool = True,
    include_images: bool = False,
    include_tables: bool = True,
    index_mode: IndexMode = "auto",    # "auto" | "never" | "always"
    # compression
    max_tokens: int = 2048,
    max_tokens_per_source: int | None = None,
    diversity: float = 0.35,
)
```

`max_results` is **search breadth** — how many fused hits come back, cheap to
raise because nothing is fetched yet. `max_sources` is **read depth** — how many
of those are actually fetched and extracted, which is what costs time. So
`Client(max_results=20).research(q, max_sources=5)` casts a wide net and reads
the best five of it.

### Client methods

| Method | Returns | Note |
|---|---|---|
| `.search(query, *, strict=False)` | `list[SearchResult]` | Fused, deduplicated. A failing provider degrades the set; `strict=True` raises only if *every* one failed |
| `.fetch(urls, *, raise_on_error=False)` | `list[Page]` | Concurrent. **Not 1:1 with input** — match on `Page.url` |
| `.read(urls, *, raise_on_error=False)` | `list[Article]` | Fetch and extract. Also not 1:1 |
| `.research(query, *, max_sources=5)` | `Research` | All four stages |
| `.save_cookies()` | `None` | Flush the jar to `cookie_file` |

### Objects

| Type | Fields |
|---|---|
| `Research` | `query`, `markdown`, `context`, `results`, `articles`, `failures` |
| `Context` | `query`, `markdown`, `tokens`, `selected`, `sources`, `units_considered` |
| `Article` | `url`, `kind`, `title`, `markdown`, `text`, `units`, `links`, `meta`, `stats`, `tokens`, `.chunks(...)` |
| `Unit` | `kind`, `level`, `text`, `markdown`, `heading_path`, `position`, `tokens` |
| `Page` | `url`, `final_url`, `status`, `body`, `content_type`, `elapsed_ms`, `rendered`, `.extract()` |
| `SearchResult` | `url`, `title`, `snippet`, `score`, `rank`, `providers` |
| `Link` | `url`, `text`, `snippet`, `heading_path` |
| `Meta` | `title`, `description`, `byline`, `published`, `site_name`, `canonical`, `language` |
| `DenoiseStats` | `html_bytes`, `markdown_bytes`, `compression`, `nodes_visited`, `nodes_dropped` |

`Article.kind` is `"article"` or `"index"`; `Unit.kind` is `"heading"`,
`"paragraph"`, `"list_item"`, `"code"`, `"quote"` or `"table"`. Every object has
`.to_dict()`.

`Context.selected` is one dict per chosen unit — `source` and `unit` are
**indices**, into `Research.articles` and that article's `.units`:

```python
for sel in ctx.selected:
    unit = articles[sel["source"]].units[sel["unit"]]
    print(sel["relevance"], sel["density"], sel["tokens"], unit.text)
```

### Utilities

| Call | Returns |
|---|---|
| `count_tokens(text)` | `int` — the estimate the budget is spent against |
| `tokenize(text)` | `list[str]` — the tokens the ranker uses |
| `density(text)` | `float` in `[0, 1]` — information density |
| `canonical_url(url)` | `str \| None` — the identity used for deduplication |
| `parse_feed(xml)` | `list[dict]` — RSS or Atom |
| `parse_sitemap(xml)` | `dict[str, list[str]]` — `urls` and nested `sitemaps` |

### Errors

All inherit `RustaiError`: `NetworkError`, `HttpStatusError`, `RobotsError`,
`ExtractError`, `ProviderError`, `BrowserError`. Batch calls swallow per-item
failures by default and report them in `Research.failures`; pass
`raise_on_error=True` when a missing page is a problem rather than a nuisance.

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

Five, because they measure different things and only one of them is about speed.
Each is a script in `benches/`, so every number here is one you can re-run.

| Benchmark | Question | Command |
|---|---|---|
| `benchmark.py` | How fast is extraction? | `python benches/benchmark.py` |
| `quality.py` | Did extraction keep the article? | `python benches/quality.py /tmp/rustai-pages` |
| `retrieval.py` | Did the answer reach the window? | `python benches/retrieval.py` |
| `tokens.py` | What did the question cost? | `python benches/tokens.py` |
| `hybrid.py` | Do embeddings earn their keep? | `python benches/hybrid.py` |

### Speed

Two corpora, and the difference between them matters more than either number.

**Synthetic** — 200 generated pages, 46 KB each, deterministic and
redistributable. `python benches/corpus.py /tmp/rustai-corpus` builds it.

| Engine | Throughput | vs `trafilatura` |
|---|---|---|
| `rustai.extract_many` | **5,898 docs/s** | **39×** |
| `rustai.extract` (loop) | 1,681 docs/s | 11× |
| `trafilatura` | 150 docs/s | 1× |

**Real pages** — 15 live pages including Wikipedia articles over 1 MB. Fetch
them with `benches/fetch_corpus.py`.

| Engine | Serial | Across cores |
|---|---|---|
| `resiliparse` | **103 docs/s** | **116 docs/s** |
| `rustai` | 64 docs/s | 77 docs/s |
| `justext` | 14 | 12 |
| `trafilatura` | 5 | 4 |

Real pages are 25× slower per document than synthetic ones, because they are
seven times larger and far messier. **Quote the second table.** The first is
useful for spotting a regression against a fixed input, not for deciding
whether this is fast enough for you.

`trafilatura` gains nothing from threads — it holds the GIL. `extract_many`
releases it and spreads across the rayon pool, which is where its multiple comes
from. Apple M1, 8 cores, CPython 3.14; `notebooks/colab_speed_benchmark.ipynb`
runs the same comparison wherever you are, and the ranking does move with the
hardware.

### Extraction quality

```bash
python benches/fetch_corpus.py /tmp/rustai-pages
python benches/quality.py      /tmp/rustai-pages --vs-trafilatura
```

Twenty real pages, scored two ways at once: five-word shingle overlap with each
page's own article container, and a count of site furniture that reached the
output.

| | recall | precision | F1 | furniture |
|---|---|---|---|---|
| `rustai` | 65.3% | 85.5% | 72.6% | 3 |
| `trafilatura` | 68.3% | 88.2% | **75.1%** | 4 |

Both numbers are needed. Judged on overlap alone the best setting is to switch
every threshold off — the container being compared against never held the
navigation, so passing the whole page through scores *better* while quadrupling
the furniture. That is why `min_block_len` stays where it is.

### What a question costs

```bash
python benches/tokens.py
```

Five stages, each where somebody's pipeline actually stops, over seven
questions:

```
raw HTML     233,184 tokens   answer present 100%
parsed text   65,071  (27.9%)                100%
extracted     18,968   (8.1%)                100%
rustai all    26,058  (11.2%)                100%
rustai slim    1,884   (0.8%)                100%
```

A dedicated extractor gets you to 8%. Ranking and a token budget get you to
0.8%, a tenth of that, with the answer still present in every question — which
is checked, because a benchmark counting only tokens would reward returning
nothing. On a 3B model with a 32k window the raw pages behind "딥러닝이 뭐야?"
do not fit at all, at 593,439 tokens; the context that answers it is 1,627.

### Did the answer arrive?

```bash
python benches/retrieval.py
```

Nine questions spanning explanation, an event on a date, and a current-value
lookup. Each carries the patterns any correct source would contain, so the
measurement holds no model and no prompt and does not move when you change
either: **answer present 96%, 90% of the budget spent on sources that mention
the subject, median 3.2s.**

Adding a caller's embedding model takes the first number to 100% at
`semantic_weight` between 0.25 and 0.5 — `python benches/hybrid.py` sweeps it.

### Caveats worth stating plainly

Single-machine numbers on corpora this repository chose. The extraction ranking
against `resiliparse` has flipped between runs on the same Colab instance. The
retrieval and token benchmarks hit the live web, so they move with what search
returns that day. Run them on your own pages before trusting any of it.

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
