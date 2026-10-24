//! Robustness audit: the journal/command parser against hostile text, and the order
//! lifecycle invariants under long, adversarial random workloads.
//!
//! Every check here is a property of the documented behaviour (README "Matching rules",
//! `docs/LIMITATIONS.md`); nothing tests an undocumented feature.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;

use proptest::prelude::*;
use titan_hft::book::BookConfig;
use titan_hft::engine::MatchingEngine;
use titan_hft::journal::{self, Journal};
use titan_hft::types::{CancelReason, Command, Event, MAX_ORDER_QTY, RejectReason, Side};
use titan_hft::workload::{SplitMix64, WorkloadConfig};

fn cfg(tick: u64, min: u64, max: u64, max_orders: u32) -> BookConfig {
    BookConfig {
        tick,
        min_price: min,
        max_price: max,
        max_orders,
    }
}

fn run(e: &mut MatchingEngine, cmd: Command) -> Vec<Event> {
    let mut v = Vec::new();
    e.process(cmd, &mut v);
    v
}

/// A valid journal used as the seed for mutation tests.
fn sample_journal() -> String {
    let cmds = journal::parse_commands(
        "L S 1 100 5\nL S 2 101 5\nL B 3 101 7\nR 2 102 9\nM B 4 20\nC 1\nC 2\nL B 1 99 1\n",
    )
    .unwrap();
    Journal::record(cfg(1, 1, 1000, 64), &cmds)
        .unwrap()
        .to_text()
}

// ---------------------------------------------------------------------------------
// Parser robustness
// ---------------------------------------------------------------------------------

/// Parsing must return `Ok` or a typed `JournalError` whose line number points inside
/// the input (or 0 = whole file). It must never panic.
fn assert_parse_is_total(text: &str) {
    let lines = text.lines().count();
    if let Err(e) = Journal::parse(text) {
        assert!(
            e.line <= lines,
            "line {} beyond input ({lines} lines)",
            e.line
        );
        assert!(!e.msg.is_empty());
    }
    if let Err(e) = journal::parse_commands(text) {
        assert!(e.line >= 1 && e.line <= lines, "bad line {}", e.line);
    }
}

#[test]
fn truncation_at_every_byte_never_panics_and_only_complete_journals_parse() {
    let text = sample_journal();
    let full = Journal::parse(&text).unwrap();
    for cut in 0..=text.len() {
        let Some(prefix) = text.get(..cut) else {
            continue; // not a char boundary (ASCII-only here, so never)
        };
        assert_parse_is_total(prefix);
        if let Ok(j) = Journal::parse(prefix) {
            // A prefix parses only once the `hash` line is present; it may then lack
            // the final newline or the last hex digits of the hash. Anything accepted
            // is either the full journal or caught by verify().
            if j != full {
                assert!(j.verify().is_err(), "truncated journal verified at {cut}");
            }
        }
    }
}

#[test]
fn crlf_line_endings_and_trailing_whitespace_are_accepted() {
    let text = sample_journal();
    let crlf = text.replace('\n', "\r\n");
    let j = Journal::parse(&crlf).unwrap();
    assert_eq!(
        j.verify().unwrap(),
        Journal::parse(&text).unwrap().final_hash
    );
    let padded: String = text.lines().map(|l| format!("  {l}\t \n")).collect();
    assert!(Journal::parse(&padded).unwrap().verify().is_ok());
    // Bare CR is not a line ending: the whole file is one unrecognized line.
    let cr = text.replace('\n', "\r");
    let e = Journal::parse(&cr).unwrap_err();
    assert_eq!(e.line, 1);
}

