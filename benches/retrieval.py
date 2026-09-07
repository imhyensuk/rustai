#!/usr/bin/env python3
"""Score what the library puts in the context window, without a model.

`quality.py` asks whether extraction kept a page's article. This asks the
question one stage later and one stage more useful: given a question, does the
context contain the answer?

That is the library's whole job. Whether a model then uses the context is the
model's job, and whether it was asked well is the prompt's -- neither belongs
in a number that describes rustai. Every measurement here is model-free and
prompt-free, so it stays comparable as those change.

Each query carries `must`: patterns that any correct source would contain. They
are the answer's fingerprint, not one source's wording -- "k1" and
"saturation|포화" for BM25 term saturation, matched against whatever the router
found. Three numbers come out:

  answer      fraction of `must` patterns present. The headline: the window
              either holds the answer or it does not.
  on-topic    share of the token budget spent on sources that mention the
              subject at all. Waste, measured -- a K-pop article pulled in by
              a surname is 700 tokens the answer could have used.
  junk        sources mentioning nothing of the subject.

    python3 benches/retrieval.py            # every query
    python3 benches/retrieval.py 날씨        # only queries whose id matches
"""

import re
import statistics
import sys
import time

import rustai

CONTACT = None          # OpenAlex/Crossref polite pool
BUDGET = 2048
SOURCES = 5

WEB = ["duckduckgo", "wikipedia:ko", "wikipedia:en", "stackexchange",
       "hackernews", "github"]
LIVE = [p for p in WEB if not p.startswith("wikipedia")]

# `topic` decides whether a source is about the subject at all; `must` decides
# whether the answer arrived. Both are patterns, so a source is free to phrase
# it in either language.
QUERIES = [
    # --- 설명형: 산문이 웹에 있는 질문. 이 라이브러리가 만들어진 목적. ---
    {"id": "bm25_saturation", "providers": WEB,
     "q": "BM25에서 term saturation이 무엇인가",
     "topic": r"bm25|랭킹|검색",
     "must": [r"k1|k₁", r"saturat|포화", r"tf|용어\s*빈도|단어\s*빈도"]},
    {"id": "deep_learning", "providers": WEB,
     "q": "딥러닝이 뭐야?",
     "topic": r"딥\s*러닝|딥러닝|deep learning|신경망",
     "must": [r"신경망|neural", r"층|layer", r"학습|training"]},
    {"id": "rust_ownership", "providers": WEB,
     "q": "러스트의 소유권 규칙은 무엇인가",
     "topic": r"러스트|rust|소유권|ownership",
     "must": [r"소유자|owner", r"스코프|scope|범위", r"드롭|drop|해제"]},
    {"id": "asyncio_loop", "providers": WEB,
     "q": "python asyncio event loop internals",
     "topic": r"asyncio|event loop|coroutine",
     "must": [r"coroutine|코루틴", r"await", r"task|태스크"]},
    {"id": "positional_encoding", "providers": WEB,
     "q": "why do transformers need positional encoding",
     "topic": r"transformer|positional|attention",
     "must": [r"position|위치", r"sin|cos|sinusoid|rotary|learned", r"order|순서"]},
    {"id": "postgres_bloat", "providers": WEB,
     "q": "postgres index bloat vacuum full",
     "topic": r"postgres|vacuum|bloat|index",
     "must": [r"vacuum", r"bloat|팽창", r"reindex|rebuild|full"]},

    # --- 사건형: 특정 날짜에 무슨 일이 있었는가. 백과사전이 강한 자리. ---
    {"id": "newyear_2026", "providers": WEB,
     "q": "2026년 1월 1일에는 어떤 일들이 있었어?",
     "topic": r"2026|1월\s*1일|새해|신정",
     "must": [r"2026", r"1월|january", r"새해|신정|양력설|new year"]},

    # --- 조회형: 지금 값이 얼마인가. 값이 JS 로 그려지는 자리. ---
    {"id": "seoul_weather", "providers": LIVE,
     "q": "오늘 서울의 날씨는?",
     "topic": r"날씨|기온|예보|weather",
     "must": [r"\d+\s*°|\d+\s*도", r"서울", r"기온|온도|최고|최저"]},
    {"id": "busan_weather", "providers": LIVE,
     "q": "오늘 부산의 날씨는?",
     "topic": r"날씨|기온|예보|weather",
     "must": [r"\d+\s*°|\d+\s*도", r"부산", r"기온|온도|최고|최저"]},
]


def score(case) -> dict:
    client = rustai.Client(
        providers=case["providers"], limit=15, max_tokens=BUDGET,
        max_tokens_per_source=700, contact_email=CONTACT, timeout=25.0,
    )
    start = time.time()
    try:
        res = client.research(case["q"], max_sources=SOURCES)
    except Exception as error:
        return {"id": case["id"], "error": f"{type(error).__name__}: {error}"}
    elapsed = time.time() - start

    grouped = {}
    for sel in res.context.selected:
        grouped.setdefault(sel["source"], []).append(sel["unit"])

    topic = re.compile(case["topic"], re.I)
    on_topic = off_topic = 0
    junk = []
    body = []
    for meta in res.context.sources:
        units = grouped.get(meta["index"])
        if not units:
            continue
        article = res.articles[meta["index"]]
        text = "\n".join(article.units[u].text for u in sorted(units))
        body.append(text)
        if topic.search(text) or topic.search(meta["title"]):
            on_topic += meta["tokens"]
        else:
            off_topic += meta["tokens"]
            junk.append((meta["tokens"], meta["title"]))

    context = "\n".join(body)
    hits = [bool(re.search(p, context, re.I)) for p in case["must"]]
    total = on_topic + off_topic
    return {
        "id": case["id"],
        "answer": sum(hits) / len(hits),
        "missing": [p for p, ok in zip(case["must"], hits) if not ok],
        "on_topic": on_topic / total if total else 0.0,
        "junk": junk,
        "tokens": total,
        "read": len(res.articles),
        "skipped": len(res.failures),
        "seconds": elapsed,
    }


def main() -> None:
    wanted = sys.argv[1] if len(sys.argv) > 1 else ""
    cases = [c for c in QUERIES if wanted in c["id"] or wanted in c["q"]]
    if not cases:
        sys.exit(f"no query matching {wanted!r}")

    rows = []
    print(f"{'query':22}{'answer':>8}{'on-topic':>10}{'tokens':>8}"
          f"{'read':>6}{'skip':>6}{'secs':>7}")
    for case in cases:
        row = score(case)
        rows.append(row)
        if "error" in row:
            print(f"{row['id']:22}  {row['error']}")
            continue
        print(f"{row['id']:22}{row['answer']:>7.0%}{row['on_topic']:>10.0%}"
              f"{row['tokens']:>8}{row['read']:>6}{row['skipped']:>6}"
              f"{row['seconds']:>6.1f}s")
        for pattern in row["missing"]:
            print(f"  {'':20}  못 찾음: /{pattern}/")
        for tokens, title in row["junk"]:
            print(f"  {'':20}  무관 {tokens:>4}tok: {title[:52]}")

    good = [r for r in rows if "error" not in r]
    if not good:
        return
    print(f"\n답 포함률 {statistics.mean(r['answer'] for r in good):.0%}   "
          f"주제 예산 {statistics.mean(r['on_topic'] for r in good):.0%}   "
          f"무관 출처 {sum(len(r['junk']) for r in good)}개   "
          f"중앙 지연 {statistics.median(r['seconds'] for r in good):.1f}초   "
          f"({len(good)} queries)")


if __name__ == "__main__":
    main()
