//! The cover cache (D-076): image files named by their content hash, and
//! `index.json`, which says what was found — or not found — for which key.
//!
//! Files and a JSON index, not a table in the library: the listening history
//! is irreplaceable, a cover is not. Three hazards of a hand-rolled store are
//! handled here rather than hoped away:
//!
//! - **A torn write.** The index and every image are written to a temporary
//!   name and renamed into place; a crash leaves the old file or the new one,
//!   never half of one.
//! - **Two writers.** The CLI and the desktop may both run, and in the
//!   desktop the background worker and an `artwork` command write from two
//!   threads. Before writing, the index on disk is read again and this
//!   writer's entry is merged into it: the other's entries are not lost, at
//!   worst one is written twice. Every write has its own temporary name.
//! - **Orphans.** An image no entry points to — left by an entry that was
//!   replaced — and a temporary file a crash left behind are removed when the
//!   cache opens, but only when older than an hour: a younger one may be
//!   another writer's, on its way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::ArtworkSource;
use super::image::{Format, Normalized, Variant};
use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result, io_err};
use crate::ids::ArtworkKey;
use crate::store::{content_name, is_content_name, is_our_temporary, write_atomic};

const INDEX_FILE: &str = "index.json";
const INDEX_VERSION: u32 = 1;

/// How long "not found" is believed. Then the key is asked again: the Cover
/// Art Archive grows, a folder gets its `cover.jpg`.
pub(crate) const NOT_FOUND_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// An orphan younger than this may belong to another process whose index
/// write has not landed yet.
const ORPHAN_GRACE: Duration = Duration::from_secs(60 * 60);

/// What is known about one key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Entry {
    Found {
        source: ArtworkSource,
        checked_at: jiff::Timestamp,
        label: StoredImage,
        thumb: StoredImage,
        /// Where it came from, for a person reading the index: a release MBID,
        /// a file path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    NotFound {
        checked_at: jiff::Timestamp,
        detail: String,
    },
}

/// One image file in the cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredImage {
    /// `<sha256>.<extension>`, inside the cache directory.
    pub(crate) file: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    version: u32,
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
}

/// The cache, with its index held in memory.
#[derive(Debug)]
pub(crate) struct ArtworkCache {
    dir: PathBuf,
    entries: BTreeMap<String, Entry>,
}

fn store_error(detail: String) -> Error {
    Error::new(Stage::ArtworkStore, ErrorKind::Artwork { detail })
}

