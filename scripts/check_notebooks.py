#!/usr/bin/env python3
"""Check that the docs only call rustai the way the installed one works.

The notebooks are for Colab, and Colab installs from PyPI. Twice now a
notebook has been written against the working tree and shipped calling an
argument that only exists on `main` -- `max_results`, then
`respect_crawl_delay` -- so the cell died on the first line that touched the
library, in front of the person who trusted it.

The README has the same failure mode and no way back: crates.io and PyPI
snapshot it at publish time and neither lets you edit it afterwards, so an
example that does not run is wrong on two package pages until the next
release.

The two have different targets, so the script takes which to check:

    # Notebooks run on Colab, which installs from PyPI -- check the release.
    python3 -m venv /tmp/released
    /tmp/released/bin/pip install rustai
    /tmp/released/bin/python scripts/check_notebooks.py notebooks

    # The README documents the version it ships with -- check the tree.
    python3 scripts/check_notebooks.py docs

Checking the README against the release would fail every time the tree adds an
API, which is exactly when the README should be describing it.

It reads every `rustai.<name>(...)` call out of every notebook and every
Markdown code block and checks the name exists and its keywords are accepted. Keywords guarded by `try:` /
`except TypeError:` are skipped, since that is how a notebook is supposed to
use something newer than the floor it supports.
"""

import ast
import json
import pathlib
import re
import sys

import rustai

# Every probe calls the real thing with the real keywords. Only `TypeError`
# counts as a failure -- anything else means the signature was accepted and the
# call then failed for its own reasons, which is what we want from a probe that
# must not touch the network. `research` is steered into an early `ValueError`
# by an empty provider list for exactly that reason.
PROBES = {
    "Client": lambda kw: rustai.Client(**{k: _sample(k) for k in kw}),
    "research": lambda kw: rustai.research(
        "q", **{**{k: _sample(k) for k in kw}, "providers": []}
    ),
    "slim": lambda kw: rustai.slim("q", [], **{k: _sample(k) for k in kw}),
    "extract": lambda kw: rustai.extract("<p>x</p>", **{k: _sample(k) for k in kw}),
    "extract_many": lambda kw: rustai.extract_many([], **{k: _sample(k) for k in kw}),
    "chunk_many": lambda kw: rustai.chunk_many([], **{k: _sample(k) for k in kw}),
    "count_tokens": None,
    "tokenize": None,
    "density": None,
    "canonical_url": None,
    "parse_feed": None,
    "parse_sitemap": None,
}

# Values that are type-correct for the arguments the docs actually pass.
SAMPLES = {
    "providers": ["duckduckgo"], "concurrency": 4, "timeout": 5.0,
    "impersonate": "chrome", "respect_robots": True, "respect_crawl_delay": True,
    "per_host_delay": 0.0, "retries": 0, "max_body_bytes": 1024,
    "accept_language": "en", "browser_fallback": False, "contact_email": None,
    "proxies": None, "max_retry_after": 1.0, "cookie_file": None,
    "max_tokens": 128, "max_tokens_per_source": 64, "diversity": 0.3,
    "include_links": True, "include_images": False, "include_tables": True,
    "index_mode": "auto", "limit": 3, "max_results": 3, "max_sources": 1,
    "include_breadcrumbs": True, "query_vector": None, "unit_vectors": None,
    "semantic_weight": 0.5, "target_tokens": 512, "overlap_tokens": 64,
    "min_tokens": 24, "urls": None, "url": None, "strict": False,
    "raise_on_error": False,
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


def blocks(path: pathlib.Path):
    """(label, source) for each independently valid chunk of Python in a file.

    Notebook cells and Markdown fences are both parsed one at a time: each is
    valid on its own, while the concatenation of several need not be.
    """
    if path.suffix == ".ipynb":
        cells = [c for c in json.loads(path.read_text(encoding="utf-8"))["cells"]
                 if c["cell_type"] == "code"]
        for number, cell in enumerate(cells, start=1):
            yield f"cell {number}", "".join(cell["source"])
        return
    # ```python … ``` only. A bare fence is usually shell or output.
    text = path.read_text(encoding="utf-8")
    for number, match in enumerate(
        re.finditer(r"^```(?:python|py)\n(.*?)^```", text, re.M | re.S), start=1
    ):
        line = text[: match.start()].count("\n") + 1
        yield f"block {number} at line {line}", match.group(1)


def main() -> None:
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    if which not in ("all", "notebooks", "docs"):
        sys.exit(f"usage: {sys.argv[0]} [all|notebooks|docs]")
    paths = []
    if which in ("all", "notebooks"):
        paths += sorted(pathlib.Path("notebooks").glob("*.ipynb"))
    if which in ("all", "docs"):
        paths += [p for p in (pathlib.Path("README.md"), pathlib.Path("CONTRIBUTING.md"))
                  if p.exists()]
    if not paths:
        sys.exit("nothing to check")
    print(f"checking against rustai {rustai.__version__}\n")

    problems = 0
    for path in paths:
        print(f"{path.name}")
        found = []
        for label, raw in blocks(path):
            # `%pip` and `!cmd` are IPython, not Python. Blanking them
            # outright empties any block they are the only statement in, so
            # they become `pass` at their own indentation instead. Test with
            # `startswith`, not `in "%!"` -- the empty string is in every
            # string, which turns every blank line into a `pass` at column
            # zero and breaks the indentation of the whole cell.
            source = "\n".join(
                (l[: len(l) - len(l.lstrip())] + "pass")
                if l.lstrip().startswith(("%", "!")) else l
                for l in raw.splitlines()
            )
            try:
                found.extend(calls(source))
            except SyntaxError:
                # A README block is often a fragment -- a signature, a `for`
                # body -- which is fine to show and impossible to parse. A
                # notebook cell is a whole program and has no such excuse.
                if path.suffix == ".ipynb":
                    print(f"  {label}: could not be parsed")
                    problems += 1
        for name, keywords, line in found:
            if not hasattr(rustai, name):
                print(f"  line {line}: rustai.{name} does not exist")
                problems += 1
                continue
            probe = PROBES.get(name)
            if probe is None:
                continue
            try:
                probe(keywords)
            except TypeError as error:
                print(f"  line {line}: rustai.{name}({', '.join(keywords)}) -> {error}")
                problems += 1
            except KeyError as error:
                # Either the argument does not exist, or it does and this
                # script has never seen it. Both need a human, so say both.
                print(f"  line {line}: rustai.{name}(... {error} ...) - no such argument, "
                      f"or a new one that needs a value in SAMPLES")
                problems += 1
            except Exception:
                # Anything but TypeError means the signature was accepted and
                # the call failed on its own terms, which is the probe working.
                pass

    print()
    if problems:
        sys.exit(f"{problems} problem(s). Examples must run on the rustai they target.")
    print("every rustai call in the docs and notebooks works on this version.")


if __name__ == "__main__":
    main()
