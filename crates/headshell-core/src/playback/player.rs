//! The player: queue + audio engine + scrobbling (PLAN §1.5, §1.6).
//!
//! The state is kept here; the CLI and the GUI only read [`Player::anchor`]
//! (D-015).
//!
//! ## Scrobbling (§1.6)
//!
//! When a track ends or is abandoned, a [`Listen`] is produced if it passed
//! [`PlayRule`], and it is written **to the same table as imported data**:
//! the past and today become a single timeline. The rule is the single
//! definition from D-008 — there is no second interpretation of the threshold
//! here.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::ids::{CanonicalId, ProviderId, ProviderTrackId};
use crate::model::{Listen, ListenSource, PlayRule, TrackRef};
use crate::provider::{AudioSource, Capabilities, Provider, ProviderRegistry};
use crate::video::{VideoNow, VideoSource};

use super::anchor::{PlayState, PlaybackAnchor};
use super::prefetch;
#[cfg(feature = "audio")]
use super::prefetch::{PlanItem, PlanReason, Prefetcher, Prepared};
use super::queue::{Queue, QueueItem};

/// The tracking record of the track being played.
///
/// The least information needed to produce a scrobble: what, when it started,
/// how long it played. The duration is read from the engine — the answer to
/// "how long did the user listen" is the audio sent to the output, not the
/// wall-clock time that passed (pauses do not count).
#[derive(Debug, Clone)]
struct NowPlaying {
    item: QueueItem,
    canonical_id: Option<CanonicalId>,
    started_at: jiff::Timestamp,
    provider: ProviderId,
    /// The segment number in the engine (D-024). This is how we tell that a
    /// transition was **heard**: if the engine moved on to another segment, the
    /// track has changed.
    #[cfg(feature = "audio")]
    seq: u64,
    /// The address it plays from: kept for "previous" and a replay (D-077).
    /// `None` for a track that is a video only (D-086): the engine never
    /// opens it.
    #[cfg(feature = "audio")]
    source: Option<AudioSource>,
    /// Time heard before the engine's present segment (D-086): a segment
    /// left behind by a jump, and the video's share. The listen adds it.
    carried_ms: u64,
}

/// Who makes the sound of the playing track (D-086).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    /// The core's audio engine.
    Audio,
    /// The shell's video player; the engine waits, paused.
    Video,
}

/// A video being shown, with the clock its player reports (D-086).
#[derive(Debug, Clone)]
struct VideoShown {
    now: VideoNow,
    position_ms: u64,
    state: PlayState,
    duration_ms: Option<u64>,
    /// When the last report came, by the monotonic clock: what the position
    /// is carried forward from, and what tells a jump from playing.
    reported_at: std::time::Instant,
    /// Heard while shown, from the reports.
    played_ms: u64,
}

/// How far ahead of the wall clock a report may move and still count as
/// played rather than a jump: the player reports about once a second, and
/// its clock and ours tick apart.
const VIDEO_REPORT_SLACK_MS: u64 = 1_500;

/// Taking the sound back from a video, a gap smaller than this is not worth
/// opening the track again for (D-086): the music goes on from where it
/// paused.
#[cfg(feature = "audio")]
const SEEK_SLACK_MS: u64 = 1_500;

/// The error for a video call while nothing plays.
fn nothing_plays() -> Error {
    Error::new(
        Stage::VideoPlayback,
        ErrorKind::InvalidInput {
            detail: "nothing is playing".to_owned(),
        },
    )
}

/// The player.
///
/// The audio engine is behind the `audio` feature; with it off the queue and
/// the anchor work, and [`Player::play`] returns an explicit error (rather
/// than silently doing nothing).
pub struct Player {
    queue: Queue,
    providers: ProviderRegistry,
    rule: PlayRule,
    now_playing: Option<NowPlaying>,
    /// Listen records that have finished but not yet been collected.
    pending_listens: Vec<Listen>,
    #[cfg(feature = "audio")]
    engine: Option<super::engine::AudioEngine>,
    /// The next track handed to the engine ahead of time (D-024): segment
    /// number, item and the address it plays from. It becomes `now_playing`
    /// when the transition is heard.
    #[cfg(feature = "audio")]
    prefetched: Option<(u64, QueueItem, AudioSource)>,
    /// Reads the queue ahead (D-077). A playing session hands its own over;
    /// a player on its own starts one when it first has a plan.
    #[cfg(feature = "audio")]
    prefetch: Option<Prefetcher>,
    /// The plan last handed over — its tracks and the queue position — so
    /// the same plan is not handed over every round.
    #[cfg(feature = "audio")]
    planned: Option<(Vec<ProviderTrackId>, usize)>,
    /// Streams that asked for a new address: slice, track, since when.
    #[cfg(feature = "audio")]
    refreshing: Vec<(u64, QueueItem, std::time::Instant)>,
    /// How the user moves through the queue this session (D-077).
    habits: super::prefetch::Habits,
    /// How each queued track and its artist were listened to, as the
    /// playing session read it from the history (D-077).
    history: std::collections::HashMap<ProviderTrackId, crate::library::ListeningProfile>,
    /// The video being shown (D-086); `None`: the engine makes the sound.
    video: Option<VideoShown>,
    /// The user watches rather than listens (D-086): a track started now
    /// opens paused, and waits for the shell to show its video.
    video_mode: bool,
    /// Can the shell show videos (D-086)? Off: a track that is a video only
    /// is passed by.
    shows_video: bool,
    /// Tracks passed by because they are videos only and nothing here can
    /// show them — counted, not dropped silently (K9).
    video_only_skipped: usize,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("queue_len", &self.queue.len())
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Sets up a player with a provider registry.
    #[must_use]
    pub fn new(providers: ProviderRegistry) -> Self {
        Self {
            queue: Queue::new(),
            providers,
            rule: PlayRule::default(),
            now_playing: None,
            pending_listens: Vec::new(),
            #[cfg(feature = "audio")]
            engine: None,
            #[cfg(feature = "audio")]
            prefetched: None,
            #[cfg(feature = "audio")]
            prefetch: None,
            #[cfg(feature = "audio")]
            planned: None,
            #[cfg(feature = "audio")]
            refreshing: Vec::new(),
            habits: super::prefetch::Habits::default(),
            history: std::collections::HashMap::new(),
            video: None,
            video_mode: false,
            shows_video: false,
            video_only_skipped: 0,
        }
    }

    /// How the user moved through queues in this session (D-077).
    #[must_use]
    pub fn habits(&self) -> super::prefetch::Habits {
        self.habits
    }