impl ArtworkCache {
    /// Opens the cache directory, creating it if needed, and removes orphans.
    ///
    /// An index that does not parse is not a reason to stop playing: it is
    /// set aside as `index.json.broken` — kept for a person to look at, not
    /// deleted — and the cache starts empty. It is logged, not swallowed.
    ///
    /// # Errors
    /// If the directory cannot be created or read.
    pub(crate) fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|source| io_err(Stage::ArtworkStore, dir, source))?;
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
            Err(source) => return Err(io_err(Stage::ArtworkStore, &path, source)),
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
                    "the cover index could not be read; it was set aside and the cache starts empty"
                );
                std::fs::rename(&path, &aside)
                    .map_err(|source| io_err(Stage::ArtworkStore, &path, source))?;
                Ok(BTreeMap::new())
            }
        }
    }

    /// Reads the index again: another process may have written to it.
    pub(crate) fn reload(&mut self) -> Result<()> {
        self.entries = self.read_index()?;
        Ok(())
    }

    /// What is known about a key. A "not found" older than
    /// [`NOT_FOUND_TTL`] counts as unknown, and so does a found entry whose
    /// file has gone.
    pub(crate) fn get(&self, key: &ArtworkKey) -> Option<&Entry> {
        let entry = self.entries.get(key.as_str())?;
        match entry {
            Entry::NotFound { checked_at, .. } => {
                let age = jiff::Timestamp::now().duration_since(*checked_at);
                (age.unsigned_abs() < NOT_FOUND_TTL).then_some(entry)
            }
            Entry::Found { label, thumb, .. } => {
                let present = |image: &StoredImage| self.dir.join(&image.file).is_file();
                (present(label) && present(thumb)).then_some(entry)
            }
        }
    }

    /// Keeps a found cover: writes both variants, then the index.
    ///
    /// # Errors
    /// If a file cannot be written.
    pub(crate) fn put_found(
        &mut self,
        key: &ArtworkKey,
        source: ArtworkSource,
        images: &Normalized,
        detail: Option<String>,
    ) -> Result<()> {
        let label = self.write_image(&images.label)?;
        let thumb = self.write_image(&images.thumb)?;
        self.put(
            key,
            Entry::Found {
                source,
                checked_at: jiff::Timestamp::now(),
                label,
                thumb,
                detail,
            },
        )
    }

    /// Keeps "not found", so the same album is not asked again for a while.
    ///
    /// # Errors
    /// If the index cannot be written.
    pub(crate) fn put_not_found(&mut self, key: &ArtworkKey, detail: String) -> Result<()> {
        self.put(
            key,
            Entry::NotFound {
                checked_at: jiff::Timestamp::now(),
                detail,
            },
        )
    }

    fn put(&mut self, key: &ArtworkKey, entry: Entry) -> Result<()> {
        // Merged into what is on disk now, not into what this process read
        // when it opened: the other writer's entries survive.
        let mut entries = self.read_index()?;
        entries.insert(key.as_str().to_owned(), entry);
        let index = IndexFile {
            version: INDEX_VERSION,
            entries,
        };
        let text = serde_json::to_string_pretty(&index)
            .map_err(|err| store_error(format!("could not write the index: {err}")))?;
        write_atomic(&self.index_path(), text.as_bytes(), Stage::ArtworkStore)?;
        self.entries = index.entries;
        Ok(())
    }

    fn write_image(&self, variant: &Variant) -> Result<StoredImage> {
        let name = content_name(&variant.bytes);
        let file = format!("{name}.{}", variant.format.extension());
        let path = self.dir.join(&file);
        // Named by content: if it is there, it is this image.
        if !path.is_file() {
            write_atomic(&path, &variant.bytes, Stage::ArtworkStore)?;
        }
        Ok(StoredImage {
            file,
            width: variant.width,
            height: variant.height,
        })
    }

    /// Reads a kept image back, with its type.
    ///
    /// # Errors
    /// If the file is gone or unreadable, or its name is not one this cache
    /// writes.
    pub(crate) fn read_image(&self, image: &StoredImage) -> Result<(Vec<u8>, Format)> {
        let format = cache_file_format(&image.file)
            .ok_or_else(|| store_error(format!("`{}` is not a cache file name", image.file)))?;
        let path = self.dir.join(&image.file);
        let bytes =
            std::fs::read(&path).map_err(|source| io_err(Stage::ArtworkStore, &path, source))?;
        Ok((bytes, format))
    }

    fn remove_orphans(&self) {
        let referenced: std::collections::BTreeSet<&str> = self
            .entries
            .values()
            .flat_map(|entry| match entry {
                Entry::Found { label, thumb, .. } => vec![label.file.as_str(), thumb.file.as_str()],
                Entry::NotFound { .. } => Vec::new(),
            })
            .collect();
        let Ok(listing) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut removed = 0usize;
        for entry in listing.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let ours = cache_file_format(name).is_some() || is_our_temporary(name);
            if !ours || referenced.contains(name) {
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
            tracing::debug!(removed, dir = %self.dir.display(), "cover cache orphans removed");
        }
    }
}

