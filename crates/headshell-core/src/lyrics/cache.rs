//! The lyrics cache (D-078): LRCLIB's answers per track, and the user's
//! shifts.
//!
//! `<data_dir>/lyrics/index.json` says, per [`LyricsKey`], what LRCLIB
//! answered and how far the user shifted the lines; the lyrics themselves are
//! `<sha256>.json` files beside it, named by their content. The file's own
//! lyrics and the provider's are not kept: they are where the track lives,
//! and read again every time — an edited `.lrc` shows at once.
//!
//! How long an answer is believed:
//!
//! - a synced version of the recording's length: always;
//! - anything less — the nearest version only, unsynced words, "instrumental",
//!   "none": [`SETTLE_TTL`], then LRCLIB is asked again. It grows, and
//!   versions get corrected.
//!
//! The shift is the user's, not the cache's: it survives LRCLIB being asked
//! again. The hazards of a hand-rolled store are handled as the cover
//! cache's are ([`crate::store`]): torn writes, two writers (the index on disk
//! is read again and merged before every write), orphans after an hour's
//! grace, and an index that does not parse set aside, not deleted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::lrclib::Answer;
use super::{Lyrics, LyricsFit};
use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result, io_err};
use crate::ids::LyricsKey;
use crate::store::{content_name, is_content_name, is_our_temporary, write_atomic};

const INDEX_FILE: &str = "index.json";
const INDEX_VERSION: u32 = 1;

/// How long an answer short of "synced, of the recording's length" is
/// believed.
pub(crate) const SETTLE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// An orphan younger than this may belong to another process whose index
/// write has not landed yet.
const ORPHAN_GRACE: Duration = Duration::from_secs(60 * 60);

/// What is kept about one track.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// LRCLIB's answer, when it was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) lrclib: Option<Kept>,
    /// The user's shift, in ms; positive, the lines come sooner.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) offset_ms: i64,
}

// A `&` is what serde's `skip_serializing_if` hands over.
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// LRCLIB's answer as kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub(crate) enum Kept {
    Found {
        checked_at: jiff::Timestamp,
        /// `<sha256>.json`, inside the cache directory.
        file: String,
        synced: bool,
        fit: LyricsFit,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delta_ms: Option<i64>,
        /// LRCLIB's id of the version, for a person reading the index.
        record: i64,
    },
    Instrumental {
        checked_at: jiff::Timestamp,
        record: i64,
    },
    None {
        checked_at: jiff::Timestamp,
        detail: String,
    },
}

