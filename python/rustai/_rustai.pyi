"""Type stubs for the compiled `rustai._rustai` extension."""

from typing import Any, Literal, Sequence

__version__: str

UnitKind = Literal["heading", "paragraph", "list_item", "code", "quote", "table"]
IndexMode = Literal["auto", "never", "always"]

class RustaiError(Exception):
    """Base class for every rustai error."""

class NetworkError(RustaiError):
    """The request failed at the transport level."""

class HttpStatusError(RustaiError):
    """The server answered with a failure status."""

class RobotsError(RustaiError):
    """robots.txt disallows this URL."""

class ExtractError(RustaiError):
    """The document could not be parsed."""

class ProviderError(RustaiError):
    """A search provider failed."""

class BrowserError(RustaiError):
    """The headless fallback was unavailable."""

class Meta:
    """Document metadata read from `<head>`."""
    @property
    def title(self) -> str | None:
        """Best available title."""
        ...
    @property
    def description(self) -> str | None:
        """Meta or Open Graph description."""
        ...
    @property
    def byline(self) -> str | None:
        """Declared author."""
        ...
    @property
    def published(self) -> str | None:
        """Publication timestamp, verbatim from the page."""
        ...
    @property
    def language(self) -> str | None:
        """BCP-47 language tag."""
        ...
    @property
    def canonical(self) -> str | None:
        """Canonical URL."""
        ...
    @property
    def site_name(self) -> str | None:
        """Site name."""
        ...
    def to_dict(self) -> dict[str, Any]:
        """Everything above, as a dict."""
        ...

class Unit:
    """One block of an extracted document."""
    @property
    def kind(self) -> UnitKind:
        """`"heading"`, `"paragraph"`, `"list_item"`, `"code"`, `"quote"` or `"table"`."""
        ...
    @property
    def level(self) -> int:
        """Heading level, or list nesting depth."""
        ...
    @property
    def text(self) -> str:
        """Plain text."""
        ...
    @property
    def markdown(self) -> str:
        """Rendered Markdown."""
        ...
    @property
    def heading_path(self) -> list[str]:
        """Enclosing headings, outermost first."""
        ...
    @property
    def position(self) -> int:
        """Index in document order."""
        ...
    @property
    def tokens(self) -> int:
        """Estimated tokens."""
        ...
    def to_dict(self) -> dict[str, Any]:
        """Everything above, as a dict."""
        ...

class DenoiseStats:
    """What the denoiser removed."""
    @property
    def nodes_visited(self) -> int:
        """Nodes considered."""
        ...
    @property
    def nodes_dropped(self) -> int:
        """Nodes rejected as boilerplate."""
        ...
    @property
    def html_bytes(self) -> int:
        """Bytes of HTML in."""
        ...
    @property
    def markdown_bytes(self) -> int:
        """Bytes of Markdown out."""
        ...
    @property
    def compression(self) -> float:
        """Size reduction in `[0, 1]`."""
        ...
    def to_dict(self) -> dict[str, Any]:
        """Everything above, as a dict."""
        ...

class Link:
    """One link harvested from a listing page."""
    @property
    def text(self) -> str:
        """Anchor text."""
        ...
    @property
    def url(self) -> str:
        """Absolute URL."""
        ...
    @property
    def snippet(self) -> str:
        """Nearby descriptive text, if the page offered any."""
        ...
    @property
    def heading_path(self) -> list[str]:
        """Enclosing section headings, outermost first."""
        ...
    def to_dict(self) -> dict[str, Any]:
        """Everything above, as a dict."""
        ...

class Article:
    """A cleaned document."""
    @property
    def url(self) -> str | None:
        """Source URL."""
        ...
    @property
    def kind(self) -> Literal["article", "index"]:
        """`"article"` for prose, `"index"` for a listing page."""
        ...
    @property
    def links(self) -> list[Link]:
        """Links harvested from a listing page."""
        ...
    @property
    def title(self) -> str | None:
        """Title, falling back to the first heading."""
        ...
    @property
    def markdown(self) -> str:
        """The whole cleaned document as Markdown."""
        ...
    @property
    def text(self) -> str:
        """The whole cleaned document as plain text."""
        ...
    @property
    def meta(self) -> Meta:
        """Metadata from `<head>`."""
        ...
    @property
    def units(self) -> list[Unit]:
        """Rankable blocks."""
        ...
    @property
    def stats(self) -> DenoiseStats:
        """Denoiser statistics."""
        ...
    @property
    def tokens(self) -> int:
        """Estimated tokens for the whole article."""
        ...
    def to_dict(self) -> dict[str, Any]:
        """The article as nested dicts."""
        ...
    def __len__(self) -> int:
        """Return len(self)."""
        ...

