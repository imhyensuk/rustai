"""Live-internet tests. Deselected unless you pass `--network`.

These hit third-party services, so they are inherently flaky and are not part of
the default suite or CI. They exist to answer the one question offline tests
cannot: does the impersonation actually get us a 200, and does the whole
pipeline hold together against real markup.
"""

import pytest

import rustai

pytestmark = pytest.mark.network


@pytest.fixture(scope="module")
def client():
    return rustai.Client(
        providers=["duckduckgo", "wikipedia:en"],
        concurrency=8,
        timeout=30.0,
        max_tokens=1500,
    )


class TestFetch:
    def test_fetches_a_real_page(self, client):
        pages = client.fetch(["https://example.com/"], raise_on_error=True)
        assert len(pages) == 1
        page = pages[0]
        assert page.status == 200
        assert "Example Domain" in page.body
        assert page.elapsed_ms >= 0
        assert not page.rendered

    def test_reads_and_extracts_a_real_article(self, client):
        articles = client.read(["https://en.wikipedia.org/wiki/BM25"], raise_on_error=True)
        assert articles, "wikipedia returned nothing"
        art = articles[0]
        assert art.title
        assert art.tokens > 100
        assert "Okapi" in art.text or "BM25" in art.text
        # Wikipedia chrome must not survive extraction.
        lowered = art.text.lower()
        assert "jump to content" not in lowered
        assert "privacy policy" not in lowered

    def test_batch_survives_a_dead_url(self, client):
        pages = client.fetch(
            ["https://example.com/", "https://no-such-host.invalid/"],
        )
        assert len(pages) == 1

    def test_raise_on_error_surfaces_the_failure(self, client):
        with pytest.raises(rustai.RustaiError):
            client.fetch(["https://no-such-host.invalid/"], raise_on_error=True)

    def test_robots_is_enforced(self):
        strict = rustai.Client(respect_robots=True)
        # Google disallows /search for every crawler.
        with pytest.raises(rustai.RobotsError):
            strict.fetch(["https://www.google.com/search?q=test"], raise_on_error=True)


class TestSearch:
    def test_wikipedia_returns_results(self):
        client = rustai.Client(providers=["wikipedia:en"])
        hits = client.search("Okapi BM25", strict=True)
        assert hits
        assert all(h.url.startswith("https://en.wikipedia.org/wiki/") for h in hits)
        assert all("wikipedia" in h.providers for h in hits)

    def test_korean_wikipedia(self):
        client = rustai.Client(providers=["wikipedia:ko"])
        hits = client.search("러스트 프로그래밍 언어", strict=True)
        assert hits
        assert hits[0].url.startswith("https://ko.wikipedia.org/wiki/")

    def test_duckduckgo_returns_results(self):
        """Impersonation is what makes this pass; without it, a challenge page."""
        client = rustai.Client(providers=["duckduckgo"])
        hits = client.search("rust tokio async runtime", strict=True)
        assert len(hits) >= 3
        assert all(h.url.startswith("http") for h in hits)

    def test_fusion_across_providers(self, client):
        hits = client.search("BM25 ranking function")
        assert hits
        assert {p for h in hits for p in h.providers} >= {"wikipedia"}
        assert hits == sorted(hits, key=lambda h: -h.score)


class TestAcademicProviders:
    """The scholarly sources: free, keyless, and the reason a web search is not enough."""

    @pytest.mark.parametrize("provider", ["arxiv", "openalex", "crossref"])
    def test_returns_scholarly_results(self, provider):
        c = rustai.Client(
            providers=[provider], contact_email="you@example.com", timeout=30.0
        )
        hits = c.search("attention is all you need transformer", strict=True)
        assert len(hits) >= 3
        assert all(h.url.startswith("http") for h in hits)
        assert all(h.title for h in hits)
        assert all(provider in h.providers for h in hits)

    def test_arxiv_links_to_abstracts_not_pdfs(self):
        c = rustai.Client(providers=["arxiv"], timeout=30.0)
        hits = c.search("transformer architecture", strict=True)
        # A PDF cannot be denoised by this pipeline, so the abstract page is
        # the only useful target.
        assert all("/abs/" in h.url for h in hits), [h.url for h in hits]
        assert not any("/pdf/" in h.url for h in hits)

    def test_openalex_reconstructs_abstracts(self):
        c = rustai.Client(
            providers=["openalex"], contact_email="you@example.com", timeout=30.0
        )
        hits = c.search("BERT language model pre-training", strict=True)
        assert any(len(h.snippet) > 200 for h in hits), "no abstract was reconstructed"

    def test_academic_and_web_results_fuse(self):
        c = rustai.Client(
            providers=["arxiv", "openalex", "wikipedia:en"],
            contact_email="you@example.com",
            timeout=30.0,
        )
        hits = c.search("Okapi BM25 ranking")
        assert hits
        assert len({p for h in hits for p in h.providers}) >= 2


class TestHtmlRedirects:
    def test_follows_a_meta_refresh_stub(self):
        """This URL serves a 200 whose body is a JS/meta-refresh redirect."""
        c = rustai.Client(timeout=30.0)
        page = c.fetch(
            ["https://blog.rust-lang.org/2024/02/08/Rust-1.76.0.html"],
            raise_on_error=True,
        )[0]
        assert page.final_url.endswith("/Rust-1.76.0/"), page.final_url
        article = page.extract()
        assert article.tokens > 300, "the redirect stub was returned instead of the post"
        assert "1.76" in (article.title or "")


class TestIndexPages:
    """Front pages: the link list is the content, and a collectable source."""

    @pytest.mark.parametrize(
        "url",
        [
            "https://news.ycombinator.com/",
            "https://blog.rust-lang.org/",
        ],
    )
    def test_listing_pages_yield_an_inventory(self, client, url):
        page = client.fetch([url], raise_on_error=True)[0]
        article = page.extract()
        assert article.kind == "index", f"{url} was read as an article"
        assert len(article.links) >= 10
        assert all(l.url.startswith("http") for l in article.links)
        assert all(l.text for l in article.links)

    @pytest.mark.parametrize(
        "url",
        [
            "https://en.wikipedia.org/wiki/BM25",
            "https://arxiv.org/abs/1706.03762",
            "https://doc.rust-lang.org/book/ch01-01-installation.html",
        ],
    )
    def test_article_pages_are_not_reclassified(self, client, url):
        article = client.fetch([url], raise_on_error=True)[0].extract()
        assert article.kind == "article", f"{url} was read as a listing"

    def test_a_listing_page_works_as_a_provider(self):
        """A site with no feed and no sitemap is still collectable."""
        c = rustai.Client(providers=["index:https://news.ycombinator.com/"], timeout=30.0)
        hits = c.search("programming language", strict=True)
        assert hits
        assert all(h.url.startswith("http") for h in hits)
        assert all("index" in h.providers for h in hits)


class TestResearch:
    def test_end_to_end(self, client):
        result = client.research("what is the BM25 ranking function", max_sources=3)
        assert result.context.markdown, result.failures
        assert result.context.tokens <= 1500
        assert result.articles
        assert result.context.sources
        for src in result.context.sources:
            assert src["url"].startswith("http")

    def test_module_level_shortcut(self):
        result = rustai.research(
            "Okapi BM25 term frequency saturation",
            max_sources=2,
            max_tokens=800,
            providers=["wikipedia:en"],
        )
        assert result.markdown
        assert result.context.tokens <= 800