/// The format of a name this cache writes (`<64 hex>.<extension>`), or
/// `None` for anything else — the cache never touches a file it did not
/// name.
fn cache_file_format(name: &str) -> Option<Format> {
    let (stem, extension) = name.split_once('.')?;
    if !is_content_name(stem) {
        return None;
    }
    Format::from_extension(extension)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn images(label: &[u8], thumb: &[u8]) -> Normalized {
        let variant = |bytes: &[u8]| Variant {
            bytes: bytes.to_vec(),
            format: Format::Png,
            width: 1,
            height: 1,
        };
        Normalized {
            label: variant(label),
            thumb: variant(thumb),
        }
    }

    #[test]
    fn a_found_cover_survives_reopening_and_its_files_are_named_by_content() {
        let dir = TempDir::new("artwork-cache");
        let key = ArtworkKey::album("artist", "album");
        let mut cache = ArtworkCache::open(dir.path()).unwrap();
        assert!(cache.get(&key).is_none());
        cache
            .put_found(&key, ArtworkSource::Embedded, &images(b"L", b"T"), None)
            .unwrap();

        let reopened = ArtworkCache::open(dir.path()).unwrap();
        let Some(Entry::Found { label, source, .. }) = reopened.get(&key) else {
            panic!("the entry was lost: {:?}", reopened.get(&key));
        };
        assert_eq!(*source, ArtworkSource::Embedded);
        assert_eq!(reopened.read_image(label).unwrap().0, b"L");
        assert!(label.file.ends_with(".png") && label.file.len() == 64 + 4);
    }

    #[test]
    fn another_writers_entry_is_not_lost_when_this_one_writes() {
        let dir = TempDir::new("artwork-cache");
        let mine = ArtworkKey::album("mine", "x");
        let theirs = ArtworkKey::album("theirs", "y");
        let mut first = ArtworkCache::open(dir.path()).unwrap();
        let mut second = ArtworkCache::open(dir.path()).unwrap();
        second.put_not_found(&theirs, "none".to_owned()).unwrap();
        // `first` opened before `second` wrote; its write must keep that entry.
        first.put_not_found(&mine, "none".to_owned()).unwrap();
        let reopened = ArtworkCache::open(dir.path()).unwrap();
        assert!(reopened.get(&mine).is_some());
        assert!(reopened.get(&theirs).is_some());
    }

    #[test]
    fn an_old_not_found_is_asked_again_and_a_missing_file_is_unknown() {
        let dir = TempDir::new("artwork-cache");
        let key = ArtworkKey::album("a", "b");
        let mut cache = ArtworkCache::open(dir.path()).unwrap();
        cache.entries.insert(
            key.as_str().to_owned(),
            Entry::NotFound {
                checked_at: jiff::Timestamp::now()
                    - jiff::SignedDuration::from_secs(31 * 24 * 60 * 60),
                detail: "old".to_owned(),
            },
        );
        assert!(cache.get(&key).is_none(), "a month-old miss is asked again");

        cache
            .put_found(&key, ArtworkSource::Folder, &images(b"L2", b"T2"), None)
            .unwrap();
        let Some(Entry::Found { label, .. }) = cache.get(&key).cloned() else {
            panic!("not stored");
        };
        std::fs::remove_file(dir.path().join(&label.file)).unwrap();
        assert!(
            cache.get(&key).is_none(),
            "an entry without its file is not a cover"
        );
    }

    #[test]
    fn a_broken_index_is_set_aside_not_deleted() {
        let dir = TempDir::new("artwork-cache");
        std::fs::write(dir.path().join(INDEX_FILE), "{ not json").unwrap();
        let cache = ArtworkCache::open(dir.path()).unwrap();
        assert!(cache.entries.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("index.json.broken")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn only_names_this_cache_writes_are_its_files() {
        let hash = "a".repeat(64);
        assert_eq!(cache_file_format(&format!("{hash}.png")), Some(Format::Png));
        assert_eq!(cache_file_format(&format!("{hash}.exe")), None);
        assert_eq!(cache_file_format("index.json"), None);
        assert_eq!(cache_file_format(&format!("{}.png", "A".repeat(64))), None);
    }
}
