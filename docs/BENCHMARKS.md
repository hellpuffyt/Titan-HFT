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

## Reproducing

```sh
cargo bench --bench latency
TITAN_RUNS=10 TITAN_COMMANDS=2000000 cargo bench --bench latency   # knobs
cargo bench --bench queue
cargo bench --bench engine
cargo test --release --test alloc -- --nocapture
```

For lower noise, run on an idle machine on AC power and close browsers and sync clients. If the checkout lives in a cloud-synced folder, set `CARGO_TARGET_DIR` outside it. On Linux, consider `taskset`/`isolcpus` and the `performance` governor; that path is not yet automated (see good-first-issues).
