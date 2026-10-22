# Known limitations

Titan-HFT is educational and research software. These are known, deliberate, or not-yet-addressed gaps.

## Scope
- **Not a trading system.** There is no networking, exchange protocol, authentication, risk management, persistence, or recovery.
- **Single instrument per engine.** Multiple books would need one engine per symbol, or a symbol field in `Command`.
- **No order modify/amend.** Cancel and re-enter instead (this loses time priority, as most venues would for a price change).
- **No self-trade prevention.** There is no account/owner concept.
- **Order ids are unique only among resting orders.** A filled or cancelled id may be reused. Real venues usually require ids to be unique for the whole session.

## Behavior
- **Conservative `book_full`.** A limit order is rejected when the node pool is full, even if it would have filled completely without resting. This is deliberate: it keeps the engine from half-executing an order it then cannot rest. Size `max_orders` generously.
- **Fixed price band.** Prices outside `[min_price, max_price]` are rejected, and the band may span at most 2^22 ticks. Instruments with unbounded price ranges would need a sparse level structure, which brings back allocation or a bounded-capacity map.
- **Quantity cap.** Order quantity is at most 2^32 lots, so level totals cannot overflow `u64`.
- **Best-level rescan.** When the best level empties, finding the next one costs O(L/64) bitset words in the worst case (L = levels in the band). This is typically one or two words; a near-empty, very wide band is the worst case.

## Security
- **Not HashDoS-resistant.** The id index uses an unkeyed splitmix64 hasher. Do not expose the engine to adversarial ids without switching to a keyed hasher.
- **Journal integrity is not authenticity.** FNV-1a detects accidental divergence. It does not detect deliberate tampering by someone who can recompute the hash.

## Measurement
- **Measured on one machine.** All published numbers come from one Apple M5 laptop under background load. Results on other CPUs and operating systems will differ.
- **Timer resolution.** macOS `Instant` has ~41.7 ns granularity, so per-op percentiles near that value are quantized.
- **No core pinning or frequency control.** macOS offers no user-facing CPU affinity, and the harness does not yet pin threads on Linux.
- **Loom and Miri are bounded.** They explore many interleavings but not all. Treat them as strong evidence, not proof.

## Not verified here
- Windows and Linux CI builds run in GitHub Actions. Their status is shown by the badge; local verification was on macOS only.