#[test]
fn extreme_numbers_are_typed_errors_or_engine_rejections() {
    // u64::MAX parses; one more does not.
    let max = u64::MAX;
    assert_eq!(
        journal::parse_command(&format!("L B {max} {max} {max}"), 1).unwrap(),
        Command::Limit {
            id: max,
            side: Side::Buy,
            price: max,
            qty: max
        }
    );
    for bad in [
        "L B 18446744073709551616 1 1",
        "L B 1 18446744073709551616 1",
        "L B 1 1 18446744073709551616",
        "L B 1 1 99999999999999999999999999999999999999999999999999",
        "M S 1 -1",
        "C 0x10",
        "C 1e3",
        "C 1.0",
        "C ",
        "R 1 2",
    ] {
        let e = journal::parse_command(bad, 7).unwrap_err();
        assert_eq!(e.line, 7, "{bad}");
    }
    // Values that parse but are out of range are engine rejections, never panics or
    // overflow, and change nothing.
    let mut e = MatchingEngine::new(cfg(5, 5, 1_000, 8)).unwrap();
    run(
        &mut e,
        Command::Limit {
            id: 1,
            side: Side::Sell,
            price: 500,
            qty: 3,
        },
    );
    let h = e.book().state_hash();
    for (cmd, reason) in [
        (
            Command::Limit {
                id: max,
                side: Side::Buy,
                price: max,
                qty: max,
            },
            RejectReason::QtyTooLarge,
        ),
        (
            Command::Limit {
                id: 2,
                side: Side::Buy,
                price: max - (max % 5),
                qty: 1,
            },
            RejectReason::PriceOutOfBand,
        ),
        (
            Command::Limit {
                id: 2,
                side: Side::Buy,
                price: max,
                qty: 1,
            },
            RejectReason::PriceOutOfBand, // u64::MAX is a multiple of 5
        ),
        (
            Command::Limit {
                id: 2,
                side: Side::Buy,
                price: 0,
                qty: 1,
            },
            RejectReason::PriceOutOfBand,
        ),
        (
            Command::Limit {
                id: 2,
                side: Side::Buy,
                price: 1_005,
                qty: 1,
            },
            RejectReason::PriceOutOfBand,
        ),
        (
            Command::Limit {
                id: 2,
                side: Side::Buy,
                price: 501,
                qty: 1,
            },
            RejectReason::BadTick,
        ),
        (
            Command::Replace {
                id: 1,
                price: max,
                qty: MAX_ORDER_QTY,
            },
            RejectReason::PriceOutOfBand,
        ),
        (
            Command::Replace {
                id: 1,
                price: 500,
                qty: MAX_ORDER_QTY + 1,
            },
            RejectReason::QtyTooLarge,
        ),
        (
            Command::Market {
                id: 9,
                side: Side::Buy,
                qty: max,
            },
            RejectReason::QtyTooLarge,
        ),
        (Command::Cancel { id: max }, RejectReason::UnknownOrder),
    ] {
        let id = cmd.id();
        assert_eq!(
            run(&mut e, cmd),
            vec![Event::Rejected { id, reason }],
            "{cmd}"
        );
        assert_eq!(e.book().state_hash(), h, "{cmd} changed state");
    }
    // The largest legal order fills against the smallest without overflow.
    let ev = run(
        &mut e,
        Command::Limit {
            id: max,
            side: Side::Buy,
            price: 1_000,
            qty: MAX_ORDER_QTY,
        },
    );
    assert_eq!(
        ev,
        vec![
            Event::Accepted { id: max },
            Event::Trade {
                taker: max,
                maker: 1,
                price: 500,
                qty: 3
            }
        ]
    );
    assert_eq!(e.book().best_bid(), Some((1_000, MAX_ORDER_QTY - 3)));
}

#[test]
fn unicode_and_control_characters_are_rejected_cleanly() {
    for bad in [
        "L B ١ 100 5",           // Arabic-Indic digit
        "L B １ 100 5",          // full-width digit
        "Ｌ B 1 100 5",          // full-width L
        "L Б 1 100 5",           // Cyrillic Be
        "L B 1 100 5 \u{200b}",  // zero-width space is not whitespace
        "L B 1\u{0} 100 5",      // NUL
        "\u{feff}L B 1 100 5",   // BOM
        "L B 1 100 5\u{1b}[31m", // ANSI escape
        "C \u{10ffff}",
    ] {
        assert!(journal::parse_command(bad, 1).is_err(), "{bad:?} accepted");
        assert_parse_is_total(bad);
    }
    // Unicode whitespace (here U+3000 and NBSP) is whitespace to `split_whitespace`;
    // the command is unambiguous, so accepting it is fine — it must not panic.
    assert!(journal::parse_command("L\u{3000}B\u{a0}1 100 5", 1).is_ok());
}

