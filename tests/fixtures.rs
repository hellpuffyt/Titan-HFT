//! Golden fixtures and the `titan` CLI, end to end.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use titan_hft::journal::{self, Divergence, Journal};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(fixture(name)).unwrap()
}

fn titan(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_titan"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn golden_journals_replay_exactly() {
    for name in [
        "price_time.journal",
        "seed42_5k.journal",
        "replace.journal",
        "seed7_replace_3k.journal",
    ] {
        let j = Journal::parse(&read(name)).unwrap();
        assert!(j.verify().is_ok(), "{name} diverged");
    }
}

#[test]
fn recording_the_command_file_reproduces_the_golden_journal_byte_for_byte() {
    for (cmds, golden) in [
        ("price_time.cmds", "price_time.journal"),
        ("seed42_5k.cmds", "seed42_5k.journal"),
        ("replace.cmds", "replace.journal"),
        ("seed7_replace_3k.cmds", "seed7_replace_3k.journal"),
    ] {
        let parsed = Journal::parse(&read(golden)).unwrap();
        let commands = journal::parse_commands(&read(cmds)).unwrap();
        let fresh = Journal::record(parsed.config, &commands).unwrap();
        assert_eq!(fresh.to_text(), read(golden), "{cmds}");
    }
}

#[test]
fn tampered_journal_is_detected_at_the_right_command() {
    let j = Journal::parse(&read("tampered.journal")).unwrap();
    match j.verify() {
        Err(Divergence::Events { index, line, .. }) => {
            assert_eq!(index, 3);
            assert_eq!(line, 9);
        }
        other => panic!("expected event divergence, got {other:?}"),
    }
}

#[test]
fn replace_fixture_shows_every_rule() {
    let j = Journal::parse(&read("replace.journal")).unwrap();
    let text = j.to_text();
    // Rule 1: reduce in place keeps priority.
    assert!(text.contains("> R 1 100 4\n< RPL 1 100 4 10 keep\n"));
    // Rule 2: increase requeues; afterwards order 2 fills before order 1.
    assert!(text.contains("> R 1 100 6\n< RPL 1 100 6 4 requeue\n"));
    assert!(text.contains("< TRADE 10 2 100 10\n< TRADE 10 1 100 2\n"));
    // Rule 3: price change requeues and crosses immediately as taker.
    assert!(text.contains("< RPL 11 103 8 8 requeue\n< TRADE 11 1 100 4\n< TRADE 11 3 103 4\n"));
    // Rule 4: unknown, filled and cancelled ids.
    for id in [99, 10, 3] {
        assert!(
            text.contains(&format!("< REJ {id} unknown_order\n")),
            "{id}"
        );
    }
    // Rule 5: invalid values reuse existing reject reasons.
    for r in ["zero_qty", "qty_too_large", "price_out_of_band"] {
        assert!(text.contains(&format!("< REJ 20 {r}\n")), "{r}");
    }
    // Rule 6: identical replace is a priority-keeping no-op; the order survives.
    assert!(text.contains("< RPL 20 95 7 7 keep\n"));
    assert!(j.verify().is_ok());
}

#[test]
fn malformed_command_files_are_rejected_with_line_numbers() {
    let cases = [
        ("missing_qty.cmds", 2, "missing qty"),
        ("bad_side.cmds", 1, "side"),
        ("unknown_command.cmds", 2, "unknown command"),
        ("negative_price.cmds", 1, "price"),
        ("trailing_token.cmds", 1, "trailing"),
        ("overflow_qty.cmds", 1, "qty"),
        ("replace_missing_qty.cmds", 2, "missing qty"),
    ];
    for (name, line, needle) in cases {
        let e = journal::parse_commands(&read(&format!("malformed/{name}"))).unwrap_err();
        assert_eq!(e.line, line, "{name}");
        assert!(e.msg.contains(needle), "{name}: {}", e.msg);
    }
}

#[test]
fn cli_run_replay_and_exit_codes() {
    let tmp = std::env::temp_dir().join(format!("titan-cli-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let cmds = tmp.join("flow.cmds");
    let jpath = tmp.join("flow.journal");

    let (code, _, err) = titan(&[
        "gen",
        "--seed",
        "5",
        "--count",
        "2000",
        "--out",
        cmds.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = titan(&[
        "run",
        cmds.to_str().unwrap(),
        "--quiet",
        "--journal",
        jpath.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("hash "));
    let (code, out, _) = titan(&["replay", jpath.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.starts_with("OK: 2000 commands"));

    let (code, _, err) = titan(&["replay", fixture("tampered.journal").to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(err.contains("DIVERGENCE"));

    let (code, _, err) = titan(&["run", fixture("malformed/bad_side.cmds").to_str().unwrap()]);
    assert_eq!(code, 2);
    assert!(err.contains("line 1"));
    assert_eq!(titan(&["run"]).0, 2);
    assert_eq!(titan(&["frobnicate"]).0, 2);
    assert_eq!(titan(&["run", "x", "--bogus", "1"]).0, 2);
    assert_eq!(titan(&["run", "x", "--tick", "0"]).0, 2);
    assert_eq!(titan(&["--help"]).0, 0);
    assert_eq!(titan(&["gen", "--count", "1", "--replace-pct", "61"]).0, 2);
    let (code, out, _) = titan(&["gen", "--count", "200", "--replace-pct", "50"]);
    assert_eq!(code, 0);
    assert!(out.lines().any(|l| l.starts_with("R ")));
    let _ = std::fs::remove_dir_all(&tmp);
}
