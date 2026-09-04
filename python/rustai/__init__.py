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
    "parse_feed",
    "parse_sitemap",
    "research",
    "slim",
    "tokenize",
]
