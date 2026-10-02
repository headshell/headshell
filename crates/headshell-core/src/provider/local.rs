//! The local file provider (PLAN §1.2, D-017).
//!
//! Scans a directory, reads the tags of audio files and keeps an index in
//! memory. It has the `SEARCH | BROWSE | STREAM` capabilities; there is no
//! `CONTROL` — the local disk is not remote-controlled.
//!
//! Reading tags needs the `audio` feature (symphonia). With the feature off
//! the provider still compiles but derives metadata from file names — so the
//! `provider list`/`search` surface works in builds without an audio pipeline
//! too.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::identity::normalize;
#[cfg(feature = "audio")]
use crate::ids::Isrc;
use crate::ids::{ProviderId, ProviderTrackId};
use crate::model::TrackRef;

use super::{
    ArtworkImage, AudioSource, Capabilities, Provider, ProviderFuture, ProviderHealth,
    ProviderInfo, ProviderLyrics, ProviderTrack,
};

/// The recognised audio file extensions.
///
/// The list is deliberately narrow: what symphonia can decode with the
/// features we turned on. Scanning an extension we do not recognise only
/// postpones the question "why won't it play?" from scan time to play time.
pub const AUDIO_EXTENSIONS: &[&str] = &["flac", "mp3", "ogg", "oga", "m4a", "mp4", "aac", "wav"];

/// The recognised video file extensions (D-086).
///
/// A video is scanned like a song — its tags, or its file name — and plays
/// as one when the core can decode its sound; the shell shows its picture.
/// One whose sound the core cannot decode (Opus in a `.webm`) is still taken:
/// it plays as a video only. `.mp4` is on both lists; what is inside it
/// decides, not the name.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm"];

/// The result of a scan — how many files were seen, how many were taken, how
/// many were skipped and why.
///
/// K9: an operation that can partly succeed returns a summary. "Found 12
/// tracks" is not enough; in a directory of 300 files, why 288 were skipped
/// must be visible.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScanSummary {
    /// The number of files visited (directories excluded).
    pub files_seen: usize,
    /// Those carrying an audio or a video extension (D-086).
    pub audio_files: usize,
    /// The number of tracks that went into the index.
    pub indexed: usize,
    /// Those derived from the file name because their tags could not be read.
    pub tag_fallback: usize,
    /// Unreadable/corrupt files.
    pub failed: usize,
    /// Inaccessible subdirectories (permissions etc.).
    pub unreadable_dirs: usize,
    /// Files not read again because their stamp did not change.
    ///
    /// This number is why scanning is fast: on the second scan almost every file
    /// lands here.
    pub unchanged: usize,
}

impl ScanSummary {
    /// Copies the counters into the diagnostics recorder.
    pub fn record_into(&self, recorder: &mut crate::diag::Recorder) {
        let n = |v: usize| i64::try_from(v).unwrap_or(i64::MAX);
        recorder.set("scan.files_seen", n(self.files_seen));
        recorder.set("scan.audio_files", n(self.audio_files));
        recorder.set("scan.indexed", n(self.indexed));
        recorder.set("scan.tag_fallback", n(self.tag_fallback));
        recorder.set("scan.failed", n(self.failed));
        recorder.set("scan.unreadable_dirs", n(self.unreadable_dirs));
        recorder.set("scan.unchanged", n(self.unchanged));
    }
}

/// An indexed local file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedFile {
    path: PathBuf,
    track: TrackRef,
}

/// A file seen in a scan — ready to be written to the persistent catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: PathBuf,
    /// The file's last modification time (ms).
    pub mtime_ms: Option<i64>,
    /// The metadata read. If `None` the file has not changed — the caller should
    /// leave the row in the catalog as it is; there is no need to read it again.
    pub track: Option<TrackRef>,
    /// Whether the metadata came from the tags (not from the file name).
    pub from_tags: bool,
}

/// The local file provider.
pub struct LocalProvider {
    id: ProviderId,
    roots: Vec<PathBuf>,
    index: RwLock<Vec<IndexedFile>>,
    last_scan: RwLock<ScanSummary>,
}

impl std::fmt::Debug for LocalProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalProvider")
            .field("roots", &self.roots)
            .field("indexed", &self.index.read().map(|i| i.len()).unwrap_or(0))
            .finish()
    }
}

