//! Playing a real file on a real audio device.
//!
//! **In an environment without an audio device (CI, a container) it skips
//! itself** — not silencing the test, but saying plainly that the condition
//! is not met and moving on. If there is a device it really plays and
//! verifies that the position advances.
//!
//! Without the `audio` feature this file compiles empty.

#![cfg(feature = "audio")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use headshell_core::config::Config;
use headshell_core::ids::{ProviderId, ProviderTrackId};
use headshell_core::model::TrackRef;
use headshell_core::playback::{
    AudioEngine, LiveSession, PlanReason, PlayState, Player, PrefetchStatus, QueueItem,
};
use headshell_core::provider::local::LocalProvider;
use headshell_core::provider::{AudioSource, ProviderRegistry};
use headshell_core::session::Session;

/// A local file source (a shortcut).
fn local(path: &std::path::Path) -> AudioSource {
    AudioSource::LocalFile {
        path: path.to_path_buf(),
    }
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/audio")).join(name)
}

/// Is there an audio output? Without one the test is meaningless.
fn has_output_device() -> bool {
    use cpal::traits::HostTrait;
    cpal::default_host().default_output_device().is_some()
}

/// Sets up the engine; returns `None` if there is no device.
fn engine_for(name: &str) -> Option<AudioEngine> {
    if !has_output_device() {
        eprintln!("no audio output — skipping the test (this is not a failure)");
        return None;
    }
    match AudioEngine::play_file(&fixture(name)) {
        Ok(engine) => Some(engine),
        Err(err) => {
            // The device seemed to be there but could not be opened (locked, no
            // permission…). We do not count this as a failure, but we do not pass
            // over it silently either.
            eprintln!(
                "could not open the audio device, skipping the test:\n{}",
                err.chain_text()
            );
            None
        }
    }
}

#[test]
fn a_real_file_plays_and_the_position_advances() {
    let Some(engine) = engine_for("tagged.flac") else {
        return;
    };

    // The duration must be read from the container: a 1-second fixture.
    let duration = engine.duration_ms().expect("the duration must be read");
    assert!(
        (900..=1100).contains(&duration),
        "{duration}ms while 1 s was expected"
    );

    // Wait for the audio to start flowing.
    let deadline = Instant::now() + Duration::from_secs(3);
    while engine.position_ms() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        engine.position_ms() > 0,
        "not a single frame played in 3 seconds (state: {})",
        engine.state()
    );

    // Wait until the track ends; a 1 s file must end within 4 s.
    let deadline = Instant::now() + Duration::from_secs(4);
    while !engine.finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        engine.finished(),
        "the track did not end (state: {})",
        engine.state()
    );
    assert_eq!(engine.state(), PlayState::Stopped);

    // The position must not exceed the duration and must stay close to it.
    let position = engine.position_ms();
    assert!(
        position >= duration.saturating_sub(150),
        "the position ({position}ms) should have reached the duration ({duration}ms)"
    );

    assert_eq!(
        engine.take_error(),
        None,
        "decoding should have finished without errors"
    );
}

#[test]
fn pausing_freezes_the_position() {
    let Some(engine) = engine_for("Test Artist - Mp3 Track.mp3") else {
        return;
    };

    let deadline = Instant::now() + Duration::from_secs(3);
    while engine.position_ms() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if engine.position_ms() == 0 {
        eprintln!("the audio did not flow — skipping the test");
        return;
    }

    engine.pause();
    assert_eq!(engine.state(), PlayState::Paused);
    let paused_at = engine.position_ms();

    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        engine.position_ms(),
        paused_at,
        "the position must not advance while paused"
    );

    engine.resume();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        engine.position_ms() > paused_at,
        "it must advance after resuming"
    );
}