    /// Carries on counting from `habits`: a new search replaces the player,
    /// not the listener.
    pub(crate) fn adopt_habits(&mut self, habits: super::prefetch::Habits) {
        self.habits = habits;
    }

    /// The queue's listening history, read by the playing session (D-077).
    pub(crate) fn set_history(
        &mut self,
        history: std::collections::HashMap<ProviderTrackId, crate::library::ListeningProfile>,
    ) {
        self.history = history;
        #[cfg(feature = "audio")]
        {
            self.planned = None;
        }
    }

    /// Does the playing track count as played yet (D-008's rule)? What tells
    /// leaving it apart from skipping it.
    fn current_counts(&self) -> bool {
        let Some(now) = &self.now_playing else {
            return false;
        };
        self.rule.counts(self.heard_ms(now), self.duration_of(now))
    }

    /// How long the track was heard: by the engine's present segment, by the
    /// ones a jump left behind, and by its video (D-086).
    fn heard_ms(&self, now: &NowPlaying) -> u64 {
        #[cfg(feature = "audio")]
        let engine = match (&self.engine, &now.source) {
            (Some(engine), Some(_)) => engine
                .played_ms_of(now.seq)
                .unwrap_or_else(|| engine.played_ms_now()),
            _ => 0,
        };
        #[cfg(not(feature = "audio"))]
        let engine = 0;
        let video = self.video.as_ref().map_or(0, |video| video.played_ms);
        now.carried_ms.saturating_add(engine).saturating_add(video)
    }

    /// The track's whole length: the engine's reading of the container
    /// first — the catalog is fed from tags and may not know it — then the
    /// video player's, then the catalog's.
    fn duration_of(&self, now: &NowPlaying) -> Option<u64> {
        #[cfg(feature = "audio")]
        let engine = match (&self.engine, &now.source) {
            (Some(engine), Some(_)) => engine.duration_of(now.seq),
            _ => None,
        };
        #[cfg(not(feature = "audio"))]
        let engine = None;
        engine
            .or_else(|| self.video.as_ref().and_then(|video| video.duration_ms))
            .or(now.item.track.duration_ms)
    }

    /// Reads the queue ahead with `prefetcher` from now on (D-077): a playing
    /// session shares one across the players it replaces, so an address
    /// resolved for one queue is not resolved again for the next.
    #[cfg(feature = "audio")]
    pub(crate) fn attach_prefetcher(&mut self, prefetcher: Prefetcher) {
        if let Some(now) = &self.now_playing
            && let Some(source) = &now.source
        {
            prefetcher.remember(&now.item.id, source);
        }
        self.prefetch = Some(prefetcher);
        self.planned = None;
    }

    /// What is being read ahead, and how it went (D-077).
    ///
    /// # Errors
    /// If nothing reads ahead in this player — "not started" is not "nothing
    /// planned" (K9) — or its lock is poisoned.
    pub fn prefetch_report(&self) -> Result<crate::playback::PrefetchReport> {
        #[cfg(feature = "audio")]
        if let Some(prefetch) = &self.prefetch {
            let mut report = prefetch.report()?;
            report.habits = self.habits;
            return Ok(report);
        }
        Err(Error::new(
            Stage::PlaybackResolve,
            ErrorKind::Audio {
                detail: "nothing reads ahead in this player (no queue has played yet)".to_owned(),
            },
        ))
    }

    /// Changes the "counted play" rule (the single definition from D-008).
    #[must_use]
    pub fn with_play_rule(mut self, rule: PlayRule) -> Self {
        self.rule = rule;
        self
    }