#[test]
fn duplicate_config_keys_are_rejected() {
    // Before the audit, a later key silently overrode an earlier one, so a journal
    // could declare two different tick sizes and replay under the last.
    for line in [
        "config tick=1 tick=5 min=5 max=100 max_orders=4",
        "config tick=1 min=1 min=2 max=100 max_orders=4",
        "config tick=1 min=1 max=100 max=200 max_orders=4",
        "config tick=1 min=1 max=100 max_orders=4 max_orders=8",
    ] {
        let text = format!("titan-journal v1\n{line}\nhash 0\n");
        let e = Journal::parse(&text).unwrap_err();
        assert_eq!(e.line, 2, "{line}");
        assert!(e.msg.contains("duplicate"), "{line}: {}", e.msg);
    }
}

#[test]
fn structural_errors_point_at_the_offending_line() {
    let ok = sample_journal();
    let lines: Vec<&str> = ok.lines().collect();
    // Swap a command line for each kind of garbage and check the reported line.
    for garbage in [
        "> L B",
        "< TRADE 1 2 3",
        "< CXL 1 2 maybe",
        "< REJ 1 not_a_reason",
        "?? what",
        "hash xyz",
        "> Z 1",
        "< ",
        ">",
    ] {
        let mut l = lines.clone();
        l[4] = garbage;
        let text = l.join("\n");
        let e = Journal::parse(&text).unwrap_err();
        assert_eq!(e.line, 5, "{garbage}: {e}");
    }
    // Content after the hash line, an event before any command, a second header.
    let after = format!("{ok}> C 1\n");
    assert!(
        Journal::parse(&after)
            .unwrap_err()
            .msg
            .contains("after final")
    );
    let mut l = lines.clone();
    l.insert(2, "< ACK 1");
    assert!(
        Journal::parse(&l.join("\n"))
            .unwrap_err()
            .msg
            .contains("before any command")
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2_000, ..ProptestConfig::default() })]

    /// Arbitrary text, including arbitrary Unicode, never panics either parser.
    #[test]
    fn arbitrary_text_never_panics(s in "\\PC{0,200}") {
        assert_parse_is_total(&s);
        for l in s.lines() {
            let _ = journal::parse_command(l, 1);
            let _ = journal::parse_event(l, 1);
        }
    }

    /// Token soup drawn from the real grammar (keywords, numbers at the edges of u64,
    /// separators) finds deeper paths than random text.
    #[test]
    fn grammar_token_soup_never_panics(
        toks in prop::collection::vec(prop_oneof![
            Just(">".to_string()), Just("<".to_string()), Just("L".to_string()),
            Just("M".to_string()), Just("C".to_string()), Just("R".to_string()),
            Just("B".to_string()), Just("S".to_string()), Just("ACK".to_string()),
            Just("REJ".to_string()), Just("TRADE".to_string()), Just("CXL".to_string()),
            Just("RPL".to_string()), Just("keep".to_string()), Just("ioc".to_string()),
            Just("hash".to_string()), Just("config".to_string()),
            Just("titan-journal v1".to_string()), Just("tick=1".to_string()),
            Just("min=1".to_string()), Just("max=9".to_string()),
            Just("max_orders=2".to_string()), Just("\n".to_string()),
            Just("\r\n".to_string()), Just("#".to_string()),
            Just(u64::MAX.to_string()), Just("18446744073709551616".to_string()),
            Just("-1".to_string()), Just("+1".to_string()), Just("0".to_string()),
            any::<u64>().prop_map(|n| n.to_string()),
            "[0-9a-f]{1,20}",
        ], 0..60)
    ) {
        let text = toks.join(" ");
        assert_parse_is_total(&text);
        if let Ok(j) = Journal::parse(&text) {
            // Whatever parses must replay without panicking.
            let _ = j.verify();
        }
    }

    /// Single-byte mutations of a valid journal: parse is total, and any mutation
    /// that still parses either verifies to the same hash or reports a divergence.
    #[test]
    fn mutated_journal_is_detected_or_rejected(pos in any::<prop::sample::Index>(), byte in any::<u8>()) {
        let text = sample_journal();
        let mut bytes = text.clone().into_bytes();
        let i = pos.index(bytes.len());
        bytes[i] = byte;
        let mutated = String::from_utf8_lossy(&bytes).into_owned();
        assert_parse_is_total(&mutated);
        if let Ok(j) = Journal::parse(&mutated) {
            let _ = j.verify();
        }
    }
}

