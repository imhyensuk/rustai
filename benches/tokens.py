#!/usr/bin/env python3
"""What does a question cost to answer, in tokens, and does the answer survive?

The speed benchmark compares this library against extractors that do one of
the things it does. This one measures the thing none of them do: turning a
question into a context window a model can afford to read.

Five stages, each the point where somebody's pipeline actually stops:

  raw HTML     no extraction at all
  parsed text  a parser's text dump -- selectolax, or BeautifulSoup
  extracted    a real extractor's article text -- resiliparse or trafilatura
  rustai all   every unit this library kept, unranked
  rustai slim  ranked and cut to a token budget

Tokens alone would reward throwing everything away, so each stage is also
checked for the answer: patterns any correct source would contain, the same
ones `retrieval.py` uses. A stage that is small and has lost the answer is a
failure, not a win.

    python3 benches/tokens.py
    python3 benches/tokens.py bm25        # one query
"""

import re
import statistics
import sys

import rustai

BUDGET = 2048
SOURCES = 5
CONTACT = None          # OpenAlex/Crossref polite pool

WEB = ["duckduckgo", "wikipedia:ko", "wikipedia:en", "stackexchange",
       "hackernews", "github"]
LIVE = [p for p in WEB if not p.startswith("wikipedia")]

QUERIES = [
    {"id": "bm25", "providers": WEB,
     "q": "BM25에서 term saturation이 무엇인가",
     "must": [r"k1|k₁", r"saturat|포화", r"tf|용어\s*빈도|단어\s*빈도"]},
    {"id": "deep_learning", "providers": WEB,
     "q": "딥러닝이 뭐야?",
     "must": [r"신경망|neural", r"학습|training"]},
    {"id": "rust_ownership", "providers": WEB,
     "q": "러스트의 소유권 규칙은 무엇인가",
     "must": [r"소유자|owner", r"스코프|scope|범위", r"드롭|drop|해제"]},
    {"id": "asyncio", "providers": WEB,
     "q": "python asyncio event loop internals",
     "must": [r"coroutine|코루틴", r"await", r"task|태스크"]},
    {"id": "positional", "providers": WEB,
     "q": "why do transformers need positional encoding",
     "must": [r"position|위치", r"sin|cos|sinusoid|rotary|learned"]},
    {"id": "postgres", "providers": WEB,
     "q": "postgres index bloat vacuum full",
     "must": [r"vacuum", r"bloat|팽창", r"reindex|rebuild|full"]},
    {"id": "seoul_weather", "providers": LIVE,
     "q": "오늘 서울의 날씨는?",
     "must": [r"\d+\s*°|\d+\s*도", r"서울"]},
]


def parsers():
    """Text dumps and real extractors, whichever are installed."""
    dump = extract = None
    try:
        from selectolax.parser import HTMLParser

        def dump(html):                                    # noqa: F811
            body = HTMLParser(html).body
            return body.text(separator=" ") if body else ""
    except ImportError:
        try:
            import bs4

            def dump(html):                                # noqa: F811
                return bs4.BeautifulSoup(html, "lxml").get_text(" ", strip=True)
        except ImportError:
            pass
    try:
        from resiliparse.extract.html2text import extract_plain_text
        from resiliparse.parse.html import HTMLTree

        def extract(html):                                 # noqa: F811
            return extract_plain_text(HTMLTree.parse(html), main_content=True)
    except ImportError:
        try:
            import trafilatura

            def extract(html):                             # noqa: F811
                return trafilatura.extract(html, include_tables=True) or ""
        except ImportError:
            pass
    return dump, extract


def answered(text: str, must) -> float:
    return sum(bool(re.search(p, text, re.I)) for p in must) / len(must)


def score(case, dump, extract) -> dict:
    client = rustai.Client(providers=case["providers"], limit=15,
                           max_tokens=BUDGET, contact_email=CONTACT, timeout=25.0)
    hits = client.search(case["q"])[:SOURCES]
    pages = client.fetch([h.url for h in hits])
    if not pages:
        return {"id": case["id"], "error": "가져온 페이지 없음"}

    stages = {}
    raw = "\n".join(p.body for p in pages)
    stages["raw HTML"] = raw
    if dump:
        stages["parsed text"] = "\n".join(dump(p.body) for p in pages)
    if extract:
        stages["extracted"] = "\n".join(extract(p.body) for p in pages)

    articles = [a for a in (rustai.extract(p.body, url=p.url) for p in pages) if a.units]
    stages["rustai all"] = "\n".join(a.markdown for a in articles)
    stages["rustai slim"] = rustai.slim(case["q"], articles, max_tokens=BUDGET).markdown

    return {
        "id": case["id"],
        "stages": {name: (rustai.count_tokens(text), answered(text, case["must"]))
                   for name, text in stages.items()},
    }


def main() -> None:
    wanted = sys.argv[1] if len(sys.argv) > 1 else ""
    cases = [c for c in QUERIES if wanted in c["id"] or wanted in c["q"]]
    if not cases:
        sys.exit(f"no query matching {wanted!r}")
    dump, extract = parsers()

    rows = [score(c, dump, extract) for c in cases]
    rows = [r for r in rows if "stages" in r]
    if not rows:
        sys.exit("측정된 질의가 없습니다")

    names = list(rows[0]["stages"])
    print(f"{'query':16}" + "".join(f"{n:>18}" for n in names))
    for row in rows:
        cells = []
        for name in names:
            tokens, found = row["stages"][name]
            cells.append(f"{tokens:>11,} {found:>5.0%}")
        print(f"{row['id']:16}" + "".join(cells))

    print(f"\n{'평균':16}", end="")
    base = None
    for name in names:
        tokens = statistics.mean(r["stages"][name][0] for r in rows)
        found = statistics.mean(r["stages"][name][1] for r in rows)
        base = base or tokens
        print(f"{tokens:>11,.0f} {found:>5.0%}", end="")
    print()
    print(f"{'raw 대비':16}", end="")
    for name in names:
        tokens = statistics.mean(r["stages"][name][0] for r in rows)
        print(f"{tokens / base:>16.1%}  ", end="")
    print("\n\n토큰만 보면 전부 버리는 쪽이 이깁니다. 답 포함률과 함께 읽으세요.")


if __name__ == "__main__":
    main()
