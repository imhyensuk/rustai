"""Measure extraction throughput and peak resident memory.

Run as `python benches/benchmark.py [engine]`, where engine is `rustai` or
`bs4`. Each engine runs in its *own process* so `ru_maxrss` is attributable to
it and nothing else; `benchmark.py all` drives both as subprocesses and prints
the comparison.

`ru_maxrss` is peak RSS for the whole process, interpreter included -- the
number that actually shows up in `top`, not a library-internal allocator stat.
"""

import json
import os
import resource
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from corpus import corpus  # noqa: E402

N_DOCS = 200
# Streaming mode mirrors how a crawler actually runs: one document in memory at
# a time, results consumed and dropped. Batch mode holds all 200 at once, which
# measures the corpus more than it measures the extractor.
STREAM = os.environ.get("RUSTAI_BENCH_STREAM") == "1"
# macOS reports ru_maxrss in bytes; Linux reports kilobytes.
RSS_DIVISOR = 1 << 20 if sys.platform == "darwin" else 1 << 10


def peak_rss_mb() -> float:
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / RSS_DIVISOR


def run_rustai(docs: list[str]) -> dict:
    import rustai

    # Warm up so the measurement is steady-state, not first-touch.
    rustai.extract(docs[0])
    start = time.perf_counter()
    if STREAM:
        chars = 0
        for i, d in enumerate(docs):
            chars += len(rustai.extract(d, f"https://example.com/{i}").markdown)
        n = len(docs)
    else:
        out = [rustai.extract(d, f"https://example.com/{i}") for i, d in enumerate(docs)]
        chars, n = sum(len(a.markdown) for a in out), len(out)
    elapsed = time.perf_counter() - start
    return {
        "engine": "rustai",
        "docs": n,
        "seconds": elapsed,
        "peak_rss_mb": peak_rss_mb(),
        "out_chars": chars,
    }


def run_bs4(docs: list[str]) -> dict:
    from bs4 import BeautifulSoup

    drop = {"script", "style", "nav", "footer", "aside", "header", "form", "svg", "noscript"}

    def extract(html: str) -> str:
        soup = BeautifulSoup(html, "lxml")
        for tag in soup.find_all(drop):
            tag.decompose()
        main = soup.find("article") or soup.body or soup
        return main.get_text("\n", strip=True)

    extract(docs[0])
    start = time.perf_counter()
    if STREAM:
        chars = sum(len(extract(d)) for d in docs)
        n = len(docs)
    else:
        out = [extract(d) for d in docs]
        chars, n = sum(len(t) for t in out), len(out)
    elapsed = time.perf_counter() - start
    return {
        "engine": "bs4+lxml",
        "docs": n,
        "seconds": elapsed,
        "peak_rss_mb": peak_rss_mb(),
        "out_chars": chars,
    }


def report(r: dict, in_mb: float) -> str:
    return (
        f"{r['engine']:<10} {r['seconds']:>7.2f}s  "
        f"{r['docs'] / r['seconds']:>7.0f} docs/s  "
        f"{in_mb / r['seconds']:>6.1f} MB/s  "
        f"peak RSS {r['peak_rss_mb']:>6.1f} MB  "
        f"output {r['out_chars'] / 1e6:.2f} MB"
    )


def main() -> None:
    engine = sys.argv[1] if len(sys.argv) > 1 else "all"
    docs = corpus(N_DOCS)
    in_mb = sum(len(d) for d in docs) / 1e6

    if engine == "rustai":
        print(json.dumps(run_rustai(docs)))
    elif engine == "bs4":
        print(json.dumps(run_bs4(docs)))
    else:
        mode = "streaming" if STREAM else "batch"
        print(f"corpus: {len(docs)} documents, {in_mb:.2f} MB of HTML ({mode})\n")
        for name in ("rustai", "bs4"):
            proc = subprocess.run(
                [sys.executable, __file__, name], capture_output=True, text=True
            )
            if proc.returncode != 0:
                print(f"{name:<10} unavailable ({proc.stderr.strip().splitlines()[-1:]})")
                continue
            print(report(json.loads(proc.stdout.strip().splitlines()[-1]), in_mb))


if __name__ == "__main__":
    main()
