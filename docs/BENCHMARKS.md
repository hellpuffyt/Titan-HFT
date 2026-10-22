# Benchmarks

Every number here was produced by the commands shown, on the machine described, on 2026-10-22. Nothing is extrapolated or taken from elsewhere. Numbers from another machine are not comparable to these unless the workload, build profile, and procedure are the same.

## Environment

| | |
|---|---|
| CPU | Apple M5, 10 logical cores (aarch64) |
| RAM | 16 GiB |
| OS | macOS 27.0.1 |
| Compiler | rustc 1.98.0 (88d9e12ae 2026-08-18) |
| Profile | `bench`: opt-level 3, `lto = "thin"`, `codegen-units = 1`, debug symbols level 1 |
| Load | **Not an idle machine.** Other builds were running; `uptime` load average was 10–19 during these runs. No core pinning (macOS has no user-facing affinity API). |

Because of that load, treat the absolute values as **rough indications** and focus on the method. Tail percentiles (p99.9, max) are especially sensitive to preemption by other processes. In a first run of the latency harness, made under heavier load with build output in a cloud-synced directory, engine throughput was 3.9–9.6 M cmd/s. The re-run below, with `CARGO_TARGET_DIR` moved out of the synced folder, gave 23–29 M cmd/s. That spread is the clearest evidence of how much a noisy host matters.

## Workload

`WorkloadConfig::default()` (`src/workload.rs`), seed 42:
- 30% cancels of a uniformly chosen previously sent order (some already filled → `unknown_order` rejects, as in real flow);
- 10% market orders; 60% limit orders;
- limit prices `mid ± U[0,50]` ticks, quantities `U[1,100]`.

At 1M commands this produces 622,273 trades. The book grows to about 36k resting orders over 1M commands. The book is preallocated for 2^20 orders, so it is never full.

## Procedure

`cargo bench --bench latency` (`benches/latency.rs`, custom harness):

1. Print machine/build metadata.
2. Measure the timer: 1M pairs of back-to-back `Instant::now()`.
3. **Engine per-command latency**: one untimed warm-up pass over the full workload, then 5 runs. Each run uses a fresh preallocated engine, times every `process()` call with `Instant`, and records into an HDR histogram (1 ns – 10 s, 3 significant digits, no auto-resize).
4. **Engine throughput**: 1 warm-up + 5 runs, timing only the whole loop (no per-op timers).
5. **SPSC round trip**: 1 warm-up + 5 runs of 1M ping-pongs between two threads over two queues (cap 1024). Reports the full round trip (two queue hops).
6. **Pipeline**: 1 warm-up + 5 runs of producer thread → SPSC (cap 4096) → engine thread.

An allocation counter (global allocator wrapper) reports allocations made during each timed engine run.

`cargo bench --bench queue` / `--bench engine`: Criterion 0.8 with default settings (3 s warm-up, 100 samples; the engine group uses 30 samples), reported as `[lower estimate, point estimate, upper estimate]`.

## Timer resolution: read this before reading percentiles

```text
timer overhead (now→now)     n=  1000000  p50=    41  p95=    42  p99=    42  p99.9=     84  max=   177791  (ns)
```

`Instant` on macOS ticks at about 24 MHz, so durations are quantized to multiples of ~41.7 ns. A per-command p50 of 42 ns means "one tick or less". Per-op percentiles below about 100 ns mostly measure the clock. The throughput numbers (no per-op timers) are the better estimate of mean cost.

## Results — latency harness (re-run, `CARGO_TARGET_DIR` outside the synced folder)