// ---------------------------------------------------------------------------------
// Order lifecycle invariants under long random workloads
// ---------------------------------------------------------------------------------

/// Book invariants that must hold after every command:
/// * not crossed (best bid < best ask);
/// * the id index and the price ladders agree (`len()`, `contains`, `order()`);
/// * every level's cached total and count equal the sum over its orders;
/// * every resting order is in band, on tick, with 0 < qty <= MAX_ORDER_QTY.
fn check_book(e: &MatchingEngine, c: &BookConfig) {
    let b = e.book();
    if let (Some((bid, _)), Some((ask, _))) = (b.best_bid(), b.best_ask()) {
        assert!(bid < ask, "crossed book: bid {bid} >= ask {ask}");
    }
    let mut n = 0usize;
    let mut per_level: HashMap<(Side, u64), (u64, u32)> = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    b.for_each_order(|o| {
        n += 1;
        assert!(seen.insert(o.id), "id {} rests twice", o.id);
        assert!(o.qty > 0 && o.qty <= MAX_ORDER_QTY, "bad qty {o:?}");
        assert!(o.price >= c.min_price && o.price <= c.max_price && o.price % c.tick == 0);
        assert_eq!(b.order(o.id), Some(o), "index disagrees with ladder");
        let s = per_level.entry((o.side, o.price)).or_default();
        s.0 += o.qty;
        s.1 += 1;
    });
    assert_eq!(n, b.len(), "index size != orders on ladders");
    assert!(n <= c.max_orders as usize);
    for side in [Side::Buy, Side::Sell] {
        let depth = b.depth(side);
        for w in depth.windows(2) {
            match side {
                Side::Buy => assert!(w[0].0 > w[1].0),
                Side::Sell => assert!(w[0].0 < w[1].0),
            }
        }
        for (p, q, cnt) in depth {
            assert_eq!(
                per_level.remove(&(side, p)),
                Some((q, cnt)),
                "level {side:?} {p}"
            );
        }
    }
    assert!(
        per_level.is_empty(),
        "orders on levels not reported by depth"
    );
}

/// Per-command event-stream checks that do not need a reference model.
struct Ledger {
    /// Quantity that entered the book or is in flight, minus what left it.
    live: i128,
    /// Remaining quantity per resting id, tracked from events alone.
    resting: HashMap<u64, (Side, u64, u64)>,
}

