//! The background worker (D-076): the playing session hands it its queue; it
//! walks the chain for each track — the playing one first, then the ones
//! after it in play order, then the ones already played — and never makes
//! playback wait.
//!
//! Why a thread of its own: the HTTP client and the MusicBrainz limiter
//! block the thread they run on (a second between requests), and the shells
//! run the core on **one** thread — on it, a cover download would hold up
//! pause and next.
//!
//! What is already known is settled at once when a queue arrives: the cache
//! is read under the lock, and only the rest waits for the thread. A new
//! queue replaces the old one's pending work; a lookup already under way is
//! finished and kept in the cache, only its answer is not written into a
//! queue that is gone.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use super::cache::{ArtworkCache, Entry};
use super::{
    ArtworkItem, ArtworkPicture, ArtworkReport, ArtworkStatus, ArtworkSummary, ArtworkVariant,
    Chain, Links, key_for,
};
use crate::diag::Stage;
use crate::encoding::data_uri;
use crate::error::{Error, ErrorKind, Result};
use crate::ids::{ArtworkKey, ProviderTrackId};
use crate::model::TrackRef;
use crate::playback::queue::QueueItem;
use crate::provider::ProviderRegistry;

/// The playing session's cover worker.
pub struct ArtworkWorker {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ArtworkWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtworkWorker")
            .field("online", &self.shared.online)
            .finish_non_exhaustive()
    }
}

struct Shared {
    state: Mutex<State>,
    signal: Condvar,
    cache: Arc<Mutex<ArtworkCache>>,
    online: bool,
}

#[derive(Default)]
struct State {
    /// Bumped by every new queue: an answer for an older one is not written.
    generation: u64,
    items: Vec<ArtworkItem>,
    tracks: Vec<(ProviderTrackId, TrackRef)>,
    /// Indices into `items`, in the order they are worked on.
    todo: VecDeque<usize>,
    /// Keys whose answer arrived since the last
    /// [`ArtworkWorker::drain_ready`] — found or not: a row whose cover was
    /// not found shows why, and it learns that from the same signal.
    ready: Vec<ArtworkKey>,
    /// A registry the thread has not picked up yet.
    registry: Option<ProviderRegistry>,
    stop: bool,
}

fn poisoned() -> Error {
    Error::new(
        Stage::ArtworkStore,
        ErrorKind::Artwork {
            detail: "the cover worker's lock is poisoned (an earlier call panicked)".to_owned(),
        },
    )
}

impl ArtworkWorker {
    /// Opens the cache and starts the thread.
    ///
    /// # Errors
    /// If the cache directory cannot be opened or the thread cannot start.
    pub(crate) fn spawn(cache_dir: &Path, links: Links) -> Result<Self> {
        let cache = Arc::new(Mutex::new(ArtworkCache::open(cache_dir)?));
        let chain = Chain::new(links, Arc::clone(&cache));
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            signal: Condvar::new(),
            cache,
            online: chain.online(),
        });
        let thread_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("headshell-artwork".to_owned())
            .spawn(move || {
                // Covers are looked up ahead of time: a plugin puts the user's
                // calls before them (D-077).
                crate::provider::ahead_of_time::mark_this_thread();
                run(&thread_shared, chain);
            })
            .map_err(|source| crate::error::io_err(Stage::ArtworkStore, cache_dir, source))?;
        Ok(Self { shared })
    }

    /// Hands over a queue, in play order, with the position playing now.
    pub(crate) fn submit(&self, items: &[QueueItem], position: usize, registry: ProviderRegistry) {
        // Another process (the CLI) may have filled the cache meanwhile.
        if let Ok(mut cache) = self.shared.cache.lock()
            && let Err(err) = cache.reload()
        {
            tracing::warn!(error = %err.chain_text().replace('\n', " "), "the cover index could not be reread");
        }
        let known: Vec<Option<ArtworkStatus>> = match self.shared.cache.lock() {
            Ok(cache) => items
                .iter()
                .map(|item| known_status(&cache, &key_for(&item.track)))
                .collect(),
            Err(_) => vec![None; items.len()],
        };

        let Ok(mut state) = self.shared.state.lock() else {
            tracing::warn!("the cover worker's lock is poisoned; the queue was not handed over");
            return;
        };
        state.generation += 1;
        state.registry = Some(registry);
        state.items = items
            .iter()
            .zip(known)
            .enumerate()
            .map(|(index, (item, known))| ArtworkItem {
                index,
                key: key_for(&item.track),
                provider: item.id.provider.clone(),
                artist: item.track.artist.clone(),
                title: item.track.title.clone(),
                album: item.track.album.clone(),
                status: known.unwrap_or(ArtworkStatus::Pending),
                notes: Vec::new(),
            })
            .collect();
        state.tracks = items
            .iter()
            .map(|item| (item.id.clone(), item.track.clone()))
            .collect();
        // The playing track first, then play order, then what was played.
        let count = items.len();
        let start = position.min(count);
        let order: VecDeque<usize> = (start..count).chain(0..start).collect();
        state.todo = order
            .into_iter()
            .filter(|&index| state.items[index].status == ArtworkStatus::Pending)
            .collect();
        drop(state);
        self.shared.signal.notify_one();
    }

    /// The keys whose answer arrived since the last call, found or not.
    ///
    /// A poisoned lock gives none here — this runs every tick — and is said
    /// by [`Self::report`], which the interface asks next.
    #[must_use]
    pub fn drain_ready(&self) -> Vec<ArtworkKey> {
        match self.shared.state.lock() {
            Ok(mut state) => std::mem::take(&mut state.ready),
            Err(_) => Vec::new(),
        }
    }

    /// The queue's covers as they stand.
    ///
    /// # Errors
    /// If the worker's lock is poisoned: an empty list would say "no queue".
    pub fn report(&self) -> Result<ArtworkReport> {
        let items = self
            .shared
            .state
            .lock()
            .map_err(|_| poisoned())?
            .items
            .clone();
        Ok(ArtworkReport {
            subject: "queue".to_owned(),
            online: self.shared.online,
            summary: ArtworkSummary::of(&items),
            items,
            written: Vec::new(),
            diag: None,
        })
    }

    /// One size of the covers of `keys`, ready for the webview. A key
    /// without a cover is left out — the interface only asks for the ones it
    /// was told are found.
    ///
    /// # Errors
    /// If the cache cannot be read.
    pub fn images(
        &self,
        keys: &[ArtworkKey],
        variant: ArtworkVariant,
    ) -> Result<Vec<ArtworkPicture>> {
        let cache = self.shared.cache.lock().map_err(|_| poisoned())?;
        images_from(&cache, keys, variant)
    }
}

