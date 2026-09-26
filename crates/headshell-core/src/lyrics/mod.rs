//! Lyrics (D-078): finding a track's words, and when each is sung — for the
//! exact recording that plays.
//!
//! ## Why "the exact recording" is the whole job
//!
//! Synced lyrics are timed against one release of a song. The same song as a
//! radio edit, a remaster with a longer silence before it, or a live take
//! runs seconds apart: its lines shown on another's clock are seconds late.
//! LRCLIB holds ten to twenty timed versions of a well-known song, of lengths
//! from 156 s to 235 s for one of them (measured, D-078). So a third party's
//! lyrics are taken **by the length of the recording that plays**, measured
//! by its decoder where it is known.
//!
//! ## The chain
//!
//! 1. **The file's own** — a `.lrc` next to it with its name, then its tags
//!    (ID3 `SYLT`/`USLT`, Vorbis `LYRICS`, MP4 `©lyr`). They win even
//!    unsynced: they were made for this file, and the user said so.
//! 2. **The server's or the plugin's own** ([`Provider::lyrics`]) — the same
//!    file on the server, the same video at the service. Synced, they win.
//! 3. **LRCLIB** — its versions of the song whose artist and title match; a
//!    synced one within [`LENGTH_WINDOW_MS`] of the recording's length, the
//!    nearest first. Without one, the nearest synced version further away —
//!    shown with how far it is, for the user to shift ([`LyricsFit::Closest`]).
//! 4. Unsynced words: the provider's, then LRCLIB's.
//!
//! **When LRCLIB is asked** is the user's rule: a local file's and a
//! server's track only once the user asks for lyrics — the lyrics view, or
//! `headshell lyrics`, which is the same request. A plugin's track the moment
//! it starts: it plays from the internet anyway. Where the track lives is
//! always asked; it is where the audio comes from.
//!
//! "Not asked yet" ([`LyricsStatus::NotRequested`]), "asked, there are none"
//! and "a link failed" are three answers (K9).
//!
//! ## What is kept
//!
//! `<data_dir>/lyrics/`: LRCLIB's answers per track — the network is asked
//! once — and the user's shifts. The file's own lyrics are read every time:
//! an edited `.lrc` shows at once. See [`cache`].
//!
//! ## Who calls it
//!
//! `headshell lyrics` ([`crate::session::Session::lyrics`]) walks the chain
//! for the tracks a query finds and waits. The playing session hands its
//! playing track to a background [`LyricsWorker`]: LRCLIB is rate-limited and
//! sometimes busy, and the core's thread must not wait for it.

pub(crate) mod cache;
mod chain;
mod local;
pub(crate) mod lrc;
pub(crate) mod lrclib;
mod worker;

use serde::{Deserialize, Serialize};

use crate::diag::DiagReport;
pub use crate::ids::LyricsKey;
use crate::ids::{ProviderId, ProviderTrackId};
#[cfg(doc)]
use crate::provider::Provider;

pub(crate) use chain::{Ask, Chain, Gathered, Links, decide};
pub(crate) use local::file_lyrics;
pub use worker::LyricsWorker;

/// How far a third party's version may be from the recording's length and
/// still be taken as its timing: LRCLIB's own window for a match, ±2 s
/// (read in its source, D-078).
pub const LENGTH_WINDOW_MS: u64 = 2_000;

/// One line of lyrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    /// When it is sung, in milliseconds from the track's start. `None` in
    /// unsynced lyrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
    /// The words. Empty in synced lyrics is a pause — an instrumental part.
    pub text: String,
}

/// A track's lyrics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    /// Does every line say when it is sung? The lines are in time order then.
    pub synced: bool,
    pub lines: Vec<LyricLine>,
    /// Who the words are credited to, when the source says (YouTube Music:
    /// "Source: LyricFind").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
}

impl Lyrics {
    /// Unsynced lyrics from lines of text.
    pub(crate) fn unsynced(lines: impl IntoIterator<Item = String>) -> Self {
        Self {
            synced: false,
            lines: lines
                .into_iter()
                .map(|text| LyricLine {
                    start_ms: None,
                    text,
                })
                .collect(),
            attribution: None,
        }
    }

