# Contributing to Titan-HFT

Thanks for your interest! This project's goal is to be **correct, readable, and honestly measured**. Contributions that improve any of those are welcome.

## Getting started

```sh
git clone https://github.com/hellpuffyt/Titan-HFT && cd Titan-HFT
cargo test
```

Look at [docs/good-first-issues.md](docs/good-first-issues.md) for starter tasks.

## Before opening a pull request

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
# if you touched src/spsc.rs or src/sync.rs:
RUSTFLAGS="--cfg loom" cargo test --release --test loom
cargo +nightly miri test --lib spsc
```

## Ground rules

- **Behavior changes need tests.** Changes to matching rules must update the reference model in `tests/properties.rs` and the rules section of the README.
- **Determinism is a contract.** If you change what events a command sequence produces, regenerate the golden journals (`titan run tests/fixtures/<name>.cmds --journal tests/fixtures/<name>.journal`) and explain why in the PR.
- **`unsafe` needs a `// SAFETY:` comment** that states the invariant relied on, and memory-ordering changes need an update to `docs/MEMORY_ORDERING.md`.
- **Performance claims need numbers.** Include the command, the machine, the before and after results, and the load conditions. Do not cite numbers from a different machine as a comparison.
- **No `unwrap`/`expect` in library code**; clippy enforces this outside tests.
- Keep pull requests focused; one logical change per PR.

## License

By contributing, you agree that your contributions are dual-licensed under MIT OR Apache-2.0, as described in the README, without any additional terms.
