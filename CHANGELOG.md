# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Order replacement: `Command::Replace { id, price, qty }`, journal syntax `R <id> <price> <qty>`, and event `RPL <id> <price> <qty> <prev_qty> keep|requeue`. A same-price reduction keeps time priority; a price change or quantity increase loses it and may cross immediately. Unknown, filled, or cancelled ids are rejected with `unknown_order`. Replace never allocates and never returns `book_full`.
- `WorkloadConfig::replace_pct` and `titan gen --replace-pct` (default 0; seeded flows from 0.1.0 are reproduced exactly, which a pinned digest test enforces).
- Tests: reference-model coverage of replace, one regression test per replace rule, a `replace.cmds` walkthrough fixture, a generated replace-heavy golden journal, a replace allocation test, and a pipeline stress case with replaces.
- `.editorconfig` and `docs/PRODUCTION_READINESS.md`.

### Changed
- `titan gen` rejects percentage mixes that exceed 100%.

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