    /// Are there no words at all? A synced pause alone counts as none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|line| line.text.trim().is_empty())
    }

    /// The line being sung at `position_ms`, shifted by `offset_ms`: the last
    /// one that has started. `None` before the first one, and always for
    /// unsynced lyrics.
    ///
    /// `offset_ms` follows the LRC convention: positive, the lines come
    /// sooner. The interface keeps a second copy of this in `lyrics.js` —
    /// it cannot ask the core every frame — and
    /// `fixtures/lyrics/line_at_cases.json` binds the two (D-078), as the
    /// anchor's cases bind the position (D-033).
    #[must_use]
    pub fn line_at(&self, position_ms: u64, offset_ms: i64) -> Option<usize> {
        if !self.synced {
            return None;
        }
        let position = i64::try_from(position_ms).unwrap_or(i64::MAX);
        let started = self.lines.partition_point(|line| {
            let start = line
                .start_ms
                .map_or(i64::MAX, |ms| i64::try_from(ms).unwrap_or(i64::MAX));
            start.saturating_sub(offset_ms) <= position
        });
        started.checked_sub(1)
    }
}

/// Where lyrics came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsSource {
    /// A `.lrc` file next to the audio file, with its name.
    Sidecar,
    /// The audio file's tags.
    Embedded,
    /// The server or plugin the track plays from.
    Provider,
    /// LRCLIB.
    Lrclib,
}

impl LyricsSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sidecar => "sidecar",
            Self::Embedded => "embedded",
            Self::Provider => "provider",
            Self::Lrclib => "lrclib",
        }
    }

    /// The file's own: made for this file, and they win even unsynced.
    #[must_use]
    pub const fn is_the_files(self) -> bool {
        matches!(self, Self::Sidecar | Self::Embedded)
    }
}

/// How the lyrics' timing is known to fit the recording that plays (D-078).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsFit {
    /// They come with the recording itself: its file, its server, its plugin.
    Own,
    /// A third party's version within [`LENGTH_WINDOW_MS`] of the
    /// recording's length.
    Length,
    /// A third party's nearest version, further than the window: its timing
    /// may be off by about the difference. The user can shift it.
    Closest,
    /// A third party's, but the recording's length is not known: nothing
    /// could be measured.
    Unmeasured,
}

/// What happened to one track's lyrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LyricsStatus {
    /// Found.
    Found {
        source: LyricsSource,
        synced: bool,
        fit: LyricsFit,
        /// The third party's version minus the recording's length, in ms:
        /// positive, the version is longer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delta_ms: Option<i64>,
    },
    /// The track has no words, the source says.
    Instrumental { source: LyricsSource },
    /// Every link allowed was asked; none had lyrics. `detail` says what was
    /// looked at.
    NotFound { detail: String },
    /// Where the track lives has none, and LRCLIB was **not asked**: nobody
    /// asked for lyrics yet (D-078). A different answer from "not found"
    /// (K9).
    NotRequested,
    /// A link failed and none after it had lyrics: the error chain.
    Failed { chain: String },
    /// Not reached yet — the background worker is on its way.
    Pending,
}

/// One track's line in a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricsItem {
    /// The position in play order.
    pub index: usize,
    /// The key its lyrics and the user's shift are kept under.
    pub key: LyricsKey,
    pub provider: ProviderId,
    pub artist: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// The recording's length the lyrics were measured against, in ms: the
    /// decoder's when it is known, the metadata's otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub status: LyricsStatus,
    /// The user's shift, in ms; positive, the lines come sooner (the LRC
    /// convention). Kept per track.
    #[serde(default)]
    pub offset_ms: i64,
    /// The lyrics themselves: `headshell lyrics --print`, and the playing
    /// track's in the interface. Left out of a report of many tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<Lyrics>,
    /// What went wrong on the way when the chain went on anyway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl LyricsItem {
    /// A track's line before any link was asked.
    pub(crate) fn pending(
        index: usize,
        id: &ProviderTrackId,
        track: &crate::model::TrackRef,
        duration_ms: Option<u64>,
    ) -> Self {
        Self {
            index,
            key: LyricsKey::of(id),
            provider: id.provider.clone(),
            artist: track.artist.clone(),
            title: track.title.clone(),
            album: track.album.clone(),
            duration_ms,
            status: LyricsStatus::Pending,
            offset_ms: 0,
            lyrics: None,
            notes: Vec::new(),
        }
    }
}

