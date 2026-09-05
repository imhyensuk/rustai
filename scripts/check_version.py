#!/usr/bin/env python3
"""Check that the version is consistent everywhere it is written down.

The version lives in two manifests, and a release is driven by a third
thing -- the git tag. Nothing makes them agree on its own: bump one and
publish, and crates.io and PyPI end up on different versions of the same
release. Run with a tag argument (``v0.1.1``) to check that too.
"""

import pathlib
import re
import sys


def version_in(path: str) -> str:
    text = pathlib.Path(path).read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    if not match:
        sys.exit(f"{path}: no version field found")
    return match.group(1)


def main() -> None:
    cargo = version_in("Cargo.toml")
    pyproject = version_in("pyproject.toml")
    print(f"Cargo.toml     {cargo}")
    print(f"pyproject.toml {pyproject}")

    if cargo != pyproject:
        sys.exit(
            f"\nversion mismatch: Cargo.toml={cargo}, pyproject.toml={pyproject}\n"
            "Bump both together, or the crate and the wheel ship as different "
            "versions of the same release."
        )

    tag = sys.argv[1] if len(sys.argv) > 1 else None
    if tag:
        want = tag[1:] if tag.startswith("v") else tag
        print(f"git tag        {tag}")
        if want != cargo:
            sys.exit(
                f"\ntag {tag} does not match the manifests ({cargo}).\n"
                "The tag names the release; the manifests decide what is "
                "actually published. Publishing them apart puts the wrong "
                "number on the artifacts, permanently."
            )

    print("consistent.")


if __name__ == "__main__":
    main()