impl Drop for ArtworkWorker {
    /// The thread is told to stop and is not waited for: a lookup under way
    /// may take a network timeout, and closing the app must not.
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stop = true;
        }
        self.shared.signal.notify_one();
    }
}

/// What the cache says, if anything.
pub(crate) fn known_status(cache: &ArtworkCache, key: &ArtworkKey) -> Option<ArtworkStatus> {
    Some(match cache.get(key)? {
        Entry::Found { source, .. } => ArtworkStatus::Found { source: *source },
        Entry::NotFound { detail, .. } => ArtworkStatus::NotFound {
            detail: detail.clone(),
        },
    })
}

/// One variant of each found key, as a `data:` URI.
pub(crate) fn images_from(
    cache: &ArtworkCache,
    keys: &[ArtworkKey],
    variant: ArtworkVariant,
) -> Result<Vec<ArtworkPicture>> {
    let mut out = Vec::new();
    for key in keys {
        let Some(Entry::Found { label, thumb, .. }) = cache.get(key) else {
            continue;
        };
        let stored = match variant {
            ArtworkVariant::Label => label,
            ArtworkVariant::Thumb => thumb,
        };
        let (bytes, format) = cache.read_image(stored)?;
        out.push(ArtworkPicture {
            key: key.clone(),
            variant,
            uri: data_uri(format.mime(), &bytes),
        });
    }
    Ok(out)
}

fn lock(shared: &Shared) -> Option<MutexGuard<'_, State>> {
    shared.state.lock().ok()
}

