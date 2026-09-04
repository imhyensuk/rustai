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
