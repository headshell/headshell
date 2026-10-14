//! A playing session: `Player` + `Session` wired together in one place.
//!
//! **Why in the core (K1):** every shell was doing the same dance — a regular
//! `tick()`, then writing the accumulated listens to the store. The TUI wrote
//! it once, the GUI a second time, mobile would have written it a third. A
//! shell that forgets to write listens to the store **silently loses
//! history** — and nothing makes the loss noticeable.
//!
//! **True to D-015:** no observers/callbacks. The shell drives the loop
//! itself, gets a `TickReport` every round and estimates the position from
//! the anchor.
//! **True to K7:** no closure parameters, generics or leaking lifetimes.

use serde::{Deserialize, Serialize};

use crate::artwork::{
    ArtworkKey, ArtworkPicture, ArtworkReport, ArtworkVariant, ArtworkWorker, Links, Online,
};
use crate::ids::ProviderTrackId;
use crate::library::WriteSummary;
use crate::lyrics::{LyricsItem, LyricsKey, LyricsWorker};
use crate::model::{Listen, TrackRef};
use crate::playback::{PlayState, PlaybackAnchor, Player, QueueItem};
use crate::session::{LookupMode, Session};
use crate::{Error, Result};

/// What happened in a `tick`.
///
/// The shell looks at this and redraws. The position is **not** here: it is
/// estimated from the `anchor`, otherwise it would have to be asked dozens of
/// times a second.
///
/// `Serialize`: the GUI sends this to the webview as it is (§3.2). No
/// separate "IPC type" is written — a translating layer would let the two
/// types drift apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickReport {
    /// Always filled in. The shell computes the position from this
    /// (`PlaybackAnchor::position_at`).
    pub anchor: PlaybackAnchor,
    /// Did the playing track change since the last report — this round, or
    /// by a command between two rounds (a seek past the end, D-090).
    pub track_changed: bool,
    /// Did the position jump since the last report (D-090): a seek, or the
    /// music taking the sound back from its video at another second. The
    /// shell's estimate must start over from `anchor` — the one change in it
    /// that the state, the rate and the track don't show.
    #[serde(default)]
    pub moved: bool,
    /// The number of listens **written** to the store this round.
    pub listens_recorded: usize,
    /// The number of listens that could not be written and are held back.
    ///
    /// Greater than zero means the store gave a write error; the records **were
    /// not thrown away**, they will be retried next round. The shell should show
    /// this.
    pub listens_pending: usize,
    /// The error chain, if writing to the store failed this round.
    ///
    /// That is why `tick` does not return `Err`: the audio keeps playing, and
    /// dropping the session would increase the data loss. But it does not stay
    /// silent either (K9).
    pub store_error: Option<String>,
    /// The queue ran out and the audio stopped.
    pub finished: bool,
    /// The covers whose answer arrived this round (D-076), found or not —
    /// only their keys: the interface asks for the statuses and the images
    /// it shows. Empty when no cover worker runs (the TUI).
    #[serde(default)]
    pub artwork_ready: Vec<ArtworkKey>,
    /// The playing track's lyrics changed this round (D-078): found, not
    /// found, a new track, a shift — the interface asks `lyrics_now`. `false`
    /// when no lyrics worker runs (the TUI).
    #[serde(default)]
    pub lyrics_ready: bool,
    /// Who makes the sound (D-086).
    #[serde(default = "audio_surface")]
    pub surface: super::Surface,
    /// The video being shown, if any (D-086).
    #[serde(default)]
    pub video: Option<crate::video::VideoNow>,
    /// The user watches (D-086): a track that has just started waits,
    /// paused, for the shell to show its video.
    #[serde(default)]
    pub video_mode: bool,
}

const fn audio_surface() -> super::Surface {
    super::Surface::Audio
}

