//! Binds the line-being-sung rule in JS to the accuracy set (D-078), as
//! `anchor_parity_js.rs` binds the position.
//!
//! `headshell-core/tests/lyrics_parity.rs` binds the Rust side; drift is
//! caught only because both read the same file. `ui/lyrics.js` is evaluated
//! in the embedded QuickJS: the test needs nothing from the machine and is
//! never skipped (D-070).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use rquickjs::{CaughtError, Context, Function, Module, Runtime, Value};

#[test]
fn the_javascript_copy_agrees_with_the_shared_truth_set() {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ui")
            .join("lyrics.js"),
    )
    .expect("STEP: LYRICS_PARITY — could not read ui/lyrics.js");
    let truth: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/lyrics/line_at_cases.json"),
        )
        .expect("STEP: LYRICS_PARITY — could not read the accuracy set"),
    )
    .expect("STEP: LYRICS_PARITY — the accuracy set is not JSON");
    let cases = truth["cases"]
        .as_array()
        .filter(|cases| !cases.is_empty())
        .expect("STEP: LYRICS_PARITY — the accuracy set is empty");

    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let failures: Vec<String> = context.with(|ctx| {
        let evaluated = Module::declare(ctx.clone(), "lyrics.js", source)
            .and_then(|module| module.eval())
            .and_then(|(module, promise)| promise.finish::<()>().map(|()| module));
        let module = match evaluated {
            Ok(module) => module,
            Err(err) => panic!(
                "STEP: LYRICS_PARITY — could not evaluate lyrics.js: {}",
                CaughtError::from_error(&ctx, err)
            ),
        };
        let line_at: Function = module.get("lineAt").unwrap();

        let mut failures = Vec::new();
        for case in cases {
            let name = case["name"].as_str().unwrap_or("?");
            let lines: Vec<serde_json::Value> = case["starts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|start| serde_json::json!({ "start_ms": start, "text": "" }))
                .collect();
            let lyrics = serde_json::json!({ "synced": case["synced"], "lines": lines });
            let lyrics: Value = ctx.json_parse(lyrics.to_string()).unwrap();
            let got: Option<f64> = line_at
                .call((
                    lyrics,
                    case["position_ms"].as_f64().unwrap(),
                    case["offset_ms"].as_f64().unwrap(),
                ))
                .unwrap();
            let expected = case["expected"].as_f64();
            if got != expected {
                failures.push(format!("  {name}\n    expected {expected:?}, got {got:?}"));
            }
        }
        failures
    });

    assert!(
        failures.is_empty(),
        "STEP: LYRICS_PARITY — {}/{} cases drifted:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// The reading position is a third of the way down, and never past what the
/// list can scroll: a short list does not scroll at all.
#[test]
fn the_reading_position_is_clamped_to_the_list() {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ui")
            .join("lyrics.js"),
    )
    .unwrap();
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let module = Module::declare(ctx.clone(), "lyrics.js", source)
            .and_then(|module| module.eval())
            .and_then(|(module, promise)| promise.finish::<()>().map(|()| module))
            .unwrap();
        let reading_top: Function = module.get("readingTop").unwrap();
        let at = |line_top: f64, height: f64, viewport: f64, content: f64| -> f64 {
            reading_top
                .call((line_top, height, viewport, content))
                .unwrap()
        };
        // A line in the middle of a long list: its centre at 36% of the view.
        assert!((at(1000.0, 40.0, 500.0, 5000.0) - 840.0).abs() < 1e-9);
        // Near the top: not above the list's start.
        assert!(at(10.0, 40.0, 500.0, 5000.0).abs() < 1e-9);
        // Near the end: not past what can be scrolled.
        assert!((at(4990.0, 40.0, 500.0, 5000.0) - 4500.0).abs() < 1e-9);
        // A list shorter than the view does not scroll.
        assert!(at(100.0, 40.0, 500.0, 300.0).abs() < 1e-9);
    });
}

/// A click on a line moves to where it starts (D-090), and there that very
/// line is the one sung: `lineStart` is `lineAt`'s inverse on every line of
/// the accuracy set whose start is its own — not shared with the next line,
/// and not clamped to the track's start by a shift.
#[test]
fn a_clicked_line_is_the_line_lit_where_it_starts() {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ui")
            .join("lyrics.js"),
    )
    .unwrap();
    let truth: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/lyrics/line_at_cases.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    let checked = context.with(|ctx| {
        let module = Module::declare(ctx.clone(), "lyrics.js", source)
            .and_then(|module| module.eval())
            .and_then(|(module, promise)| promise.finish::<()>().map(|()| module))
            .unwrap();
        let line_at: Function = module.get("lineAt").unwrap();
        let line_start: Function = module.get("lineStart").unwrap();
        let mut checked = 0;
        for case in truth["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap_or("?");
            let starts = case["starts"].as_array().unwrap();
            let offset = case["offset_ms"].as_f64().unwrap();
            let lines: Vec<serde_json::Value> = starts
                .iter()
                .map(|start| serde_json::json!({ "start_ms": start, "text": "" }))
                .collect();
            let lyrics = serde_json::json!({ "synced": case["synced"], "lines": lines });
            for (index, start) in starts.iter().enumerate() {
                let at: Option<f64> = line_start
                    .call((ctx.json_parse(lyrics.to_string()).unwrap(), index, offset))
                    .unwrap();
                let Some(start) = start.as_f64() else {
                    assert_eq!(at, None, "{name}: a line without a start has no place");
                    continue;
                };
                if case["synced"] != serde_json::Value::Bool(true) {
                    assert_eq!(at, None, "{name}: unsynced lines have no place");
                    continue;
                }
                let shared =
                    starts.get(index + 1).and_then(serde_json::Value::as_f64) == Some(start);
                if shared || start - offset < 0.0 {
                    continue;
                }
                let lit: Option<f64> = line_at
                    .call((
                        ctx.json_parse(lyrics.to_string()).unwrap(),
                        at.unwrap(),
                        offset,
                    ))
                    .unwrap();
                #[allow(clippy::cast_precision_loss)]
                let index = index as f64;
                assert_eq!(
                    lit,
                    Some(index),
                    "{name}: line {index} clicked, another lit"
                );
                checked += 1;
            }
        }
        checked
    });
    assert!(
        checked > 5,
        "the set gave too few lines to check: {checked}"
    );
}
