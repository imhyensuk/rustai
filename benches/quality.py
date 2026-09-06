#!/usr/bin/env python3
"""Score extraction quality on a corpus of saved pages.

Two measurements, because either alone points the wrong way.

**Overlap** compares the extracted text against the page's own article
container -- `<article>`, `<main>`, or a known content class -- as sets of
five-word shingles, giving recall, precision and F1. Shingles rather than
characters because character overlap scores well even when the order is
wrong, and rather than sentences because whitespace normalisation moves
those around.

**Furniture** counts site chrome that reached the output: "Jump to
content", "Skip to main", cookie notices, and their Korean equivalents.
Overlap cannot see these. The reference is the inside of the article
container, which never held the navigation, so a threshold that lets the
whole page through scores *better* on overlap while being obviously
worse. Tuning on overlap alone lands on switching every threshold off.

    python3 benches/fetch_corpus.py /tmp/rustai-pages
    python3 benches/quality.py      /tmp/rustai-pages

`corpus.py` is a different thing -- a synthetic generator for the throughput
benchmark, where determinism matters more than realism. Extraction quality
cannot be measured against pages this repository wrote.
"""

import json
import pathlib
import re
import sys

import rustai

SHINGLE = 5

FURNITURE = [
    "Jump to content", "Skip to main", "Skip to content", "Privacy Policy",
    "Terms of Service", "All rights reserved", "Cookie", "Sign in", "Log in",
    "Create account", "Subscribe to", "Follow us", "Newsletter", "Main menu",
    "Toggle", "Donate",
    "본문 바로가기", "개인정보", "이용약관", "로그인", "회원가입", "무단 전재",
]


def visible(fragment: str) -> str:
    for tag in ("script", "style", "nav", "aside", "footer", "form", "noscript"):
        fragment = re.sub(rf"<{tag}\b.*?</{tag}\s*>", " ", fragment, flags=re.S | re.I)
    return re.sub(r"\s+", " ", re.sub(r"<[^>]+>", " ", fragment)).strip()


def reference(html: str) -> str | None:
    """The page's own idea of where its article is."""
    for pattern in (
        r"<article\b[^>]*>(.*?)</article\s*>",
        r"<main\b[^>]*>(.*?)</main\s*>",
        r'<div\b[^>]*class="[^"]*(?:mw-parser-output|post-content|entry-content)[^"]*"[^>]*>(.*)',
    ):
        found = re.search(pattern, html, re.S | re.I)
        if found:
            text = visible(found.group(1))
            if len(text) > 200:
                return text
    return None


def shingles(text: str) -> set[tuple[str, ...]]:
    words = re.findall(r"\w+", text.lower())
    return {tuple(words[i:i + SHINGLE]) for i in range(max(0, len(words) - SHINGLE + 1))}


def main() -> None:
    directory = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/rustai-pages")
    urls_file = directory / "urls.json"
    urls = json.loads(urls_file.read_text()) if urls_file.exists() else {}

    rows, furniture = [], 0
    for path in sorted(directory.glob("*.html")):
        html = path.read_text(encoding="utf-8", errors="replace")
        ref = reference(html)
        if not ref:
            continue
        got = rustai.extract(html, url=urls.get(path.stem)).text
        want, have = shingles(ref), shingles(got)
        if not want:
            continue
        hit = len(want & have)
        recall = hit / len(want)
        precision = hit / len(have) if have else 0.0
        f1 = 2 * recall * precision / (recall + precision) if recall + precision else 0.0
        rows.append((path.stem, recall, precision, f1))
        furniture += sum(1 for f in FURNITURE if f.lower() in got.lower())

    if not rows:
        sys.exit(
            f"no scoreable pages in {directory}\n"
            f"Fetch the corpus first: python3 benches/fetch_corpus.py {directory}"
        )

    rows.sort(key=lambda r: r[3])
    print(f"{'page':22} {'recall':>8} {'precision':>10} {'F1':>7}")
    for name, recall, precision, f1 in rows:
        print(f"{name:22} {recall:7.1%} {precision:9.1%} {f1:6.1%}")
    n = len(rows)
    print(
        f"\nmean recall {sum(r[1] for r in rows)/n:.1%}  "
        f"precision {sum(r[2] for r in rows)/n:.1%}  "
        f"F1 {sum(r[3] for r in rows)/n:.1%}   ({n} pages)"
    )
    print(f"furniture leaked: {furniture} occurrences across {n} pages")


if __name__ == "__main__":
    main()
