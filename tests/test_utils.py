"""Offline tests for the standalone helpers and the client's validation."""

import pytest

import rustai


class TestTokens:
    def test_estimate_grows_with_length(self):
        assert rustai.count_tokens("") == 0
        assert rustai.count_tokens("hello world") > 0
        assert rustai.count_tokens("a " * 100) > rustai.count_tokens("a " * 10)

    def test_cjk_costs_more_per_character(self):
        assert rustai.count_tokens("데이터 수집") > rustai.count_tokens("data")

    def test_tokenize_lowercases_and_splits(self):
        assert rustai.tokenize("Hello,  World! 42") == ["hello", "world", "42"]

    def test_tokenize_emits_cjk_bigrams(self):
        toks = rustai.tokenize("한국어")
        assert "한국" in toks and "국어" in toks


class TestDensity:
    def test_facts_outscore_filler(self):
        facts = rustai.density(
            "The 0.1.0 release cut resident memory from 412 MB to 28 MB across 1,200 documents."
        )
        filler = rustai.density(
            "In this article we are going to take a look at some of the things you may want to know."
        )
        assert facts > filler

    def test_boilerplate_is_penalised(self):
        clean = rustai.density("The parser walks the arena once and caches three metrics per node.")
        legal = rustai.density(
            "The parser walks the arena once and caches three metrics per node. "
            "All rights reserved. Privacy policy."
        )
        assert legal < clean

    @pytest.mark.parametrize("text", ["", "   "])
    def test_empty_is_zero(self, text):
        assert rustai.density(text) == 0.0


class TestCanonicalUrl:
    def test_strips_decorations(self):
        assert rustai.canonical_url(
            "https://www.Example.com/post/?utm_source=x&fbclid=y#top"
        ) == rustai.canonical_url("http://example.com/post")

    def test_keeps_meaningful_query_and_sorts_it(self):
        assert rustai.canonical_url("https://x.dev/s?b=2&a=1") == rustai.canonical_url(
            "https://x.dev/s?a=1&b=2"
        )
        assert rustai.canonical_url("https://x.dev/s?a=1") != rustai.canonical_url(
            "https://x.dev/s?a=9"
        )

    @pytest.mark.parametrize("bad", ["not a url", "javascript:alert(1)", "file:///etc/passwd"])
    def test_rejects_non_http(self, bad):
        assert rustai.canonical_url(bad) is None


class TestFeedParsing:
    def test_rss(self):
        items = rustai.parse_feed(
            "<rss><channel><item><title>T &amp; U</title>"
            "<link>https://a.dev/1</link><description>Body.</description>"
            "<pubDate>Tue, 01 Sep 2026 00:00:00 GMT</pubDate></item></channel></rss>"
        )
        assert items == [
            {
                "title": "T & U",
                "link": "https://a.dev/1",
                "summary": "Body.",
                "published": "Tue, 01 Sep 2026 00:00:00 GMT",
            }
        ]

    def test_atom(self):
        items = rustai.parse_feed(
            '<feed><entry><title>A</title><link href="https://a.dev/2"/>'
            "<summary>S</summary></entry></feed>"
        )
        assert items[0]["link"] == "https://a.dev/2"

    def test_sitemap_and_index(self):
        assert rustai.parse_sitemap(
            "<urlset><url><loc>https://a.dev/1</loc></url></urlset>"
        ) == {"urls": ["https://a.dev/1"], "sitemaps": []}
        assert rustai.parse_sitemap(
            "<sitemapindex><sitemap><loc>https://a.dev/s.xml</loc></sitemap></sitemapindex>"
        ) == {"urls": [], "sitemaps": ["https://a.dev/s.xml"]}


class TestClientConstruction:
    """No network: only that configuration is validated up front."""

    def test_defaults_build(self):
        assert "duckduckgo" in repr(rustai.Client())

    @pytest.mark.parametrize(
        "provider",
        ["duckduckgo", "ddg", "wikipedia", "wikipedia:ko", "searxng:https://searx.be",
         "rss:https://a.dev/f.xml", "sitemap:https://a.dev/s.xml",
         "arxiv", "openalex", "crossref", "OpenAlex"],
    )
    def test_provider_specs_accepted(self, provider):
        rustai.Client(providers=[provider])

    @pytest.mark.parametrize("provider", ["tavily", "searxng", "rss", "", "firecrawl"])
    def test_bad_provider_specs_rejected(self, provider):
        with pytest.raises(ValueError):
            rustai.Client(providers=[provider])

    def test_empty_provider_list_rejected(self):
        with pytest.raises(ValueError):
            rustai.Client(providers=[])

    @pytest.mark.parametrize(
        "impersonate", ["chrome", "firefox", "safari", "random", "none", "chrome_143"]
    )
    def test_impersonation_presets(self, impersonate):
        rustai.Client(impersonate=impersonate)

    def test_academic_providers_and_contact_email(self):
        c = rustai.Client(
            providers=["arxiv", "openalex", "crossref"],
            contact_email="you@example.com",
        )
        assert "arxiv" in repr(c)

    def test_unknown_impersonation_profile_rejected(self):
        with pytest.raises(ValueError, match="unknown impersonation profile"):
            rustai.Client(impersonate="netscape_2")

    @pytest.mark.parametrize("kwargs", [
        {"timeout": 0},
        {"timeout": -1.0},
        {"concurrency": 0},
        {"diversity": 1.5},
        {"diversity": -0.1},
    ])
    def test_invalid_settings_rejected(self, kwargs):
        with pytest.raises(ValueError):
            rustai.Client(**kwargs)


class TestExceptions:
    def test_hierarchy(self):
        for name in [
            "NetworkError", "HttpStatusError", "RobotsError",
            "ExtractError", "ProviderError", "BrowserError",
        ]:
            assert issubclass(getattr(rustai, name), rustai.RustaiError)
        assert issubclass(rustai.RustaiError, Exception)

    def test_public_api_is_exported(self):
        assert set(rustai.__all__) <= set(dir(rustai))
        assert rustai.__version__
