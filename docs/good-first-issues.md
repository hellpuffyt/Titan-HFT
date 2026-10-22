# Good first issues

Concrete, self-contained tasks for new contributors. Comment on (or open) a GitHub issue before starting so work isn't duplicated.

1. **Core pinning on Linux in the latency harness.** Add an optional `TITAN_PIN=1` that pins the producer and engine threads to distinct cores with `sched_setaffinity` (behind `cfg(target_os = "linux")`), and print the chosen cores in the metadata block. *Skills: Rust, a little libc.*

2. **Order modify (quantity decrease keeps priority).** Add `Command::Reduce { id, by }`, which decreases a resting order's quantity in place without losing its queue position. Add journal syntax (`R <id> <qty>`), extend the reference model in `tests/properties.rs`, and document the rule. *Skills: Rust; touches engine, journal, tests.*

3. **Binary journal format.** Add a compact fixed-width binary encoding of commands and events next to the text one, with a round-trip property test and a `titan convert` subcommand. *Skills: Rust, serialization.*

4. **Self-trade prevention.** Add an optional `owner: u32` to orders and a "cancel resting" STP policy, with tests showing that the same owner never trades against itself. *Skills: Rust, matching rules.*

5. **Compare the id hasher.** Benchmark the custom splitmix64 hasher against `std`'s SipHash and `foldhash` on the engine workload, and document the results with the method from `docs/BENCHMARKS.md`. *Skills: benchmarking.*
