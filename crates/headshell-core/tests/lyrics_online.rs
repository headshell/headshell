//! Lyrics, **against the real LRCLIB** (D-078).
//!
//! The chain's unit tests run on canned answers: they test the logic. This
//! tests that the logic meets the data it was written for. The fixture is a
//! real recording's tags over 251 s of silence — Portishead's "Sour Times"
//! from *Dummy* — and LRCLIB's versions of it are the trap: festival takes
//! within two seconds of the album's length, the album's own version three
//! seconds longer (measured 2026-09-26). The lines must come from a version of
//! the album, not from a festival stage.
//!
//! **Part of the default run** (D-043), the two failures kept apart (K9):
//! - **Not reaching** LRCLIB is not a failure: the test skips itself and says
//!   why — before it starts (no TCP), or on the way (a network error, a busy
//!   server after its retries).
//! - **Reaching it and getting the unexpected** fails.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::path::Path;

use headshell_core::lyrics::{LyricsFit, LyricsSource, LyricsStatus};
use headshell_core::provider::ProviderRegistry;
use headshell_core::session::{LyricsRequest, Session};

/// Can `host` be reached over TCP. Only DNS + connect: "is there a
/// network", not the service's health — that is the test's job.
fn reachable(host: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = (host, 443).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| {
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5)).is_ok()
    })
}

#[tokio::test]
async fn a_real_recordings_lyrics_come_from_its_albums_version() {
    const TEST: &str = "lyrics from LRCLIB";
    if !reachable("lrclib.net") {
        eprintln!("{TEST}: could not reach lrclib.net:443 — skipped (taken as no network)");
        return;
    }

    let config = support::TestConfig::new("lyrics-online");
    let mut session = Session::open((*config).clone()).unwrap();
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/artwork/Portishead - Sour Times.flac");
    let request = LyricsRequest {
        file: Some(file),
        print: true,
        ..LyricsRequest::default()
    };
    let report = session
        .lyrics(&ProviderRegistry::new(), request)
        .await
        .unwrap_or_else(|err| panic!("{TEST}: {}", err.chain_text()));

    let item = &report.items[0];
    match &item.status {
        LyricsStatus::Found {
            source,
            synced,
            fit,
            delta_ms,
        } => {
            assert_eq!(*source, LyricsSource::Lrclib, "{item:#?}");
            assert!(*synced, "LRCLIB has synced versions of it: {item:#?}");
            assert!(
                matches!(fit, LyricsFit::Length | LyricsFit::Closest),
                "the recording's length is known: {item:#?}"
            );
            // The album's version, not a festival's: LRCLIB's album versions
            // run 251–254 s; the festival takes nearest in length are on
            // no album of its name, and the guard keeps them out.
            let delta = delta_ms.unwrap_or(i64::MAX).abs();
            assert!(delta <= 5_000, "{item:#?}");
            let lines = &item.lyrics.as_ref().expect("--print").lines;
            assert!(lines.len() > 5, "{} lines", lines.len());
        }
        LyricsStatus::Failed { chain }
            if chain.contains("STEP: NETWORK_REQUEST") || chain.contains("stayed busy") =>
        {
            eprintln!("{TEST}: {chain} — skipped (network)");
        }
        other => panic!("{TEST}: {other:?}\n{item:#?}"),
    }
}
