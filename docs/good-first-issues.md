# Good first issues

Concrete, self-contained tasks for new contributors. Comment on (or open) a GitHub issue before starting so work isn't duplicated.

1. **Core pinning on Linux in the latency harness.** Add an optional `TITAN_PIN=1` that pins the producer and engine threads to distinct cores with `sched_setaffinity` (behind `cfg(target_os = "linux")`), and print the chosen cores in the metadata block. *Skills: Rust, a little libc.*

2. **Conditional replace.** Add an optional `expected_qty` to `Command::Replace` so a replace is rejected (new reason `stale_replace`) if the order's remaining quantity changed since the client last saw it. This closes the "replace races a partial fill" gap in `docs/LIMITATIONS.md`. Extend the reference model and the journal syntax. *Skills: Rust; touches engine, journal, tests.*

3. **Binary journal format.** Add a compact fixed-width binary encoding of commands and events next to the text one, with a round-trip property test and a `titan convert` subcommand. *Skills: Rust, serialization.*

4. **Self-trade prevention.** Add an optional `owner: u32` to orders and a "cancel resting" STP policy, with tests showing that the same owner never trades against itself. *Skills: Rust, matching rules.*

5. **Compare the id hasher.** Benchmark the custom splitmix64 hasher against `std`'s SipHash and `foldhash` on the engine workload, and document the results with the method from `docs/BENCHMARKS.md`. *Skills: benchmarking.*
