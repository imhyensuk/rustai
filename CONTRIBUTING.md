# Contributing

Thanks for taking a look. Bug reports and pull requests are both welcome.

## Getting set up

```bash
git clone https://github.com/imhyensuk/rustai
cd rustai
python -m venv .venv && source .venv/bin/activate
pip install maturin pytest
maturin develop --release
```

## The checks CI runs

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo test --doc
pytest
```

Do not run `cargo clippy --all-features`: the `nightly-simd` feature enables
`tl`'s `portable_simd`, which does not build on stable. Lint each real
combination instead — that is what `.github/workflows/ci.yml` does.

The live-internet tests are off by default and are not part of CI:

```bash
pytest --network
```

## Notes on the code

- **The denoiser earns its keep on real pages, not synthetic ones.** If you
  change a heuristic in `src/denoise/`, add a test with markup from a page that
  actually broke — a fixture in `tests/integration.rs` is the right home.
- **Every heuristic constant should be able to explain itself.** The thresholds
  in `DenoiseConfig`, `SlimConfig` and `density.rs` all carry a comment saying
  what they trade off. Please keep that up when you add one.
- **The token budget is a guarantee, not an estimate.** `rank::slim` renders,
  measures, and trims until the output fits. If you change the renderer, keep
  that loop correct.
- **Politeness defaults stay on.** `robots.txt`, per-host pacing and body caps
  are the reason this library is safe to hand to someone. Making them
  configurable is fine; making them default-off is not.

## Notebooks

They are for Colab, and Colab installs from PyPI. Writing one against this
checkout ships a cell that dies on the first line touching the library, in
front of whoever trusted it — which has happened twice, once for `max_results`
and once for `respect_crawl_delay`. Check against the published package:

```bash
python3 -m venv /tmp/released
/tmp/released/bin/pip install rustai
/tmp/released/bin/python scripts/check_notebooks.py
```

CI runs the same thing. To use something newer than the released floor, guard
it with `try` / `except TypeError` and fall back — the checker skips calls
inside such a block, since that is the honest way to reach forward.

## Benchmarks

Throughput, against a synthetic corpus so the numbers are deterministic:

```bash
python benches/corpus.py /tmp/rustai-corpus
cargo run --release --example bench -- /tmp/rustai-corpus
python benches/benchmark.py
```

Extraction quality, against real pages, because a corpus this repository wrote
cannot tell you whether the denoiser survives the open web:

```bash
python benches/fetch_corpus.py /tmp/rustai-pages
python benches/quality.py      /tmp/rustai-pages
```

What the router and slimmer put in the context window, with no model in the
loop, because a number that describes rustai should not move when someone
changes their prompt:

```bash
python benches/retrieval.py
```

Each query carries the patterns any correct source would contain, so the
headline is simply whether the answer reached the window. It also reports what
share of the budget went to sources that mention the subject at all.

Quality is scored two ways at once — shingle overlap with each page's own
article container, and a count of site furniture that reached the output.
Overlap alone says to switch every threshold off, because the container being
compared against never held the navigation. Read both numbers or neither.

If a change moves any of them, say so in the pull request.
