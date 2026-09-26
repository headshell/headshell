//! The thread that hosts the core: the tick loop + the job queue (D-032,
//! D-033).
//!
//! **The shell only drives the loop.** Advancing, writing listens and
//! producing the report happen inside `LiveSession::tick()` — the same call
//! as the TUI; the same dance is not written a second time (the Golden Rule).
//!
//! Commands and ticks run in order on **the same** thread. No lock, no race:
//! the channel already puts them in order. Why it is like this is written at
//! the top of [`crate::state`].
//!
//! ## Why events do not go out on a timer
//!
//! D-028 measured the bridge carrying ~10,000 events per second; so this is
//! not a performance measure. The reason is this: the webview **estimates the
//! position from the anchor** (D-015), so a "still playing" message carries
//! zero information. Only what the estimate cannot know is sent — the track
//! changed, a listen was written, the store gave an error, the queue ended,
//! a cover arrived (D-076), the playing track's lyrics changed (D-078), **or
//! the part of the anchor that feeds the estimate changed** (state, rate,
//! duration, track identity).
//!
//! The last item was not on D-033's first list, and it silently froze the
//! interface: audio starts as `Buffering` and moves to `Playing`, and since
//! `Buffering` does not advance, the progress bar stayed at 0:00. Details in
//! [`worth_sending`].

use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use headshell_core::playback::TickReport;

use crate::state::{CommandError, Core, Job};

/// The round interval. Independent of the audio pipeline: only the question
/// "did the track end". The same value as the TUI — both shells advance at
/// the same rhythm.
const TICK: Duration = Duration::from_millis(200);

/// The event that goes out when something notable happened in a round. Its
/// payload is a `TickReport`.
pub const TICK_EVENT: &str = "headshell://tick";

/// The event that goes out when `tick()` returns an error. Its payload is a
/// `CommandError`.
///
/// The session **is not dropped**: the error belongs to one track, not to the
/// whole session. But it is not swallowed either (K9).
pub const ERROR_EVENT: &str = "headshell://error";

/// Starts the core on its own thread.
///
/// # Errors
/// If the thread or the runtime cannot be set up.
pub fn spawn(
    core: Core,
    jobs: mpsc::UnboundedReceiver<Job>,
    app: AppHandle,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("headshell-core".to_owned())
        .spawn(move || run(core, jobs, &app))
}

fn run(mut core: Core, mut jobs: mpsc::UnboundedReceiver<Job>, app: &AppHandle) {
    // A single-threaded runtime: the core produces futures that are not
    // `Send`, and they will all run here anyway. The **shell** picks the
    // runtime (the convention) — the CLI makes the same choice.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(source) => {
            eprintln!("STEP: CONFIG_LOAD\n  could not set up the core runtime: {source}");
            return;
        }
    };

    rt.block_on(async move {
        let mut ticker = tokio::time::interval(TICK);
        // Late rounds must not pile up and fire back to back: if it is late,
        // it is skipped. `tick()` is not a counter but the question "did
        // anything happen".
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // The unpredictable part of the last anchor sent. It is compared so
        // the same thing is not sent twice.
        let mut last = Notable::of(&core.live.anchor());

        loop {
            tokio::select! {
                job = jobs.recv() => match job {
                    Some(job) => job(&mut core).await,
                    // Every sending end is gone: the app is closing.
                    None => break,
                },
                _ = ticker.tick() => run_tick(&mut core, app, &mut last).await,
            }
        }

        // Shutdown: `tick` writes every round (D-032), so nothing is expected
        // to be left to write here — if something is, the store was failing
        // at that moment and this is the last attempt. With the window gone
        // there is no surface to show it on, but the loss must not be silent
        // either.
        match core.live.shutdown() {
            Ok(summary) if summary.inserted > 0 => {
                eprintln!("listens written at shutdown: {}", summary.inserted);
            }
            Ok(_) => {}
            Err(err) => {
                eprintln!("{}", err.chain_text());
                eprintln!("listens still held back: {}", core.live.pending_listens());
            }
        }
    });
}

/// The **unpredictable** part of the anchor.
///
/// The webview advances the position with `position_ms + (now - wall_time) ×
/// rate`. These fields are that formula's inputs or its context: when they
/// change the estimate goes wrong; when they do not, sending carries zero
/// information.
///
/// `position_ms` is left out on purpose: that is the estimate's job anyway.
#[derive(Debug, Clone, PartialEq)]
struct Notable {
    track: Option<headshell_core::ids::CanonicalId>,
    state: headshell_core::playback::PlayState,
    rate: f64,
    duration_ms: Option<u64>,
}

impl Notable {
    fn of(anchor: &headshell_core::playback::PlaybackAnchor) -> Self {
        Self {
            track: anchor.track.clone(),
            state: anchor.state,
            rate: anchor.rate,
            duration_ms: anchor.duration_ms,
        }
    }
}