/// The playing session.
///
/// `Session` (library, statistics, diagnostics) and `Player` (audio, queue)
/// live here together. Access to both stays open: search and statistics are
/// needed while playing too.
pub struct LiveSession {
    session: Session,
    player: Player,
    /// Listens that could not be written to the store. Since `take_listens`
    /// pulls them out of the player, **if they are not kept here they are lost**
    /// when the write fails.
    pending: Vec<Listen>,
    /// The cover worker, if the shell started one (D-076).
    artwork: Option<ArtworkWorker>,
    /// The queue last handed to it: play order and position.
    artwork_seen: Option<(Vec<ProviderTrackId>, usize)>,
    /// The queue whose listening history the player last got (D-077).
    history_seen: Option<Vec<ProviderTrackId>>,
    /// The lyrics worker, if the shell started one (D-078).
    lyrics: Option<LyricsWorker>,
    /// The playing track last handed to it — its id and queue position —
    /// or `Some(None)`: nothing played.
    lyrics_seen: Option<Option<(ProviderTrackId, usize)>>,
    /// Is the user's lyrics view open? While it is, every track's lyrics may
    /// be looked up at LRCLIB (D-078).
    lyrics_wanted: bool,
    /// Reads the queue ahead (D-077). The session's, not a player's: a new
    /// search replaces the player, and an address resolved for the last queue
    /// is not resolved again for this one.
    #[cfg(feature = "audio")]
    prefetch: crate::playback::prefetch::Prefetcher,
    /// What the last report was made from (D-090): the playing track, its
    /// queue position and the player's jump count. A command runs between
    /// two rounds, and what it changed is reported by the next one.
    reported: Reported,
}

/// See [`LiveSession::reported`].
type Reported = (Option<TrackRef>, usize, u64);

fn reported_of(player: &Player) -> Reported {
    (
        player.current_track().cloned(),
        player.queue().position(),
        player.seeks(),
    )
}

impl LiveSession {
    /// Wires a session and a player together.
    #[must_use]
    pub fn new(session: Session, player: Player) -> Self {
        #[cfg(feature = "audio")]
        let (player, prefetch) = {
            let mut player = player;
            let prefetch = crate::playback::prefetch::Prefetcher::new();
            player.attach_prefetcher(prefetch.clone());
            (player, prefetch)
        };
        Self {
            session,
            reported: reported_of(&player),
            player,
            pending: Vec::new(),
            artwork: None,
            artwork_seen: None,
            history_seen: None,
            lyrics: None,
            lyrics_seen: None,
            lyrics_wanted: false,
            #[cfg(feature = "audio")]
            prefetch,
        }
    }

    /// Hands the player its queue's listening history when the queue
    /// changed (D-077): what reading ahead guesses its third track with. The
    /// history is read here, on the session's thread, once per queue.
    fn sync_history(&mut self) {
        let items = self.player.queue().items();
        let ids: Vec<ProviderTrackId> = items.iter().map(|item| item.id.clone()).collect();
        if self.history_seen.as_ref() == Some(&ids) {
            return;
        }
        let profiles = self
            .session
            .listening_profiles(&items, crate::model::PlayRule::default());
        self.player.set_history(profiles);
        self.history_seen = Some(ids);
    }

    /// What is being read ahead for the queue, and how it went (D-077).
    ///
    /// # Errors
    /// In a build without the audio pipeline — nothing plays, so nothing is
    /// read ahead — or if the read-ahead lock is poisoned.
    pub fn prefetch_report(&self) -> Result<crate::playback::PrefetchReport> {
        self.player.prefetch_report()
    }

    /// Starts the cover worker (D-076). From then on every queue the player
    /// plays is handed to it — at the next `tick` or report — and the answers
    /// that arrive are announced in [`TickReport::artwork_ready`].
    ///
    /// `mode` is the `--online` choice: `Offline` looks only where the tracks
    /// live (their tags, their folders, their provider); `Online` asks
    /// MusicBrainz and the Cover Art Archive too.
    ///
    /// # Errors
    /// If the cache directory cannot be opened, or online was asked for in a
    /// build without an HTTP client.
    pub fn start_artwork(&mut self, mode: LookupMode) -> Result<()> {
        let online = match mode {
            LookupMode::Offline => None,
            LookupMode::Online => Some(Online::from_build()?),
        };
        let links = Links {
            registry: self.player.providers().clone(),
            online,
        };
        self.artwork = Some(ArtworkWorker::spawn(
            &self.session.config().artwork_dir(),
            links,
        )?);
        self.artwork_seen = None;
        Ok(())
    }

    /// Hands the queue to the cover worker if it changed since the last time.
    fn sync_artwork(&mut self) {
        let Some(worker) = &self.artwork else {
            return;
        };
        let view = self.player.queue().view();
        let stamp = (
            view.items.iter().map(|item| item.id.clone()).collect(),
            view.position,
        );
        if self.artwork_seen.as_ref() != Some(&stamp) {
            worker.submit(&view.items, view.position, self.player.providers().clone());
            self.artwork_seen = Some(stamp);
        }
    }