```text
# Titan-HFT latency report
machine: cpu=Apple M5 | logical_cores=10 | ram_gib=16 | os=macOS 27.0.1 | arch=aarch64
build:   rustc 1.98.0 (88d9e12ae 2026-08-18) | profile=optimized (bench profile: opt-level=3, lto=thin, codegen-units=1)
config:  runs=5 commands=1000000 roundtrips=1000000
timer overhead (now→now)     n=  1000000  p50=    41  p95=    42  p99=    42  p99.9=     84  max=   177791  (ns)

## Engine: per-command latency (single thread, CountingSink, workload seed=42 commands=1000000)
run 1                        n=  1000000  p50=    83  p95=   208  p99=   333  p99.9=    500  max=  4284415  (ns)
                             throughput=5.21 M cmd/s (incl. timer overhead)  trades=622273  allocations_during_run=0
run 2                        n=  1000000  p50=    83  p95=   208  p99=   334  p99.9=    500  max=  8658943  (ns)
                             throughput=5.12 M cmd/s (incl. timer overhead)  trades=622273  allocations_during_run=0
run 3                        n=  1000000  p50=    42  p95=   167  p99=   292  p99.9=    417  max=   470271  (ns)
                             throughput=7.59 M cmd/s (incl. timer overhead)  trades=622273  allocations_during_run=0
run 4                        n=  1000000  p50=    42  p95=   166  p99=   250  p99.9=    375  max=   648703  (ns)
                             throughput=9.57 M cmd/s (incl. timer overhead)  trades=622273  allocations_during_run=0
run 5                        n=  1000000  p50=    42  p95=    84  p99=   167  p99.9=    292  max=    43615  (ns)
                             throughput=11.24 M cmd/s (incl. timer overhead)  trades=622273  allocations_during_run=0
all runs                     n=  5000000  p50=    42  p95=   167  p99=   292  p99.9=    458  max=  8658943  (ns)

## Engine: throughput without per-command timers (5 runs + 1 warm-up)
run 1: 23.04 M cmd/s  (43.4 ns/cmd mean)  allocations_during_run=0
run 2: 25.99 M cmd/s  (38.5 ns/cmd mean)  allocations_during_run=0
run 3: 25.94 M cmd/s  (38.6 ns/cmd mean)  allocations_during_run=0
run 4: 28.61 M cmd/s  (35.0 ns/cmd mean)  allocations_during_run=0
run 5: 23.07 M cmd/s  (43.4 ns/cmd mean)  allocations_during_run=0

## SPSC queue: cross-thread round-trip (ping-pong over two queues, cap 1024; RTT includes both hops)
run 1 RTT                    n=  1000000  p50=   333  p95=   459  p99=   541  p99.9=  10959  max=   899071  (ns)
run 2 RTT                    n=  1000000  p50=   334  p95=   458  p99=   500  p99.9=   9631  max=   735743  (ns)
run 3 RTT                    n=  1000000  p50=   333  p95=   459  p99=   500  p99.9=   9463  max=   373247  (ns)
run 4 RTT                    n=  1000000  p50=   334  p95=   459  p99=   500  p99.9=  10007  max=  1270783  (ns)
run 5 RTT                    n=  1000000  p50=   333  p95=   500  p99=   542  p99.9=  11215  max=  6488063  (ns)
all runs RTT                 n=  5000000  p50=   333  p95=   459  p99=   541  p99.9=  10127  max=  6488063  (ns)

## Pipeline: producer thread -> SPSC (cap 4096) -> engine thread (5 runs + 1 warm-up)
run 1: 18.23 M cmd/s end-to-end  producer_full_spins=90926  final_hash=f1f1874b01cb0bc8
run 2: 17.95 M cmd/s end-to-end  producer_full_spins=119893  final_hash=f1f1874b01cb0bc8
run 3: 17.65 M cmd/s end-to-end  producer_full_spins=130682  final_hash=f1f1874b01cb0bc8
run 4: 17.60 M cmd/s end-to-end  producer_full_spins=191476  final_hash=f1f1874b01cb0bc8
run 5: 17.12 M cmd/s end-to-end  producer_full_spins=153823  final_hash=f1f1874b01cb0bc8
```

Observations:
- Every run had **zero allocations** in the timed loop, and all pipeline runs reached the same final hash (deterministic).
- Max latencies in the milliseconds are OS preemption on a busy machine, not engine work. They do not repeat at the same command index.
- Throughput with per-op timers (5–11 M cmd/s) is much lower than without (23–29 M cmd/s). Two `Instant::now()` calls cost more than a typical `process()` call, so per-op instrumentation dominates the measurement.
- The pipeline runs at about 17–18 M cmd/s. The engine thread is the bottleneck: the producer frequently finds the 4096-slot queue full.

## Results — Criterion