/// Should this round be sent to the webview?
///
/// A separate function because the real decision is here and it must be
/// testable: a wrong "no" freezes the interface **silently**. Indeed the
/// first version did exactly that — the `Buffering → Playing` transition was
/// not on the list, the estimate does not advance in `Buffering`, so the
/// progress bar stayed at 0:00 and no error was visible.
fn worth_sending(last: &Notable, report: &TickReport) -> bool {
    report.track_changed
        || report.listens_recorded > 0
        || report.store_error.is_some()
        || report.finished
        || !report.artwork_ready.is_empty()
        || report.lyrics_ready
        || Notable::of(&report.anchor) != *last
}

/// One round: advance the core, report only what is notable.
async fn run_tick(core: &mut Core, app: &AppHandle, last: &mut Notable) {
    match core.live.tick().await {
        Ok(report) => {
            if worth_sending(last, &report) {
                *last = Notable::of(&report.anchor);
                let _ = app.emit(TICK_EVENT, &report);
            }
        }
        Err(err) => {
            let _ = app.emit(ERROR_EVENT, CommandError::from(err));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use headshell_core::playback::{PlayState, PlaybackAnchor};

    fn anchor(state: PlayState, position_ms: u64, duration_ms: Option<u64>) -> PlaybackAnchor {
        PlaybackAnchor {
            position_ms,
            rate: if state == PlayState::Playing {
                1.0
            } else {
                0.0
            },
            state,
            duration_ms,
            // `stopped()` for the timestamp: this package does not depend on
            // `jiff` directly, and it has no need to.
            ..PlaybackAnchor::stopped()
        }
    }

    fn report(anchor: PlaybackAnchor) -> TickReport {
        TickReport {
            anchor,
            track_changed: false,
            listens_recorded: 0,
            listens_pending: 0,
            store_error: None,
            finished: false,
            artwork_ready: Vec::new(),
            lyrics_ready: false,
        }
    }

    /// If only time passed, nothing is sent: the webview already advances the
    /// position from the anchor.
    #[test]
    fn a_tick_that_only_advanced_time_is_not_worth_sending() {
        let last = Notable::of(&anchor(PlayState::Playing, 1_000, Some(240_000)));
        let next = report(anchor(PlayState::Playing, 30_000, Some(240_000)));

        assert!(!worth_sending(&last, &next));
    }

    /// **The reason for this test is a real bug.** The first version only sent
    /// track changes/listens/errors/the end. When playback starts the engine
    /// first says `Buffering`, then moves to `Playing` once the buffer fills —
    /// but since that transition was not sent, the webview kept the `Buffering`
    /// anchor it had, and since `Buffering` does not advance, the bar froze at
    /// 0:00. No error was visible; it just looked like "not playing".
    #[test]
    fn the_buffering_to_playing_transition_must_be_sent() {
        let last = Notable::of(&anchor(PlayState::Buffering, 0, Some(10_000)));
        let next = report(anchor(PlayState::Playing, 0, Some(10_000)));

        assert!(worth_sending(&last, &next));
    }

    /// The duration can be read late from the container: `None` → `Some` is a
    /// change too, otherwise the progress bar could never learn its ratio.
    #[test]
    fn learning_the_duration_later_is_a_change() {
        let last = Notable::of(&anchor(PlayState::Playing, 0, None));
        let next = report(anchor(PlayState::Playing, 5_000, Some(240_000)));

        assert!(worth_sending(&last, &next));
    }

    #[test]
    fn a_recorded_listen_is_always_worth_sending() {
        let last = Notable::of(&anchor(PlayState::Playing, 0, Some(240_000)));
        let mut next = report(anchor(PlayState::Playing, 1_000, Some(240_000)));
        next.listens_recorded = 1;

        assert!(worth_sending(&last, &next));
    }

    #[test]
    fn a_store_error_is_never_swallowed() {
        let last = Notable::of(&anchor(PlayState::Playing, 0, Some(240_000)));
        let mut next = report(anchor(PlayState::Playing, 1_000, Some(240_000)));
        next.store_error = Some("STEP: LIBRARY_WRITE\n  disk full".to_owned());

        assert!(worth_sending(&last, &next));
    }

    /// A cover's answer is news the estimate cannot know (D-076): without the
    /// event a row keeps its placeholder, and its reason, until the next
    /// track.
    #[test]
    fn an_arrived_cover_is_worth_sending() {
        let last = Notable::of(&anchor(PlayState::Playing, 0, Some(240_000)));
        let mut next = report(anchor(PlayState::Playing, 1_000, Some(240_000)));
        assert!(!worth_sending(&last, &next));
        next.artwork_ready =
            vec![headshell_core::artwork::ArtworkKey::parse("album:0123456789abcdef").unwrap()];
        assert!(worth_sending(&last, &next));
    }

    /// The playing track's lyrics arriving is news the estimate cannot know
    /// (D-078): without the event the lyrics view keeps saying "looking".
    #[test]
    fn arrived_lyrics_are_worth_sending() {
        let last = Notable::of(&anchor(PlayState::Playing, 0, Some(240_000)));
        let mut next = report(anchor(PlayState::Playing, 1_000, Some(240_000)));
        assert!(!worth_sending(&last, &next));
        next.lyrics_ready = true;
        assert!(worth_sending(&last, &next));
    }
}
