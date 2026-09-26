//! The background worker (D-078): the playing session hands it the playing
//! track; it walks the chain for it and never makes playback wait.
//!
//! One track, not the queue: the user's rule is "the moment it starts", and
//! LRCLIB is rate-limited and sometimes busy — asking ahead for a queue of
//! thirty would spend its patience on tracks nobody reaches.
//!
//! What the links answered is kept for the playing track ([`Gathered`]): when
//! the user opens the lyrics view afterwards, only LRCLIB is asked — the
//! plugin is not asked a second time.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::cache::LyricsCache;
use super::chain::{Own, Remote};
use super::{Ask, Chain, Gathered, Links, LyricsItem, LyricsKey, LyricsStatus, decide};
use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::playback::queue::QueueItem;
use crate::provider::ProviderRegistry;

/// The playing session's lyrics worker.
pub struct LyricsWorker {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for LyricsWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LyricsWorker").finish_non_exhaustive()
    }
}

struct Shared {
    state: Mutex<State>,
    signal: Condvar,
    cache: Arc<Mutex<LyricsCache>>,
}

#[derive(Default)]
struct State {
    /// Bumped by every new track: an answer for an older one is not written.
    generation: u64,
    current: Option<Current>,
    /// The thread has something to ask.
    todo: bool,
    /// The playing track's answer changed since the last
    /// [`LyricsWorker::drain_ready`].
    ready: bool,
    /// A registry the thread has not picked up yet.
    registry: Option<ProviderRegistry>,
    stop: bool,
}

struct Current {
    item: QueueItem,
    duration_ms: Option<u64>,
    requested: bool,
    gathered: Gathered,
    /// What the interface is shown: the status, the lyrics, the shift.
    line: LyricsItem,
}

fn poisoned() -> Error {
    Error::new(
        Stage::LyricsStore,
        ErrorKind::Lyrics {
            detail: "the lyrics worker's lock is poisoned (an earlier call panicked)".to_owned(),
        },
    )
}

impl LyricsWorker {
    /// Opens the cache and starts the thread.
    ///
    /// # Errors
    /// If the cache directory cannot be opened or the thread cannot start.
    pub(crate) fn spawn(cache_dir: &Path, links: Links) -> Result<Self> {
        let cache = Arc::new(Mutex::new(LyricsCache::open(cache_dir)?));
        let chain = Chain::new(links, Arc::clone(&cache));
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
            cache,
        });
        let thread_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("headshell-lyrics".to_owned())
            .spawn(move || {
                // Looked up beside playback: a plugin puts the user's calls
                // before this one (D-077).
                crate::provider::ahead_of_time::mark_this_thread();
                run(&thread_shared, chain);
            })
            .map_err(|source| crate::error::io_err(Stage::LyricsStore, cache_dir, source))?;
        Ok(Self { shared })
    }

    /// A track started — or nothing plays (`None`). `requested`: the user's
    /// lyrics view is open, so LRCLIB may be asked at once.
    pub(crate) fn play(
        &self,
        playing: Option<(&QueueItem, usize)>,
        duration_ms: Option<u64>,
        requested: bool,
        registry: ProviderRegistry,
    ) {
        let offset = playing.and_then(|(item, _)| {
            let mut cache = self.shared.cache.lock().ok()?;
            // Another process (the CLI) may have written meanwhile.
            if let Err(err) = cache.reload() {
                tracing::warn!(error = %err.chain_text().replace('\n', " "), "the lyrics index could not be reread");
            }
            Some(cache.offset(&LyricsKey::of(&item.id)))
        });
        let Ok(mut state) = self.shared.state.lock() else {
            tracing::warn!("the lyrics worker's lock is poisoned; the track was not handed over");
            return;
        };
        state.generation += 1;
        state.registry = Some(registry);
        state.current = playing.map(|(item, index)| Current {
            item: item.clone(),
            duration_ms,
            requested,
            gathered: Gathered::default(),
            line: LyricsItem {
                offset_ms: offset.unwrap_or(0),
                ..LyricsItem::pending(index, &item.id, &item.track, duration_ms)
            },
        });
        state.todo = state.current.is_some();
        state.ready = true;
        drop(state);
        self.shared.signal.notify_one();
    }

    /// The user asked for lyrics: LRCLIB may be asked for the playing track,
    /// when its own answer does not already settle it. Asked again after a
    /// failure, the links that failed are asked anew — the interface's "try
    /// again".
    pub(crate) fn request(&self) {
        let Ok(mut state) = self.shared.state.lock() else {
            return;
        };
        let Some(current) = state.current.as_mut() else {
            return;
        };
        let failed = matches!(current.line.status, LyricsStatus::Failed { .. });
        if current.requested && !failed {
            return;
        }
        current.requested = true;
        if failed {
            if matches!(current.gathered.own, Some(Own::Failed { .. })) {
                current.gathered.own = None;
            }
            if matches!(current.gathered.lrclib, Some(Remote::Failed { .. })) {
                current.gathered.lrclib = None;
            }
        }
        if matches!(
            current.line.status,
            LyricsStatus::NotRequested | LyricsStatus::Failed { .. }
        ) {
            current.line.status = LyricsStatus::Pending;
        }
        state.todo = true;
        state.ready = true;
        drop(state);
        self.shared.signal.notify_one();
    }

    /// Did the playing track's answer change since the last call?
    ///
    /// A poisoned lock gives `false` here — this runs every tick — and is said
    /// by [`Self::current`], which the interface asks next.
    #[must_use]
    pub fn drain_ready(&self) -> bool {
        match self.shared.state.lock() {
            Ok(mut state) => std::mem::take(&mut state.ready),
            Err(_) => false,
        }
    }

    /// The playing track's lyrics as they stand; `None` when nothing plays.
    ///
    /// # Errors
    /// If the worker's lock is poisoned: `None` would say "nothing plays".
    pub fn current(&self) -> Result<Option<LyricsItem>> {
        let state = self.shared.state.lock().map_err(|_| poisoned())?;
        Ok(state.current.as_ref().map(|current| current.line.clone()))
    }

    /// Keeps the user's shift for a track, and shows it at once when it is
    /// the playing one.
    ///
    /// # Errors
    /// If the index cannot be written, or a lock is poisoned.
    pub fn shift(&self, key: &LyricsKey, offset_ms: i64) -> Result<Option<LyricsItem>> {
        self.cache()?.set_offset(key, offset_ms)?;
        let mut state = self.shared.state.lock().map_err(|_| poisoned())?;
        if let Some(current) = state.current.as_mut()
            && current.line.key == *key
        {
            current.line.offset_ms = offset_ms;
        }
        Ok(state.current.as_ref().map(|current| current.line.clone()))
    }

    fn cache(&self) -> Result<MutexGuard<'_, LyricsCache>> {
        self.shared.cache.lock().map_err(|_| poisoned())
    }
}