/// The counts (K9): how many tracks, and what happened to them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricsSummary {
    pub tracks: usize,
    pub sidecar: usize,
    pub embedded: usize,
    pub provider: usize,
    pub lrclib: usize,
    /// Of the found ones: how many are synced…
    pub synced: usize,
    /// …and how many of those only as the nearest version, not by length.
    pub closest: usize,
    pub instrumental: usize,
    pub not_found: usize,
    pub not_requested: usize,
    pub failed: usize,
    pub pending: usize,
}

impl LyricsSummary {
    /// Counts the items.
    #[must_use]
    pub fn of(items: &[LyricsItem]) -> Self {
        let mut summary = Self {
            tracks: items.len(),
            ..Self::default()
        };
        for item in items {
            match &item.status {
                LyricsStatus::Found {
                    source,
                    synced,
                    fit,
                    ..
                } => {
                    match source {
                        LyricsSource::Sidecar => summary.sidecar += 1,
                        LyricsSource::Embedded => summary.embedded += 1,
                        LyricsSource::Provider => summary.provider += 1,
                        LyricsSource::Lrclib => summary.lrclib += 1,
                    }
                    if *synced {
                        summary.synced += 1;
                        if *fit == LyricsFit::Closest {
                            summary.closest += 1;
                        }
                    }
                }
                LyricsStatus::Instrumental { .. } => summary.instrumental += 1,
                LyricsStatus::NotFound { .. } => summary.not_found += 1,
                LyricsStatus::NotRequested => summary.not_requested += 1,
                LyricsStatus::Failed { .. } => summary.failed += 1,
                LyricsStatus::Pending => summary.pending += 1,
            }
        }
        summary
    }

    /// How many have lyrics.
    #[must_use]
    pub const fn found(&self) -> usize {
        self.sidecar + self.embedded + self.provider + self.lrclib
    }
}

/// The lyrics of a list of tracks: what `headshell lyrics` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricsReport {
    /// What was looked at: the query or the file.
    pub subject: String,
    pub items: Vec<LyricsItem>,
    pub summary: LyricsSummary,
    /// The run's diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diag: Option<DiagReport>,
}