class Page:
    """A raw HTTP response."""
    @property
    def url(self) -> str:
        """URL as requested."""
        ...
    @property
    def final_url(self) -> str:
        """URL after redirects."""
        ...
    @property
    def status(self) -> int:
        """HTTP status code."""
        ...
    @property
    def content_type(self) -> str | None:
        """`Content-Type` header."""
        ...
    @property
    def body(self) -> str:
        """Decoded response body."""
        ...
    @property
    def elapsed_ms(self) -> int:
        """Wall time in milliseconds."""
        ...
    @property
    def rendered(self) -> bool:
        """Whether headless Chrome produced this body."""
        ...
    def extract(self) -> Article:
        """Extract this page without fetching it again."""
        ...
    def to_dict(self) -> dict[str, Any]:
        """Everything above, as a dict."""
        ...

class SearchResult:
    """One search hit."""
    @property
    def title(self) -> str:
        """Result title."""
        ...
    @property
    def url(self) -> str:
        """Absolute URL."""
        ...
    @property
    def snippet(self) -> str:
        """Provider snippet."""
        ...
    @property
    def providers(self) -> list[str]:
        """Providers that returned this URL."""
        ...
    @property
    def score(self) -> float:
        """Fused rank-fusion score."""
        ...
    @property
    def rank(self) -> int:
        """Best position this URL reached at any *single* provider, zero-based.

        Not this result's position in the fused list — for that, enumerate the
        list you were handed. A result ranked first by two providers and a
        result ranked first by one both report `0`; what separates them is
        `score`, which is what the list is sorted by.
        """
        ...
    def to_dict(self) -> dict[str, Any]:
        """Everything above, as a dict."""
        ...

class Context:
    """A compressed context window."""
    @property
    def query(self) -> str:
        """The query it was built for."""
        ...
    @property
    def markdown(self) -> str:
        """Ready-to-prompt Markdown."""
        ...
    @property
    def tokens(self) -> int:
        """Estimated tokens."""
        ...
    @property
    def sources(self) -> list[dict[str, Any]]:
        """Sources that contributed, as dicts."""
        ...
    @property
    def selected(self) -> list[dict[str, Any]]:
        """Per-unit selection detail, as dicts."""
        ...
    @property
    def units_considered(self) -> int:
        """Units available before selection."""
        ...
    def to_dict(self) -> dict[str, Any]:
        """Everything above, as a dict."""
        ...
    def __str__(self) -> str:
        """Return str(self)."""
        ...

class Research:
    """The result of a full research run."""
    @property
    def query(self) -> str:
        """The query as asked."""
        ...
    @property
    def context(self) -> Context:
        """The compressed context."""
        ...
    @property
    def markdown(self) -> str:
        """Ready-to-prompt Markdown, straight from the context."""
        ...
    @property
    def results(self) -> list[SearchResult]:
        """Search hits, best first."""
        ...
    @property
    def articles(self) -> list[Article]:
        """Articles that were successfully read."""
        ...
    @property
    def failures(self) -> list[tuple[str, str]]:
        """`(stage, message)` pairs for non-fatal failures."""
        ...
    def to_dict(self) -> dict[str, Any]:
        """Everything above, as a dict."""
        ...
    def __str__(self) -> str:
        """Return str(self)."""
        ...