impl Drop for LyricsWorker {
    /// The thread is told to stop and is not waited for: a lookup under way
    /// may take a network timeout, and closing the app must not.
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stop = true;
        }
        self.shared.signal.notify_one();
    }
}

fn lock(shared: &Shared) -> Option<MutexGuard<'_, State>> {
    shared.state.lock().ok()
}

fn run(shared: &Shared, mut chain: Chain) {
    loop {
        // Wait for work; take what the playing track needs.
        let (generation, item, duration_ms, requested, mut gathered) = {
            let Some(mut state) = lock(shared) else {
                tracing::warn!("the lyrics worker's lock is poisoned; the worker stops");
                return;
            };
            loop {
                if state.stop {
                    return;
                }
                if let Some(registry) = state.registry.take() {
                    chain.set_registry(registry);
                }
                if state.todo
                    && let Some(current) = &state.current
                {
                    let taken = (
                        state.generation,
                        current.item.clone(),
                        current.duration_ms,
                        current.requested,
                        current.gathered.clone(),
                    );
                    state.todo = false;
                    break taken;
                }
                state = match shared.signal.wait(state) {
                    Ok(state) => state,
                    Err(_) => {
                        tracing::warn!("the lyrics worker's lock is poisoned; the worker stops");
                        return;
                    }
                };
            }
        };

        let ask = Ask {
            id: &item.id,
            track: &item.track,
            duration_ms,
            file: None,
            requested,
        };
        chain.gather(&ask, &mut gathered);
        let outcome = decide(&gathered);
        if !outcome.notes.is_empty() {
            tracing::info!(
                track = item.track.display_name(),
                notes = outcome.notes.join(" | "),
                "lyrics: a link failed on the way"
            );
        }

        let Some(mut state) = lock(shared) else {
            return;
        };
        if state.generation != generation {
            // Another track started meanwhile; LRCLIB's answer is in the
            // cache if it was one to keep.
            continue;
        }
        let gave_way = outcome.status == LyricsStatus::Pending;
        let Some(current) = state.current.as_mut() else {
            continue;
        };
        // The user may have asked while the thread was away: that request
        // stands, and is served next.
        let asked_meanwhile = current.requested && !requested;
        current.gathered = gathered;
        current.line.status = outcome.status;
        current.line.lyrics = outcome.lyrics;
        current.line.notes = outcome.notes;
        state.ready = true;
        // A plugin that gave way to the user's call is asked again once that
        // call is through (the gate orders it, D-077).
        if gave_way || asked_meanwhile {
            state.todo = true;
        }
    }
}