impl Kept {
    /// Is it still believed?
    fn fresh(&self) -> bool {
        let (checked_at, forever) = match self {
            Self::Found {
                checked_at,
                synced,
                fit,
                ..
            } => (checked_at, *synced && *fit == LyricsFit::Length),
            Self::Instrumental { checked_at, .. } | Self::None { checked_at, .. } => {
                (checked_at, false)
            }
        };
        forever
            || jiff::Timestamp::now()
                .duration_since(*checked_at)
                .unsigned_abs()
                < SETTLE_TTL
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    version: u32,
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
}

/// The cache, with its index held in memory.
#[derive(Debug)]
pub(crate) struct LyricsCache {
    dir: PathBuf,
    entries: BTreeMap<String, Entry>,
}

fn store_error(detail: String) -> Error {
    Error::new(Stage::LyricsStore, ErrorKind::Lyrics { detail })
}

impl LyricsCache {
    /// Opens the cache directory, creating it if needed, and removes
    /// orphans. An index that does not parse is set aside as
    /// `index.json.broken` — logged, kept for a person to look at — and the
    /// cache starts empty: it is no reason to stop playing.
    ///
    /// # Errors
    /// If the directory cannot be created or read.
    pub(crate) fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|source| io_err(Stage::LyricsStore, dir, source))?;
        let mut cache = Self {
            dir: dir.to_path_buf(),
            entries: BTreeMap::new(),
        };
        cache.entries = cache.read_index()?;
        cache.remove_orphans();
        Ok(cache)
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_FILE)
    }

    fn read_index(&self) -> Result<BTreeMap<String, Entry>> {
        let path = self.index_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(source) => return Err(io_err(Stage::LyricsStore, &path, source)),
        };
        match serde_json::from_str::<IndexFile>(&text) {
            Ok(index) if index.version == INDEX_VERSION => Ok(index.entries),
            outcome => {
                let reason = match outcome {
                    Ok(index) => {
                        format!("index version {}, expected {INDEX_VERSION}", index.version)
                    }
                    Err(err) => err.to_string(),
                };
                let aside = self.dir.join(format!("{INDEX_FILE}.broken"));
                tracing::warn!(
                    path = %path.display(),
                    aside = %aside.display(),
                    reason,
                    "the lyrics index could not be read; it was set aside and the cache starts empty"
                );
                std::fs::rename(&path, &aside)
                    .map_err(|source| io_err(Stage::LyricsStore, &path, source))?;
                Ok(BTreeMap::new())
            }
        }
    }

    /// Reads the index again: another process may have written to it.
    pub(crate) fn reload(&mut self) -> Result<()> {
        self.entries = self.read_index()?;
        Ok(())
    }

    /// The user's shift for a track; `0` when there is none.
    pub(crate) fn offset(&self, key: &LyricsKey) -> i64 {
        self.entries
            .get(key.as_str())
            .map_or(0, |entry| entry.offset_ms)
    }

    /// LRCLIB's answer for a track, if it is still believed and its lyrics
    /// file is still there.
    ///
    /// # Errors
    /// If the lyrics file cannot be read or parsed.
    pub(crate) fn lrclib(&self, key: &LyricsKey) -> Result<Option<Answer>> {
        let Some(kept) = self
            .entries
            .get(key.as_str())
            .and_then(|entry| entry.lrclib.as_ref())
            .filter(|kept| kept.fresh())
        else {
            return Ok(None);
        };
        Ok(Some(match kept {
            Kept::Found {
                file,
                fit,
                delta_ms,
                record,
                ..
            } => {
                let path = self.dir.join(file);
                if !is_ours(file) || !path.is_file() {
                    // The index names a file that is gone: asked again.
                    return Ok(None);
                }
                let text = std::fs::read(&path)
                    .map_err(|source| io_err(Stage::LyricsStore, &path, source))?;
                let lyrics: Lyrics = serde_json::from_slice(&text).map_err(|err| {
                    store_error(format!("{} does not parse: {err}", path.display()))
                })?;
                Answer::Found {
                    lyrics,
                    fit: *fit,
                    delta_ms: *delta_ms,
                    record: *record,
                }
            }
            Kept::Instrumental { record, .. } => Answer::Instrumental { record: *record },
            Kept::None { detail, .. } => Answer::None {
                detail: detail.clone(),
            },
        }))
    }

    /// Keeps LRCLIB's answer for a track.
    ///
    /// # Errors
    /// If a file cannot be written.
    pub(crate) fn put_lrclib(&mut self, key: &LyricsKey, answer: &Answer) -> Result<()> {
        let checked_at = jiff::Timestamp::now();
        let kept = match answer {
            Answer::Found {
                lyrics,
                fit,
                delta_ms,
                record,
            } => {
                let bytes = serde_json::to_vec(lyrics)
                    .map_err(|err| store_error(format!("could not write the lyrics: {err}")))?;
                let file = format!("{}.json", content_name(&bytes));
                let path = self.dir.join(&file);
                // Named by content: if it is there, it is these lyrics.
                if !path.is_file() {
                    write_atomic(&path, &bytes, Stage::LyricsStore)?;
                }
                Kept::Found {
                    checked_at,
                    file,
                    synced: lyrics.synced,
                    fit: *fit,
                    delta_ms: *delta_ms,
                    record: *record,
                }
            }
            Answer::Instrumental { record } => Kept::Instrumental {
                checked_at,
                record: *record,
            },
            Answer::None { detail } => Kept::None {
                checked_at,
                detail: detail.clone(),
            },
        };
        self.update(key, |entry| entry.lrclib = Some(kept))
    }

    /// Keeps the user's shift for a track.
    ///
    /// # Errors
    /// If the index cannot be written.
    pub(crate) fn set_offset(&mut self, key: &LyricsKey, offset_ms: i64) -> Result<()> {
        self.update(key, |entry| entry.offset_ms = offset_ms)
    }

    fn update(&mut self, key: &LyricsKey, change: impl FnOnce(&mut Entry)) -> Result<()> {
        // Merged into what is on disk now, not into what this process read
        // when it opened: the other writer's entries survive.
        let mut entries = self.read_index()?;
        let entry = entries.entry(key.as_str().to_owned()).or_default();
        change(entry);
        if *entry == Entry::default() {
            entries.remove(key.as_str());
        }
        let index = IndexFile {
            version: INDEX_VERSION,
            entries,
        };
        let text = serde_json::to_string_pretty(&index)
            .map_err(|err| store_error(format!("could not write the index: {err}")))?;
        write_atomic(&self.index_path(), text.as_bytes(), Stage::LyricsStore)?;
        self.entries = index.entries;
        Ok(())
    }

    fn remove_orphans(&self) {
        let referenced: std::collections::BTreeSet<&str> = self
            .entries
            .values()
            .filter_map(|entry| match &entry.lrclib {
                Some(Kept::Found { file, .. }) => Some(file.as_str()),
                _ => None,
            })
            .collect();
        let Ok(listing) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut removed = 0usize;
        for entry in listing.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !(is_ours(name) || is_our_temporary(name)) || referenced.contains(name) {
                continue;
            }
            let old_enough = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > ORPHAN_GRACE);
            if old_enough && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::debug!(removed, dir = %self.dir.display(), "lyrics cache orphans removed");
        }
    }
}