    /// The queue's covers as they stand (D-076).
    ///
    /// # Errors
    /// If no cover worker runs — "not started" is not "no covers" (K9) — or
    /// its lock is poisoned.
    pub fn artwork_report(&mut self) -> Result<ArtworkReport> {
        self.sync_artwork();
        self.worker()?.report()
    }

    /// Covers for tracks on screen besides the queue — the play box's
    /// suggestions (D-080). Only where each track lives is asked (its file,
    /// its provider), never the third parties: a suggestion list changes at
    /// every pause in typing. What the cache knows comes back at once; the
    /// rest is looked up in the background — after the playing track,
    /// before the rest of the queue — and a found one is announced in
    /// [`TickReport::artwork_ready`]. A new list replaces the old one's
    /// pending lookups.
    ///
    /// # Errors
    /// If no cover worker runs — "not started" is not "no covers" (K9).
    pub fn artwork_shown(&mut self, items: &[QueueItem]) -> Result<ArtworkReport> {
        // The worker's providers are the ones the player plays from.
        self.sync_artwork();
        Ok(self.worker()?.show(items))
    }

    /// One size of the covers of `keys` as `data:` URIs, for the interface.
    ///
    /// # Errors
    /// If no cover worker runs, or the cache cannot be read.
    pub fn artwork_images(
        &self,
        keys: &[ArtworkKey],
        variant: ArtworkVariant,
    ) -> Result<Vec<ArtworkPicture>> {
        self.worker()?.images(keys, variant)
    }

    /// Starts the lyrics worker (D-078). From then on the playing track is
    /// handed to it — at the next `tick` or call — and a change of its
    /// answer is announced in [`TickReport::lyrics_ready`].
    ///
    /// Where the track lives is asked at once: its file, its server, its
    /// plugin. LRCLIB — for a plugin's track at once, for the others only
    /// once the lyrics view is open ([`Self::set_lyrics_wanted`]). There is
    /// no `--online` here: the user's request is the consent (D-078).
    ///
    /// # Errors
    /// If the cache directory cannot be opened or the thread cannot start.
    pub fn start_lyrics(&mut self) -> Result<()> {
        let links = crate::lyrics::Links::from_build(self.player.providers().clone());
        self.lyrics = Some(LyricsWorker::spawn(
            &self.session.config().lyrics_dir(),
            links,
        )?);
        self.lyrics_seen = None;
        Ok(())
    }

    /// Hands the playing track to the lyrics worker if it changed since the
    /// last time.
    fn sync_lyrics(&mut self) {
        let Some(worker) = &self.lyrics else {
            return;
        };
        let position = self.player.queue().position();
        let playing = self.player.current_item();
        let stamp = playing.map(|item| (item.id.clone(), position));
        if self.lyrics_seen.as_ref() == Some(&stamp) {
            return;
        }
        worker.play(
            playing.map(|item| (item, position)),
            self.player.anchor().duration_ms,
            self.lyrics_wanted,
            self.player.providers().clone(),
        );
        self.lyrics_seen = Some(stamp);
    }

    /// The playing track's lyrics as they stand; `None` when nothing plays.
    ///
    /// # Errors
    /// If no lyrics worker runs — "not started" is not "no lyrics" (K9) — or
    /// its lock is poisoned.
    pub fn lyrics_now(&mut self) -> Result<Option<LyricsItem>> {
        self.sync_lyrics();
        self.lyrics_worker()?.current()
    }

    /// The lyrics view opened (`true`) or closed. While it is open, the
    /// playing track's lyrics — and every next track's — may be looked up at
    /// LRCLIB: opening it is the user asking (D-078).
    ///
    /// # Errors
    /// As [`Self::lyrics_now`].
    pub fn set_lyrics_wanted(&mut self, wanted: bool) -> Result<Option<LyricsItem>> {
        self.lyrics_wanted = wanted;
        self.sync_lyrics();
        let worker = self.lyrics_worker()?;
        if wanted {
            worker.request();
        }
        worker.current()
    }