fn run(shared: &Shared, mut chain: Chain) {
    loop {
        // Wait for work; take the next item.
        let (generation, index, id, track) = {
            let Some(mut state) = lock(shared) else {
                tracing::warn!("the cover worker's lock is poisoned; the worker stops");
                return;
            };
            loop {
                if state.stop {
                    return;
                }
                if let Some(registry) = state.registry.take() {
                    chain.set_registry(registry);
                }
                if let Some(index) = state.todo.pop_front() {
                    let (id, track) = state.tracks[index].clone();
                    break (state.generation, index, id, track);
                }
                state = match shared.signal.wait(state) {
                    Ok(state) => state,
                    Err(_) => {
                        tracing::warn!("the cover worker's lock is poisoned; the worker stops");
                        return;
                    }
                };
            }
        };

        let outcome = chain.resolve(&id, &track);
        if !outcome.notes.is_empty() {
            tracing::info!(
                key = outcome.key.as_str(),
                notes = outcome.notes.join(" | "),
                "cover: a link failed on the way"
            );
        }

        let Some(mut state) = lock(shared) else {
            return;
        };
        if state.generation != generation {
            // The queue changed meanwhile; the answer is in the cache if it was
            // one to keep, and the new queue reads it from there.
            continue;
        }
        if outcome.status == ArtworkStatus::Pending {
            // The plugin was needed for a call the user made (D-077): this row
            // is asked again, first, once that call is through.
            state.todo.push_front(index);
            continue;
        }
        // Every row of the same album gets the answer at once.
        let mut settled = HashSet::new();
        for (row, item) in state.items.iter_mut().enumerate() {
            if item.key == outcome.key && (row == index || item.status == ArtworkStatus::Pending) {
                item.status = outcome.status.clone();
                item.notes.clone_from(&outcome.notes);
                settled.insert(row);
            }
        }
        state.todo.retain(|row| !settled.contains(row));
        if !settled.is_empty() && !state.ready.contains(&outcome.key) {
            state.ready.push(outcome.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artwork::ArtworkSource;
    use crate::ids::ProviderId;
    use crate::provider::local::LocalProvider;
    use crate::test_support::TempDir;

    /// A 2×2 PNG, written by hand: the smallest real image a folder can hold.
    fn tiny_png() -> Vec<u8> {
        crate::artwork::image::tests_support::png_2x2()
    }

    fn wait_until(worker: &ArtworkWorker, done: impl Fn(&ArtworkReport) -> bool) -> ArtworkReport {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let report = worker.report().unwrap();
            if done(&report) || std::time::Instant::now() > deadline {
                return report;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn the_playing_track_comes_first_and_an_album_is_resolved_once() {
        let music = TempDir::new("artwork-worker-music");
        let album = music.path().join("Album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("cover.png"), tiny_png()).unwrap();
        let mut items = Vec::new();
        for title in ["One", "Two", "Three"] {
            let path = album.join(format!("Artist - {title}.wav"));
            std::fs::write(&path, b"not audio").unwrap();
            items.push(QueueItem {
                id: ProviderTrackId::new(ProviderId::new("local"), path.to_string_lossy()),
                track: TrackRef::new("Artist", title).with_album(Some("Album".to_owned())),
            });
        }
        // A real audio file without a picture, played first: offline, nothing
        // more can be asked. (In a build without `audio` its tags cannot be
        // read at all — that is a failure, not "not checked".)
        let bare = music.path().join("Loose");
        std::fs::create_dir_all(&bare).unwrap();
        let path = bare.join("Other - Four.ogg");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/audio/Other Artist - Ogg Track.ogg");
        std::fs::copy(fixture, &path).unwrap();
        items.push(QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), path.to_string_lossy()),
            track: TrackRef::new("Other", "Four").with_album(Some("Loose".to_owned())),
        });
        // A file whose tags cannot be read and with no folder image: that is a
        // failure, not "not checked" — the file may be broken (K9).
        let broken = music.path().join("Broken");
        std::fs::create_dir_all(&broken).unwrap();
        let path = broken.join("Third - Five.flac");
        std::fs::write(&path, b"not audio").unwrap();
        items.push(QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), path.to_string_lossy()),
            track: TrackRef::new("Third", "Five").with_album(Some("Broken".to_owned())),
        });

        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(LocalProvider::new(vec![
            music.path().to_path_buf(),
        ])));
        let cache = TempDir::new("artwork-worker-cache");
        let worker = ArtworkWorker::spawn(
            cache.path(),
            Links {
                registry: registry.clone(),
                online: None,
            },
        )
        .unwrap();

        worker.submit(&items, 3, registry);
        let report = wait_until(&worker, |r| r.summary.pending == 0);
        assert_eq!(report.summary.tracks, 5);
        assert_eq!(report.summary.folder, 3, "{report:#?}");
        let (not_checked, failed) = if cfg!(feature = "audio") {
            (1, 1)
        } else {
            (0, 2)
        };
        assert_eq!(
            report.summary.not_checked_offline, not_checked,
            "offline: the third parties were not asked"
        );
        assert_eq!(report.summary.failed, failed, "{report:#?}");
        let broken = &report.items[4];
        assert!(
            matches!(&broken.status, ArtworkStatus::Failed { chain } if chain.contains("ARTWORK_READ")),
            "{broken:#?}"
        );
        let ready = worker.drain_ready();
        assert_eq!(ready.len(), 3, "one key per album, found or not: {ready:?}");
        assert!(worker.drain_ready().is_empty(), "drained");

        let thumbs = worker.images(&ready, ArtworkVariant::Thumb).unwrap();
        assert_eq!(thumbs.len(), 1, "only the found album has images");
        assert_eq!(thumbs[0].key, report.items[0].key);
        assert!(thumbs[0].uri.starts_with("data:image/png;base64,"));
        let labels = worker.images(&ready, ArtworkVariant::Label).unwrap();
        assert_eq!(labels[0].variant, ArtworkVariant::Label);
        assert!(labels[0].uri.starts_with("data:image/png;base64,"));

        // Handed the same queue again, the known album is settled at once.
        worker.submit(&items, 0, ProviderRegistry::new());
        let report = worker.report().unwrap();
        assert_eq!(
            report.items[0].status,
            ArtworkStatus::Found {
                source: ArtworkSource::Folder
            }
        );
    }
}
