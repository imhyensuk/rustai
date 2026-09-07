#!/usr/bin/env python3
"""Does an embedding model, folded into the ranking, find more answers?

`slim` takes caller-supplied vectors and blends cosine similarity into BM25
relevance. This sweeps how much, against the same queries and the same
answer patterns `retrieval.py` uses, so the question is settled by measurement
rather than by the fact that hybrid search is fashionable.

Needs an embedding model, which this library deliberately does not bundle:

    pip install sentence-transformers
    python3 benches/hybrid.py

Measured once, on a multilingual MiniLM:

    semantic_weight   0.0    0.25   0.5    0.75   1.0
    answer found      96%    100%   100%   96%    96%

Both extremes lose. Pure BM25 misses "layer" in an answer about deep learning
that never uses the word; pure cosine loses `rust_ownership`, where the exact
terms are the point. The gain lives in the middle, which is why the default
`semantic_weight` is 0.5.

Embedding is also the expensive half: 1.2s to 7.9s for 96 to 2,249 units,
against 2-4s for the whole search-and-extract pipeline. Vectors are worth it
for a question BM25 cannot phrase, not as a default for everything.
"""

import re
import statistics
import sys
import time

import rustai

sys.path.insert(0, "benches")
from retrieval import BUDGET, QUERIES, SOURCES  # noqa: E402

CONTACT = None
MODEL = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"
WEIGHTS = [0.0, 0.25, 0.5, 0.75, 1.0]


def answered(text: str, must) -> float:
    return sum(bool(re.search(p, text, re.I)) for p in must) / len(must)


def main() -> None:
    try:
        from sentence_transformers import SentenceTransformer
    except ImportError:
        sys.exit("pip install sentence-transformers")

    model = SentenceTransformer(MODEL)
    print(f"{MODEL}\n")
    print(f"{'query':22}" + "".join(f"{w:>7}" for w in WEIGHTS) + f"{'units':>8}{'embed':>8}")

    rows = {w: [] for w in WEIGHTS}
    for case in QUERIES:
        client = rustai.Client(providers=case["providers"], limit=15,
                               max_tokens=BUDGET, contact_email=CONTACT, timeout=25.0)
        try:
            hits = client.search(case["q"])[:SOURCES]
            pages = client.fetch([h.url for h in hits])
        except Exception as error:
            print(f"{case['id']:22}  건너뜀: {type(error).__name__}")
            continue
        articles = [a for a in (rustai.extract(p.body, url=p.url) for p in pages) if a.units]
        if not articles:
            continue

        # The order slim expects: every unit of every article, flattened.
        units = [u.text for a in articles for u in a.units]
        start = time.time()
        unit_vectors = model.encode(units, batch_size=64, show_progress_bar=False).tolist()
        query_vector = model.encode(case["q"]).tolist()
        embed_seconds = time.time() - start

        cells = []
        for weight in WEIGHTS:
            context = rustai.slim(case["q"], articles, max_tokens=BUDGET,
                                  query_vector=query_vector, unit_vectors=unit_vectors,
                                  semantic_weight=weight)
            score = answered(context.markdown, case["must"])
            rows[weight].append(score)
            cells.append(f"{score:>6.0%} ")
        print(f"{case['id']:22}" + "".join(cells)
              + f"{len(units):>8,}{embed_seconds:>7.1f}s")

    scored = [w for w in WEIGHTS if rows[w]]
    if not scored:
        sys.exit("측정된 질의가 없습니다")
    print(f"\n{'평균 답 포함률':22}"
          + "".join(f"{statistics.mean(rows[w]):>6.0%} " for w in scored))
    best = max(scored, key=lambda w: statistics.mean(rows[w]))
    print(f"\n최적 semantic_weight: {best}   (기본값 0.5)")


if __name__ == "__main__":
    main()
