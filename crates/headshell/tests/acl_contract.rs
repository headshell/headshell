//! The window's command list and its permissions name the same commands
//! (D-086).
//!
//! The page is served from a loopback address, and Tauri lets such a page
//! call an app command only with a permission: `command_names.rs` is where
//! the permissions come from (`build.rs`, `main.rs`). A command registered in
//! `generate_handler!` but missing from that list is refused at its first
//! call — and the refusal shows only in the webview's console.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

include!("../src/command_names.rs");

const MAIN_RS: &str = include_str!("../src/main.rs");

#[test]
fn every_registered_command_is_granted_and_nothing_else() {
    let handler = MAIN_RS
        .split_once("generate_handler![")
        .expect("no generate_handler! in main.rs")
        .1
        .split_once("])")
        .expect("generate_handler! does not close")
        .0;
    let registered: BTreeSet<&str> = handler
        .split("commands::")
        .skip(1)
        .map(|rest| {
            rest.split(|c: char| c == ',' || c.is_whitespace())
                .next()
                .unwrap()
        })
        .collect();
    let granted: BTreeSet<&str> = COMMANDS.iter().copied().collect();
    assert!(
        registered.len() > 60,
        "the scan came up short: {registered:?}"
    );
    assert_eq!(
        COMMANDS.len(),
        granted.len(),
        "command_names.rs names a command twice"
    );
    let missing: Vec<&&str> = registered.difference(&granted).collect();
    let extra: Vec<&&str> = granted.difference(&registered).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "registered but not granted: {missing:?}; granted but not registered: {extra:?}"
    );
}
