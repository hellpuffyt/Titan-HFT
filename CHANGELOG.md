# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

The next release will be **0.2.0**: this section contains breaking API changes (under Cargo's 0.x rules, a minor bump is the breaking-change bump). `Cargo.toml` still says 0.1.0; the version is bumped at release time.

### Changed (breaking)
- `types::Command` gained the variant `Replace { id, price, qty }`. Exhaustive `match`es on `Command` no longer compile. **Migration:** add a `Command::Replace { .. }` arm, or a wildcard `_` arm (required anyway, see below).
- `types::Event` gained the variant `Replaced { id, price, qty, prev_qty, kept_priority }`. **Migration:** add an `Event::Replaced { .. }` arm or `_`.
- `workload::WorkloadConfig` gained the public fields `replace_pct: u64` and `replace_mix: ReplaceMix`. Struct literals that list every field no longer compile. **Migration:** end the literal with `..Default::default()` (the defaults `0` / `ReplaceMix::Mixed` reproduce 0.1.0 flows exactly).
- `Command`, `Event`, `RejectReason`, `CancelReason` (and the new `ReplaceMix`) are now `#[non_exhaustive]`. Downstream `match`es need a wildcard arm even if they list every current variant. **Rationale:** we are breaking these enums anyway in this release, and the engine is expected to grow new commands, events, and reject reasons (see `docs/good-first-issues.md`). With the attribute, those additions become non-breaking minor releases instead of a breaking release each time. Behavior is unchanged; inside the crate, matches stay exhaustive, and the `types::tests::variant_canary` test fails to compile when a variant is added. Structs (`BookConfig`, `WorkloadConfig`, `RestingOrder`, `CountingSink`, `PipelineReport`) are deliberately **not** marked: a `#[non_exhaustive]` struct cannot be built with a struct literal outside the crate, even with `..Default::default()`.
- `titan gen` now exits with code 2 if `cancel + replace + market` percentages exceed 100 (previously the generator silently produced a skewed mix).

### Added
- Order replacement: `Command::Replace { id, price, qty }`, journal syntax `R <id> <price> <qty>`, and event `RPL <id> <price> <qty> <prev_qty> keep|requeue`. A same-price reduction keeps time priority; a price change or quantity increase loses it and may cross immediately. Unknown, filled, or cancelled ids are rejected with `unknown_order`. Replace never allocates and never returns `book_full`.
- `workload::ReplaceMix` (`Mixed` default, `ReduceOnly`, `MoveOnly`) to select the kind of generated replaces.
- Benchmarks: replace-heavy workloads in the Criterion engine bench and the latency harness, with per-command-kind latency.
- Doc-tests showing downstream usage (wildcard arm, `..Default::default()`), and a `compile_fail` test proving `Event` is non-exhaustive.
- `WorkloadConfig::replace_pct` and `titan gen --replace-pct` (default 0; seeded flows from 0.1.0 are reproduced exactly, which a pinned digest test enforces).
- Tests: reference-model coverage of replace, one regression test per replace rule, a `replace.cmds` walkthrough fixture, a generated replace-heavy golden journal, a replace allocation test, and a pipeline stress case with replaces.
- `.editorconfig` and `docs/PRODUCTION_READINESS.md`.

## [0.1.0] - 2026-10-22

### Added
- Bounded wait-free SPSC ring buffer with cache-padded indices and cached remote indices; loom model tests and Miri runs.
- Price-ladder order book with intrusive FIFO levels, a non-empty-level bitset, a slab node pool, and an id index; all storage preallocated.
- Deterministic matching engine: limit, market (IOC), and cancel; price-time priority; maker-price execution; explicit reject reasons.
- Text journal with replay verification and first-divergence diagnostics.
- Seeded synthetic workload generator.
- Producer → SPSC → engine pipeline.
- `titan` CLI: `run`, `replay`, `gen`.
- Tests: unit, differential property tests against a reference model, stress/saturation, allocation counting, golden fixtures, malformed input, CLI exit codes.
- Benchmarks: Criterion (queue, engine) and an HDR-histogram latency harness with machine metadata.
- Documentation: architecture, memory ordering, benchmark methodology and results, limitations.