impl LocalProvider {
    /// Sets up a provider that scans the given root directories (it does not
    /// scan yet).
    #[must_use]
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            id: ProviderId::new("local"),
            roots,
            index: RwLock::new(Vec::new()),
            last_scan: RwLock::new(ScanSummary::default()),
        }
    }

    /// Rescans the root directories and replaces the index.
    ///
    /// # Errors
    /// If no root can be read. Errors in individual files **are not errors** —
    /// they are counted and reported in the summary (K9).
    pub fn rescan_now(&self) -> Result<ScanSummary> {
        let mut summary = ScanSummary::default();
        let mut found = Vec::new();

        for root in &self.roots {
            scan_dir(root, &mut found, &mut summary)?;
        }

        // The order must be deterministic; the same directory must give the same
        // result in two runs.
        found.sort_by(|a, b| a.path.cmp(&b.path));
        summary.indexed = found.len();

        let mut index = self.index.write().map_err(|_| poisoned())?;
        *index = found;
        let mut last = self.last_scan.write().map_err(|_| poisoned())?;
        *last = summary.clone();
        Ok(summary)
    }

    /// The summary of the last scan.
    ///
    /// # Errors
    /// If the internal lock is broken.
    pub fn last_scan(&self) -> Result<ScanSummary> {
        Ok(self.last_scan.read().map_err(|_| poisoned())?.clone())
    }

    /// The **newest directory stamp** in the root tree (D-025).
    ///
    /// Only directories are visited; files are not `stat`ed: the question is "is
    /// a scan worth it", not "what changed". When a file is added/deleted the
    /// mtime of its directory changes, so additions and deletions show up here.
    ///
    /// **What does not show:** a file edited in place (re-tagging). The file
    /// changes but the directory stamp does not. Catching that would mean
    /// `stat`ing every file — that is, the incremental scan itself. If the user
    /// changed tags they should run `headshell provider scan`; knowing this is
    /// better than saying "unchanged" as if we did not know it (K9).
    ///
    /// An unreadable directory **is not skipped silently**: it is counted, and
    /// if there is an unreadable directory "I don't know" (`None`) is returned —
    /// there might be a change there, and saying "unchanged" would hide it.
    ///
    /// # Errors
    /// It produces no errors right now; it is a `Result` so the signature does
    /// not have to change later.
    pub fn newest_dir_mtime_ms(&self) -> Result<Option<i64>> {
        let mut newest: Option<i64> = None;
        let mut unreadable = 0usize;
        for root in &self.roots {
            walk_dir_stamps(root, &mut newest, &mut unreadable);
        }
        if unreadable > 0 {
            tracing::debug!(
                unreadable,
                "there is an unreadable directory; the staleness question could not be answered"
            );
            return Ok(None);
        }
        Ok(newest)
    }

    /// Scans the root directories and produces rows **ready to be written to the
    /// persistent catalog**.
    ///
    /// `known` is the previously seen `path → mtime_ms` map; the tags of a file
    /// whose stamp has not changed are **not read again**. This is the expensive
    /// part of a scan: in a library of 10,000 files decoding them all every time
    /// takes minutes, comparing stamps takes seconds.
    ///
    /// No metadata is returned for unchanged files; the caller leaves those rows
    /// in the catalog as they are. That is why it returns `Option<TrackRef>`.
    ///
    /// # Errors
    /// If a root directory cannot be read. Errors in individual files are
    /// counted in the summary (K9).
    pub fn scan_for_catalog(
        &self,
        known: &std::collections::HashMap<String, i64>,
    ) -> Result<(Vec<ScannedFile>, ScanSummary)> {
        let mut summary = ScanSummary::default();
        let mut out = Vec::new();

        for root in &self.roots {
            scan_dir_for_catalog(root, known, &mut out, &mut summary)?;
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        summary.indexed = out.len();

        let mut last = self.last_scan.write().map_err(|_| poisoned())?;
        *last = summary.clone();
        Ok((out, summary))
    }

    /// Produces a provider id from a file path.
    fn track_id(&self, path: &Path) -> ProviderTrackId {
        ProviderTrackId::new(self.id.clone(), path.to_string_lossy().into_owned())
    }

    /// Is the path under one of the scanned roots?
    ///
    /// `canonicalize` resolves symbolic links and `..`; otherwise a path like
    /// `~/Music/../../etc/passwd` would slip past the check. If the path cannot
    /// be resolved (the file does not exist) it **is rejected** — accepting the
    /// suspicious is a file-read vulnerability.
    fn is_within_roots(&self, path: &Path) -> bool {
        let Ok(target) = path.canonicalize() else {
            return false;
        };
        self.roots.iter().any(|root| {
            root.canonicalize()
                .is_ok_and(|root| target.starts_with(&root))
        })
    }
}

fn poisoned() -> Error {
    Error::new(
        Stage::ProviderCall,
        ErrorKind::InvalidInput {
            detail: "the local provider index is broken (the lock was poisoned)".to_owned(),
        },
    )
}