    /// Keeps the user's shift of a track's lyrics, in ms; positive, the
    /// lines come sooner. Returns the playing track's lyrics.
    ///
    /// # Errors
    /// If no lyrics worker runs, or the shift cannot be written.
    pub fn shift_lyrics(&mut self, key: &LyricsKey, offset_ms: i64) -> Result<Option<LyricsItem>> {
        self.sync_lyrics();
        self.lyrics_worker()?.shift(key, offset_ms)
    }

    fn lyrics_worker(&self) -> Result<&LyricsWorker> {
        self.lyrics.as_ref().ok_or_else(|| {
            Error::new(
                crate::diag::Stage::LyricsStore,
                crate::ErrorKind::Lyrics {
                    detail: "no lyrics worker runs in this session".to_owned(),
                },
            )
        })
    }

    fn worker(&self) -> Result<&ArtworkWorker> {
        self.artwork.as_ref().ok_or_else(|| {
            Error::new(
                crate::diag::Stage::ArtworkStore,
                crate::ErrorKind::Artwork {
                    detail: "no cover worker runs in this session".to_owned(),
                },
            )
        })
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    #[must_use]
    pub fn player(&self) -> &Player {
        &self.player
    }

    pub fn player_mut(&mut self) -> &mut Player {
        &mut self.player
    }

    /// The current anchor.
    #[must_use]
    pub fn anchor(&self) -> PlaybackAnchor {
        self.player.anchor()
    }

    /// Replaces the playing player with a new one (starting a new queue).
    ///
    /// **The old player's accumulated listens are taken.** Writing
    /// `*live.player_mut() = new` directly would drop the old player without
    /// calling `take_listens`: the moment the user made a new search, the track
    /// they had just listened to would silently be lost. The records move from
    /// here to `pending`, and the first `tick` writes them.
    ///
    /// The old audio pipeline closes — `Player` stops when dropped.
    pub fn replace_player(&mut self, player: Player) {
        self.player.stop();
        self.pending.extend(self.player.take_listens());
        let habits = self.player.habits();
        // Whether the shell can show videos is the shell's, not the queue's
        // (D-086): a new search keeps it.
        let shows_video = self.player.shows_video();
        self.player = player;
        self.player.adopt_habits(habits);
        self.player.set_shows_video(shows_video);
        self.history_seen = None;
        #[cfg(feature = "audio")]
        self.player.attach_prefetcher(self.prefetch.clone());
        // The new player may carry a newer registry (a server was added):
        // the same queue is handed to the cover worker again with it.
        self.artwork_seen = None;
        self.lyrics_seen = None;
    }

    /// One round: advance the player, write the accumulated listens
    /// **immediately**.
    ///
    /// The write happens every round, not on exit. In an interface left open for
    /// hours, writing on exit would take the whole session's history with it on
    /// a crash or a `kill`. Most rounds have nothing to write — `take_listens`
    /// comes back empty and the store is never touched.
    ///
    /// # Errors
    /// If the player fails while advancing (the source could not be opened, the
    /// code could not be decoded). **A store write error does not produce an
    /// `Err`** — it lands in `TickReport::store_error`.
    pub async fn tick(&mut self) -> Result<TickReport> {
        // The queue position is looked at too: the same track can be in the
        // queue twice, and comparing only the `TrackRef` would miss the
        // transition between them.
        //
        // The case it does not catch is written down on purpose: when
        // `RepeatMode::One` restarts the same track, neither the track nor the
        // position changes, and `track_changed` stays `false`. That is also
        // what is right for the shell — the track shown is the same; the
        // anchor's position says it started over, and `listens_recorded` says
        // there is a new listen.
        //
        // "Before" is the last report, not the start of this round (D-090): a
        // command between two rounds — a seek past the end — changes the
        // track too, and its answer is only the anchor.
        self.sync_history();
        let tick_result = self.player.tick().await;

        // The accumulated listens must be written even if the player failed:
        // the error belongs to one track, the history to the whole session.
        let (recorded, store_error) = self.flush();

        let after = reported_of(&self.player);
        let anchor = self.player.anchor();
        let finished =
            self.player.state() == PlayState::Stopped && self.player.current_track().is_none();

        tick_result?;

        self.sync_artwork();
        let artwork_ready = match &self.artwork {
            Some(worker) => worker.drain_ready(),
            None => Vec::new(),
        };
        self.sync_lyrics();
        let lyrics_ready = self.lyrics.as_ref().is_some_and(LyricsWorker::drain_ready);

        // Taken only for a report that goes out: after a failed round the
        // next one still says what changed.
        let before = std::mem::replace(&mut self.reported, after.clone());
        Ok(TickReport {
            anchor,
            track_changed: (&before.0, before.1) != (&after.0, after.1),
            moved: before.2 != after.2,
            listens_recorded: recorded,
            listens_pending: self.pending.len(),
            store_error,
            finished,
            artwork_ready,
            lyrics_ready,
            surface: self.player.surface(),
            video: self.player.video_now().cloned(),
            video_mode: self.player.video_mode(),
        })
    }

    /// The videos for the playing track (D-086), from the video role's
    /// provider.
    ///
    /// # Errors
    /// If nothing plays, or as [`Session::video_for`].
    pub async fn video_for_current(&self) -> Result<crate::video::VideoPlan> {
        let item = self.player.current_item().cloned().ok_or_else(|| {
            crate::Error::new(
                crate::diag::Stage::VideoLookup,
                crate::ErrorKind::NotFound {
                    what: "a playing track to find a video for".to_owned(),
                },
            )
        })?;
        self.session.video_for(self.player.providers(), &item).await
    }

    /// Closes the session: stops the audio, writes every remaining listen.
    ///
    /// # Errors
    /// If the last write fails. The records are then **still at hand**;
    /// `pending_listens()` says how many could not be written.
    pub fn shutdown(&mut self) -> Result<WriteSummary> {
        self.player.stop();
        self.pending.extend(self.player.take_listens());
        if self.pending.is_empty() {
            return Ok(WriteSummary::default());
        }
        let summary = self.session.record_listens(&self.pending)?;
        self.pending.clear();
        Ok(summary)
    }

    /// The number of listens not yet written to the store.
    #[must_use]
    pub fn pending_listens(&self) -> usize {
        self.pending.len()
    }

    /// Tries to write the accumulated listens. If it cannot, it **holds on** to
    /// them.
    fn flush(&mut self) -> (usize, Option<String>) {
        self.pending.extend(self.player.take_listens());
        if self.pending.is_empty() {
            return (0, None);
        }
        let outcome = self.session.record_listens(&self.pending);
        absorb(&mut self.pending, outcome)
    }
}

/// Applies a write result to the buffer.
///
/// A separate function, because the real claim is **on the failure path**: a
/// record that could not be written is not thrown away, it stays at hand.
/// Testing this with a real broken store was tried and was not reliable —
/// with an open file handle SQLite keeps writing even in a read-only
/// directory. A test whose condition cannot be met turns green and proves
/// nothing; the decision was pulled out here so it can be tested directly.
fn absorb(pending: &mut Vec<Listen>, outcome: Result<WriteSummary>) -> (usize, Option<String>) {
    match outcome {
        Ok(summary) => {
            pending.clear();
            (summary.inserted, None)
        }
        // The records stay in `pending`: the next round will retry.
        Err(err) => (0, Some(Error::chain_text(&err))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::diag::Stage;
    use crate::error::ErrorKind;
    use crate::ids::ProviderId;
    use crate::model::ListenSource;
    use crate::provider::ProviderRegistry;

    fn temp_dir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(&format!("live-{label}"))
    }

    fn live_at(dir: &std::path::Path) -> LiveSession {
        let session = Session::open(Config::with_data_dir(dir)).expect("the session must open");
        LiveSession::new(session, Player::new(ProviderRegistry::new()))
    }

    fn listen(title: &str) -> Listen {
        Listen {
            track: TrackRef::new("Artist", title),
            played_at: jiff::Timestamp::now(),
            ms_played: 200_000,
            source: ListenSource::Playback {
                provider: ProviderId::new("local"),
            },
            canonical_id: None,
        }
    }

    #[tokio::test]
    async fn an_idle_session_reports_finished_without_inventing_listens() {
        let dir = temp_dir("idle");
        let mut live = live_at(&dir);

        let report = live.tick().await.expect("an idle tick must not fail");

        assert!(
            report.finished,
            "the queue is empty and the audio has stopped"
        );
        assert!(!report.track_changed);
        assert_eq!(report.listens_recorded, 0);
        assert_eq!(report.listens_pending, 0);
        assert!(report.store_error.is_none());
        assert_eq!(report.anchor.state, PlayState::Stopped);
    }

    #[test]
    fn flushing_writes_listens_and_empties_the_buffer() {
        let dir = temp_dir("write");
        let mut live = live_at(&dir);
        live.pending.push(listen("One"));
        live.pending.push(listen("Two"));

        let (recorded, error) = live.flush();

        assert_eq!(recorded, 2, "both must be written");
        assert!(error.is_none(), "{error:?}");
        assert_eq!(live.pending_listens(), 0, "the buffer must empty");
    }

    /// The real claim: **if the store cannot write, no listen is lost.**
    ///
    /// `take_listens` pulls the records out of the player; if the write fails
    /// and they are not held on to, there is no place to get them back from.
    /// Writing used to happen only on exit, so a single error would take the
    /// whole session with it.
    #[test]
    fn a_failing_store_keeps_the_listens_instead_of_dropping_them() {
        let mut pending = vec![listen("Must not be lost"), listen("Nor this one")];

        let (recorded, error) = absorb(
            &mut pending,
            Err(Error::new(
                Stage::LibraryWrite,
                ErrorKind::InvalidInput {
                    detail: "disk full".to_owned(),
                },
            )),
        );

        assert_eq!(recorded, 0, "none were written");
        assert_eq!(
            pending.len(),
            2,
            "the records must stay AT HAND, not be thrown away"
        );
        let text = error.expect("the error must be reported, not swallowed");
        assert!(
            text.starts_with("STEP: "),
            "the stage must be reported (K9): {text}"
        );
        assert!(
            text.contains("disk full"),
            "the reason must be visible: {text}"
        );
    }

    #[test]
    fn a_successful_store_empties_the_buffer() {
        let mut pending = vec![listen("Written")];

        let (recorded, error) = absorb(
            &mut pending,
            Ok(WriteSummary {
                offered: 1,
                inserted: 1,
                duplicates: 0,
                new_tracks: 1,
            }),
        );

        assert_eq!(recorded, 1);
        assert!(error.is_none());
        assert!(
            pending.is_empty(),
            "a written record must not stay in the buffer"
        );
    }

    /// Starting a new queue must not delete the records left over from the
    /// previous round.
    ///
    /// The part this test **does not cover** is written down on purpose: is the
    /// old player's own `pending_listens` carried over here? `Player`'s field is
    /// private and producing listens needs a real audio pipeline; that path is
    /// tested by `tests/playback_local.rs`. What is tested here is that the
    /// buffer **is not thrown away** during the replacement.
    #[test]
    fn replacing_the_player_swaps_the_queue_and_keeps_pending_listens() {
        use crate::ids::ProviderTrackId;
        use crate::playback::QueueItem;

        let dir = temp_dir("replace");
        let mut live = live_at(&dir);
        live.pending
            .push(listen("Left over from the previous round"));

        let mut replacement = Player::new(ProviderRegistry::new());
        replacement.queue_mut().replace(vec![QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), "1"),
            track: TrackRef::new("Artist", "New queue"),
        }]);

