//! `headshell-core` — the core of a provider-independent listening identity.
//!
//! **The Golden Rule:** all logic lives here. The CLI, the GUI and the mobile
//! bindings only call this API; none of them does any work on its own.

// CLAUDE.md: no `unwrap`/`expect`/`panic!` in the core — **except in tests**.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod artwork;
pub mod config;
pub mod diag;
mod encoding;
pub mod error;
pub mod identity;
pub mod ids;
pub mod import;
pub mod library;
pub mod lyrics;
pub mod model;
pub mod net;
pub mod playback;
pub mod plugin;
pub mod provider;
pub mod secrets;
pub mod session;
pub mod sleeve;
pub mod stats;
mod store;
#[cfg(test)]
mod test_support;

pub use error::{Error, ErrorKind, Result};
