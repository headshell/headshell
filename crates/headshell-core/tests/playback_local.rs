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
// The device's turn is held across the async tests' awaits on purpose: each
// `#[tokio::test]` runs on its own thread and runtime, so nothing waits on
// the holder.
#![allow(clippy::await_holding_lock)]

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

/// The machine's one sound device: the tests that open it take turns. The
/// tests of one binary run on parallel threads, and two streams opened at
/// once on ALSA's hardware device failed with "I/O error (5)" — once a few
/// runs, more often after D-090 added two more such tests.
fn device_turn() -> std::sync::MutexGuard<'static, ()> {
    static DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    let _turn = device_turn();
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

    // Wait until the track ends; a 1 s file must end within 4 s. `Stopped`
    // comes a buffer after the buffer drains — the callback that drains it
    // still says `Playing` — so both are waited for (`remote_http.rs`, the
    // same race on macOS CI's ~450 ms buffers).
    let deadline = Instant::now() + Duration::from_secs(4);
    while !(engine.finished() && engine.state() == PlayState::Stopped) && Instant::now() < deadline
    {
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
    let _turn = device_turn();
    // The 15 s track: a CI machine plays in long buffers, and the 2 s one
    // could run out between the steps below.
    let Some(engine) = engine_for("fingerprint_sample.flac") else {
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
    // A buffer the output took just before the pause is still counted as it
    // finishes; one callback later the position holds (D-090).
    std::thread::sleep(Duration::from_millis(60));
    let paused_at = engine.position_ms();

    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        engine.position_ms(),
        paused_at,
        "the position must not advance while paused"
    );
    assert_eq!(
        engine.state(),
        PlayState::Paused,
        "the output must not take the pause back"
    );

    // The output counts a buffer when it takes one, and macOS CI's buffers
    // are long — 450 ms of position arrived in one step there: the next one
    // may come a while after the resume. Waited for, not timed.
    engine.resume();
    let deadline = Instant::now() + Duration::from_secs(2);
    while engine.position_ms() <= paused_at && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        engine.position_ms() > paused_at,
        "it must advance after resuming (still at {} ms after 2 s)",
        engine.position_ms()
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
    let _turn = device_turn();
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
    let _turn = device_turn();
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
    let _turn = device_turn();
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

/// D-090: the user moves in the playing track. The track opens again at the
/// second asked for, a paused one stays paused, a step goes from where it
/// is, and the session's next report says the position jumped — once.
/// Past the end the track ends and the queue moves on, as when it ends by
/// itself, and the report says the track changed.
#[tokio::test]
async fn the_user_moves_in_the_track_and_past_its_end() {
    let _turn = device_turn();
    if !has_output_device() {
        eprintln!("no audio output — skipping the seek test (this is not a failure)");
        return;
    }
    let dir = support::TempDir::new("seek");
    let session = Session::open(Config::with_data_dir(dir.path())).unwrap();
    let mut registry = ProviderRegistry::new();
    registry.register(Arc::new(LocalProvider::new(vec![fixture("")])));
    let items: Vec<QueueItem> = ["fingerprint_sample.flac", "tagged.flac"]
        .iter()
        .map(|name| QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), fixture(name).to_string_lossy()),
            track: TrackRef::new("Artist", *name),
        })
        .collect();
    let mut player = Player::new(registry);
    if let Err(err) = player.play_items(items).await {
        eprintln!(
            "could not open the audio device, skipping the test:\n{}",
            err.chain_text()
        );
        return;
    }
    // Paused, the position holds still while the test looks.
    player.pause();
    let mut live = LiveSession::new(session, player);
    let quiet = live.tick().await.unwrap();
    assert!(!quiet.moved && !quiet.track_changed, "{quiet:?}");

    live.player_mut().seek(9_000).await.unwrap();
    let anchor = live.player().anchor();
    // A coarse seek lands on a packet just before the second: FLAC frames
    // here are a fraction of a second long.
    assert!(
        (8_500..=9_000).contains(&anchor.position_ms),
        "the track should be near 9 s, it is at {} ms",
        anchor.position_ms
    );
    assert_eq!(
        anchor.state,
        PlayState::Paused,
        "a paused track stays paused"
    );
    let report = live.tick().await.unwrap();
    assert!(report.moved, "the jump must reach the shell: {report:?}");
    assert!(!report.track_changed, "{report:?}");
    assert!(!live.tick().await.unwrap().moved, "a jump is said once");

    live.player_mut()
        .seek_by(-headshell_core::playback::SEEK_STEP_MS)
        .await
        .unwrap();
    let back = live.player().anchor().position_ms;
    assert!(
        (3_500..=4_000).contains(&back),
        "a step back from ~9 s lands near 4 s, it is at {back} ms"
    );
    live.player_mut().seek_by(-60_000).await.unwrap();
    assert!(
        live.player().anchor().position_ms < 100,
        "back past the start is the start"
    );

    let at = live.player().queue().position();
    live.player_mut().seek(15_000).await.unwrap();
    assert_eq!(
        live.player().queue().position(),
        at + 1,
        "the end of the track moves the queue on"
    );
    assert_eq!(
        live.player()
            .current_track()
            .map(|track| track.title.as_str()),
        Some("tagged.flac")
    );
    let report = live.tick().await.unwrap();
    assert!(
        report.track_changed,
        "a seek past the end changes the track: {report:?}"
    );
    live.player_mut().stop();
}

/// D-090 with a video shown (D-086): the video's player makes the sound, so
/// a seek moves the video's clock — the shell moves its player to the
/// anchor — and past the video's end it stops at the end: the video ends as
/// videos do, and the shell says so.
#[tokio::test]
async fn a_seek_moves_the_videos_clock_while_a_video_is_shown() {
    let _turn = device_turn();
    if !has_output_device() {
        eprintln!("no audio output — skipping the video seek test (this is not a failure)");
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
    player
        .video_report(1_000, PlayState::Paused, Some(60_000))
        .unwrap();
    let before = player.seeks();

    player.seek(30_000).await.unwrap();
    assert_eq!(player.surface(), headshell_core::playback::Surface::Video);
    assert_eq!(player.anchor().position_ms, 30_000);
    assert_eq!(
        player.seeks(),
        before + 1,
        "the jump is counted for the shell"
    );

    player.seek(90_000).await.unwrap();
    assert_eq!(
        player.anchor().position_ms,
        60_000,
        "past the video's end is its end"
    );
    assert_eq!(
        player.queue().position(),
        0,
        "the video ends the track, not the seek"
    );
    player.stop();
}
