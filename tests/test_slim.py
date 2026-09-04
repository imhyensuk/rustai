"""Offline tests for ranking and context compression."""

import pytest

import rustai

TOKIO = """<article><h1>Tokio and rayon</h1>
<p>Tokio drives the asynchronous I/O: thousands of in-flight requests on a handful of OS threads, with no per-request stack to pay for.</p>
<p>Rayon then handles the CPU side, parsing every fetched document across a work-stealing pool of exactly as many threads as there are cores.</p>
<p>Measured on an M1, the pipeline sustained 520 documents per second at 4.8 MiB of resident memory.</p>
<p>Subscribe to our newsletter for more. All rights reserved. Privacy policy. Follow us on social media.</p>
</article>"""

PAPRIKA = """<article><h1>Cooking with paprika</h1>
<p>Paprika is best bloomed in fat before the liquid goes in, which keeps the flavour from turning dusty and flat in the pot.</p>
<p>Smoked varieties come from peppers dried over oak, and they will overwhelm a delicate stock if you are not careful with them.</p>
</article>"""


@pytest.fixture(scope="module")
def articles():
    return [
        rustai.extract(TOKIO, "https://a.dev/tokio"),
        rustai.extract(PAPRIKA, "https://b.dev/paprika"),
    ]


class TestSelection:
    def test_relevant_source_wins_the_budget(self, articles):
        ctx = rustai.slim("tokio rayon parallel parsing", articles, max_tokens=200)
        by_source = {s["url"]: s["tokens"] for s in ctx.sources}
        assert by_source.get("https://a.dev/tokio", 0) > by_source.get("https://b.dev/paprika", 0)

    @pytest.mark.parametrize("budget", [40, 90, 200, 500])
    def test_budget_is_never_exceeded(self, articles, budget):
        ctx = rustai.slim("memory throughput", articles, max_tokens=budget)
        assert sum(s["tokens"] for s in ctx.selected) <= budget

    def test_boilerplate_is_left_out(self, articles):
        ctx = rustai.slim("tokio rayon", articles, max_tokens=1000)
        assert "All rights reserved" not in ctx.markdown
        assert "newsletter" not in ctx.markdown.lower()

    def test_duplicate_sources_collapse(self):
        dupes = [
            rustai.extract(TOKIO, "https://a.dev/tokio"),
            rustai.extract(TOKIO, "https://mirror.dev/tokio"),
        ]
        ctx = rustai.slim("tokio rayon", dupes, max_tokens=600)
        assert len(ctx.sources) == 1, ctx.markdown

    def test_per_source_cap(self, articles):
        ctx = rustai.slim("tokio rayon memory", articles, max_tokens=800, max_tokens_per_source=40)
        assert all(s["tokens"] <= 40 for s in ctx.sources), ctx.sources


class TestOutput:
    def test_sources_are_cited(self, articles):
        ctx = rustai.slim("tokio", articles, max_tokens=400)
        assert "<https://a.dev/tokio>" in ctx.markdown
        assert ctx.markdown.startswith("## ")

    def test_article_headings_sit_below_the_source_header(self, articles):
        ctx = rustai.slim("tokio rayon memory", articles, max_tokens=600)
        source_titles = {s["title"] for s in ctx.sources}
        for line in ctx.markdown.splitlines():
            assert not line.startswith("# "), f"h1 leaked into the context: {line}"
            if line.startswith("## "):
                # The only `##` allowed is a source header; a document's own
                # headings must be demoted beneath it.
                assert line[3:] in source_titles, line

    def test_str_is_the_markdown(self, articles):
        ctx = rustai.slim("tokio", articles, max_tokens=200)
        assert str(ctx) == ctx.markdown

    def test_scoring_detail_is_exposed(self, articles):
        ctx = rustai.slim("rayon work stealing", articles, max_tokens=400)
        assert ctx.units_considered >= len(ctx.selected)
        for s in ctx.selected:
            assert set(s) >= {"source", "unit", "score", "relevance", "density", "tokens"}
            assert 0.0 <= s["relevance"] <= 1.0
            assert 0.0 <= s["density"] <= 1.0


class TestEdgeCases:
    def test_no_articles(self):
        ctx = rustai.slim("anything", [], max_tokens=500)
        assert ctx.markdown == ""
        assert ctx.tokens == 0
        assert ctx.units_considered == 0

    def test_empty_query_falls_back_to_density(self, articles):
        ctx = rustai.slim("", articles, max_tokens=300)
        assert ctx.markdown
        assert all(s["relevance"] == 0.0 for s in ctx.selected)

    def test_query_matching_nothing_still_returns_valid_output(self, articles):
        ctx = rustai.slim("zzzzz qqqqq wwwww", articles, max_tokens=300)
        assert isinstance(ctx.markdown, str)
        assert ctx.tokens >= 0

    @pytest.mark.parametrize("bad", [-0.5, 1.5])
    def test_diversity_is_validated(self, articles, bad):
        with pytest.raises(ValueError):
            rustai.slim("x", articles, diversity=bad)

    def test_zero_budget_selects_nothing(self, articles):
        ctx = rustai.slim("tokio", articles, max_tokens=0)
        assert ctx.selected == []