/// A lyrics file this cache writes: `<64 hex>.json`.
fn is_ours(name: &str) -> bool {
    name.strip_suffix(".json").is_some_and(is_content_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ProviderId, ProviderTrackId};
    use crate::test_support::TempDir;

    fn key(name: &str) -> LyricsKey {
        LyricsKey::of(&ProviderTrackId::new(ProviderId::new("local"), name))
    }

    fn found(fit: LyricsFit) -> Answer {
        Answer::Found {
            lyrics: super::super::lrc::parse("[00:01.00]one\n[00:02.00]two"),
            fit,
            delta_ms: Some(400),
            record: 11,
        }
    }

    #[test]
    fn an_answer_and_a_shift_survive_reopening_and_the_shift_outlives_a_new_answer() {
        let dir = TempDir::new("lyrics-cache");
        let track = key("/a.flac");
        let mut cache = LyricsCache::open(dir.path()).unwrap();
        assert_eq!(cache.lrclib(&track).unwrap(), None);
        cache.put_lrclib(&track, &found(LyricsFit::Length)).unwrap();
        cache.set_offset(&track, 500).unwrap();

        let reopened = LyricsCache::open(dir.path()).unwrap();
        assert_eq!(
            reopened.lrclib(&track).unwrap(),
            Some(found(LyricsFit::Length))
        );
        assert_eq!(reopened.offset(&track), 500);

        let mut again = reopened;
        again
            .put_lrclib(
                &track,
                &Answer::None {
                    detail: "gone".to_owned(),
                },
            )
            .unwrap();
        assert_eq!(
            again.offset(&track),
            500,
            "the user's shift is not the cache's"
        );
        again.set_offset(&track, 0).unwrap();
        assert_eq!(again.offset(&track), 0);
    }

    #[test]
    fn another_writers_entry_is_not_lost_when_this_one_writes() {
        let dir = TempDir::new("lyrics-cache-two");
        let mut first = LyricsCache::open(dir.path()).unwrap();
        let mut second = LyricsCache::open(dir.path()).unwrap();
        second.set_offset(&key("/theirs"), -250).unwrap();
        first.set_offset(&key("/mine"), 250).unwrap();
        let reopened = LyricsCache::open(dir.path()).unwrap();
        assert_eq!(reopened.offset(&key("/theirs")), -250);
        assert_eq!(reopened.offset(&key("/mine")), 250);
    }

    #[test]
    fn only_a_synced_version_of_the_length_is_believed_for_good() {
        let dir = TempDir::new("lyrics-cache-ttl");
        let track = key("/a.flac");
        let mut cache = LyricsCache::open(dir.path()).unwrap();
        cache
            .put_lrclib(&track, &found(LyricsFit::Closest))
            .unwrap();
        let old = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(31 * 24 * 60 * 60);
        let age = |cache: &mut LyricsCache| {
            if let Some(Entry {
                lrclib:
                    Some(
                        Kept::Found { checked_at, .. }
                        | Kept::Instrumental { checked_at, .. }
                        | Kept::None { checked_at, .. },
                    ),
                ..
            }) = cache.entries.get_mut(track.as_str())
            {
                *checked_at = old;
            }
        };
        age(&mut cache);
        assert_eq!(
            cache.lrclib(&track).unwrap(),
            None,
            "a month-old nearest version is asked again"
        );
        cache.put_lrclib(&track, &found(LyricsFit::Length)).unwrap();
        age(&mut cache);
        assert!(
            cache.lrclib(&track).unwrap().is_some(),
            "an exact one is kept"
        );
    }

    #[test]
    fn a_missing_lyrics_file_is_asked_again_and_a_broken_index_is_set_aside() {
        let dir = TempDir::new("lyrics-cache-broken");
        let track = key("/a.flac");
        let mut cache = LyricsCache::open(dir.path()).unwrap();
        cache.put_lrclib(&track, &found(LyricsFit::Length)).unwrap();
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_ours)
            {
                std::fs::remove_file(path).unwrap();
            }
        }
        assert_eq!(cache.lrclib(&track).unwrap(), None);

        std::fs::write(dir.path().join(INDEX_FILE), "{ not json").unwrap();
        let reopened = LyricsCache::open(dir.path()).unwrap();
        assert!(reopened.entries.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("index.json.broken")).unwrap(),
            "{ not json"
        );
    }
}
