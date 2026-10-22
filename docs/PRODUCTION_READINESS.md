# Production-readiness evidence

This file records **evidence**, not a verdict. The classification at the end is a *proposal* by the author of the change. The maintainer decides after independent verification.

| | |
|---|---|
| Tested commit | `30fb5f4` (branch `feat/order-replace`, base `1cfd689` = `main`). Earlier evidence for `ce49c51` / `7ef3907` is kept where marked. |
| Date | 2026-10-22 |
| Documented scope | Educational / research matching engine (see README). **Not** a trading system: production readiness can only ever mean "for its documented scope". |

## Scope and architecture summary

A single-crate Rust library plus the `titan` CLI:
- a bounded, wait-free SPSC ring buffer (the only `unsafe` code);
- a preallocated price-ladder order book with intrusive FIFO levels;
- a deterministic price-time-priority matching engine (limit, market/IOC, cancel, replace);
- a replayable text journal;
- a seeded synthetic workload generator;
- a producer → queue → engine pipeline.

Details: [ARCHITECTURE.md](ARCHITECTURE.md) and [MEMORY_ORDERING.md](MEMORY_ORDERING.md).

## Supported platforms and prerequisites

- Rust ≥ 1.86 (edition 2024; the library alone needs 1.85, the Criterion dev-dependency needs 1.86). No system libraries.
- Verified locally on macOS 27.0.1 / Apple M5 (aarch64).
- Linux x86-64 and Windows x86-64 are covered **only by CI**. Per the maintainer, CI was green on `1cfd689` and on `7ef3907` (PR #3). CI has not yet run on `30fb5f4`.
- Loom needs `RUSTFLAGS="--cfg loom"`. Miri needs a nightly toolchain with `miri` and `rust-src`.

## Clean-clone setup (exact commands)

```sh
git clone https://github.com/hellpuffyt/Titan-HFT && cd Titan-HFT
cargo test --release
cargo run --release --bin titan -- gen --seed 42 --count 5000 --out flow.cmds
cargo run --release --bin titan -- run flow.cmds --quiet --journal flow.journal
cargo run --release --bin titan -- replay flow.journal
```

Clean-clone runs (`git clone` into a scratch directory, `CARGO_TARGET_DIR` outside the clone) were made at `ce49c51` (every row below) and at `30fb5f4` (tests, `gen`, `run`, `replay flow.journal`, and `git status`; same results except the test count). `c5c0d4d` was also checked alone: 56 passed, 0 failed, 0 ignored. Both runs followed the steps above literally:

| Step | Exit | Actual result |
|---|---|---|
| `cargo test --release --locked` | 0 | 59 passed, 0 failed, 0 ignored at `30fb5f4` (53 at `ce49c51`) |
| `titan gen --seed 42 --count 5000 --out flow.cmds` | 0 | file written |
| `titan run flow.cmds --quiet --journal flow.journal` | 0 | `hash 62d641d8087e8866` (same as README and as 0.1.0) |
| `titan replay flow.journal` | 0 | `OK: 5000 commands, 8051 events reproduced exactly; final hash 62d641d8087e8866` |
| `titan replay tests/fixtures/seed7_replace_3k.journal` | 0 | `OK: 3000 commands, 4366 events reproduced exactly; final hash 549407ba228690e7` |
| `git status --short` afterwards | — | clean (quick-start outputs are gitignored) |

## Commands executed on `30fb5f4` and their outcomes

Run on the development machine. Each line is the real exit code.

| Command | Exit | Outcome |
|---|---|---|
| `cargo fmt --all --check` | 0 | clean |
| `cargo clippy --all-targets --all-features --locked -- -D warnings` | 0 | no warnings |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features --locked` | 0 | no warnings |
| `cargo test --all-features --locked` | 0 | **59 passed, 0 failed, 0 ignored** (38 on 0.1.0; 53 on `ce49c51`; +6 here, including 3 doc-tests, one of them `compile_fail`) |
| `cargo bench --no-run --locked` | 0 | all three benches compile |
| `cargo deny check` | 0 | advisories ok, bans ok, licenses ok, sources ok |
| `cargo audit` (advisory DB with 1348 advisories, fetched 2026-10-22) | 0 | 0 vulnerabilities in 101 crates (includes dev-dependencies; the only runtime dependency is `thiserror`) |
| `RUSTFLAGS="--cfg loom" cargo test --release --test loom --locked` | 0 | 2 passed (run on `ce49c51`; `src/spsc.rs` and `src/sync.rs` are unchanged since `1cfd689`) |
| `cargo +nightly miri test --lib spsc --locked` | 0 | 7 passed (same note) |
| `cargo bench --locked --bench latency` ×4, `--bench engine` ×3 | 0 | see Performance; every timed engine run asserted 0 allocations |

No test is skipped or environment-gated: there are no `#[ignore]` tests. The loom tests compile only under `--cfg loom` and are run explicitly, as shown above.

## Test results by category

| Category | Where | Result |
|---|---|---|
| Unit | `src/*.rs` (36 tests) | pass |
| Differential property (engine vs. naive reference model, 512 cases × 2 properties per run; limit/market/cancel/replace) | `tests/properties.rs` | pass |
| Concurrency / saturation (queue caps 1–4096; pipeline with replaces; 2M items through 8 slots; `Arc` leak/double-free) | `tests/stress.rs` | pass |
| Allocation (counting global allocator) | `tests/alloc.rs` | 0 allocations: 500k commands / 621,360 trades; 500k commands with 100,295 replaces; reduce-only and move-only 30% mixes; cold start |
| API / semver (downstream usage) | doc-tests on `Event`, `WorkloadConfig`; `compile_fail` on `Event`; `types::tests::variant_canary` | pass; the `compile_fail` test was verified to fail when `#[non_exhaustive]` is removed |
| Golden fixtures / replay / tampering / malformed input / CLI exit codes | `tests/fixtures.rs` | pass |
| Model checking | `tests/loom.rs` (loom) | pass; fails as expected if the `tail` store is weakened to `Relaxed` |
| UB detection | Miri on `spsc` tests | pass |
| E2E | the clean-clone quick start above (CLI end to end) | pass |

**Mutation checks made during development** (each was reverted; the suite failed as required):
1. a fill that splits quantity incorrectly → property test failed;
2. a replace that keeps priority on a quantity increase → property test failed and shrank to a minimal sequence;
3. a requeued replace that skips matching → property test failed;
4. a `Relaxed` producer `tail` store → loom reported a causality violation.

## Security pass

| Check | Result |
|---|---|
| Secrets / private paths in tracked files | `git grep` for token patterns, private keys, local paths: no matches. The maintainer's pre-publication hygiene gate (every commit in history: filenames, blobs > 1 MiB, secret patterns) is run on the commit that adds this file; its result is reported in the pull request. |
| Dependency audit | `cargo audit` 0 vulnerabilities; `cargo deny` licenses/sources/bans ok |
| Untrusted input | Command and journal parsers reject malformed lines with line numbers and exit code 2 (fixtures in `tests/fixtures/malformed/`). Quantities are capped at 2^32 so level totals cannot overflow. The price band and the pool size are bounded. |
| Injection / path traversal / SSRF / authz | Not applicable: no network, no shell, no auth. The CLI reads and writes only the paths the user passes. |
| Sensitive logging | None: the engine logs nothing. |
| Unsafe defaults | No listeners or network binding at all. |
| Known non-goals | Unkeyed id hasher (not HashDoS-resistant); FNV-1a journal hash (not tamper-proof). Both documented in `LIMITATIONS.md` and `SECURITY.md`. |

## CI status

- `main` @ `1cfd689`: **all jobs green**. The jobs are lint (fmt, clippy, docs), test on ubuntu-latest, macos-latest, and windows-latest, loom, Miri, and cargo-deny. The maintainer verified this through the GitHub check-runs API; this author did not observe it directly.
- `feat/order-replace` @ `7ef3907` (PR #3): **all jobs green**, per the maintainer (lint, deny, loom, miri, test ubuntu/macos/windows). Not observed directly by this author.
- `feat/order-replace` @ `30fb5f4`: **not yet run** (not pushed).
- Portable checks (lint, tests, deny) are separate jobs from the toolchain-specific ones (loom, Miri on nightly). No job needs privileges, hardware, or external services. Workflow permissions are `contents: read`, and every action is pinned to a commit SHA.

## Performance methodology

See [BENCHMARKS.md](BENCHMARKS.md): warm-up, 5 repeated runs, HDR histograms (p50/p95/p99/p99.9/max), the timer's own resolution reported alongside, allocation counts, machine metadata, and the exact commands. Replace workloads (10%/30% mixed, 30% reduce-only, 30% move-only) were measured on `30fb5f4`: 4 harness sessions × 5 runs and 3 Criterion invocations. The machine was **heavily loaded** (1-minute load average ≈ 50 on 10 cores for three sessions, ≈ 10 for one), and the same workload varied up to 6× between sessions. The results are therefore reported as spreads with raw output (`docs/bench-results/2026-10-22-replace/`), not as stable figures. What the data supports: both replace paths allocate nothing; an in-place reduce is at the timer floor (≈ 42 ns) at p50 in the 30% workloads; a requeue is 83–125 ns at p50 there. Throughput differences between workloads mostly reflect command mix (many generated replaces target filled orders and are cheap rejections), not replace cost. Cancels being slower in the baseline and 10% workloads has not been investigated.

## Bugs found and fixed

| Bug | Fix | Regression test |
|---|---|---|
| (0.1.0) Very large quantities could overflow a level's `u64` total | `MAX_ORDER_QTY = 2^32`, reject `qty_too_large` | `engine::tests::rejections_change_nothing`; property-test strategy emits oversize quantities |
| (this change) First draft of the replace fixture tested rules 5/6 against an already-filled order, so they hit `unknown_order` instead of the intended reasons | Fixture reworked to target a live order | `fixtures::replace_fixture_shows_every_rule` asserts each rule's exact output |

No expected values from 0.1.0 were changed. The default seeded flow is pinned by `workload::tests::replace_pct_zero_reproduces_the_original_flow` (digest computed from the `1cfd689` generator), and the 0.1.0 golden journals replay byte-for-byte.

## Known limitations and unresolved risks

See [LIMITATIONS.md](LIMITATIONS.md). Most relevant: a single instrument; replace is not conditional (a replace racing a partial fill can increase exposure); no self-trade prevention; the band is limited to 2^22 ticks; benchmarks come from one machine; loom and Miri explore a bounded set of executions.

## External requirements for further verification

- GitHub Actions run of `30fb5f4` (Linux, Windows, nightly Miri). No secrets or paid services needed.
- An idle Linux host with core isolation, for publication-quality latency numbers (optional).

## Acceptance checklist

| # | Item | Expected behavior | Command / test | Result | Actual result | Reference |
|---|---|---|---|---|---|---|
| 1 | Builds | compiles on stable ≥ 1.86 | `cargo build --locked` (via `cargo test`) | PASS | built in clean clone | `Cargo.toml` |
| 2 | Lint | fmt, clippy `-D warnings`, rustdoc `-D warnings` clean | see commands table | PASS | exit 0 ×3 | `.github/workflows/ci.yml` |
| 3 | Price-time priority | better price first, FIFO at equal price, maker price | `engine::tests::price_then_time_priority`, property test | PASS | ok | `src/engine.rs` |
| 4 | Market = IOC | remainder cancelled, never rests | `engine::tests::market_is_ioc` | PASS | ok | `src/engine.rs` |
| 5 | Validation | every reject leaves the book unchanged | `engine::tests::rejections_change_nothing`, `bad_tick_rejected`, `book_full_rejects_new_limits` | PASS | ok | `src/engine.rs` |
| 6 | Replace: reduce keeps priority | same price, qty ≤ remaining → `RPL … keep`, position unchanged | `engine::tests::replace_reduce_same_price_keeps_priority`, `…same_qty_is_a_priority_keeping_no_op` | PASS | ok | `MatchingEngine::replace`, `OrderBook::reduce` |
| 7 | Replace: increase loses priority | → back of level | `engine::tests::replace_qty_increase_loses_priority` | PASS | ok | same |
| 8 | Replace: price change loses priority, may cross | requeue; trades as taker | `…price_change_loses_priority_even_when_reducing`, `…into_a_cross_matches_immediately_as_taker`, `…that_fully_fills_leaves_nothing_resting` | PASS | ok | same |
| 9 | Replace: unknown/filled/cancelled id | `unknown_order` | `…fully_fills…`, `replace_of_cancelled_order_is_unknown`, rejections test | PASS | ok | same |
| 10 | Replace: invalid values | `zero_qty`/`qty_too_large`/`bad_tick`/`price_out_of_band`, order untouched | `replace_rejections_leave_the_order_untouched` | PASS | ok | same |
| 11 | Replace never `book_full` | requeue works with a full pool | `replace_requeue_works_when_the_pool_is_full` | PASS | ok | same |
| 12 | Replace allocation-free | 0 allocations | `alloc::steady_state_matching_with_replaces_does_not_allocate` | PASS | 0 allocs / 100,295 replaces | `tests/alloc.rs` |
| 13 | Matches reference model incl. replace | identical event streams; invariants hold | `properties::engine_matches_reference_model` | PASS | 512 cases ok | `tests/properties.rs` |
| 14 | Determinism | same input → same events + hash | `properties::same_input_same_output`, golden journals | PASS | ok | `src/journal.rs` |
| 15 | Replay detects divergence | exit 1 + first divergent command | `fixtures::tampered_journal_is_detected_at_the_right_command`, CLI test | PASS | ok | `Journal::verify` |
| 16 | Malformed input | line-numbered error, exit 2 | `fixtures::malformed_command_files_are_rejected_with_line_numbers`, journal unit tests | PASS | ok | `src/journal.rs` |
| 17 | Backward compatibility | 0.1.0 flows/journals unchanged | `workload::…reproduces_the_original_flow`, golden byte-for-byte test, README hash | PASS | `62d641d8087e8866` | `src/workload.rs` |
| 18 | Queue memory safety | no UB / data race | Miri (7), loom (2), stress | PASS | ok | `src/spsc.rs` |
| 19 | Concurrency equivalence | pipeline == direct | `stress::pipeline_equals_direct_under_saturation`, `…_with_replaces` | PASS | ok | `src/pipeline.rs` |
| 20 | Quick start | README commands succeed in a clean clone | clean-clone table above | PASS | matches README | `README.md` |
| 21 | Dependency hygiene | no vulns, allowed licenses | `cargo deny check`, `cargo audit` | PASS | 0 vulns | `deny.toml` |
| 22 | CI on this commit (`30fb5f4`) | all jobs green | GitHub Actions | NOT RUN | not pushed yet (green on `7ef3907` per maintainer) | `.github/workflows/ci.yml` |
| 23 | Cross-platform | Linux/Windows tests pass | CI matrix | PASS on `1cfd689` and `7ef3907` | per maintainer's check-run verification | CI |
| 24 | Semver documented | breaking changes listed with migration notes; next release 0.2.0 | `CHANGELOG.md` [Unreleased] | PASS | 4 breaking items + 1 behavior change listed | `CHANGELOG.md` |
| 25 | Public enums extensible | downstream exhaustive match fails to compile; wildcard usage compiles | doc-tests on `types::Event` (incl. `compile_fail,E0004`) | PASS | ok; mutation-checked | `src/types.rs` |
| 26 | Replace benchmarks | replace mixes measured, per-outcome latency, ≥ 3 runs, load recorded, 0 allocations | `cargo bench --bench latency`, `--bench engine` | PASS (measured; not stable) | 4 + 3 sessions, 0 allocations in every timed run | `benches/`, `docs/BENCHMARKS.md` |

## Classification proposal (maintainer decides)

**Proposed: "Partially Verified".** Reason: every local check and the clean-clone runs pass on `30fb5f4`, but CI has not yet run on this exact commit (it was green on `7ef3907`); and the benchmark evidence comes from a heavily loaded host, so it supports only relative and allocation claims, not absolute performance. If CI is green on this branch, the evidence would support **"Production-Ready for Its Documented Scope"**, where the documented scope is an *educational and research engine*, not a trading system.