/// Scans a directory recursively.
///
/// If a subdirectory cannot be read it **does not stop**: it counts and
/// carries on. Losing a library of 10,000 files over a single permission
/// error is unacceptable.
/// Walks the directory tree and finds the newest directory stamp (directories
/// only).
///
/// The error **is not swallowed**: every unreadable directory is counted and
/// the caller turns it into "I don't know".
fn walk_dir_stamps(dir: &Path, newest: &mut Option<i64>, unreadable: &mut usize) {
    let stamp = std::fs::metadata(dir)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|since| i64::try_from(since.as_millis()).ok());
    match stamp {
        Some(ms) => {
            if newest.is_none_or(|current| ms > current) {
                *newest = Some(ms);
            }
        }
        None => *unreadable += 1,
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        *unreadable += 1;
        return;
    };
    for entry in entries.flatten() {
        // `file_type` does not follow symbolic links: a link loop must not hang the
        // scan.
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            walk_dir_stamps(&entry.path(), newest, unreadable);
        }
    }
}

fn scan_dir(dir: &Path, out: &mut Vec<IndexedFile>, summary: &mut ScanSummary) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) => {
            // If the root directory cannot be read, that is a real error: the user gave
            // a wrong path.
            if out.is_empty() && summary.files_seen == 0 {
                return Err(crate::error::io_err(Stage::ProviderCall, dir, source));
            }
            summary.unreadable_dirs += 1;
            tracing::warn!(dir = %dir.display(), error = %source, "could not read the directory, skipping it");
            return Ok(());
        }
    };

    for entry in entries {
        let Ok(entry) = entry else {
            summary.failed += 1;
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            summary.failed += 1;
            continue;
        };

        if file_type.is_dir() {
            scan_dir(&path, out, summary)?;
            continue;
        }
        summary.files_seen += 1;

        if !has_audio_extension(&path) {
            continue;
        }
        summary.audio_files += 1;

        match read_track(&path) {
            Ok((track, from_tags)) => {
                if !from_tags {
                    summary.tag_fallback += 1;
                }
                out.push(IndexedFile { path, track });
            }
            Err(err) => {
                summary.failed += 1;
                tracing::warn!(
                    file = %path.display(),
                    error = %err.chain_text(),
                    "could not read the file, skipping it"
                );
            }
        }
    }
    Ok(())
}

/// Scans for the persistent catalog: does not read the tags of unchanged
/// files.
fn scan_dir_for_catalog(
    dir: &Path,
    known: &std::collections::HashMap<String, i64>,
    out: &mut Vec<ScannedFile>,
    summary: &mut ScanSummary,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) => {
            if out.is_empty() && summary.files_seen == 0 {
                return Err(crate::error::io_err(Stage::ProviderCall, dir, source));
            }
            summary.unreadable_dirs += 1;
            tracing::warn!(dir = %dir.display(), error = %source, "could not read the directory, skipping it");
            return Ok(());
        }
    };

    for entry in entries {
        let Ok(entry) = entry else {
            summary.failed += 1;
            continue;
        };
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            summary.failed += 1;
            continue;
        };

        if file_type.is_dir() {
            scan_dir_for_catalog(&path, known, out, summary)?;
            continue;
        }
        summary.files_seen += 1;

        if !has_audio_extension(&path) {
            continue;
        }
        summary.audio_files += 1;

        let mtime_ms = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|since| i64::try_from(since.as_millis()).ok());

        // If the stamp did not change, do not read the tags again — this is the
        // expensive part of a scan.
        let reference = path.to_string_lossy().into_owned();
        if let (Some(mtime), Some(previous)) = (mtime_ms, known.get(&reference)) {
            if mtime == *previous {
                summary.unchanged += 1;
                out.push(ScannedFile {
                    path,
                    mtime_ms,
                    track: None,
                    from_tags: false,
                });
                continue;
            }
        }

        match read_track(&path) {
            Ok((track, from_tags)) => {
                if !from_tags {
                    summary.tag_fallback += 1;
                }
                out.push(ScannedFile {
                    path,
                    mtime_ms,
                    track: Some(track),
                    from_tags,
                });
            }
            Err(err) => {
                summary.failed += 1;
                tracing::warn!(
                    file = %path.display(),
                    error = %err.chain_text(),
                    "could not read the file, skipping it"
                );
            }
        }
    }
    Ok(())
}

fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .is_some_and(|ext| {
            AUDIO_EXTENSIONS.contains(&ext.as_str()) || VIDEO_EXTENSIONS.contains(&ext.as_str())
        })
}

/// Might the file carry a picture? Only a video extension can.
fn has_video_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
}

/// Does the file hold a video track (D-086)? The extension only says it
/// might; the container's tracks decide (an `.mp4` is often audio alone).
///
/// Without the `audio` feature the container cannot be opened, and the
/// extension is all there is to go by.
pub(crate) fn has_video(path: &Path) -> Result<bool> {
    if !has_video_extension(path) {
        return Ok(false);
    }
    #[cfg(feature = "audio")]
    {
        tags::has_video(path)
    }
    #[cfg(not(feature = "audio"))]
    {
        Ok(true)
    }
}

