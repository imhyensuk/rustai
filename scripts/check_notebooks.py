#!/usr/bin/env python3
"""Check that the notebooks only call rustai the way the installed one works.

The notebooks are for Colab, and Colab installs from PyPI. Twice now a
notebook has been written against the working tree and shipped calling an
argument that only exists on `main` -- `max_results`, then
`respect_crawl_delay` -- so the cell died on the first line that touched the
library, in front of the person who trusted it.

Run this against the *released* package, not the tree, which is the whole
point:

    python3 -m venv /tmp/released
    /tmp/released/bin/pip install rustai trafilatura
    /tmp/released/bin/python scripts/check_notebooks.py

It reads every `rustai.<name>(...)` call out of every notebook and checks the
name exists and its keywords are accepted. Keywords guarded by `try:` /
`except TypeError:` are skipped, since that is how a notebook is supposed to
use something newer than the floor it supports.
"""

import ast
import json
import pathlib
import sys

import rustai

# `Client(...)` is a class; the rest are functions. Both answer to the same
# question -- would this call raise TypeError -- but only by being called.
PROBES = {
    "Client": lambda kw: rustai.Client(**{k: _sample(k) for k in kw}),
    "research": None,       # network; checked for name and arity only
    "slim": None,
    "extract": None,
    "extract_many": None,
    "count_tokens": None,
    "tokenize": None,
    "density": None,
    "canonical_url": None,
    "parse_feed": None,
    "parse_sitemap": None,
}

# Values that are type-correct for the arguments a notebook actually passes.
SAMPLES = {
    "providers": ["duckduckgo"], "concurrency": 4, "timeout": 5.0,
    "impersonate": "chrome", "respect_robots": True, "respect_crawl_delay": True,
    "per_host_delay": 0.0, "retries": 0, "max_body_bytes": 1024,
    "accept_language": "en", "browser_fallback": False, "contact_email": None,
    "proxies": None, "max_retry_after": 1.0, "cookie_file": None,
    "max_tokens": 128, "max_tokens_per_source": 64, "diversity": 0.3,
    "include_links": True, "include_images": False, "include_tables": True,
    "index_mode": "auto", "limit": 3, "max_results": 3, "max_sources": 1,
}


def _sample(name: str):
    if name not in SAMPLES:
        raise KeyError(name)
    return SAMPLES[name]


def guarded(tree: ast.AST) -> set[int]:
    """Line numbers inside a `try:` that catches TypeError."""
    lines = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.Try):
            continue
        catches = any(
            isinstance(h.type, ast.Name) and h.type.id == "TypeError"
            or isinstance(h.type, ast.Tuple)
            and any(isinstance(e, ast.Name) and e.id == "TypeError" for e in h.type.elts)
            for h in node.handlers
        )
        if catches:
            for child in ast.walk(node):
                if hasattr(child, "lineno"):
                    lines.add(child.lineno)
    return lines


def calls(source: str):
    """Every `rustai.<name>(...)`, with its keyword names and line.

    Raises `SyntaxError`, deliberately. A checker that treats "could not read
    this" as "nothing wrong here" is worse than no checker.
    """
    tree = ast.parse(source)
    skip = guarded(tree)
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        f = node.func
        if not (isinstance(f, ast.Attribute) and isinstance(f.value, ast.Name)
                and f.value.id == "rustai"):
            continue
        if node.lineno in skip:
            continue
        yield f.attr, [k.arg for k in node.keywords if k.arg], node.lineno


def main() -> None:
    notebooks = sorted(pathlib.Path("notebooks").glob("*.ipynb"))
    if not notebooks:
        sys.exit("notebooks/ 아래에 .ipynb 가 없습니다")
    print(f"rustai {rustai.__version__} 에 대고 검사합니다\n")

    problems = 0
    for path in notebooks:
        cells = [c for c in json.loads(path.read_text())["cells"]
                 if c["cell_type"] == "code"]
        print(f"{path.name}")
        found = []
        for number, cell in enumerate(cells, start=1):
            # `%pip` and `!cmd` are IPython, not Python. Blanking them
            # outright empties any block they are the only statement in, so
            # they become `pass` at their own indentation instead. Test with
            # `startswith`, not `in "%!"` -- the empty string is in every
            # string, which turns every blank line into a `pass` at column
            # zero and breaks the indentation of the whole cell.
            source = "\n".join(
                (l[: len(l) - len(l.lstrip())] + "pass")
                if l.lstrip().startswith(("%", "!")) else l
                for l in "".join(cell["source"]).splitlines()
            )
            # Cells are parsed one at a time: each is valid Python on its own,
            # while the concatenation of several need not be.
            try:
                found.extend(calls(source))
            except SyntaxError as error:
                print(f"  셀 {number}: 파싱 실패 {error.lineno}행 — {error.msg}")
                problems += 1
        for name, keywords, line in found:
            if not hasattr(rustai, name):
                print(f"  {line:>4}행  rustai.{name} 이 없습니다")
                problems += 1
                continue
            probe = PROBES.get(name)
            if probe is None:
                continue
            try:
                probe(keywords)
            except TypeError as error:
                print(f"  {line:>4}행  rustai.{name}({', '.join(keywords)}) → {error}")
                problems += 1
            except KeyError as error:
                print(f"  {line:>4}행  {error} 의 견본값이 없습니다 — SAMPLES 에 추가하세요")
                problems += 1

    print()
    if problems:
        sys.exit(f"{problems}건. 노트북은 릴리스된 rustai 에서 돌아야 합니다.")
    print("노트북의 rustai 호출이 모두 이 버전에서 동작합니다.")


if __name__ == "__main__":
    main()
