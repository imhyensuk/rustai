"""rustai — a zero-cost local research pipeline.

Collect, denoise and compress the open web into something a small local model
can actually read, without an API key and without a heavyweight Python crawler
in the process.

The three things you will use most::

    import rustai

    # 1. One call: search, read, and compress to a context window.
    result = rustai.research("what is BM25", max_sources=5, max_tokens=2048)
    print(result.markdown)

    # 2. A reusable client, so the connection pool is shared.
    client = rustai.Client(providers=["duckduckgo", "wikipedia:en"], max_tokens=4096)
    articles = client.read(["https://example.com/post"])

    # 3. Offline: turn HTML you already have into clean Markdown.
    article = rustai.extract(html, url="https://example.com/post")
    articles = rustai.extract_many(list_of_html)   # parallel across every core

Listing pages -- front pages, archives, feeds rendered for people -- are
collected as link inventories rather than prose, and work as a source::

    front = rustai.extract(html, url)      # .kind == "index"
    for link in front.links:
        print(link.text, link.url)

    client = rustai.Client(providers=["index:https://news.ycombinator.com/"])

Scholarly sources are first-class alongside web search::

    client = rustai.Client(
        providers=["arxiv", "openalex", "crossref", "europepmc", "duckduckgo"],
        contact_email="you@example.com",   # OpenAlex/Crossref polite pool
    )

Some questions are answered by a conversation rather than a paper::

    client = rustai.Client(providers=["hackernews", "stackexchange", "github"])

When a question wants corroboration rather than depth, stop any one page from
taking the whole window::

    r = rustai.research("what is BM25 term saturation", max_tokens_per_source=500)

Everything blocking releases the GIL, so these calls parallelise across threads.
"""

from ._rustai import (
    Article,
    BrowserError,
    Client,
    Context,
    DenoiseStats,
    ExtractError,
    HttpStatusError,
    Link,
    Meta,
    NetworkError,
    Page,
    ProviderError,
    Research,
    RobotsError,
    RustaiError,
    SearchResult,
    Unit,
    __version__,
    canonical_url,
    count_tokens,
    density,
    extract,
    extract_many,
    parse_feed,
    parse_sitemap,
    research,
    slim,
    tokenize,
)

__all__ = [
    "Article",
    "BrowserError",
    "Client",
    "Context",
    "DenoiseStats",
    "ExtractError",
    "HttpStatusError",
    "Link",
    "Meta",
    "NetworkError",
    "Page",
    "ProviderError",
    "Research",
    "RobotsError",
    "RustaiError",
    "SearchResult",
    "Unit",
    "__version__",
    "canonical_url",
    "count_tokens",
    "density",
    "extract",
    "extract_many",
    "parse_feed",
    "parse_sitemap",
    "research",
    "slim",
    "tokenize",
]