// The worker's test reads a real file's tags: it needs `audio`.
#[cfg(all(test, feature = "audio"))]
mod tests {
    use super::*;
    use crate::ids::{ProviderId, ProviderTrackId};
    use crate::lyrics::LyricsSource;
    use crate::lyrics::lrclib::Lrclib;
    use crate::model::TrackRef;
    use crate::net::fake::FakeHttp;
    use crate::provider::local::LocalProvider;
    use crate::test_support::TempDir;

    fn wait_until(worker: &LyricsWorker, done: impl Fn(&LyricsItem) -> bool) -> LyricsItem {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let line = worker.current().unwrap().expect("a track plays");
            if done(&line) || std::time::Instant::now() > deadline {
                return line;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// A real file without lyrics is "none" only when its tags can be read
    /// — without `audio` it is a failure, and says so.
    #[test]
    fn a_local_track_shows_its_files_lyrics_and_goes_online_only_when_asked() {
        let music = TempDir::new("lyrics-worker-music");
        let with_lrc = music.path().join("Band - With.flac");
        std::fs::write(&with_lrc, b"not audio").unwrap();
        std::fs::write(music.path().join("Band - With.lrc"), "[00:01.00]mine\n").unwrap();
        // A real audio file with no lyrics anywhere.
        let without = music.path().join("Band - Without.ogg");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/audio/Other Artist - Ogg Track.ogg"),
            &without,
        )
        .unwrap();
        let item = |path: &Path, title: &str| QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), path.to_string_lossy()),
            track: TrackRef::new("Band", title),
        };

        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(LocalProvider::new(vec![
            music.path().to_path_buf(),
        ])));
        let search = r#"[{"id":5,"trackName":"Without","artistName":"Band","duration":200.0,
            "instrumental":false,"syncedLyrics":"[00:02.00]theirs"}]"#;
        let http = Arc::new(FakeHttp::new().route("/api/search?", search));
        let cache = TempDir::new("lyrics-worker-cache");
        let worker = LyricsWorker::spawn(
            cache.path(),
            Links {
                registry: registry.clone(),
                lrclib: Ok(Lrclib::for_tests(http.clone(), "http://lrclib.test")),
            },
        )
        .unwrap();

        // The file's own `.lrc`.
        let first = item(&with_lrc, "With");
        worker.play(Some((&first, 0)), Some(200_000), false, registry.clone());
        let line = wait_until(&worker, |line| line.status != LyricsStatus::Pending);
        assert!(
            matches!(
                line.status,
                LyricsStatus::Found {
                    source: LyricsSource::Sidecar,
                    synced: true,
                    ..
                }
            ),
            "{line:?}"
        );
        assert_eq!(line.lyrics.unwrap().lines[0].text, "mine");
        assert!(worker.drain_ready());

        // Nothing of its own: not asked until the user asks.
        let second = item(&without, "Without");
        worker.play(Some((&second, 1)), Some(200_000), false, registry.clone());
        let line = wait_until(&worker, |line| line.status != LyricsStatus::Pending);
        assert_eq!(line.status, LyricsStatus::NotRequested, "{line:?}");
        assert!(http.requests().is_empty());

        worker.request();
        let line = wait_until(&worker, |line| {
            !matches!(
                line.status,
                LyricsStatus::Pending | LyricsStatus::NotRequested
            )
        });
        assert!(
            matches!(
                line.status,
                LyricsStatus::Found {
                    source: LyricsSource::Lrclib,
                    ..
                }
            ),
            "{line:?}"
        );
        assert_eq!(http.requests().len(), 1);

        // The shift is kept, and shown at once.
        let key = line.key.clone();
        let shifted = worker.shift(&key, 500).unwrap().unwrap();
        assert_eq!(shifted.offset_ms, 500);
        worker.play(Some((&second, 1)), Some(200_000), false, registry.clone());
        assert_eq!(
            worker.current().unwrap().unwrap().offset_ms,
            500,
            "kept per track"
        );

        // Nothing plays.
        worker.play(None, None, false, registry);
        assert_eq!(worker.current().unwrap(), None);
    }
}
