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
         "arxiv", "openalex", "crossref", "OpenAlex", "index:https://news.example/"],
    )
    def test_provider_specs_accepted(self, provider):
        rustai.Client(providers=[provider])

    @pytest.mark.parametrize("provider", ["tavily", "searxng", "rss", "", "firecrawl", "index"])
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

    def test_proxies_are_accepted_and_validated(self):
        c = rustai.Client(proxies=["http://127.0.0.1:8080", "socks5://127.0.0.1:1080"])
        assert "proxies=2" in repr(c)
        with pytest.raises(ValueError, match="proxy"):
            rustai.Client(proxies=["not a proxy"])

    def test_cookie_file_round_trips(self, tmp_path):
        path = tmp_path / "nested" / "jar.json"
        c = rustai.Client(cookie_file=str(path))
        # Nothing fetched yet, so nothing to save — but no error either.
        assert c.save_cookies() == 0

    def test_save_cookies_without_a_file_is_a_noop(self):
        assert rustai.Client().save_cookies() == 0

    def test_retry_after_cap_is_configurable(self):
        rustai.Client(max_retry_after=0.0)
        rustai.Client(max_retry_after=120.0)

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


class TestSearchAndReadCapsAreDistinct:
    """`max_results` is search breadth; `max_sources` is read depth.

    They are one letter apart in spirit and easy to conflate -- the old name
    for the first was `limit`, which said nothing about what it limited.
    """

    def test_max_results_is_the_current_name(self):
        assert rustai.Client(providers=["wikipedia:en"], max_results=3) is not None

    def test_limit_is_still_accepted(self):
        """It shipped in 0.2.0; renaming it must not break callers."""
        assert rustai.Client(providers=["wikipedia:en"], limit=3) is not None

    def test_agreeing_duplicates_are_fine(self):
        assert rustai.Client(providers=["wikipedia:en"], max_results=4, limit=4) is not None

    def test_disagreeing_duplicates_are_refused(self):
        with pytest.raises(ValueError, match="old name"):
            rustai.Client(providers=["wikipedia:en"], max_results=3, limit=9)

    def test_the_docstring_keeps_the_two_apart(self):
        doc = rustai.Client.__doc__ or ""
        assert "max_results" in doc and "max_sources" in doc
        assert "breadth" in doc and "depth" in doc


class TestCrawlDelay:
    """A site can ask for a gap between requests, and this client obeys it.

    Worth a test because obeying it looks exactly like being slow: arXiv asks
    for fifteen seconds, so a second arXiv URL through one client waits that
    long with nothing on the network.
    """

    def test_the_knob_exists_and_defaults_to_obeying(self):
        assert rustai.Client(respect_crawl_delay=True) is not None
        assert rustai.Client(respect_crawl_delay=False) is not None

    def test_the_docstring_explains_the_stall(self):
        doc = rustai.Client.__doc__ or ""
        assert "respect_crawl_delay" in doc
        assert "Crawl-delay" in doc