/// D-024's claim: while two tracks play back to back **the output never
/// stops.**
///
/// In the old design a new cpal stream and a new decoder were set up for
/// every track; that was the gap. Now the stream stays open and the next
/// track's samples are appended behind the finished one.
#[test]
fn two_tracks_play_back_to_back_without_the_output_ever_stopping() {
    if !has_output_device() {
        eprintln!("no audio output — skipping the gapless test (this is not a failure)");
        return;
    }
    let engine = match AudioEngine::open() {
        Ok(engine) => engine,
        Err(err) => {
            eprintln!(
                "could not open the audio device, skipping the test:\n{}",
                err.chain_text()
            );
            return;
        }
    };

    let first = local(&fixture("tagged.flac"));
    let second = local(&fixture("Test Artist - Mp3 Track.mp3"));

    let seq0 = engine
        .play_source(&first)
        .expect("the first track must open");
    // The second track is queued **while** the first plays: the condition for
    // gapless.
    let seq1 = engine.enqueue(&second);
    assert_ne!(seq0, seq1);

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut switched = false;
    let mut stopped_before_switch = false;
    while Instant::now() < deadline {
        if engine.current_seq() == Some(seq1) {
            switched = true;
            break;
        }
        // The output must not look **stopped** before the transition is heard.
        if engine.state() == PlayState::Stopped {
            stopped_before_switch = true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    assert!(
        switched,
        "it should have moved on to the second track (state: {})",
        engine.state()
    );
    assert!(
        !stopped_before_switch,
        "the output stopped at the transition — gapless is broken"
    );
    assert!(
        !engine.finished(),
        "the engine must not count as finished while a track is queued"
    );

    // The finished track's scrobble must see its own length, not the output's
    // total.
    let played = engine
        .played_ms_of(seq0)
        .expect("the first slice must be known");
    assert!(
        (900..=1200).contains(&played),
        "the 1-second track should have played in full: {played}ms"
    );
    // The new track's position must be counted **from the start**.
    assert!(
        engine.position_ms() < 900,
        "the second track must start from the beginning: {}ms",
        engine.position_ms()
    );
}

/// D-077, wired end to end: in a playing session the read-ahead thread makes
/// the next track ready, "next" starts from it, and "previous" finds the
/// address the session kept — the counts say so.
#[tokio::test]
async fn next_and_previous_start_from_what_was_read_ahead() {
    if !has_output_device() {
        eprintln!("no audio output — skipping the read-ahead test (this is not a failure)");
        return;
    }
    let dir = support::TempDir::new("read-ahead");
    let session = Session::open(Config::with_data_dir(dir.path())).unwrap();
    let mut registry = ProviderRegistry::new();
    registry.register(Arc::new(LocalProvider::new(vec![fixture("")])));
    let items: Vec<QueueItem> = [
        "tagged.flac",
        "Test Artist - Mp3 Track.mp3",
        "Other Artist - Ogg Track.ogg",
        "fingerprint_sample.flac",
    ]
    .iter()
    .map(|name| QueueItem {
        id: ProviderTrackId::new(ProviderId::new("local"), fixture(name).to_string_lossy()),
        track: TrackRef::new("Artist", *name),
    })
    .collect();

    let mut player = Player::new(registry.clone());
    if let Err(err) = player.play_items(items.clone()).await {
        eprintln!(
            "could not open the audio device, skipping the test:\n{}",
            err.chain_text()
        );
        return;
    }
    // Paused, the first track does not end on its own while the test looks.
    player.pause();
    let mut live = LiveSession::new(session, player);

    // A round hands the plan over; the thread makes the next track ready.
    let deadline = Instant::now() + Duration::from_secs(5);
    let planned = loop {
        live.tick().await.unwrap();
        let report = live.prefetch_report().unwrap();
        if report
            .items
            .iter()
            .all(|item| matches!(item.status, PrefetchStatus::Ready { .. }))
            && !report.items.is_empty()
            || Instant::now() > deadline
        {
            break report;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let reasons: Vec<PlanReason> = planned.items.iter().map(|item| item.reason).collect();
    assert_eq!(
        reasons,
        [PlanReason::Next, PlanReason::AfterNext, PlanReason::Likely],
        "{planned:?}"
    );
    assert_eq!(
        planned.items[2].why.as_deref(),
        Some("the queue's own continuation"),
        "with no history and no habits yet, the third is where the queue goes on"
    );

    live.player_mut().next().await.unwrap();
    let report = live.prefetch_report().unwrap();
    let counters = report.counters;
    assert!(
        counters.hits >= 1,
        "next did not start from the read-ahead: {counters:?}"
    );
    assert_eq!(counters.misses, 0, "{counters:?}");
    assert_eq!(
        report.habits.skipped, 1,
        "a track left at once is a skip: {report:?}"
    );

    live.player_mut().previous().await.unwrap();
    let counters = live.prefetch_report().unwrap().counters;
    assert!(
        counters.hits >= 2,
        "previous did not find the kept address: {counters:?}"
    );
    assert_eq!(counters.misses, 0, "{counters:?}");

    // A new search replaces the player; the session's read-ahead stays with
    // the session, and what it kept goes on to the new queue.
    let mut replacement = Player::new(registry);
    replacement.queue_mut().replace(vec![items[1].clone()]);
    live.replace_player(replacement);
    let kept = live
        .prefetch_report()
        .expect("the new player reads ahead with the session's thread");
    assert_eq!(
        kept.counters, counters,
        "the session's counts went with the old player"
    );
    live.player_mut().stop();
}

#[test]
fn a_corrupt_file_fails_with_the_decode_stage() {
    // It works without an audio device too: the error is at the decoding
    // stage, before the output.
    let err =
        AudioEngine::play_file(&fixture("corrupt.flac")).expect_err("a corrupt file must not open");
    assert_eq!(err.stage(), headshell_core::diag::Stage::PlaybackDecode);
}

#[test]
fn a_missing_file_names_the_path() {
    let err = AudioEngine::play_file(&fixture("missing.flac")).expect_err("a missing file");
    assert_eq!(err.stage(), headshell_core::diag::Stage::PlaybackDecode);
    assert!(
        err.chain_text().contains("missing.flac"),
        "the error must name the file:\n{}",
        err.chain_text()
    );
}

/// D-086: the music hands its sound to a video and takes it back where the
/// video was — the track is opened again at that second, and the anchor
/// says so.
#[tokio::test]
async fn the_music_goes_on_from_where_its_video_was() {
    if !has_output_device() {
        eprintln!("no audio output — skipping the video hand-over test (this is not a failure)");
        return;
    }
    let mut registry = ProviderRegistry::new();
    registry.register(Arc::new(LocalProvider::new(vec![fixture("")])));
    let item = QueueItem {
        id: ProviderTrackId::new(
            ProviderId::new("local"),
            fixture("fingerprint_sample.flac").to_string_lossy(),
        ),
        track: TrackRef::new("Artist", "Chords"),
    };
    let mut player = Player::new(registry);
    if let Err(err) = player.play_items(vec![item]).await {
        eprintln!(
            "could not open the audio device, skipping the test:\n{}",
            err.chain_text()
        );
        return;
    }

    player
        .show_video(
            headshell_core::video::VideoSource::Youtube {
                video_id: "CCHdMIEGaaM".to_owned(),
            },
            ProviderId::new("tube"),
            1_000,
        )
        .unwrap();
    assert_eq!(player.surface(), headshell_core::playback::Surface::Video);
    assert!(
        !player.video_now().unwrap().only,
        "the track has its own sound"
    );
    player
        .video_report(1_000, PlayState::Playing, None)
        .unwrap();

    player.show_audio(9_000).unwrap();
    assert_eq!(player.surface(), headshell_core::playback::Surface::Audio);
    let position = player.anchor().position_ms;
    // A coarse seek lands on a packet near the second: FLAC frames here are
    // a fraction of a second long.
    assert!(
        (8_000..=10_500).contains(&position),
        "the music should pick up near 9 s, it is at {position} ms"
    );
}