        live.replace_player(replacement);

        assert_eq!(
            live.player().queue().len(),
            1,
            "the new queue must be in effect"
        );
        assert_eq!(
            live.pending_listens(),
            1,
            "the old buffer must not be thrown away"
        );
    }

    /// D-078: "no worker was started" is not "no lyrics" (K9), and with
    /// nothing playing the answer is `None`, whether the view is open or not.
    #[tokio::test]
    async fn lyrics_say_when_no_worker_runs_and_nothing_plays_is_none() {
        let dir = temp_dir("lyrics");
        let mut live = live_at(&dir);
        let err = live.lyrics_now().unwrap_err();
        assert!(
            err.chain_text().contains("no lyrics worker"),
            "{}",
            err.chain_text()
        );

        live.start_lyrics().unwrap();
        assert_eq!(live.lyrics_now().unwrap(), None);
        assert_eq!(live.set_lyrics_wanted(true).unwrap(), None);
        let report = live.tick().await.unwrap();
        assert!(report.finished);
        assert_eq!(live.set_lyrics_wanted(false).unwrap(), None);
    }

    #[test]
    fn shutdown_writes_what_is_still_pending() {
        let dir = temp_dir("close");
        let mut live = live_at(&dir);
        live.pending.push(listen("Son"));

        let summary = live.shutdown().expect("shutdown must be able to write");

        assert_eq!(summary.inserted, 1);
        assert_eq!(live.pending_listens(), 0);
    }
}