    /// The providers this player plays from — the cover worker asks the same
    /// ones (D-076).
    #[must_use]
    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }

    /// The queue (read).
    #[must_use]
    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// The queue (write). A change does not stop the playing track.
    pub fn queue_mut(&mut self) -> &mut Queue {
        &mut self.queue
    }

    /// The current state. While a video is shown, its player's (D-086).
    #[must_use]
    pub fn state(&self) -> PlayState {
        if let Some(video) = &self.video {
            return video.state;
        }
        #[cfg(feature = "audio")]
        {
            if let Some(engine) = &self.engine {
                return engine.state();
            }
        }
        PlayState::Stopped
    }

    /// Who makes the sound now (D-086).
    #[must_use]
    pub fn surface(&self) -> Surface {
        if self.video.is_some() {
            Surface::Video
        } else {
            Surface::Audio
        }
    }

    /// The video being shown (D-086).
    #[must_use]
    pub fn video_now(&self) -> Option<&VideoNow> {
        self.video.as_ref().map(|video| &video.now)
    }

    /// Is the user watching rather than listening (D-086)?
    #[must_use]
    pub fn video_mode(&self) -> bool {
        self.video_mode
    }

    /// The single representation of state (D-015). The consumer computes the
    /// position from this.
    #[must_use]
    pub fn anchor(&self) -> PlaybackAnchor {
        let state = self.state();
        #[cfg(feature = "audio")]
        let (position_ms, duration_ms) = self
            .engine
            .as_ref()
            .map_or((0, None), |e| (e.position_ms(), e.duration_ms()));
        #[cfg(not(feature = "audio"))]
        let (position_ms, duration_ms) = (0, None);
        // A video's clock is its player's: the last report, carried forward
        // while it plays (D-086).
        let (position_ms, duration_ms) = match &self.video {
            Some(video) => {
                let since = if video.state.advances() {
                    u64::try_from(video.reported_at.elapsed().as_millis()).unwrap_or(u64::MAX)
                } else {
                    0
                };
                (
                    video.position_ms.saturating_add(since),
                    video.duration_ms.or(duration_ms),
                )
            }
            None => (position_ms, duration_ms),
        };

        PlaybackAnchor {
            track: self
                .now_playing
                .as_ref()
                .and_then(|np| np.canonical_id.clone()),
            wall_time: jiff::Timestamp::now(),
            position_ms,
            rate: if state.advances() { 1.0 } else { 0.0 },
            state,
            duration_ms: duration_ms.or_else(|| {
                self.now_playing
                    .as_ref()
                    .and_then(|np| np.item.track.duration_ms)
            }),
        }
    }

    /// The metadata of the playing track.
    #[must_use]
    pub fn current_track(&self) -> Option<&TrackRef> {
        self.now_playing.as_ref().map(|np| &np.item.track)
    }

    /// The playing track with its provider id — what its lyrics are looked
    /// up by (D-078).
    #[must_use]
    pub fn current_item(&self) -> Option<&QueueItem> {
        self.now_playing.as_ref().map(|np| &np.item)
    }

    /// Replaces the queue and starts playing from the start.
    ///
    /// # Errors
    /// If the queue is empty or the first track cannot be played.
    pub async fn play_items(&mut self, items: Vec<QueueItem>) -> Result<()> {
        if items.is_empty() {
            return Err(Error::new(
                Stage::PlaybackResolve,
                ErrorKind::InvalidInput {
                    detail: "there is no track to play".to_owned(),
                },
            ));
        }
        self.queue.replace(items);
        self.start_current().await
    }

    /// Adds tracks at the end of the queue (D-080) — with shuffle on too, the
    /// play order gets them last. Into an empty queue they start playing:
    /// there is nothing for them to come after.
    ///
    /// The read-ahead sees them at the next tick; its plan is rebuilt from
    /// the queue every round (D-077).
    ///
    /// # Errors
    /// If there is no track, or the queue was empty and the first one cannot
    /// be played.
    pub async fn enqueue(&mut self, items: Vec<QueueItem>) -> Result<()> {
        if items.is_empty() {
            return Err(Error::new(
                Stage::PlaybackResolve,
                ErrorKind::InvalidInput {
                    detail: "there is no track to add to the queue".to_owned(),
                },
            ));
        }
        if self.queue.is_empty() {
            return self.play_items(items).await;
        }
        self.queue.append(items);
        Ok(())
    }

    /// Plays the current track in the queue.
    ///
    /// A track that is a video only (D-086) is shown when the shell can show
    /// videos ([`Self::set_shows_video`]); one that cannot — the CLI, the
    /// TUI — passes it by, counted in [`Self::video_only_skipped`], and plays
    /// the next one.
    ///
    /// # Errors
    /// If the queue is empty, the provider cannot be found or the audio pipeline
    /// cannot be set up; or every track left is a video only and nothing here
    /// can show one.
    pub async fn start_current(&mut self) -> Result<()> {
        // Close the playing track: the half-finished listen record should be
        // produced.
        self.finish_current_listen();
        let mut passed = 0;
        loop {
            let item = self.queue.current().cloned().ok_or_else(|| {
                Error::new(
                    Stage::PlaybackResolve,
                    ErrorKind::NotFound {
                        what: "a track to play in the queue".to_owned(),
                    },
                )
            })?;
            let video_only = self.provider_has(&item.id, Capabilities::VIDEO)
                && !self.provider_has(&item.id, Capabilities::STREAM);
            if !video_only || self.shows_video {
                return self.start_item(item, video_only).await;
            }
            self.video_only_skipped += 1;
            passed += 1;
            if passed >= self.queue.len() || self.queue.next().is_none() {
                self.stop();
                return Err(Error::new(
                    Stage::PlaybackResolve,
                    ErrorKind::Unsupported {
                        provider: item.id.provider.to_string(),
                        what: "playing a video here — its tracks are videos only, and they play in the app's window".to_owned(),
                        capabilities: self
                            .providers
                            .get(&item.id.provider)
                            .map(|provider| provider.info().capabilities.describe())
                            .unwrap_or_default(),
                    },
                ));
            }
        }
    }

    /// Videos only passed by because nothing here can show them (D-086).
    #[must_use]
    pub fn video_only_skipped(&self) -> usize {
        self.video_only_skipped
    }

    /// Can the shell show videos (D-086)? The desktop can; with it off — the
    /// default — a track that is a video only is passed by, and a sound that
    /// cannot be decoded is the error it is.
    pub fn set_shows_video(&mut self, shows: bool) {
        self.shows_video = shows;
    }

    /// Can the shell show videos?
    #[must_use]
    pub fn shows_video(&self) -> bool {
        self.shows_video
    }

    /// Starts one track: its video when that is all there is, its sound
    /// otherwise.
    async fn start_item(&mut self, item: QueueItem, video_only: bool) -> Result<()> {
        // A track that is a video only never opens the engine (D-086).
        if video_only {
            return self.start_video_only(item).await;
        }

        // Read ahead (D-077): the track may be ready, or being made ready at
        // this moment — then it is waited for, not resolved a second time.
        #[cfg(feature = "audio")]
        if let Some(prepared) = self.prefetch.as_ref().and_then(|p| p.for_start(&item.id)) {
            match self.start_prepared(&prepared, item.clone()) {
                Ok(()) => return Ok(()),
                // The address kept for it no longer opens: resolved again below.
                Err(err) => {
                    tracing::info!(
                        track = item.track.display_name(),
                        error = %err.chain_text().replace('\n', " "),
                        "the address read ahead no longer opens; resolving it again"
                    );
                    if let Some(prefetch) = &self.prefetch {
                        prefetch.forget_stale(&item.id);
                    }
                }
            }
        }

        let source = self.resolve_source(&item.id).await?;
        #[cfg(feature = "audio")]
        if let Some(prefetch) = &self.prefetch {
            prefetch.remember(&item.id, &source);
        }
        match self.start_source(&source, item.clone()) {
            Ok(()) => Ok(()),
            // A sound the core cannot decode — Opus in a `.webm` — is not the
            // end of a video: shown alone, the video is the sound (D-086).
            // When it has none either, the decoding error is the answer.
            Err(err)
                if err.stage() == Stage::PlaybackDecode
                    && self.shows_video
                    && self.provider_has(&item.id, Capabilities::VIDEO) =>
            {
                match self.start_video_only(item).await {
                    Ok(()) => {
                        tracing::info!(
                            error = %err.chain_text().replace('\n', " "),
                            "the sound could not be decoded; the video plays alone"
                        );
                        Ok(())
                    }
                    Err(_) => Err(err),
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Does the provider of `id` have `capability`?
    fn provider_has(&self, id: &ProviderTrackId, capability: Capabilities) -> bool {
        self.providers
            .get(&id.provider)
            .is_some_and(|provider| provider.info().capabilities.contains(capability))
    }

    /// Starts a track whose video is all there is (D-086): the engine stays
    /// shut, the shell's player makes the sound and reports its clock.
    async fn start_video_only(&mut self, item: QueueItem) -> Result<()> {
        let provider = self.providers.get(&item.id.provider).ok_or_else(|| {
            Error::new(
                Stage::VideoLookup,
                ErrorKind::NotFound {
                    what: format!("provider {:?}", item.id.provider.as_str()),
                },
            )
        })?;
        let source = provider.resolve_video(&item.id).await?.ok_or_else(|| {
            Error::new(
                Stage::VideoLookup,
                ErrorKind::NotFound {
                    what: format!("a video for {}", item.track.display_name()),
                },
            )
        })?;
        source.check()?;
        #[cfg(feature = "audio")]
        {
            self.engine = None;
            self.prefetched = None;
            self.refreshing.clear();
        }
        self.video = Some(VideoShown {
            now: VideoNow {
                source,
                provider: item.id.provider.clone(),
                only: true,
            },
            position_ms: 0,
            state: PlayState::Buffering,
            duration_ms: item.track.duration_ms.filter(|ms| *ms > 0),
            reported_at: std::time::Instant::now(),
            played_ms: 0,
        });
        self.now_playing = Some(NowPlaying {
            provider: item.id.provider.clone(),
            canonical_id: None,
            started_at: jiff::Timestamp::now(),
            item,
            #[cfg(feature = "audio")]
            seq: 0,
            #[cfg(feature = "audio")]
            source: None,
            carried_ms: 0,
        });
        Ok(())
    }

    /// Hands the sound to the shell's video player (D-086): the engine
    /// pauses where it is, and the video's reports are the clock from now
    /// on. `at_ms` is where the shell starts the video — the music's
    /// position, as the anchor said.
    ///
    /// # Errors
    /// If nothing plays, or the source is not one a shell can show.
    pub fn show_video(
        &mut self,
        source: VideoSource,
        provider: ProviderId,
        at_ms: u64,
    ) -> Result<()> {
        source.check()?;
        let Some(now) = &self.now_playing else {
            return Err(nothing_plays());
        };
        #[cfg(feature = "audio")]
        let only = now.source.is_none();
        #[cfg(not(feature = "audio"))]
        let only = true;
        let duration_ms = self
            .video
            .as_ref()
            .and_then(|video| video.duration_ms)
            .or(now.item.track.duration_ms);
        #[cfg(feature = "audio")]
        if let Some(engine) = &self.engine {
            engine.pause();
        }
        // A video shown before for this track (a refusal moved to the next
        // match) leaves its heard time with the track.
        if let (Some(old), Some(now)) = (self.video.take(), self.now_playing.as_mut()) {
            now.carried_ms = now.carried_ms.saturating_add(old.played_ms);
        }
        self.video = Some(VideoShown {
            now: VideoNow {
                source,
                provider,
                only,
            },
            position_ms: at_ms,
            state: PlayState::Buffering,
            duration_ms,
            reported_at: std::time::Instant::now(),
            played_ms: 0,
        });
        self.video_mode = true;
        Ok(())
    }

    /// The video player's clock (D-086), reported about once a second and
    /// at every change. What it played counts toward the listen; a jump
    /// does not.
    ///
    /// # Errors
    /// If no video is shown — a report that came too late, after the track
    /// moved on — so the shell can drop it.
    pub fn video_report(
        &mut self,
        position_ms: u64,
        state: PlayState,
        duration_ms: Option<u64>,
    ) -> Result<()> {
        let Some(video) = self.video.as_mut() else {
            return Err(Error::new(
                Stage::VideoPlayback,
                ErrorKind::InvalidInput {
                    detail: "no video is shown; the report is late".to_owned(),
                },
            ));
        };
        if video.state.advances() {
            let wall = u64::try_from(video.reported_at.elapsed().as_millis()).unwrap_or(u64::MAX);
            let moved = position_ms.saturating_sub(video.position_ms);
            if position_ms >= video.position_ms
                && moved <= wall.saturating_add(VIDEO_REPORT_SLACK_MS)
            {
                video.played_ms = video.played_ms.saturating_add(moved);
            }
        }
        video.position_ms = position_ms;
        video.state = state;
        if let Some(duration) = duration_ms.filter(|ms| *ms > 0) {
            video.duration_ms = Some(duration);
        }
        video.reported_at = std::time::Instant::now();
        Ok(())
    }

    /// Takes the sound back from the video (D-086): the music goes on from
    /// `at_ms` — where the video was. With no video shown, a track that
    /// waited for one (watching, D-086) just starts.
    ///
    /// # Errors
    /// If nothing plays, or the track has no sound of its own here — its
    /// video is all there is.
    pub fn show_audio(&mut self, at_ms: u64) -> Result<()> {
        let Some(now) = &self.now_playing else {
            return Err(nothing_plays());
        };
        #[cfg(feature = "audio")]
        let only = now.source.is_none();
        #[cfg(not(feature = "audio"))]
        let only = true;
        if only {
            return Err(Error::new(
                Stage::VideoPlayback,
                ErrorKind::Unsupported {
                    provider: now.provider.to_string(),
                    what: format!(
                        "playing {} as music — its video is all there is",
                        now.item.track.display_name()
                    ),
                    capabilities: self
                        .providers
                        .get(&now.provider)
                        .map(|provider| provider.info().capabilities.describe())
                        .unwrap_or_default(),
                },
            ));
        }
        if let (Some(old), Some(now)) = (self.video.take(), self.now_playing.as_mut()) {
            now.carried_ms = now.carried_ms.saturating_add(old.played_ms);
        }
        self.video_mode = false;
        #[cfg(feature = "audio")]
        {
            let here = self
                .engine
                .as_ref()
                .map_or(0, super::engine::AudioEngine::position_ms);
            if here.abs_diff(at_ms) > SEEK_SLACK_MS
                && let Err(err) = self.seek(at_ms)
            {
                // The music goes on from where it paused rather than not at
                // all; the jump's failure is said (K9).
                tracing::warn!(
                    at_ms,
                    error = %err.chain_text().replace('\n', " "),
                    "the music could not move to the video's position; it goes on from where it paused"
                );
            }
            if let Some(engine) = &self.engine {
                engine.resume();
            }
        }
        #[cfg(not(feature = "audio"))]
        let _ = at_ms;
        Ok(())
    }

    /// The video ran to its end (D-086): the track ended, as if its sound
    /// had, and the queue moves on. `false`: the queue is over.
    ///
    /// A report with no video shown (late, after the track moved on) is
    /// ignored: `Ok(true)`, nothing changes.
    ///
    /// # Errors
    /// If the next track cannot be played.
    pub async fn video_ended(&mut self) -> Result<bool> {
        if self.video.is_none() {
            return Ok(true);
        }
        self.habits.finished += 1;
        self.finish_current_listen();
        if self.queue.advance_after_finish().is_some() {
            self.start_current().await?;
            return Ok(true);
        }
        self.stop();
        Ok(false)
    }

    /// Moves the playing music to `at_ms` (D-086): the track is opened again
    /// there; what was heard before the jump stays with the listen.
    ///
    /// # Errors
    /// If nothing plays from the engine, or the source cannot be opened or
    /// moved.
    #[cfg(feature = "audio")]
    pub fn seek(&mut self, at_ms: u64) -> Result<()> {
        let Some(now) = &self.now_playing else {
            return Err(nothing_plays());
        };
        let Some(source) = now.source.clone() else {
            return Err(Error::new(
                Stage::PlaybackResolve,
                ErrorKind::InvalidInput {
                    detail: "the track is a video only; its player moves it".to_owned(),
                },
            ));
        };
        let (heard, paused) = self.engine.as_ref().map_or((0, false), |engine| {
            (
                engine
                    .played_ms_of(now.seq)
                    .unwrap_or_else(|| engine.played_ms_now()),
                engine.state() == PlayState::Paused,
            )
        });
        let engine = super::engine::AudioEngine::open()?;
        let seq = engine.play_source_at(&source, None, at_ms)?;
        if paused {
            engine.pause();
        }
        self.engine = Some(engine);
        self.prefetched = None;
        self.refreshing.clear();
        if let Some(now) = self.now_playing.as_mut() {
            now.carried_ms = now.carried_ms.saturating_add(heard);
            now.seq = seq;
        }
        Ok(())
    }

    /// Asks the provider for a playable source.
    ///
    /// Asking a provider without the capability for a stream is not "no
    /// results" but an explicit "I can't" (K9).
    async fn resolve_source(&self, id: &ProviderTrackId) -> Result<AudioSource> {
        let provider = prefetch::streaming_provider(&self.providers, id)?;
        provider
            .resolve_source(id)
            .await?
            .ok_or_else(|| prefetch::no_source(id))
    }

    /// Hands the source to the audio pipeline.
    ///
    /// In a build without the audio pipeline the arguments are consumed by the
    /// `let _ = (source, item);` below; we **do not add** a separate
    /// `unused_variables` expectation, because the lint never fires and the
    /// unmet expectation itself became an error (`cargo clippy -p
    /// headshell-core`).
    fn start_source(&mut self, source: &AudioSource, item: QueueItem) -> Result<()> {
        #[cfg(feature = "audio")]
        {
            let prepared = Prepared {
                source: source.clone(),
                head: None,
            };
            self.start_prepared(&prepared, item)
        }
        #[cfg(not(feature = "audio"))]
        {
            let _ = (source, item);
            Err(Error::new(
                Stage::PlaybackOutput,
                ErrorKind::Audio {
                    detail: "this build has no audio pipeline (the `audio` feature is off)"
                        .to_owned(),
                },
            ))
        }
    }

    /// Hands a source to the audio pipeline — from its head, when one was
    /// fetched ahead of time (D-077).
    #[cfg(feature = "audio")]
    fn start_prepared(&mut self, prepared: &Prepared, item: QueueItem) -> Result<()> {
        // A start at the user's request: we set the engine up **again**.
        // Gapless is only for natural transitions; when the user presses a key,
        // playing read-ahead audio would mean playing the wrong track.
        let engine = super::engine::AudioEngine::open()?;
        let seq = engine.play_source_from(&prepared.source, prepared.head.as_deref())?;
        // Watching (D-086): the track waits, paused, for its video.
        if self.video_mode {
            engine.pause();
        }
        self.engine = Some(engine);
        self.prefetched = None;
        self.refreshing.clear();
        self.video = None;

        self.now_playing = Some(NowPlaying {
            provider: item.id.provider.clone(),
            canonical_id: None,
            started_at: jiff::Timestamp::now(),
            item,
            seq,
            source: Some(prepared.source.clone()),
            carried_ms: 0,
        });
        Ok(())
    }

    /// Pauses. While a video is shown it is the shell's player that pauses
    /// (D-086); the engine is paused already, and its report says so.
    pub fn pause(&self) {
        #[cfg(feature = "audio")]
        if self.video.is_none()
            && let Some(engine) = &self.engine
        {
            engine.pause();
        }
    }

    /// Resumes. While a video is shown the shell's player resumes (D-086).
    pub fn resume(&self) {
        #[cfg(feature = "audio")]
        if self.video.is_none()
            && let Some(engine) = &self.engine
        {
            engine.resume();
        }
    }

    /// Stops playing and closes the listen record.
    pub fn stop(&mut self) {
        self.finish_current_listen();
        self.video = None;
        #[cfg(feature = "audio")]
        {
            self.engine = None;
        }
    }

    /// Moves on to the next track at the user's request.
    ///
    /// # Errors
    /// If the next track cannot be played.
    pub async fn next(&mut self) -> Result<bool> {
        if self.now_playing.is_some() {
            if self.current_counts() {
                self.habits.finished += 1;
            } else {
                self.habits.skipped += 1;
            }
        }
        if self.queue.next().is_none() {
            self.stop();
            return Ok(false);
        }
        self.start_current().await?;
        Ok(true)
    }

    /// Goes back to the previous track.
    ///
    /// # Errors
    /// If the previous track cannot be played.
    pub async fn previous(&mut self) -> Result<bool> {
        if self.queue.previous().is_none() {
            return Ok(false);
        }
        self.habits.went_back += 1;
        self.start_current().await?;
        Ok(true)
    }

    /// Jumps to a given position in the queue and plays it.
    ///
    /// The "pick from the list" behaviour of the TUI/GUI. If the position is
    /// invalid it returns `false` and the playing track is not disturbed.
    ///
    /// # Errors
    /// If the chosen track cannot be played.
    pub async fn jump_to(&mut self, position: usize) -> Result<bool> {
        if !self.queue.jump_to(position) {
            return Ok(false);
        }
        self.habits.jumped += 1;
        self.start_current().await?;
        Ok(true)
    }

    /// Resumes if paused, pauses if playing.
    ///
    /// For interfaces controlled with a single key; the state logic lives here so
    /// the TUI and the GUI do not make the same decision twice.
    pub fn toggle_pause(&self) {
        match self.state() {
            PlayState::Playing | PlayState::Buffering => self.pause(),
            PlayState::Paused => self.resume(),
            PlayState::Stopped => {}
        }
    }

    /// If the track ended naturally, moves on to the next one.
    ///
    /// The caller (the TUI loop, the GUI timer) is expected to call this
    /// regularly. `RepeatMode::One` restarts the same track here — the one place
    /// where it differs from a transition at the user's request (see
    /// [`Queue::advance_after_finish`]).
    ///
    /// # Errors
    /// If the next track cannot be played.
    pub async fn tick(&mut self) -> Result<()> {
        #[cfg(feature = "audio")]
        {
            let Some((current_seq, finished)) = self
                .engine
                .as_ref()
                .map(|engine| (engine.current_seq(), engine.finished()))
            else {
                self.sync_plan();
                return Ok(());
            };

            // — 1. Was the transition **heard**? If the engine moved on to the
            // segment we handed it ahead of time, the track has changed; the
            // audio never stopped.
            if let Some((seq, item, source)) = self.prefetched.clone()
                && current_seq == Some(seq)
            {
                self.habits.finished += 1;
                self.finish_current_listen();
                self.queue.advance_after_finish();
                self.now_playing = Some(NowPlaying {
                    provider: item.id.provider.clone(),
                    canonical_id: None,
                    started_at: jiff::Timestamp::now(),
                    item,
                    seq,
                    source: Some(source),
                    carried_ms: 0,
                });
                self.prefetched = None;
            }

            // — 2. Is there anything left to play?
            if finished {
                if let Some(engine) = &self.engine
                    && let Some(detail) = engine.take_error()
                {
                    // Do not swallow the decoding error — carry it to diagnostics (K9).
                    tracing::warn!(error = %detail, "error while playing");
                }
                if self.now_playing.is_some() {
                    self.habits.finished += 1;
                }
                self.finish_current_listen();
                // If the read-ahead could not be done (a provider error, wrapping at
                // the end of the queue) we fall back to the old way: set the engine
                // up again. There is a gap, but playback does not stop.
                if self.queue.advance_after_finish().is_some() {
                    return self.start_current().await;
                }
                self.engine = None;
                self.prefetched = None;
                self.sync_plan();
                return Ok(());
            }

            // — 3. None of this waits (D-077): the plan goes to the read-ahead
            // thread, the next track goes to the engine once it is ready, and a
            // stream whose address went stale gets a new one once it is.
            self.sync_plan();
            self.hand_over_next();
            self.refresh_stale_streams();
        }
        Ok(())
    }

    /// Hands the read-ahead thread the tracks likely to play next, when they
    /// changed (D-077). Nothing is planned while nothing plays.
    #[cfg(feature = "audio")]
    fn sync_plan(&mut self) {
        let mut plan: Vec<PlanItem> = Vec::new();
        if self.engine.is_some() {
            let position = self.queue.position();
            // Repeat one plays the same track again when it ends: that one is
            // the likeliest, then what "next" would play.
            if self.queue.repeat() == super::queue::RepeatMode::One
                && let Some(current) = self.queue.current()
            {
                plan.push(PlanItem {
                    item: current.clone(),
                    index: position,
                    reason: PlanReason::Again,
                    why: None,
                });
            }
            let reasons = [PlanReason::Next, PlanReason::AfterNext];
            plan.extend(
                self.queue
                    .upcoming(reasons.len())
                    .into_iter()
                    .zip(reasons)
                    .enumerate()
                    .map(|(step, (item, reason))| PlanItem {
                        item: item.clone(),
                        index: position + 1 + step,
                        reason,
                        why: None,
                    }),
            );
            // The third: guessed from how the user listens (D-077).
            if plan.len() < prefetch::PLAN_DEPTH {
                let order = self.queue.items();
                if let Some(pick) =
                    prefetch::likely::third(&order, position, &self.history, self.habits)
                    && let Some(item) = order.get(pick.index)
                    && !plan.iter().any(|planned| planned.item.id == item.id)
                {
                    plan.push(PlanItem {
                        item: item.clone(),
                        index: pick.index,
                        reason: PlanReason::Likely,
                        why: Some(pick.why),
                    });
                }
            }
            // A track that is a video only has no stream to read ahead
            // (D-086).
            plan.retain(|planned| self.provider_has(&planned.item.id, Capabilities::STREAM));
            plan.truncate(prefetch::PLAN_DEPTH);
        }
        let stamp = (
            plan.iter().map(|planned| planned.item.id.clone()).collect(),
            self.queue.position(),
        );
        if self.planned.as_ref() == Some(&stamp) {
            return;
        }
        if plan.is_empty() && self.prefetch.is_none() {
            return;
        }
        let prefetch = self.prefetch.get_or_insert_with(Prefetcher::new);
        if let Some(now) = &self.now_playing
            && let Some(source) = &now.source
        {
            prefetch.remember(&now.item.id, source);
        }
        prefetch.plan(plan, self.providers.clone());
        self.planned = Some(stamp);
    }

    /// Hands the next track to the engine while today's is still playing
    /// (D-024) — once the read-ahead thread has it ready, never by waiting
    /// for it here (D-077).
    #[cfg(feature = "audio")]
    fn hand_over_next(&mut self) {
        if self.prefetched.is_some() {
            return;
        }
        let Some(engine) = &self.engine else { return };
        // Early if the engine still has work waiting: the buffer is already full.
        if engine.queued_len() > 0 {
            return;
        }
        let Some(item) = self.queue.peek_after_finish().cloned() else {
            return;
        };
        let Some(prepared) = self.prefetch.as_ref().and_then(|p| p.ready(&item.id)) else {
            return;
        };
        let seq = engine.enqueue_from(&prepared.source, prepared.head.clone());
        self.prefetched = Some((seq, item, prepared.source));
    }

    /// A stream begun from its head whose address no longer answers asks for
    /// a new one (D-077): the read-ahead thread resolves it before anything
    /// else, and it is handed over the round it is ready. The head plays
    /// meanwhile.
    #[cfg(feature = "audio")]
    fn refresh_stale_streams(&mut self) {
        let Some(engine) = &self.engine else { return };
        let wanting = engine.wanting_address();
        self.refreshing.retain(|(seq, ..)| wanting.contains(seq));
        let Some(prefetch) = &self.prefetch else {
            return;
        };
        for seq in wanting {
            if let Some((_, item, since)) = self.refreshing.iter().find(|(kept, ..)| *kept == seq) {
                if let Some(source) = prefetch.resolved_since(&item.id, *since)
                    && engine.give_address(seq, &source)
                {
                    tracing::info!(
                        track = item.track.display_name(),
                        "a stream got a new address"
                    );
                }
                continue;
            }
            let item = match (&self.now_playing, &self.prefetched) {
                (Some(now), _) if now.seq == seq => now.item.clone(),
                (_, Some((handed, item, _))) if *handed == seq => item.clone(),
                _ => continue,
            };
            prefetch.refresh(&item);
            self.refreshing.push((seq, item, std::time::Instant::now()));
        }
    }

    /// Takes the accumulated listen records and empties the list.
    ///
    /// The caller writes them to the library — **to the same table** as imported
    /// data (§1.6): the past and today become a single timeline.
    #[must_use]
    pub fn take_listens(&mut self) -> Vec<Listen> {
        std::mem::take(&mut self.pending_listens)
    }

    /// Closes the playing track's listen record.
    ///
    /// Plays that do not pass the threshold **produce no record**, but they are
    /// not lost: `PlayRule` already says "not counted", and this is a deliberate
    /// filter (D-008).
    fn finish_current_listen(&mut self) {
        let Some(now_playing) = self.now_playing.take() else {
            return;
        };

        // The segment's own duration: if a transition happened, the output
        // is already on the next track and `position_ms()` shows that. The
        // scrobble of the finished track must ask for its own segment
        // (D-024). What a jump left behind, and what its video played, is
        // heard time too (D-086).
        let ms_played = self.heard_ms(&now_playing);

        // We ask the **engine** for the duration first: the catalog is fed
        // from tags and stays `None` for an untagged file. With the duration
        // unknown, `PlayRule`'s "half the track" arm cannot work, the rule
        // falls back to the 30 s threshold, and a short track listened to
        // from start to end produced no scrobble (D-008's rule did not
        // change; the data given to it was fixed).
        let duration_ms = self.duration_of(&now_playing);
        // The video belonged to this track.
        self.video = None;

        if !self.rule.counts(ms_played, duration_ms) {
            tracing::debug!(
                track = now_playing.item.track.display_name(),
                ms_played,
                "stayed below the threshold; no scrobble produced"
            );
            return;
        }

        // The measured duration goes into the record too. Otherwise a listen
        // written here as "recorded" would be filtered out when `stats`
        // re-applied the rule: the CLI would say "4 listens" and the stats
        // would show 2. The duration read from the container is a more
        // reliable measurement than the one from the tags.
        let mut track = now_playing.item.track;
        track.duration_ms = duration_ms;

        self.pending_listens.push(Listen {
            track,
            played_at: now_playing.started_at,
            ms_played,
            source: ListenSource::Playback {
                provider: now_playing.provider,
            },
            canonical_id: now_playing.canonical_id,
        });
    }
}

/// Adds a provider to the registry and sets up a player (a convenience).
#[must_use]
pub fn with_provider(provider: Arc<dyn Provider>) -> Player {
    let mut registry = ProviderRegistry::new();
    registry.register(provider);
    Player::new(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        Capabilities, ProviderFuture, ProviderHealth, ProviderInfo, ProviderTrack,
    };

    /// A fake provider without the capability: no `STREAM`.
    struct ControlOnlyProvider;

    impl Provider for ControlOnlyProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: ProviderId::new("remote"),
                display_name: "Control only".to_owned(),
                capabilities: Capabilities::CONTROL,
            }
        }
        fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
            Box::pin(async move {
                Ok(ProviderHealth {
                    id: ProviderId::new("remote"),
                    reachable: true,
                    track_count: None,
                    detail: None,
                })
            })
        }
        fn search<'a>(
            &'a self,
            _query: &'a str,
            _limit: usize,
        ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
            Box::pin(async move { Ok(Vec::new()) })
        }
        fn resolve_source<'a>(
            &'a self,
            _id: &'a ProviderTrackId,
        ) -> ProviderFuture<'a, Option<AudioSource>> {
            Box::pin(async move { Ok(None) })
        }
    }

    fn item(provider: &str, id: &str) -> QueueItem {
        QueueItem {
            id: ProviderTrackId::new(ProviderId::new(provider), id),
            track: TrackRef::new("Artist", "Track"),
        }
    }

    /// A video service (D-086): it shows videos and plays no sound of its
    /// own — every track is a video only.
    struct VideoOnlyProvider;

    impl Provider for VideoOnlyProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: ProviderId::new("tube"),
                display_name: "Tube".to_owned(),
                capabilities: Capabilities::SEARCH | Capabilities::VIDEO,
            }
        }
        fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
            Box::pin(async move {
                Ok(ProviderHealth {
                    id: ProviderId::new("tube"),
                    reachable: true,
                    track_count: None,
                    detail: None,
                })
            })
        }
        fn search<'a>(
            &'a self,
            _query: &'a str,
            _limit: usize,
        ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
            Box::pin(async move { Ok(Vec::new()) })
        }
        fn resolve_source<'a>(
            &'a self,
            _id: &'a ProviderTrackId,
        ) -> ProviderFuture<'a, Option<AudioSource>> {
            Box::pin(async move { Ok(None) })
        }
        fn resolve_video<'a>(
            &'a self,
            id: &'a ProviderTrackId,
        ) -> ProviderFuture<'a, Option<VideoSource>> {
            Box::pin(async move {
                Ok(Some(VideoSource::Youtube {
                    video_id: id.id.clone(),
                }))
            })
        }
    }

    /// A track that is a video only plays without the engine: the video's
    /// reports are its clock, what they played is its listen, and a jump is
    /// not played time (D-086).
    #[tokio::test]
    async fn a_video_only_track_is_heard_through_its_reports() {
        let mut player = with_provider(Arc::new(VideoOnlyProvider));
        player.set_shows_video(true);
        player
            .play_items(vec![
                item("tube", "CCHdMIEGaaM"),
                item("tube", "5NV6Rdv1a3I"),
            ])
            .await
            .expect("a video-only track starts");
        assert_eq!(player.surface(), Surface::Video);
        let now = player.video_now().expect("the video is shown").clone();
        assert!(now.only, "the video is all the sound there is");
        assert_eq!(
            now.source,
            VideoSource::Youtube {
                video_id: "CCHdMIEGaaM".to_owned()
            }
        );
        assert_eq!(player.state(), PlayState::Buffering);

        player
            .video_report(0, PlayState::Playing, Some(3_000))
            .unwrap();
        player
            .video_report(1_000, PlayState::Playing, None)
            .unwrap();
        // Played up to the pause: 2.5 s so far.
        player.video_report(2_500, PlayState::Paused, None).unwrap();
        // A jump while paused is not time played…
        player
            .video_report(40_000, PlayState::Playing, None)
            .unwrap();
        // …the playing after it is.
        player
            .video_report(41_000, PlayState::Playing, None)
            .unwrap();
        let anchor = player.anchor();
        assert_eq!(anchor.state, PlayState::Playing);
        assert!(anchor.position_ms >= 41_000, "{anchor:?}");
        assert_eq!(anchor.duration_ms, Some(3_000));

        // Music cannot take over: there is none.
        let err = player.show_audio(41_000).unwrap_err();
        assert_eq!(err.stage(), Stage::VideoPlayback);

        assert!(player.video_ended().await.unwrap(), "the next one starts");
        let listens = player.take_listens();
        assert_eq!(listens.len(), 1, "3.5 s heard of a 3 s video pass the rule");
        assert_eq!(listens[0].ms_played, 3_500);
        assert_eq!(
            player.video_now().map(|now| now.source.clone()),
            Some(VideoSource::Youtube {
                video_id: "5NV6Rdv1a3I".to_owned()
            })
        );

        // The last one ends the queue; a report that comes after is late.
        assert!(!player.video_ended().await.unwrap());
        assert_eq!(player.surface(), Surface::Audio);
        assert!(
            player
                .video_report(1_000, PlayState::Playing, None)
                .is_err()
        );
    }

    /// Where no video can be shown — the CLI, the TUI — a video-only track is
    /// passed by and counted; when nothing else is left, that is the error.
    #[tokio::test]
    async fn without_a_screen_a_video_only_track_is_passed_by() {
        let mut player = with_provider(Arc::new(VideoOnlyProvider));
        let err = player
            .play_items(vec![
                item("tube", "CCHdMIEGaaM"),
                item("tube", "5NV6Rdv1a3I"),
            ])
            .await
            .unwrap_err();
        assert_eq!(err.stage(), Stage::PlaybackResolve);
        assert!(
            err.chain_text().contains("play in the app's window"),
            "{}",
            err.chain_text()
        );
        assert_eq!(player.video_only_skipped(), 2);
        assert_eq!(player.surface(), Surface::Audio);
    }

    /// A video for a track nothing plays is refused, not kept.
    #[test]
    fn a_video_needs_a_playing_track() {
        let mut player = Player::new(ProviderRegistry::new());
        let err = player
            .show_video(
                VideoSource::Youtube {
                    video_id: "CCHdMIEGaaM".to_owned(),
                },
                ProviderId::new("tube"),
                0,
            )
            .unwrap_err();
        assert_eq!(err.stage(), Stage::VideoPlayback);
    }

    #[tokio::test]
    async fn streaming_from_a_control_only_provider_is_an_explicit_refusal() {
        let mut player = with_provider(Arc::new(ControlOnlyProvider));
        let err = player
            .play_items(vec![item("remote", "1")])
            .await
            .expect_err("must not play without the STREAM capability");

        assert_eq!(err.stage(), Stage::PlaybackResolve);
        let text = err.chain_text();
        assert!(text.contains("cannot do this"), "{text}");
        assert!(
            text.contains("CONTROL"),
            "the capabilities must be visible: {text}"
        );
    }

    #[tokio::test]
    async fn an_unknown_provider_is_named_in_the_error() {
        let mut player = Player::new(ProviderRegistry::new());
        let err = player
            .play_items(vec![item("missing", "1")])
            .await
            .expect_err("an unregistered provider");
        assert!(err.chain_text().contains("missing"), "{}", err.chain_text());
    }

    #[tokio::test]
    async fn playing_an_empty_list_is_rejected() {
        let mut player = Player::new(ProviderRegistry::new());
        let err = player.play_items(vec![]).await.expect_err("an empty list");
        assert!(err.chain_text().contains("there is no track to play"));
    }

    #[tokio::test]
    async fn an_added_track_comes_last_in_the_play_order_shuffled_or_not() {
        let mut player = Player::new(ProviderRegistry::new());
        player.queue_mut().replace(vec![
            item("local", "a"),
            item("local", "b"),
            item("local", "c"),
        ]);
        player.enqueue(vec![item("local", "added")]).await.unwrap();
        let order: Vec<String> = player
            .queue()
            .items()
            .into_iter()
            .map(|i| i.id.id)
            .collect();
        assert_eq!(order, ["a", "b", "c", "added"]);

        // Shuffled, the list keeps its own order, and the play order — what
        // "next" follows — still gets the new track last.
        player.queue_mut().set_shuffle(true);
        player.enqueue(vec![item("local", "later")]).await.unwrap();
        let order = player.queue().items();
        assert_eq!(order.len(), 5);
        assert_eq!(order.last().map(|i| i.id.id.as_str()), Some("later"));
    }

    #[tokio::test]
    async fn adding_to_an_empty_queue_plays_and_adding_nothing_is_said() {
        // Nothing to come after: it is played — here, by a provider that is not
        // registered, so the attempt shows as that provider's error rather than
        // a silent queue nobody hears.
        let mut player = Player::new(ProviderRegistry::new());
        let err = player
            .enqueue(vec![item("missing", "1")])
            .await
            .expect_err("an empty queue starts playing what is added");
        assert!(err.chain_text().contains("missing"), "{}", err.chain_text());

        let err = player
            .enqueue(Vec::new())
            .await
            .expect_err("nothing to add");
        assert!(
            err.chain_text().contains("no track to add"),
            "{}",
            err.chain_text()
        );
    }

    #[test]
    fn a_fresh_player_reports_a_stopped_anchor() {
        let player = Player::new(ProviderRegistry::new());
        let anchor = player.anchor();
        assert_eq!(anchor.state, PlayState::Stopped);
        assert_eq!(anchor.track, None);
        assert_eq!(anchor.rate, 0.0, "time must not advance while stopped");
    }

    #[test]
    fn listens_are_only_produced_above_the_play_rule() {
        let mut player = Player::new(ProviderRegistry::new());
        // Set up a "playing track" by hand; no engine, ms_played will be 0.
        player.now_playing = Some(NowPlaying {
            item: item("local", "x"),
            canonical_id: None,
            started_at: jiff::Timestamp::UNIX_EPOCH,
            provider: ProviderId::new("local"),
            #[cfg(feature = "audio")]
            seq: 0,
            #[cfg(feature = "audio")]
            source: Some(AudioSource::LocalFile {
                path: std::path::PathBuf::from("x"),
            }),
            carried_ms: 0,
        });
        player.finish_current_listen();
        assert!(
            player.take_listens().is_empty(),
            "a track played for 0 ms must not produce a scrobble"
        );
    }
}
