"""Offline tests for extraction and denoising."""

import pytest

import rustai

ARTICLE = """<!doctype html><html lang="en"><head>
<title>Zero-cost crawling — Example Blog</title>
<meta property="og:site_name" content="Example Blog">
<meta name="description" content="Why Rust for crawling.">
<meta property="article:published_time" content="2026-01-02T03:04:05Z">
<script type="application/ld+json">{"@type":"Article","author":{"name":"A. Author"}}</script>
</head><body>
<nav class="main-nav"><a href="/a">A</a><a href="/b">B</a><a href="/c">C</a><a href="/d">D</a></nav>
<div class="ad-slot"><div><div><span></span></div></div></div>
<main><article class="post-content">
 <h1>Zero-cost crawling</h1>
 <p>Python crawlers pay for a DOM they never asked for, and it shows in resident memory, in latency, and in the bill.</p>
 <h2>Numbers</h2>
 <p>The release cut resident memory from 412 MB to 28 MB and parsed 1,200 documents in 3.4 seconds on 8 cores.</p>
 <ul><li>Low memory usage across the board</li><li>Fast parsing on every core</li></ul>
 <pre><code class="language-rust">let x = 1;</code></pre>
 <table><tr><th>Tool</th><th>RSS</th></tr><tr><td>rustai</td><td>28 MB</td></tr></table>
 <p>Read the <a href="/docs/guide">full guide</a> for a walkthrough of every stage in the pipeline.</p>
</article></main>
<aside class="related"><a href="/1">One</a><a href="/2">Two</a></aside>
<footer>© 2026 Example. All rights reserved. Privacy policy. Subscribe to our newsletter.</footer>
</body></html>"""


@pytest.fixture(scope="module")
def article():
    return rustai.extract(ARTICLE, "https://example.com/post")


class TestMetadata:
    def test_title_drops_the_site_suffix(self, article):
        assert article.title == "Zero-cost crawling"
        assert article.meta.site_name == "Example Blog"

    def test_reads_head_and_json_ld(self, article):
        assert article.meta.language == "en"
        assert article.meta.description == "Why Rust for crawling."
        assert article.meta.published == "2026-01-02T03:04:05Z"
        assert article.meta.byline == "A. Author"

    def test_meta_to_dict(self, article):
        d = article.meta.to_dict()
        assert d["title"] == "Zero-cost crawling"
        assert set(d) >= {"title", "description", "byline", "published", "language"}


class TestDenoising:
    @pytest.mark.parametrize(
        "noise",
        ["all rights reserved", "subscribe to our newsletter", "privacy policy"],
    )
    def test_footer_boilerplate_is_gone(self, article, noise):
        assert noise not in article.text.lower()

    def test_navigation_and_rails_are_gone(self, article):
        assert "](/a)" not in article.markdown
        assert "One" not in article.text

    def test_compression_is_reported(self, article):
        assert 0.0 < article.stats.compression < 1.0
        assert article.stats.html_bytes == len(ARTICLE.encode("utf-8"))
        assert article.stats.nodes_dropped > 0


class TestMarkdown:
    def test_structure_survives(self, article):
        md = article.markdown
        assert "# Zero-cost crawling" in md
        assert "## Numbers" in md
        assert "- Low memory usage across the board" in md
        assert "```rust\nlet x = 1;\n```" in md
        assert "| Tool | RSS |" in md

    def test_relative_links_are_absolutised(self, article):
        assert "(https://example.com/docs/guide)" in article.markdown

    def test_images_are_off_by_default(self):
        html = '<article><p>Text with an image below that is long enough to keep.</p><img src="/a.png" alt="A"></article>'
        assert "![" not in rustai.extract(html, "https://x.dev/").markdown
        with_img = rustai.extract(html, "https://x.dev/", include_images=True)
        assert "![A](https://x.dev/a.png)" in with_img.markdown

    def test_tables_can_be_dropped(self, article):
        plain = rustai.extract(ARTICLE, "https://example.com/post", include_tables=False)
        assert "| Tool | RSS |" not in plain.markdown

    def test_links_can_be_flattened(self):
        html = '<article><p>Read the <a href="/g">full guide</a> for a walkthrough of the pipeline.</p></article>'
        flat = rustai.extract(html, "https://x.dev/", include_links=False)
        assert "full guide" in flat.markdown
        assert "](" not in flat.markdown


class TestUnits:
    def test_units_carry_kinds_paths_and_costs(self, article):
        kinds = {u.kind for u in article.units}
        assert {"heading", "paragraph", "list_item", "code", "table"} <= kinds
        para = next(u for u in article.units if u.text.startswith("The release cut"))
        assert para.heading_path == ["Zero-cost crawling", "Numbers"]
        assert para.tokens > 0
        assert para.position > 0

    def test_len_matches_units(self, article):
        assert len(article) == len(article.units)

    def test_tokens_sum_to_the_article_total(self, article):
        assert article.tokens == sum(u.tokens for u in article.units)