/// Reads a track's metadata from the file.
///
/// Returns `(track, from_tags)`. If the second field is `false`, the metadata
/// was derived from the file name — not a loss, but a drop that must be
/// counted (K9).
pub(crate) fn read_track(path: &Path) -> Result<(TrackRef, bool)> {
    #[cfg(feature = "audio")]
    {
        match tags::read(path) {
            Ok(Some(track)) => return Ok((track, true)),
            Ok(None) => {}
            Err(err) => return Err(err),
        }
    }
    Ok((track_from_filename(path), false))
}

/// Derives metadata from the file name if there are no tags.
///
/// Recognises the `Artist - Title.flac` form; if it does not, the title is
/// the file name and the artist the parent directory. We do not make things
/// up — we say what we found.
fn track_from_filename(path: &Path) -> TrackRef {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let parent_name = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str());

    if let Some((artist, title)) = stem.split_once(" - ") {
        let artist = artist.trim();
        let title = title.trim();
        if !artist.is_empty() && !title.is_empty() {
            return TrackRef::new(artist, title).with_album(parent_name.map(str::to_owned));
        }
    }

    TrackRef::new(parent_name.unwrap_or("Unknown Artist"), stem)
        .with_album(parent_name.map(str::to_owned))
}

/// Reading tags — only with the `audio` feature.
#[cfg(feature = "audio")]
mod tags {
    use super::{Isrc, Path, Result, TrackRef};
    use crate::diag::Stage;
    use crate::error::{Error, ErrorKind};

    use symphonia::core::formats::probe::Hint;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardTag};

    /// Reads the file's tags.
    ///
    /// `Ok(None)`: the file opened but there are no usable tags — the caller
    /// falls back to the file name. `Err`: the file could not be opened/is
    /// corrupt.
    pub(super) fn read(path: &Path) -> Result<Option<TrackRef>> {
        let file = std::fs::File::open(path)
            .map_err(|source| crate::error::io_err(Stage::PlaybackDecode, path, source))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let mut reader = symphonia::default::get_probe()
            .probe(&hint, mss, Default::default(), MetadataOptions::default())
            .map_err(|source| {
                Error::new(
                    Stage::PlaybackDecode,
                    ErrorKind::Audio {
                        detail: format!("could not open {}: {source}", path.display()),
                    },
                )
            })?;

        // Take the duration from the container: the distinguishing field of fuzzy
        // matching (K6).
        let duration_ms = reader.tracks().iter().find_map(|track| {
            let time_base = track.time_base?;
            let duration = track.duration?;
            let millis = time_base.calc_duration(duration)?.as_millis();
            u64::try_from(millis).ok().filter(|ms| *ms > 0)
        });

        let mut artist = None;
        let mut album_artist = None;
        let mut title = None;
        let mut album = None;
        let mut isrc = None;

        let mut collect = |tags: &[symphonia::core::meta::Tag]| {
            for tag in tags {
                match tag.std.as_ref() {
                    Some(StandardTag::Artist(v)) if artist.is_none() => {
                        artist = Some(v.to_string());
                    }
                    Some(StandardTag::AlbumArtist(v)) if album_artist.is_none() => {
                        album_artist = Some(v.to_string());
                    }
                    Some(StandardTag::TrackTitle(v)) if title.is_none() => {
                        title = Some(v.to_string());
                    }
                    Some(StandardTag::Album(v)) if album.is_none() => {
                        album = Some(v.to_string());
                    }
                    Some(StandardTag::IdentIsrc(v)) if isrc.is_none() => {
                        isrc = Isrc::parse(v);
                    }
                    _ => {}
                }
            }
        };

        if let Some(revision) = reader.metadata().current() {
            collect(&revision.media.tags);
        }

        // If either the title or the artist is missing the tags count as unusable.
        let (Some(title), Some(artist)) = (title, artist.or(album_artist)) else {
            return Ok(None);
        };

        Ok(Some(
            TrackRef::new(artist, title)
                .with_album(album)
                .with_duration_ms(duration_ms)
                .with_isrc(isrc),
        ))
    }

    /// Does the container hold a video track (D-086)?
    pub(super) fn has_video(path: &Path) -> Result<bool> {
        let file = std::fs::File::open(path)
            .map_err(|source| crate::error::io_err(Stage::VideoLookup, path, source))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let reader = symphonia::default::get_probe()
            .probe(&hint, mss, Default::default(), MetadataOptions::default())
            .map_err(|source| {
                Error::new(
                    Stage::VideoLookup,
                    ErrorKind::Audio {
                        detail: format!("could not open {}: {source}", path.display()),
                    },
                )
            })?;
        Ok(reader.tracks().iter().any(|track| {
            matches!(
                track.codec_params,
                Some(symphonia::core::codecs::CodecParameters::Video(_))
            )
        }))
    }
}