impl Ledger {
    fn apply(&mut self, e: &MatchingEngine, cmd: Command, events: &[Event], before_hash: u64) {
        let rejected = matches!(events, [Event::Rejected { .. }]);
        if rejected {
            assert_eq!(
                e.book().state_hash(),
                before_hash,
                "rejected {cmd} changed state"
            );
            if let Event::Rejected { id, reason } = events[0] {
                assert_eq!(id, cmd.id());
                if reason == RejectReason::DuplicateId {
                    assert!(self.resting.contains_key(&id));
                }
                if reason == RejectReason::UnknownOrder {
                    assert!(!self.resting.contains_key(&id), "{cmd}: id is resting");
                }
            }
        }
        for ev in events {
            match *ev {
                Event::Accepted { id } => {
                    assert_eq!(id, cmd.id());
                    assert!(
                        !self.resting.contains_key(&id),
                        "{cmd}: accepted a duplicate"
                    );
                    self.live += match cmd {
                        Command::Limit { qty, .. } | Command::Market { qty, .. } => i128::from(qty),
                        other => panic!("ACK for {other}"),
                    };
                }
                Event::Replaced {
                    id,
                    qty,
                    prev_qty,
                    price,
                    ..
                } => {
                    let (side, _, rem) = self.resting.remove(&id).expect("replace of unknown id");
                    assert_eq!(rem, prev_qty);
                    self.live += i128::from(qty) - i128::from(prev_qty);
                    self.resting.insert(id, (side, price, qty));
                }
                Event::Trade {
                    taker,
                    maker,
                    price,
                    qty,
                } => {
                    assert!(qty > 0);
                    assert_ne!(taker, maker, "self-match across ids");
                    assert_eq!(taker, cmd.id());
                    // The taker's side and limit are respected.
                    let (taker_side, limit) = match cmd {
                        Command::Limit { side, price, .. } => (side, Some(price)),
                        Command::Market { side, .. } => (side, None),
                        // Recorded by the preceding `Replaced` event.
                        Command::Replace { price, .. } => (self.resting[&taker].0, Some(price)),
                        other => panic!("trade during {other}"),
                    };
                    let m = self
                        .resting
                        .get_mut(&maker)
                        .expect("trade with non-resting maker");
                    assert_eq!(m.1, price, "trade not at the maker's price");
                    assert_ne!(taker_side, m.0, "trade on the same side");
                    if let Some(l) = limit {
                        match taker_side {
                            Side::Buy => assert!(price <= l),
                            Side::Sell => assert!(price >= l),
                        }
                    }
                    assert!(qty <= m.2, "partial-fill overdraw");
                    m.2 -= qty;
                    if m.2 == 0 {
                        self.resting.remove(&maker);
                    }
                    self.live -= 2 * i128::from(qty);
                }
                Event::Cancelled { id, qty, reason } => {
                    assert!(qty > 0);
                    if reason == CancelReason::User {
                        let (_, _, rem) = self.resting.remove(&id).expect("cancel of unknown id");
                        assert_eq!(rem, qty, "cancel did not return the remaining qty");
                    } else {
                        assert!(
                            matches!(cmd, Command::Market { .. }),
                            "IOC cancel for {cmd}"
                        );
                    }
                    self.live -= i128::from(qty);
                }
                Event::Rejected { .. } => assert!(rejected, "reject mixed with other events"),
                other => panic!("unmodelled event {other}"),
            }
        }
        // Whatever is left of an accepted limit / requeued replace now rests.
        if let Command::Limit {
            id, side, price, ..
        } = cmd
        {
            if let Some(o) = e.book().order(id) {
                if !rejected {
                    self.resting.insert(id, (side, price, o.qty));
                }
            }
        }
        if let Command::Replace { id, .. } = cmd {
            match e.book().order(id) {
                Some(o) => {
                    self.resting.insert(id, (o.side, o.price, o.qty));
                }
                None => {
                    self.resting.remove(&id);
                }
            }
        }
        // Event-derived state equals the book exactly.
        let mut total: i128 = 0;
        let mut count = 0usize;
        e.book().for_each_order(|o| {
            total += i128::from(o.qty);
            count += 1;
            assert_eq!(
                self.resting.get(&o.id),
                Some(&(o.side, o.price, o.qty)),
                "{cmd}: ledger drift for {}",
                o.id
            );
        });
        assert_eq!(count, self.resting.len());
        assert_eq!(total, self.live, "{cmd}: quantity not conserved");
    }
}

/// Adversarial random flow: tiny id space (duplicates, stale cancels/replaces of
/// filled ids, id reuse), prices that straddle the band and the tick grid, quantities
/// at 0 / MAX / MAX+1, a pool small enough to hit `book_full`.
fn adversarial(rng: &mut SplitMix64, c: &BookConfig) -> Command {
    let id = rng.below(40);
    let side = if rng.below(2) == 0 {
        Side::Buy
    } else {
        Side::Sell
    };
    let span = (c.max_price - c.min_price) / c.tick;
    let price = match rng.below(20) {
        0 => c.min_price.saturating_sub(c.tick),
        1 => c.max_price + c.tick,
        2 => c.min_price + rng.below(span + 1) * c.tick + 1 + rng.below(c.tick - 1),
        3 => u64::MAX,
        _ => c.min_price + rng.below(span + 1) * c.tick,
    };
    let qty = match rng.below(40) {
        0 => 0,
        1 => MAX_ORDER_QTY,
        2 => MAX_ORDER_QTY + 1,
        _ => 1 + rng.below(30),
    };
    match rng.below(10) {
        0..=3 => Command::Limit {
            id,
            side,
            price,
            qty,
        },
        4 => Command::Market { id, side, qty },
        5 | 6 => Command::Cancel { id },
        _ => Command::Replace { id, price, qty },
    }
}