```text
engine/workload_100k_default_mix
                        time:   [2.7011 ms 2.7223 ms 2.7417 ms]
                        thrpt:  [36.474 Melem/s 36.734 Melem/s 37.023 Melem/s]
spsc/same_thread/push_pop_u64
                        time:   [3.6798 ns 3.6917 ns 3.7046 ns]
                        thrpt:  [269.93 Melem/s 270.88 Melem/s 271.75 Melem/s]
spsc/cross_thread/cap_64
                        time:   [5.5431 ms 5.9552 ms 6.3727 ms]
                        thrpt:  [15.692 Melem/s 16.792 Melem/s 18.041 Melem/s]
spsc/cross_thread/cap_1024
                        time:   [2.0332 ms 2.2815 ms 2.5535 ms]
                        thrpt:  [39.161 Melem/s 43.831 Melem/s 49.183 Melem/s]
spsc/cross_thread/cap_16384
                        time:   [993.66 µs 1.0960 ms 1.2012 ms]
                        thrpt:  [83.250 Melem/s 91.240 Melem/s 100.64 Melem/s]
```

- The engine group's 100k workload has a smaller book than the 1M runs, which helps cache behavior. That explains most of the gap between 36.7 M cmd/s here and 23–29 M cmd/s in the harness.
- `cross_thread` moves 100,000 `u64`s per sample. The timed region starts after the producer thread is spawned and includes the final `join`. Larger queues amortize cache-line transfers between cores, which is why throughput scales strongly with capacity.
- `same_thread` is a push immediately followed by a pop on one core, the best case with no coherence traffic.

## Allocation evidence

`cargo test --release --test alloc -- --nocapture`:

```text
measured 500000 commands, 621360 trades, book size 18053 -> 36401, allocations = 0
```

The counter is checked first by a sanity test (a `vec![1u8; 64]` must register at least one allocation). The claim is limited to what is measured: `MatchingEngine::process` with a non-allocating sink, after `MatchingEngine::new`, with the book within its configured capacity. `MatchingEngine::new` allocates all storage, and passing a `Vec<Event>` sink allocates when the `Vec` grows.

## Order-replace workloads (branch `feat/order-replace`, 2026-10-22)

Everything above this section describes **0.1.0** and is unchanged. This section adds replace-heavy workloads. Everything in it was measured on the same machine and toolchain, with the same profile and method (warm-up pass, 5 timed runs per workload, HDR histograms, allocation counter).

### Read this first: the machine was heavily loaded

Other builds were saturating the machine. The 1-minute load average was **≈ 50 on 10 cores** during harness sessions 1–3, about 40 → 11 across the Criterion runs, and about 10 during harness session 4. The absolute numbers below are **not stable** and must not be quoted as the engine's performance. The *spread* between repeated runs is reported for exactly that reason. In one workload, session 2's median was 6× lower than session 3's. Every timed run below had **0 allocations**; the harness asserts this and would exit non-zero otherwise.

### Workloads

All workloads use seed 42, 1M commands (harness) or 100k commands (Criterion), and the 0.1.0 base mix of 30% cancels and 10% market orders. Only `replace_pct` / `replace_mix` change. Limit orders fill the remainder, so a replace-heavy workload has fewer limit orders.

| Name | `replace_pct` | `replace_mix` |
|---|---|---|
| baseline (Criterion id `workload_100k_default_mix`, unchanged from 0.1.0) | 0 | — |
| replace 10% mixed | 10 | `Mixed` (50/50 same-price reduce / move) |
| replace 30% mixed | 30 | `Mixed` |
| replace 30% reduce-only | 30 | `ReduceOnly` |
| replace 30% move-only | 30 | `MoveOnly` |

**What the generator actually produces.** The generator remembers what it *sent*, not what has since filled, so many replaces (and cancels) target orders that are already gone and are rejected with `unknown_order`. The harness therefore classifies every command by its **outcome**: `limit`, `market`, `cancel` (successful), `replace_keep`, `replace_requeue`, or `rejected` (any rejected command). Measured counts per 1M-command run, from session 1:

| Workload | limit | market | cancel | replace_keep | replace_requeue | rejected |
|---|---|---|---|---|---|---|
| baseline | 600,248 | 100,083 | 35,883 | 0 | 0 | 263,786 |
| replace 10% mixed | 499,542 | 100,328 | 27,914 | 4,605 | 4,616 | 362,995 |
| replace 30% mixed | 299,659 | 102,086 | 33,561 | 14,589 | 18,954 | 531,151 |
| replace 30% reduce-only | 300,091 | 101,090 | 24,381 | 21,655 | 2,646 | 550,137 |
| replace 30% move-only | 299,696 | 102,994 | 33,354 | 162 | 32,724 | 531,070 |