impl Provider for LocalProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            display_name: "Local files".to_owned(),
            // No CONTROL: the local disk is not remote-controlled. ARTWORK: the
            // picture in a file's tags, or an image in its folder (D-076).
            // LYRICS: a `.lrc` beside the file, or the lyrics in its tags
            // (D-078). VIDEO: a video file is its own video (D-086).
            capabilities: Capabilities::SEARCH
                | Capabilities::BROWSE
                | Capabilities::STREAM
                | Capabilities::ARTWORK
                | Capabilities::LYRICS
                | Capabilities::VIDEO,
        }
    }

    fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
        Box::pin(async move {
            let missing: Vec<String> = self
                .roots
                .iter()
                .filter(|root| !root.is_dir())
                .map(|root| root.display().to_string())
                .collect();

            let count = self.index.read().map_err(|_| poisoned())?.len();
            Ok(ProviderHealth {
                id: self.id.clone(),
                reachable: missing.is_empty(),
                track_count: Some(count),
                detail: if missing.is_empty() {
                    (count == 0).then(|| {
                        "the index is empty — `headshell provider scan` may not have been run"
                            .to_owned()
                    })
                } else {
                    Some(format!("unreachable directory: {}", missing.join(", ")))
                },
            })
        })
    }

    /// It answers by looking at directory stamps (D-025) — without a full scan.
    fn catalog_changed_since(&self, since_ms: i64) -> ProviderFuture<'_, Option<bool>> {
        Box::pin(async move { Ok(self.newest_dir_mtime_ms()?.map(|newest| newest > since_ms)) })
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
        Box::pin(async move {
            let needle = normalize::normalize_text(query);
            if needle.is_empty() {
                return Ok(Vec::new());
            }
            let index = self.index.read().map_err(|_| poisoned())?;
            let hits = index
                .iter()
                .filter(|file| {
                    let artist = normalize::normalize_text(&file.track.artist);
                    let title = normalize::normalize_text(&file.track.title);
                    let album = file
                        .track
                        .album
                        .as_deref()
                        .map(normalize::normalize_text)
                        .unwrap_or_default();
                    artist.contains(&needle) || title.contains(&needle) || album.contains(&needle)
                })
                .take(limit)
                .map(|file| ProviderTrack {
                    id: self.track_id(&file.path),
                    track: file.track.clone(),
                })
                .collect();
            Ok(hits)
        })
    }

    fn scan_catalog<'a>(
        &'a self,
        known: &'a std::collections::HashMap<String, i64>,
    ) -> ProviderFuture<'a, Option<super::CatalogScan>> {
        Box::pin(async move {
            let (files, summary) = self.scan_for_catalog(known)?;
            let tracks = files
                .into_iter()
                .map(|file| super::ScannedItem {
                    id: self.track_id(&file.path),
                    mtime_ms: file.mtime_ms,
                    track: file.track,
                    from_tags: file.from_tags,
                })
                .collect();
            Ok(Some(super::CatalogScan { tracks, summary }))
        })
    }

    fn resolve_source<'a>(
        &'a self,
        id: &'a ProviderTrackId,
    ) -> ProviderFuture<'a, Option<AudioSource>> {
        Box::pin(async move {
            if id.provider != self.id {
                return Ok(None);
            }
            let path = PathBuf::from(&id.id);
            // Verify that the path is **under** the scanned roots.
            //
            // This used to look at the memory index; the index now lives in the
            // persistent catalog, and the provider does not see it. The root check
            // does the same job and is sturdier: `resolve_source` is not an
            // arbitrary file-read surface, `/etc/passwd` cannot be played from here.
            if !self.is_within_roots(&path) {
                return Ok(None);
            }
            if !path.is_file() {
                // Deleted after it was indexed: not a silent None but an explicit error.
                return Err(Error::new(
                    Stage::PlaybackResolve,
                    ErrorKind::NotFound {
                        what: format!("{} (deleted since it was indexed)", path.display()),
                    },
                ));
            }
            Ok(Some(AudioSource::LocalFile { path }))
        })
    }

    /// A video file is its own video (D-086); an audio file has none. The
    /// same root check as `resolve_source`: the shell is handed a path to
    /// serve, and only the music folders' files may be one.
    fn resolve_video<'a>(
        &'a self,
        id: &'a ProviderTrackId,
    ) -> ProviderFuture<'a, Option<crate::video::VideoSource>> {
        Box::pin(async move {
            if id.provider != self.id {
                return Ok(None);
            }
            let path = PathBuf::from(&id.id);
            if !self.is_within_roots(&path) {
                return Ok(None);
            }
            if !path.is_file() {
                return Err(Error::new(
                    Stage::VideoLookup,
                    ErrorKind::NotFound {
                        what: format!("{} (deleted since it was indexed)", path.display()),
                    },
                ));
            }
            Ok(has_video(&path)?.then_some(crate::video::VideoSource::LocalFile { path }))
        })
    }

    /// The picture in the file's tags, then an image in its folder (D-076).
    /// `size` is ignored: a file has the size it has, the core resizes.
    ///
    /// The same guard as `resolve_source`: only a file under the scanned roots
    /// — this is not a surface for reading arbitrary files.
    fn artwork<'a>(
        &'a self,
        id: &'a ProviderTrackId,
        size: u32,
    ) -> ProviderFuture<'a, Option<ArtworkImage>> {
        let _ = size;
        Box::pin(async move {
            if id.provider != self.id {
                return Ok(None);
            }
            let path = PathBuf::from(&id.id);
            if !self.is_within_roots(&path) {
                return Ok(None);
            }
            crate::artwork::file_cover(&path)
        })
    }

    /// A `.lrc` beside the file with its name, then the lyrics in its tags
    /// (D-078). The same guard as `resolve_source`: only a file under the
    /// scanned roots.
    fn lyrics<'a>(&'a self, id: &'a ProviderTrackId) -> ProviderFuture<'a, Option<ProviderLyrics>> {
        Box::pin(async move {
            if id.provider != self.id {
                return Ok(None);
            }
            let path = PathBuf::from(&id.id);
            if !self.is_within_roots(&path) {
                return Ok(None);
            }
            crate::lyrics::file_lyrics(&path)
        })
    }
}