/// Text from a file or a tag: UTF-8, with or without its byte order mark, or
/// UTF-16 with one. `None` for anything else — guessing a legacy code page
/// would turn `ş` into `þ` without a word (K9); the caller says why.
pub(crate) fn decode_text(bytes: &[u8]) -> Option<String> {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8(rest.to_vec()).ok(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        _ => String::from_utf8(bytes.to_vec()).ok(),
    }
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| unit([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synced(starts: &[u64]) -> Lyrics {
        Lyrics {
            synced: true,
            lines: starts
                .iter()
                .map(|&ms| LyricLine {
                    start_ms: Some(ms),
                    text: format!("at {ms}"),
                })
                .collect(),
            attribution: None,
        }
    }

    #[test]
    fn the_line_sung_is_the_last_one_that_started() {
        let lyrics = synced(&[1_000, 5_000, 9_000]);
        assert_eq!(lyrics.line_at(0, 0), None, "before the first line");
        assert_eq!(lyrics.line_at(999, 0), None);
        assert_eq!(
            lyrics.line_at(1_000, 0),
            Some(0),
            "a line starts on its stamp"
        );
        assert_eq!(lyrics.line_at(4_999, 0), Some(0));
        assert_eq!(lyrics.line_at(9_000, 0), Some(2));
        assert_eq!(lyrics.line_at(u64::MAX, 0), Some(2), "long after the last");
    }

    #[test]
    fn a_positive_shift_brings_the_lines_sooner() {
        let lyrics = synced(&[1_000, 5_000]);
        assert_eq!(lyrics.line_at(4_500, 500), Some(1));
        assert_eq!(lyrics.line_at(4_500, -500), Some(0));
        // A line shifted to before the track's start is sung from its start.
        assert_eq!(lyrics.line_at(0, 2_000), Some(0));
        assert_eq!(lyrics.line_at(0, i64::MIN), None, "no overflow");
        assert_eq!(lyrics.line_at(0, i64::MAX), Some(1), "no overflow");
    }

    #[test]
    fn unsynced_lyrics_have_no_line_being_sung() {
        let lyrics = Lyrics::unsynced(["one".to_owned(), "two".to_owned()]);
        assert_eq!(lyrics.line_at(10_000, 0), None);
        assert!(!lyrics.is_empty());
        assert!(Lyrics::unsynced([" ".to_owned()]).is_empty());
    }

    #[test]
    fn text_is_utf8_or_utf16_with_its_mark_and_nothing_is_guessed() {
        assert_eq!(
            decode_text("Sil Baştan".as_bytes()).as_deref(),
            Some("Sil Baştan")
        );
        assert_eq!(
            decode_text(&[0xEF, 0xBB, 0xBF, b'a']).as_deref(),
            Some("a"),
            "the byte order mark is not a character"
        );
        let mut le = vec![0xFF, 0xFE];
        le.extend("ş".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode_text(&le).as_deref(), Some("ş"));
        let mut be = vec![0xFE, 0xFF];
        be.extend("ş".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(decode_text(&be).as_deref(), Some("ş"));
        // Windows-1254 `ş` is 0xFE: not UTF-8, and not guessed.
        assert_eq!(decode_text(&[b'b', 0xFE, b'k']), None);
    }

    #[test]
    fn a_status_is_flat_in_json() {
        let json = serde_json::to_value(LyricsStatus::Found {
            source: LyricsSource::Lrclib,
            synced: true,
            fit: LyricsFit::Closest,
            delta_ms: Some(-7_000),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "status": "found", "source": "lrclib", "synced": true,
                "fit": "closest", "delta_ms": -7000
            })
        );
        let json = serde_json::to_value(LyricsStatus::NotRequested).unwrap();
        assert_eq!(json, serde_json::json!({"status": "not_requested"}));
    }

    #[test]
    fn the_summary_counts_every_status_once() {
        let id = ProviderTrackId::new(ProviderId::new("local"), "/a.flac");
        let item = |status| LyricsItem {
            status,
            ..LyricsItem::pending(0, &id, &crate::model::TrackRef::new("A", "B"), None)
        };
        let found = |source, synced, fit| LyricsStatus::Found {
            source,
            synced,
            fit,
            delta_ms: None,
        };
        let items = vec![
            item(found(LyricsSource::Sidecar, true, LyricsFit::Own)),
            item(found(LyricsSource::Lrclib, true, LyricsFit::Closest)),
            item(found(LyricsSource::Provider, false, LyricsFit::Own)),
            item(LyricsStatus::Instrumental {
                source: LyricsSource::Lrclib,
            }),
            item(LyricsStatus::NotFound {
                detail: "x".to_owned(),
            }),
            item(LyricsStatus::NotRequested),
            item(LyricsStatus::Failed {
                chain: "STEP: X".to_owned(),
            }),
            item(LyricsStatus::Pending),
        ];
        let summary = LyricsSummary::of(&items);
        assert_eq!(summary.tracks, 8);
        assert_eq!(summary.found(), 3);
        assert_eq!((summary.synced, summary.closest), (2, 1));
        assert_eq!(
            (
                summary.instrumental,
                summary.not_found,
                summary.not_requested,
                summary.failed,
                summary.pending
            ),
            (1, 1, 1, 1, 1)
        );
    }
}
