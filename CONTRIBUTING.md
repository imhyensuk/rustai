# Contributing

Thanks for taking a look. Bug reports and pull requests are both welcome.

## Getting set up

```bash
git clone https://github.com/hyeonseok-im/rustai
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

## Benchmarks

```bash
python benches/corpus.py /tmp/rustai-corpus
cargo run --release --example bench -- /tmp/rustai-corpus
python benches/benchmark.py
```

If a change moves those numbers, say so in the pull request.
