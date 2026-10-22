## What and why

<!-- One logical change. Link the issue if there is one. -->

## Checklist

- [ ] `cargo fmt --all` and `cargo clippy --all-targets --all-features -- -D warnings` are clean
- [ ] `cargo test --all-features` passes
- [ ] Matching-rule changes update the reference model (`tests/properties.rs`) and the README
- [ ] Changes to `src/spsc.rs` / `src/sync.rs`: loom and Miri pass, and `docs/MEMORY_ORDERING.md` is updated
- [ ] Performance claims include command, machine, before/after numbers, and load conditions
- [ ] Golden journals regenerated only if event output intentionally changed (explained above)