#[test]
fn long_adversarial_workloads_keep_every_invariant() {
    // (tick, min, max, max_orders): narrow band forces crosses; tick 5 exercises
    // bad_tick; a 16-order pool exercises book_full; min=0 exercises the low edge.
    let configs = [
        cfg(1, 1, 30, 16),
        cfg(5, 0, 200, 16),
        cfg(7, 700, 7_000, 64),
        cfg(1, 1, 1 << 20, 1_024),
    ];
    let mut tally: HashMap<&'static str, u64> = HashMap::new();
    for (k, c) in configs.iter().enumerate() {
        for seed in 0..4u64 {
            let mut rng = SplitMix64::new(seed * 1_000 + k as u64);
            let mut e = MatchingEngine::new(*c).unwrap();
            let mut ledger = Ledger {
                live: 0,
                resting: HashMap::new(),
            };
            for _ in 0..50_000 {
                let cmd = adversarial(&mut rng, c);
                let before = e.book().state_hash();
                let events = run(&mut e, cmd);
                assert!(!events.is_empty(), "{cmd} produced no event");
                ledger.apply(&e, cmd, &events, before);
                check_book(&e, c);
                for ev in &events {
                    let key = match ev {
                        Event::Rejected { reason, .. } => reason.as_str(),
                        Event::Trade { .. } => "trade",
                        Event::Replaced {
                            kept_priority: true,
                            ..
                        } => "replace_keep",
                        Event::Replaced { .. } => "replace_requeue",
                        Event::Cancelled {
                            reason: CancelReason::Ioc,
                            ..
                        } => "ioc",
                        Event::Cancelled { .. } => "cancel",
                        _ => "ack",
                    };
                    *tally.entry(key).or_default() += 1;
                }
            }
        }
    }
    // The workload really reached every branch it claims to test.
    for k in [
        "zero_qty",
        "qty_too_large",
        "bad_tick",
        "price_out_of_band",
        "duplicate_id",
        "unknown_order",
        "book_full",
        "trade",
        "replace_keep",
        "replace_requeue",
        "ioc",
        "cancel",
        "ack",
    ] {
        assert!(
            tally.get(k).copied().unwrap_or(0) > 0,
            "branch {k} never exercised"
        );
    }
}

// ---------------------------------------------------------------------------------
// Named lifecycle scenarios
// ---------------------------------------------------------------------------------

#[test]
fn stale_commands_after_fill_are_rejected_without_side_effects() {
    let mut e = MatchingEngine::new(cfg(1, 1, 1_000, 8)).unwrap();
    run(
        &mut e,
        Command::Limit {
            id: 1,
            side: Side::Sell,
            price: 100,
            qty: 5,
        },
    );
    run(
        &mut e,
        Command::Limit {
            id: 2,
            side: Side::Buy,
            price: 100,
            qty: 5,
        },
    ); // fills 1
    let h = e.book().state_hash();
    assert!(e.book().is_empty());
    for cmd in [
        Command::Cancel { id: 1 },
        Command::Cancel { id: 2 },
        Command::Replace {
            id: 1,
            price: 100,
            qty: 1,
        },
        Command::Replace {
            id: 2,
            price: 99,
            qty: 1,
        },
    ] {
        assert_eq!(
            run(&mut e, cmd),
            vec![Event::Rejected {
                id: cmd.id(),
                reason: RejectReason::UnknownOrder
            }],
            "{cmd}"
        );
        assert_eq!(e.book().state_hash(), h);
    }
    // Double cancel: second one is stale.
    run(
        &mut e,
        Command::Limit {
            id: 3,
            side: Side::Buy,
            price: 90,
            qty: 2,
        },
    );
    assert_eq!(
        run(&mut e, Command::Cancel { id: 3 }),
        vec![Event::Cancelled {
            id: 3,
            qty: 2,
            reason: CancelReason::User
        }]
    );
    assert_eq!(
        run(&mut e, Command::Cancel { id: 3 }),
        vec![Event::Rejected {
            id: 3,
            reason: RejectReason::UnknownOrder
        }]
    );
    // A filled id may be reused (documented rule 5).
    assert_eq!(
        run(
            &mut e,
            Command::Limit {
                id: 1,
                side: Side::Buy,
                price: 50,
                qty: 1
            }
        ),
        vec![Event::Accepted { id: 1 }]
    );
}

