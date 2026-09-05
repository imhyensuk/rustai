#!/usr/bin/env python3
"""Keep the type stub's docstrings identical to the compiled module's.

The Rust side documents everything and PyO3 turns those comments into
``__doc__``, so ``help()`` has always worked. Editors do not call
``help()`` -- they read the ``.pyi`` -- so the same text has to exist in
both places, and the only way it stays there is by being generated.

    python3 scripts/sync_stub_docs.py            # write
    python3 scripts/sync_stub_docs.py --check    # verify, for CI
"""

import ast
import sys
import textwrap

import rustai

MODULE = rustai._rustai
PATH = "python/rustai/_rustai.pyi"


def runtime_doc(owner, name=None):
    try:
        obj = getattr(owner, name) if name else owner
    except AttributeError:
        return None
    return ((obj.__doc__ or "").strip()) or None


def render(doc, indent):
    body = textwrap.dedent(doc).strip()
    if "\n" not in body:
        return [f'{indent}"""{body}"""']
    head, *rest = body.splitlines()
    return [f'{indent}"""{head}'] + [f"{indent}{r}".rstrip() for r in rest] + [f'{indent}"""']


def first_line_of(node):
    """Where a definition starts on the page, decorators included."""
    if getattr(node, "decorator_list", None):
        return min(d.lineno for d in node.decorator_list)
    return node.lineno


def walk(tree):
    """Yield (node, indent, owner, attribute) for everything documentable."""
    for node in tree.body:
        if isinstance(node, ast.ClassDef):
            yield node, "    ", MODULE, node.name
            cls = getattr(MODULE, node.name, None)
            for item in node.body:
                if isinstance(item, ast.FunctionDef):
                    yield item, "        ", cls, item.name
        elif isinstance(node, ast.FunctionDef):
            yield node, "    ", MODULE, node.name


def main():
    src = open(PATH, encoding="utf-8").read()
    tree = ast.parse(src)
    lines = src.splitlines()

    if "--check" in sys.argv:
        problems = []
        for node, _, owner, attr in walk(tree):
            want = runtime_doc(owner, attr)
            have = ast.get_docstring(node)
            if want != have:
                where = f"{attr} (line {node.lineno})"
                problems.append(
                    f"  {where}: stub has {have!r}, module has {want!r}"
                )
        if problems:
            print("stub docstrings are out of date:")
            print("\n".join(problems[:20]))
            sys.exit("\nRun: python3 scripts/sync_stub_docs.py")
        print("stub docstrings are in sync")
        return

    edits = []
    for node, indent, owner, attr in walk(tree):
        doc = runtime_doc(owner, attr)
        if not doc or ast.get_docstring(node) == doc:
            continue
        if isinstance(node, ast.ClassDef):
            edits.append((first_line_of(node.body[0]) - 1, indent, doc, None))
        else:
            edits.append((node.lineno - 1, indent, doc, node))

    out = list(lines)
    for ln, indent, doc, node in sorted(edits, key=lambda e: (-e[0], e[3] is None)):
        if node is not None:
            end = node.end_lineno - 1
            text = "\n".join(out[ln:end + 1]).rstrip()
            if text.endswith("..."):
                head = text[: text.rfind("...")].rstrip().rstrip(":")
                out[ln:end + 1] = [head + ":"] + render(doc, indent) + [f"{indent}..."]
                continue
        out[ln:ln] = render(doc, indent)

    open(PATH, "w", encoding="utf-8").write("\n".join(out) + "\n")
    print(f"{len(edits)} docstrings synced")


if __name__ == "__main__":
    main()