class TestExtractMany:
    def test_matches_the_serial_path(self):
        docs = [ARTICLE, ARTICLE.replace("Zero-cost", "Low-cost"), "<article><p>" + "x " * 40 + "</p></article>"]
        batch = rustai.extract_many(docs)
        serial = [rustai.extract(d) for d in docs]
        assert [a.markdown for a in batch] == [a.markdown for a in serial]

    def test_output_is_one_to_one_with_input(self):
        docs = [ARTICLE, "", "   ", "<html></html>"]
        assert len(rustai.extract_many(docs)) == len(docs)

    def test_urls_are_applied_positionally(self):
        docs = [ARTICLE, ARTICLE]
        arts = rustai.extract_many(docs, ["https://a.dev/1", None])
        assert arts[0].url == "https://a.dev/1"
        assert arts[1].url is None
        assert "(https://a.dev/docs/guide)" in arts[0].markdown

    def test_length_mismatch_is_rejected(self):
        with pytest.raises(ValueError, match="urls has"):
            rustai.extract_many([ARTICLE, ARTICLE], ["https://a.dev/1"])

    def test_render_options_are_honoured(self):
        arts = rustai.extract_many([ARTICLE], include_tables=False)
        assert "| Tool | RSS |" not in arts[0].markdown

    def test_empty_input(self):
        assert rustai.extract_many([]) == []


LISTING = """<html><body>
<header><nav><a href="/">Home</a><a href="/about">About us here</a></nav></header>
<main>
  <h2>Technology</h2>
  <div class="card"><h3><a href="/story/rust">Rust cuts crawler memory by an order of magnitude</a></h3>
    <p>A new pipeline holds under five megabytes while parsing hundreds of documents.</p></div>
  <div class="card"><h3><a href="/story/http3">What actually changed in HTTP/3</a></h3>
    <p>The transport moved to UDP, and head-of-line blocking went with it.</p></div>
  <div class="card"><h3><a href="/story/bm25">BM25 is still the baseline to beat</a></h3>
    <p>Twenty years on, the ranking function remains hard to improve upon.</p></div>
  <h2>Opinion</h2>
  <div class="card"><h3><a href="/opinion/agents">Agents are not a product category</a></h3>
    <p>A short argument about naming things properly.</p></div>
  <div class="card"><h3><a href="/opinion/slm">The case for small local models</a></h3>
    <p>Latency, privacy and cost all point the same way.</p></div>
  <a href="/page/2">Next page</a>
</main>
<footer><a href="/privacy">Privacy policy</a></footer>
</body></html>"""


@pytest.fixture(scope="module")
def listing():
    return rustai.extract(LISTING, "https://wire.example/")


class TestIndexPages:
    def test_classified_as_an_index(self, listing):
        assert listing.kind == "index"
        assert len(listing.links) == 5, [l.url for l in listing.links]

    def test_links_carry_url_snippet_and_section(self, listing):
        bm25 = next(l for l in listing.links if l.url.endswith("/story/bm25"))
        assert bm25.text == "BM25 is still the baseline to beat"
        assert bm25.url == "https://wire.example/story/bm25"
        assert "ranking function" in bm25.snippet
        assert bm25.heading_path == ["Technology"]

    @pytest.mark.parametrize("junk", ["/about", "/page/2", "/privacy"])
    def test_controls_are_not_inventory(self, listing, junk):
        assert not any(junk in l.url for l in listing.links)

    def test_markdown_is_rankable(self, listing):
        assert "## Technology" in listing.markdown
        assert "- [BM25 is still the baseline to beat](https://wire.example/story/bm25)" in listing.markdown
        ctx = rustai.slim("ranking functions", [listing], max_tokens=120)
        assert "BM25" in ctx.markdown

    def test_articles_are_not_reclassified(self, article):
        assert article.kind == "article"
        assert article.links == []

    def test_mode_never_disables_it(self):
        art = rustai.extract(LISTING, "https://wire.example/", index_mode="never")
        assert art.kind == "article"
        assert art.links == []

    def test_mode_always_harvests_from_an_article(self):
        art = rustai.extract(ARTICLE, "https://example.com/post", index_mode="always")
        assert art.kind == "article", "forcing must not reclassify"
        assert "Zero-cost crawling" in art.markdown

    def test_invalid_mode_is_rejected(self):
        with pytest.raises(ValueError, match="index_mode"):
            rustai.extract(LISTING, index_mode="sometimes")

    def test_link_to_dict(self, listing):
        d = listing.links[0].to_dict()
        assert set(d) == {"text", "url", "snippet", "heading_path"}


class TestEdgeCases:
    @pytest.mark.parametrize("html", ["", "   ", "<html></html>", "not html at all"])
    def test_degenerate_input_does_not_raise(self, html):
        assert isinstance(rustai.extract(html).markdown, str)

    def test_url_is_optional(self):
        assert rustai.extract("<article><p>Some prose here to keep.</p></article>").url is None

    def test_korean_content_survives(self):
        html = (
            "<article><h1>러스트 크롤러</h1>"
            "<p>파이썬 크롤러는 필요 없는 DOM 비용을 지불하며, 이는 메모리 점유율과 지연 시간에 그대로 드러납니다.</p>"
            "</article>"
        )
        art = rustai.extract(html)
        assert art.title == "러스트 크롤러"
        assert "메모리 점유율" in art.text
        assert art.tokens > 0