/// Makes it registrable in the registry.
impl LocalProvider {
    /// Wraps it in an `Arc`, ready for the registry.
    #[must_use]
    pub fn shared(self) -> Arc<dyn Provider> {
        Arc::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> crate::test_support::TempDir {
        crate::test_support::TempDir::new(&format!("local-{label}"))
    }

    /// The directory of the real audio fixtures (sine tones produced with
    /// `ffmpeg`, names in English — D-036).
    fn audio_fixtures() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/audio"))
    }

    /// Copies a fixture into the temporary directory.
    fn copy_fixture(dir: &Path, name: &str, to: &str) {
        let target = dir.join(to);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("subdirectory");
        }
        std::fs::copy(audio_fixtures().join(name), &target)
            .unwrap_or_else(|e| panic!("{name} must be copied: {e}"));
    }

    /// The synthetic video fixtures (D-086).
    #[cfg(feature = "audio")]
    fn video_fixtures() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/video"))
    }

    /// A video file is scanned like a song and is its own video; an `.mp4`
    /// without a picture and a song are not (D-086).
    #[cfg(feature = "audio")]
    #[tokio::test]
    async fn a_video_file_is_its_own_video() {
        let dir = temp_dir("video");
        for name in [
            "Video Artist - Clip.mp4",
            "Webm Artist - Opus Clip.webm",
            "Audio Artist - Plain.mp4",
        ] {
            std::fs::copy(video_fixtures().join(name), dir.join(name)).expect("fixture");
        }
        copy_fixture(&dir, "tagged.flac", "tagged.flac");

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        let summary = provider.rescan_now().expect("scan");
        assert_eq!(summary.indexed, 4, "{summary:?}");

        let video = |name: &str| {
            let id = provider.track_id(&dir.join(name));
            let provider = &provider;
            async move { provider.resolve_video(&id).await.expect("resolve_video") }
        };
        assert_eq!(
            video("Video Artist - Clip.mp4").await,
            Some(crate::video::VideoSource::LocalFile {
                path: dir.join("Video Artist - Clip.mp4")
            })
        );
        assert!(video("Webm Artist - Opus Clip.webm").await.is_some());
        assert_eq!(video("Audio Artist - Plain.mp4").await, None);
        assert_eq!(video("tagged.flac").await, None);

        // A path outside the music folders is never handed to the shell.
        let outside = ProviderTrackId::new(provider.id.clone(), "/etc/passwd");
        assert_eq!(
            provider.resolve_video(&outside).await.expect("outside"),
            None
        );
    }

    fn write(dir: &Path, name: &str, body: &[u8]) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("subdirectory");
        }
        std::fs::write(&path, body).expect("the file must be written");
    }

    #[test]
    fn scan_counts_every_file_it_saw_and_skipped() {
        let dir = temp_dir("scan");
        copy_fixture(&dir, "tagged.flac", "tagged.flac");
        copy_fixture(&dir, "Test Artist - Mp3 Track.mp3", "Sub/Track.mp3");
        write(&dir, "cover.jpg", b"cover");
        write(&dir, "notes.txt", b"note");

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        let summary = provider.rescan_now().expect("scan");

        assert_eq!(summary.files_seen, 4);
        assert_eq!(summary.audio_files, 2, "only flac and mp3");
        assert_eq!(summary.indexed, 2);
        assert_eq!(summary.failed, 0, "valid files must not fail");
        // K9: the numbers must add up — a lost file must not be swallowed silently.
        assert_eq!(summary.indexed + summary.failed, summary.audio_files);
    }

    #[cfg(feature = "audio")]
    #[test]
    fn a_corrupt_file_is_counted_not_swallowed() {
        // A corrupt file must not bring the scan down, but it must not be invisible
        // either (K9). Only meaningful with `audio` on: the decoder notices the
        // corruption.
        let dir = temp_dir("corrupt");
        copy_fixture(&dir, "tagged.flac", "sound.flac");
        write(&dir, "corrupt.flac", b"this is not an audio file");

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        let summary = provider.rescan_now().expect("the scan must carry on");

        assert_eq!(summary.audio_files, 2);
        assert_eq!(summary.indexed, 1, "only the sound file must be indexed");
        assert_eq!(summary.failed, 1, "the corrupt file must be counted");
    }

    /// With `audio` off the metadata comes from the file name; in this mode the
    /// scan must still work and every file must go into the index (with no
    /// decoder, no file counts as "corrupt").
    #[cfg(not(feature = "audio"))]
    #[test]
    fn without_the_audio_feature_metadata_comes_from_filenames() {
        let dir = temp_dir("no-audio-feature");
        copy_fixture(&dir, "tagged.flac", "First Last - Track.flac");
        write(&dir, "corrupt.flac", b"this is not an audio file");

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        let summary = provider.rescan_now().expect("scan");

        assert_eq!(summary.audio_files, 2);
        assert_eq!(
            summary.indexed, 2,
            "without a decoder no file is filtered out"
        );
        assert_eq!(
            summary.tag_fallback, 2,
            "all of them must fall back to the file name and be counted"
        );
    }

    #[cfg(feature = "audio")]
    #[test]
    fn tags_are_read_from_the_file_not_guessed_from_its_name() {
        let dir = temp_dir("tags");
        // The file name is deliberately misleading: the tags must win.
        copy_fixture(&dir, "tagged.flac", "wrong-name.flac");

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        let summary = provider.rescan_now().expect("scan");
        assert_eq!(summary.tag_fallback, 0, "the tags must be readable");

        let index = provider.index.read().unwrap();
        let track = &index[0].track;
        assert_eq!(track.artist, "Test Artist");
        // The diacritics in the title are there **on purpose**: the tags are UTF-8
        // and this is the only proof of that path. The fixture itself is in English
        // (D-036); the diacritics are not a leftover of another language but the
        // very thing being tested.
        assert_eq!(track.title, "Sine 440 ünïcode");
        assert_eq!(track.album.as_deref(), Some("Fixture Album"));
        // The duration must be read from the container — the distinguishing field
        // of fuzzy matching (K6).
        let duration = track.duration_ms.expect("the duration must be read");
        assert!(
            (900..=1100).contains(&duration),
            "a 1-second fixture, read: {duration}ms"
        );

        drop(index);
    }

    #[cfg(feature = "audio")]
    #[test]
    fn an_untagged_file_falls_back_to_its_name_and_is_counted() {
        let dir = temp_dir("untagged");
        copy_fixture(
            &dir,
            "Other Artist - Ogg Track.ogg",
            "Other Artist - Ogg Track.ogg",
        );

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        let summary = provider.rescan_now().expect("scan");
        assert_eq!(summary.indexed, 1);
        assert_eq!(
            summary.tag_fallback, 1,
            "an untagged file must count as a drop"
        );

        let index = provider.index.read().unwrap();
        assert_eq!(index[0].track.artist, "Other Artist");
        assert_eq!(index[0].track.title, "Ogg Track");

        drop(index);
    }

    #[test]
    fn filename_fallback_parses_artist_and_title() {
        let track = track_from_filename(Path::new("/music/Album/Radiohead - Creep.flac"));
        assert_eq!(track.artist, "Radiohead");
        assert_eq!(track.title, "Creep");
        assert_eq!(track.album.as_deref(), Some("Album"));
    }

    #[test]
    fn filename_fallback_uses_the_parent_dir_when_there_is_no_dash() {
        let track = track_from_filename(Path::new("/music/Portishead/Roads.mp3"));
        assert_eq!(
            track.artist, "Portishead",
            "the parent directory counts as the artist"
        );
        assert_eq!(track.title, "Roads");
    }

    #[test]
    fn filename_fallback_never_invents_an_empty_field() {
        // Broken names like " - Creep.flac" must not produce an empty artist.
        let track = track_from_filename(Path::new("/music/ - Creep.flac"));
        assert!(!track.artist.is_empty());
        assert!(!track.title.is_empty());
    }

    #[test]
    fn capabilities_exclude_control() {
        let provider = LocalProvider::new(vec![]);
        let caps = provider.info().capabilities;
        assert!(caps.contains(Capabilities::STREAM));
        assert!(caps.contains(Capabilities::SEARCH));
        assert!(
            !caps.contains(Capabilities::CONTROL),
            "the local disk is not remote-controlled"
        );
    }

    #[cfg(feature = "audio")]
    #[tokio::test]
    async fn search_matches_tags_and_filename_derived_fields() {
        let dir = temp_dir("search");
        copy_fixture(&dir, "tagged.flac", "tagged.flac");
        copy_fixture(
            &dir,
            "Other Artist - Ogg Track.ogg",
            "Other Artist - Ogg Track.ogg",
        );

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        provider.rescan_now().expect("scan");

        // The artist from the tags.
        let hits = provider.search("test artist", 10).await.expect("search");
        assert_eq!(hits.len(), 1, "{hits:?}");

        // The artist from the file name (the ogg has no tags).
        let other = provider.search("other", 10).await.expect("search");
        assert_eq!(other.len(), 1, "{other:?}");

        let none = provider.search("nonexistent", 10).await.expect("search");
        assert!(none.is_empty());
    }

    /// Search must find the file name fields whichever way the metadata came.
    /// This test is the same in both modes: the name is `Common Artist - Common
    /// Track`.
    #[tokio::test]
    async fn search_finds_filename_derived_tracks_in_both_modes() {
        let dir = temp_dir("search-common");
        copy_fixture(
            &dir,
            "Other Artist - Ogg Track.ogg",
            "Common Artist - Common Track.ogg",
        );

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        provider.rescan_now().expect("scan");

        let hits = provider.search("common artist", 10).await.expect("search");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(provider.search("nothing", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_respects_the_limit() {
        let dir = temp_dir("limit");
        for index in 0..4 {
            copy_fixture(
                &dir,
                "Other Artist - Ogg Track.ogg",
                &format!("Common Artist - Track {index}.ogg"),
            );
        }
        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        provider.rescan_now().expect("scan");

        let hits = provider.search("common", 2).await.expect("search");
        assert_eq!(hits.len(), 2, "the limit must not be exceeded");
    }

    #[tokio::test]
    async fn resolve_source_refuses_paths_outside_the_index() {
        let dir = temp_dir("source");
        copy_fixture(
            &dir,
            "Other Artist - Ogg Track.ogg",
            "Common Artist - Common Track.ogg",
        );

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        provider.rescan_now().expect("scan");

        // A file in the index can be played.
        let indexed = provider.search("common", 1).await.unwrap();
        let source = provider
            .resolve_source(&indexed[0].id)
            .await
            .expect("resolution");
        assert!(matches!(source, Some(AudioSource::LocalFile { .. })));

        // An arbitrary path must be rejected — this is not a file-read surface.
        let outside = ProviderTrackId::new(ProviderId::new("local"), "/etc/passwd");
        assert_eq!(provider.resolve_source(&outside).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_file_deleted_after_indexing_is_not_playable() {
        let dir = temp_dir("deleted");
        copy_fixture(
            &dir,
            "Other Artist - Ogg Track.ogg",
            "Common Artist - Common Track.ogg",
        );

        let provider = LocalProvider::new(vec![dir.to_path_buf()]);
        provider.rescan_now().expect("scan");
        let indexed = provider.search("common", 1).await.unwrap();

        std::fs::remove_file(dir.join("Common Artist - Common Track.ogg"))
            .expect("must be deleted");

        // A deleted file returns `None`. The root check rests on
        // `canonicalize`, and a path that does not exist cannot be resolved —
        // not accepting the suspicious is better than loosening the check "so
        // the error message is nicer". Session explains the situation to the
        // user: "no playable source found".
        assert_eq!(
            provider.resolve_source(&indexed[0].id).await.unwrap(),
            None,
            "a deleted file must not look playable"
        );
    }

    #[tokio::test]
    async fn resolve_source_rejects_traversal_out_of_the_roots() {
        // Paths like `<root>/../../etc/passwd` start under the root and leave it;
        // `canonicalize` resolves that.
        let dir = temp_dir("escape");
        copy_fixture(&dir, "tagged.flac", "song.flac");
        let provider = LocalProvider::new(vec![dir.to_path_buf()]);

        let escaping = dir.join("..").join("..").join("etc").join("passwd");
        let id = ProviderTrackId::new(
            ProviderId::new("local"),
            escaping.to_string_lossy().into_owned(),
        );
        assert_eq!(
            provider.resolve_source(&id).await.unwrap(),
            None,
            "a path leaving the root must be rejected"
        );

        // A real file under the root, on the other hand, must stay playable.
        let ok_id = ProviderTrackId::new(
            ProviderId::new("local"),
            dir.join("song.flac").to_string_lossy().into_owned(),
        );
        assert!(
            provider.resolve_source(&ok_id).await.unwrap().is_some(),
            "a file under the root must be playable"
        );
    }

    #[tokio::test]
    async fn health_reports_a_missing_root() {
        let provider = LocalProvider::new(vec![PathBuf::from("/nonexistent/dir")]);
        let health = provider.health().await.expect("health");
        assert!(!health.reachable);
        assert!(
            health
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("unreachable"),
            "{health:?}"
        );
    }
}