A "reduce" is sometimes a requeue: the generator reduces relative to the quantity it sent, so after a partial fill the new quantity can be *larger* than what remains. A "move" occasionally keeps priority when the random new price equals the old one. Because the workloads differ this much in composition (half as many limit orders at 30%, twice as many cheap rejections), **throughput differences between workloads mostly reflect the command mix, not the cost of a replace.** Use the per-outcome latency rows to compare operation costs.

### Commands

Since this change, the harness runs the per-command and the untimed throughput passes for each workload in turn (the 0.1.0 output above came from the earlier layout, which measured only the baseline). The queue round-trip and pipeline sections are unchanged and still use the baseline workload.

```sh
uptime; cargo bench --locked --bench latency; uptime     # 4 sessions
uptime; cargo bench --locked --bench engine; uptime      # 3 invocations
```

Raw output of every run, including the load before and after each run and each workload section: [`bench-results/2026-10-22-replace/`](bench-results/2026-10-22-replace/). In those files, the `uptime` lines were reduced to time and load averages. In Criterion invocations 2 and 3, the second `time:`/`thrpt:` pair under each benchmark is Criterion's comparison with the previous invocation, not a new measurement. Because the harness sections are short (~1–4 s) and the 1-minute load average is a decaying average, the per-section "load before/after" values often repeat.

### Engine throughput without per-op timers (latency harness, M cmd/s, min / median / max of 5 runs)

| Workload | Session 1 (load ≈ 54→52) | Session 2 (≈ 52→53) | Session 3 (≈ 53→50) | Session 4 (≈ 11→10) |
|---|---|---|---|---|
| baseline | 12.70 / 14.80 / 16.53 | 13.22 / 14.27 / 15.10 | 10.82 / 15.20 / 17.11 | 17.59 / 21.15 / 25.26 |
| replace 10% mixed | 7.64 / 13.18 / 14.16 | **2.25 / 2.79 / 7.17** | 16.93 / 17.68 / 18.62 | 22.75 / 24.33 / 27.59 |
| replace 30% mixed | 18.45 / 19.19 / 21.01 | 12.47 / 19.60 / 20.42 | 17.18 / 23.30 / 25.19 | 26.13 / 27.23 / 30.91 |
| replace 30% reduce-only | 20.30 / 21.49 / 21.63 | 11.20 / 13.94 / 19.80 | 22.89 / 24.87 / 27.85 | 31.28 / 32.30 / 34.82 |
| replace 30% move-only | 13.99 / 19.90 / 20.85 | 16.80 / 18.46 / 21.85 | 23.23 / 24.28 / 25.76 | 29.34 / 30.70 / 30.80 |

- The session-2 "replace 10% mixed" collapse (2.79 vs 17.68 M cmd/s median in session 3, with an identical command stream processed by a deterministic engine) is host interference, not engine behavior.
- At lower load (session 4) the baseline median, 21.15 M cmd/s, is within reach of the 0.1.0 measurement of the same workload (23.0–28.6 M cmd/s at load 10–15, above). At load ≈ 50 it was 14–15 M cmd/s.

### Per-command latency by outcome (ns, p50 / p99, all 5 runs of a session combined)

The timer resolution is ~41.7 ns (see above), so values of 41–42 ns mean "at most one timer tick". The full p50/p95/p99/p99.9/max rows and per-run p50/p99 ranges are in the raw files.

| Workload | Outcome | S1 | S2 | S3 | S4 |
|---|---|---|---|---|---|
| baseline | limit | 83 / 250 | 84 / 250 | 84 / 291 | 83 / 209 |
| baseline | cancel | 291 / 500 | 292 / 500 | 292 / 542 | 209 / 458 |
| replace 10% mixed | replace_keep | 250 / 541 | 291 / 500 | 250 / 459 | 167 / 375 |
| replace 10% mixed | replace_requeue | 333 / 625 | 333 / 625 | 292 / 542 | 208 / 417 |
| replace 30% mixed | replace_keep | 42 / 167 | 42 / 208 | 42 / 125 | 42 / 84 |
| replace 30% mixed | replace_requeue | 125 / 291 | 125 / 292 | 84 / 250 | 83 / 208 |
| replace 30% reduce-only | replace_keep | 42 / 167 | 42 / 209 | 42 / 208 | 42 / 84 |
| replace 30% reduce-only | replace_requeue | 125 / 250 | 125 / 292 | 125 / 250 | 83 / 167 |
| replace 30% move-only | replace_requeue | 84 / 250 | 84 / 292 | 84 / 291 | 83 / 167 |

