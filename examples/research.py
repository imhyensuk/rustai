"""End-to-end demo: search, read, denoise, and compress to a context window.

    python examples/research.py "what is the BM25 ranking function"

Hits the live internet. Everything else in this repository runs offline.
"""

import sys
import time

import rustai


def main() -> int:
    query = " ".join(sys.argv[1:]) or "what is the BM25 ranking function"
    budget = 1200

    client = rustai.Client(
        providers=["duckduckgo", "wikipedia:en"],
        max_tokens=budget,
        concurrency=8,
        timeout=30.0,
    )

    started = time.perf_counter()
    result = client.research(query, max_sources=4)
    elapsed = time.perf_counter() - started

    print(f"query      {query!r}")
    print(f"elapsed    {elapsed:.2f}s")
    print(f"results    {len(result.results)} hits, {len(result.articles)} read")

    raw = sum(a.stats.html_bytes for a in result.articles)
    clean = sum(a.stats.markdown_bytes for a in result.articles)
    if raw:
        print(f"denoised   {raw / 1024:.0f} KB HTML -> {clean / 1024:.0f} KB Markdown "
              f"({100 * (1 - clean / raw):.0f}% dropped)")
    print(f"context    {result.context.tokens}/{budget} tokens from "
          f"{result.context.units_considered} candidate blocks")

    for stage, why in result.failures:
        print(f"  skipped  {stage}: {why}")

    print("\n" + "=" * 72)
    print(result.markdown)
    print("=" * 72)

    print("\nsources")
    for src in result.context.sources:
        print(f"  {src['tokens']:>5} tok  {src['url']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
