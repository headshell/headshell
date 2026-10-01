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

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::ids::{CanonicalId, ProviderId, ProviderTrackId};
use crate::model::{Listen, ListenSource, PlayRule, TrackRef};
use crate::provider::{AudioSource, Provider, ProviderRegistry};

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
    #[cfg(feature = "audio")]
    source: AudioSource,
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
        #[cfg(feature = "audio")]
        let (played, duration) = self.engine.as_ref().map_or((0, None), |engine| {
            (
                engine
                    .played_ms_of(now.seq)
                    .unwrap_or_else(|| engine.position_ms()),
                engine.duration_of(now.seq).or(now.item.track.duration_ms),
            )
        });
        #[cfg(not(feature = "audio"))]
        let (played, duration) = (0, now.item.track.duration_ms);
        self.rule.counts(played, duration)
    }

    /// Reads the queue ahead with `prefetcher` from now on (D-077): a playing
    /// session shares one across the players it replaces, so an address
    /// resolved for one queue is not resolved again for the next.
    #[cfg(feature = "audio")]
    pub(crate) fn attach_prefetcher(&mut self, prefetcher: Prefetcher) {
        if let Some(now) = &self.now_playing {
            prefetcher.remember(&now.item.id, &now.source);
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

    /// The current state.
    #[must_use]
    pub fn state(&self) -> PlayState {
        #[cfg(feature = "audio")]
        {
            if let Some(engine) = &self.engine {
                return engine.state();
            }
        }
        PlayState::Stopped
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
    /// # Errors
    /// If the queue is empty, the provider cannot be found or the audio pipeline
    /// cannot be set up.
    pub async fn start_current(&mut self) -> Result<()> {
        let item = self.queue.current().cloned().ok_or_else(|| {
            Error::new(
                Stage::PlaybackResolve,
                ErrorKind::NotFound {
                    what: "a track to play in the queue".to_owned(),
                },
            )
        })?;

        // Close the playing track: the half-finished listen record should be
        // produced.
        self.finish_current_listen();

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
        self.start_source(&source, item)?;
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
        self.engine = Some(engine);
        self.prefetched = None;
        self.refreshing.clear();

        self.now_playing = Some(NowPlaying {
            provider: item.id.provider.clone(),
            canonical_id: None,
            started_at: jiff::Timestamp::now(),
            item,
            seq,
            source: prepared.source.clone(),
        });
        Ok(())
    }

    /// Pauses.
    pub fn pause(&self) {
        #[cfg(feature = "audio")]
        if let Some(engine) = &self.engine {
            engine.pause();
        }
    }

    /// Resumes.
    pub fn resume(&self) {
        #[cfg(feature = "audio")]
        if let Some(engine) = &self.engine {
            engine.resume();
        }
    }

    /// Stops playing and closes the listen record.
    pub fn stop(&mut self) {
        self.finish_current_listen();
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
                    source,
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
        if let Some(now) = &self.now_playing {
            prefetch.remember(&now.item.id, &now.source);
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
        // scrobble of the finished track must ask for its own segment (D-024).
        #[cfg(feature = "audio")]
        let ms_played = self.engine.as_ref().map_or(0, |engine| {
            engine
                .played_ms_of(now_playing.seq)
                .unwrap_or_else(|| engine.position_ms())
        });
        #[cfg(not(feature = "audio"))]
        let ms_played = 0u64;

        // We ask the **engine** for the duration first: the catalog is fed
        // from tags and stays `None` for an untagged file. With the duration
        // unknown, `PlayRule`'s "half the track" arm cannot work, the rule
        // falls back to the 30 s threshold, and a short track listened to
        // from start to end produced no scrobble (D-008's rule did not
        // change; the data given to it was fixed).
        #[cfg(feature = "audio")]
        let duration_ms = self
            .engine
            .as_ref()
            .and_then(|engine| engine.duration_of(now_playing.seq))
            .or(now_playing.item.track.duration_ms);
        #[cfg(not(feature = "audio"))]
        let duration_ms = now_playing.item.track.duration_ms;

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
            source: AudioSource::LocalFile {
                path: std::path::PathBuf::from("x"),
            },
        });
        player.finish_current_listen();
        assert!(
            player.take_listens().is_empty(),
            "a track played for 0 ms must not produce a scrobble"
        );
    }
}
