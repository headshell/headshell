//! The accuracy set for the line being sung (D-078).
//!
//! **Why a shared fixture and not only a unit test:** the interface lights
//! the line being sung every frame without asking the core, so a second copy
//! of the rule lives in JS (`ui/lyrics.js`). Two copies drift where nobody
//! looks — a line lit a frame early on one side only.
//!
//! `fixtures/lyrics/line_at_cases.json` is the single source of truth both
//! sides read. This file binds the Rust side; `headshell/tests/lyrics_parity_js.rs`
//! binds the JS side.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use headshell_core::lyrics::{LyricLine, Lyrics};

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    synced: bool,
    starts: Vec<u64>,
    position_ms: u64,
    offset_ms: i64,
    expected: Option<usize>,
}

#[derive(serde::Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[test]
fn every_case_in_the_shared_truth_set_matches_the_core_rule() {
    let path = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/lyrics/line_at_cases.json"
    ));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
    let cases: Cases = serde_json::from_str(&text).expect("the accuracy set must parse");
    assert!(
        cases.cases.len() >= 11,
        "the accuracy set has shrunk: {} cases",
        cases.cases.len()
    );

    let mut failures = Vec::new();
    for case in &cases.cases {
        let lyrics = Lyrics {
            synced: case.synced,
            lines: case
                .starts
                .iter()
                .map(|&ms| LyricLine {
                    start_ms: Some(ms),
                    text: String::new(),
                })
                .collect(),
            attribution: None,
        };
        let got = lyrics.line_at(case.position_ms, case.offset_ms);
        if got != case.expected {
            failures.push(format!(
                "  {}: expected {:?}, got {:?}",
                case.name, case.expected, got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the core's rule drifted from the shared set:\n{}",
        failures.join("\n")
    );
}