#[test]
fn duplicate_ids_are_rejected_for_every_entry_command() {
    let mut e = MatchingEngine::new(cfg(1, 1, 1_000, 8)).unwrap();
    run(
        &mut e,
        Command::Limit {
            id: 7,
            side: Side::Buy,
            price: 90,
            qty: 4,
        },
    );
    let h = e.book().state_hash();
    for cmd in [
        Command::Limit {
            id: 7,
            side: Side::Buy,
            price: 90,
            qty: 1,
        },
        Command::Limit {
            id: 7,
            side: Side::Sell,
            price: 10,
            qty: 1,
        }, // would cross itself
        Command::Market {
            id: 7,
            side: Side::Sell,
            qty: 1,
        },
    ] {
        assert_eq!(
            run(&mut e, cmd),
            vec![Event::Rejected {
                id: 7,
                reason: RejectReason::DuplicateId
            }],
            "{cmd}"
        );
        assert_eq!(e.book().state_hash(), h);
    }
}

#[test]
fn partial_fills_keep_priority_and_report_exact_remainders() {
    let mut e = MatchingEngine::new(cfg(1, 1, 1_000, 8)).unwrap();
    run(
        &mut e,
        Command::Limit {
            id: 1,
            side: Side::Sell,
            price: 100,
            qty: 10,
        },
    );
    run(
        &mut e,
        Command::Limit {
            id: 2,
            side: Side::Sell,
            price: 100,
            qty: 10,
        },
    );
    // Three partial fills of the front order; it stays in front.
    for (taker, q, left) in [(10, 3, 7), (11, 4, 3), (12, 2, 1)] {
        assert_eq!(
            run(
                &mut e,
                Command::Market {
                    id: taker,
                    side: Side::Buy,
                    qty: q
                }
            ),
            vec![
                Event::Accepted { id: taker },
                Event::Trade {
                    taker,
                    maker: 1,
                    price: 100,
                    qty: q
                }
            ]
        );
        assert_eq!(e.book().order(1).unwrap().qty, left);
        assert_eq!(e.book().best_ask(), Some((100, left + 10)));
    }
    // A taker that spans the partially-filled front and the next order.
    assert_eq!(
        run(
            &mut e,
            Command::Limit {
                id: 13,
                side: Side::Buy,
                price: 100,
                qty: 4
            }
        ),
        vec![
            Event::Accepted { id: 13 },
            Event::Trade {
                taker: 13,
                maker: 1,
                price: 100,
                qty: 1
            },
            Event::Trade {
                taker: 13,
                maker: 2,
                price: 100,
                qty: 3
            },
        ]
    );
    assert!(!e.book().contains(1));
    // Cancel returns exactly the unfilled remainder.
    assert_eq!(
        run(&mut e, Command::Cancel { id: 2 }),
        vec![Event::Cancelled {
            id: 2,
            qty: 7,
            reason: CancelReason::User
        }]
    );
    assert!(e.book().is_empty());
}

// ---------------------------------------------------------------------------------
// Replay determinism
// ---------------------------------------------------------------------------------

/// Pinned fingerprints of generated workloads. These are computed by the engine and
/// checked on every CI platform (Linux x86_64, macOS arm64, Windows x86_64), so a
/// platform-dependent result (hasher, pointer width, iteration order) fails CI.
#[test]
fn replay_hashes_are_pinned_across_runs_and_platforms() {
    for (seed, replace_pct, want) in [
        (42u64, 0u64, "e850f79b85aaf0c2/5743f3ea2d97ffe5"),
        (7, 20, "9efbfc9bbac0e9f2/d3d59ca3570a97c3"),
        (1, 25, "258d6bc441a98fe7/9e8a4461dbd43199"),
    ] {
        let w = WorkloadConfig {
            seed,
            commands: 100_000,
            replace_pct,
            ..WorkloadConfig::default()
        };
        let cmds = w.generate();
        let a = Journal::record(w.book_config(1 << 16), &cmds).unwrap();
        let b = Journal::record(w.book_config(1 << 16), &cmds).unwrap();
        assert_eq!(a, b, "two in-process runs differ");
        let text = a.to_text();
        let back = Journal::parse(&text).unwrap();
        assert_eq!(back.verify().unwrap(), a.final_hash);
        // Fingerprint the whole event stream too, not only the final book.
        let stream = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        assert_eq!(
            format!("{:016x}/{stream:016x}", a.final_hash),
            want,
            "seed {seed} replace {replace_pct}: book hash / event-stream hash"
        );
    }
}