class Client:
    """A reusable pipeline: connection pool, providers, extractor and slimmer."""
    def __init__(
        self,
        *,
        providers: Sequence[str] | None = None,
        concurrency: int = 16,
        timeout: float = 20.0,
        impersonate: str = "chrome",
        respect_robots: bool = True,
        per_host_delay: float = 0.25,
        retries: int = 2,
        max_body_bytes: int = 8388608,
        accept_language: str = "en-US,en;q=0.9",
        browser_fallback: bool = False,
        contact_email: str | None = None,
        proxies: Sequence[str] | None = None,
        max_retry_after: float = 60.0,
        cookie_file: str | None = None,
        max_tokens: int = 2048,
        diversity: float = 0.35,
        include_links: bool = True,
        include_images: bool = False,
        include_tables: bool = True,
        index_mode: IndexMode = "auto",
        limit: int = 10,
    ) -> None:
        """Initialize self.  See help(type(self)) for accurate signature."""
        ...
    def search(self, query: str, *, strict: bool = False) -> list[SearchResult]:
        """Search every configured provider and return fused results.

        Provider failures are not raised: a partial result set is more useful
        than an exception when one of five providers is having a bad day. Pass
        `strict=True` to raise when *every* provider failed.
        """
        ...
    def fetch(
        self, urls: Sequence[str], *, raise_on_error: bool = False
    ) -> list[Page]:
        """Fetch URLs concurrently.

        **The result is not 1:1 with the input.** With `raise_on_error=False`
        (the default) a failed URL is simply absent, so one dead link cannot
        lose you the other nineteen — but neither can you tell which one went,
        and indexing the result against the input you passed will be wrong.
        Match on `Page.url` instead, or pass `raise_on_error=True` when a
        missing page is a problem rather than a nuisance.

        This differs deliberately from `extract_many`, which is 1:1 and raises:
        there, an input is a document you already hold, and losing one silently
        would be a bug in your own pipeline. Here an input is somebody else's
        server, and it is allowed to be down.
        """
        ...
    def read(
        self, urls: Sequence[str], *, raise_on_error: bool = False
    ) -> list[Article]:
        """Fetch and extract in one step.

        **The result is not 1:1 with the input** — see `fetch`. Match on
        `Article.url` rather than by position, or pass `raise_on_error=True`.
        """
        ...
    def research(self, query: str, *, max_sources: int = 5) -> Research:
        """Search, read the top results, and compress them into a context window."""
        ...
    def save_cookies(self) -> int:
        """Write the cookie jar to the `cookie_file` this client was built with.

        Returns how many cookies were saved, or 0 when no file was configured.
        Clearance cookies are the expensive part of getting through a bot wall;
        saving them means not paying for them again next run.
        """
        ...

def extract(
    html: str,
    url: str | None = None,
    *,
    include_links: bool = True,
    include_images: bool = False,
    include_tables: bool = True,
    index_mode: IndexMode = "auto",
) -> Article:
    """Denoise a raw HTML string into Markdown. No network access."""
    ...
def extract_many(
    documents: Sequence[str],
    urls: Sequence[str | None] | None = None,
    *,
    include_links: bool = True,
    include_images: bool = False,
    include_tables: bool = True,
    index_mode: IndexMode = "auto",
) -> list[Article]:
    """Denoise many HTML strings at once, in parallel across the rayon pool.

    This is the batch form of [`extract`]. Parsing is CPU-bound and per-document
    independent, so a list of 20 documents costs roughly one document's wall
    time on a multi-core machine — and because the GIL is released for the whole
    call, it parallelises whether or not the caller uses threads.

    `urls` is optional; when given it must be the same length as `documents`.
    Output is 1:1 with the input, so a failed document raises rather than
    silently shifting every later index.
    """
    ...
def slim(
    query: str,
    articles: Sequence[Article],
    *,
    max_tokens: int = 2048,
    diversity: float = 0.35,
    max_tokens_per_source: int | None = None,
    include_breadcrumbs: bool = True,
) -> Context:
    """Rank and compress already-extracted articles into a context window."""
    ...
def research(
    query: str,
    *,
    max_sources: int = 5,
    max_tokens: int = 2048,
    providers: Sequence[str] | None = None,
    impersonate: str = "chrome",
    respect_robots: bool = True,
    contact_email: str | None = None,
) -> Research:
    """Search, read and compress in one call, using a throwaway client."""
    ...
def count_tokens(text: str) -> int:
    """Estimate how many LLM tokens a string costs."""
    ...
def tokenize(text: str) -> list[str]:
    """Split text into the tokens the ranker uses."""
    ...
def density(text: str) -> float:
    """Score a block of text for information density, `0.0`–`1.0`."""
    ...
def canonical_url(url: str) -> str | None:
    """Normalise a URL to the identity used for deduplication."""
    ...
def parse_feed(xml: str) -> list[dict[str, Any]]:
    """Parse an RSS or Atom feed into a list of dicts."""
    ...
def parse_sitemap(xml: str) -> dict[str, list[str]]:
    """Parse a sitemap into `{"urls": [...], "sitemaps": [...]}`."""
    ...