Session 1, the full rows for one workload (raw):

```text
## Engine workload: replace 30% mixed  (seed=42 commands=1000000 cancel=30% replace=30% Mixed market=10%)
by outcome, all 5 runs combined (count is per run):
  limit            n/run= 299659  p50=  125  p95=  208  p99=  250  p99.9=   458  max=  2316287  | per-run p50 84..125  p99 250..292 (ns)
  market           n/run= 102086  p50=   84  p95=  208  p99=  291  p99.9=   542  max=  1320959  | per-run p50 83..125  p99 250..292 (ns)
  cancel           n/run=  33561  p50=   42  p95=  125  p99=  208  p99.9=   417  max=   898047  | per-run p50 42..42  p99 208..208 (ns)
  replace_keep     n/run=  14589  p50=   42  p95=   84  p99=  167  p99.9=   500  max=   120383  | per-run p50 42..42  p99 166..208 (ns)
  replace_requeue  n/run=  18954  p50=  125  p95=  167  p99=  291  p99.9=   583  max=   297727  | per-run p50 84..125  p99 250..291 (ns)
  rejected         n/run= 531151  p50=   41  p95=   42  p99=   84  p99.9=   167  max=   565759  | per-run p50 41..41  p99 84..84 (ns)
throughput with per-op timers: min 6.40 / median 7.48 / max 7.81 M cmd/s  (max/min = 1.22, n=5)
throughput without timers: min 18.45 / median 19.19 / max 21.01 M cmd/s  (max/min = 1.14, n=5)
```

What the measurements support:
- **An in-place reduce (`replace_keep`) is the cheapest successful book operation measured.** In the 30% workloads it sat at the timer floor at p50 in every session. A requeue was 83–125 ns at p50 in the 30% workloads (two to three ticks), consistent with unlink + insert (+ matching when it crosses). In the 10% workload, both replace outcomes were slower (keep 167–291 ns, requeue 208–333 ns), in line with that workload's slower cancels; see the next point.
- **Both replace paths allocate nothing**, across every run of every workload (asserted), and in `tests/alloc.rs`.
- **Successful operations (cancel, replace_keep) are several times slower in the baseline and 10% workloads than in the 30% workloads** (cancel p50 ≈ 290 ns vs 42 ns). This was measured, but its cause has **not been investigated**. One hypothesis is that the baseline grows a larger resting book, which means more cache misses on the id index and the ladder. It is untested, and the book sizes were not recorded in these runs.

What they do **not** support: any absolute "Titan does N replaces/s" claim, or ranking workloads by throughput as if they differed only in replace cost.

### Criterion engine bench (3 invocations, 100k commands, 30 samples each)

| Benchmark | Inv. 1 (load 40→23) | Inv. 2 (22→13) | Inv. 3 (13→11) | max/min |
|---|---|---|---|---|
| `workload_100k_default_mix` (0.1.0 baseline id) | 28.71 | 31.24 | 29.52 | 1.09 |
| `workload_100k_replace10_mixed` | 30.65 | 35.40 | 34.78 | 1.15 |
| `workload_100k_replace30_mixed` | 27.22 | 39.66 | 30.43 | 1.46 |
| `workload_100k_replace30_reduce_only` | 31.54 | 42.44 | 30.22 | 1.40 |
| `workload_100k_replace30_move_only` | 33.57 | 39.75 | 30.20 | 1.32 |

Values are Criterion point estimates in M commands/s. The confidence intervals are in the raw files. The 0.1.0 baseline measured 36.7 M cmd/s under lighter load; run-to-run variation of up to 1.46× between invocations here means the two cannot be compared meaningfully.

## Reproducing

```sh
cargo bench --bench latency                 # includes the replace workloads
TITAN_RUNS=10 TITAN_COMMANDS=2000000 cargo bench --bench latency   # knobs
cargo bench --bench queue
cargo bench --bench engine
cargo test --release --test alloc -- --nocapture
```

For lower noise, run on an idle machine on AC power and close browsers and sync clients. If the checkout lives in a cloud-synced folder, set `CARGO_TARGET_DIR` outside it. On Linux, consider `taskset`/`isolcpus` and the `performance` governor; that path is not yet automated (see good-first-issues).
