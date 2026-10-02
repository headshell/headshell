//! Music videos (D-086): a track's video form, and how it is found.
//!
//! A video is not a second track. It is another way of **showing** the one in
//! the queue: the listen is the same listen, the queue the same queue. What
//! changes is who makes the sound — the core's audio engine, or the shell's
//! video player — and the player keeps one clock across both (the "surface",
//! `playback::Surface`).
//!
//! ## Why the sources are a closed set
//!
//! The core never plays a video; the shell does. Every kind here needs the
//! shell's support **and** a line in its content security policy (a YouTube
//! embed is a frame from one origin, a local file a media URL from another).
//! A kind the shells do not know would be a source nobody can show, so a new
//! kind is added here together with the shells' support — adding one does not
//! raise the plugin api (the "adding doesn't raise" rule); a plugin returning
//! a kind not listed here is a contract violation.
//!
//! ## Why a YouTube video is only an id
//!
//! YouTube's own player plays it (the IFrame embed): the stream is never
//! resolved, nothing is downloaded, the ads and the counting stay YouTube's.
//! That player has rules — it stays visible while it plays, nothing is laid
//! over it, it is never smaller than 200×200 — and they are the shell's to
//! keep (D-086). An id is all the shell needs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::ids::{ProviderId, ProviderTrackId};
use crate::model::TrackRef;
use crate::provider::ProviderTrack;

/// Where a video comes from. A closed set — see the module's documentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VideoSource {
    /// A video file on this machine. Only the local provider gives this; a
    /// plugin cannot (the same line `AudioSource::LocalFile` draws, K3).
    LocalFile { path: PathBuf },
    /// A YouTube video, shown in YouTube's own player.
    Youtube { video_id: String },
}

impl VideoSource {
    /// Checks what a provider handed over before a shell gets it: a YouTube
    /// id has a fixed shape, and anything else in it would end up in a URL.
    ///
    /// # Errors
    /// If the id is not eleven URL-safe base64 characters.
    pub fn check(&self) -> Result<()> {
        match self {
            Self::LocalFile { .. } => Ok(()),
            Self::Youtube { video_id } => {
                if is_youtube_id(video_id) {
                    Ok(())
                } else {
                    Err(Error::new(
                        Stage::VideoLookup,
                        ErrorKind::InvalidInput {
                            detail: format!(
                                "{video_id:?} is not a YouTube video id (11 characters of A-Z a-z 0-9 _ -)"
                            ),
                        },
                    ))
                }
            }
        }
    }
}

/// Eleven characters of URL-safe base64: the shape of every YouTube video id.
#[must_use]
pub fn is_youtube_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One video that may be the track's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoCandidate {
    /// The video provider's id for it.
    pub id: ProviderTrackId,
    /// What the provider says it is.
    pub track: TrackRef,
    /// How sure the match is, 0.0–1.0; `1.0` for the track's own video.
    pub score: f64,
    pub source: VideoSource,
}

/// What `video_for` found for a track: the videos to try, best first.
///
/// An empty `candidates` is an answer — "I looked, there is none" — with
/// the reason in the counts; "I could not look" (no video provider, the
/// provider failed) is an error instead (K9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoPlan {
    /// The track the video is for.
    pub track: TrackRef,
    /// The provider asked for the video.
    pub provider: ProviderId,
    /// The video is the track's own (the same file, the same plugin item):
    /// no search was made.
    pub own: bool,
    /// The videos to try, best first. The shell moves to the next when a
    /// player refuses one (an owner who keeps the video off other sites).
    pub candidates: Vec<VideoCandidate>,
    /// Search rows that were not this track (another song, another artist,
    /// a live take) — counted, not dropped silently.
    pub refused: usize,
    /// Rows that looked right but gave no video when asked.
    pub without_video: usize,
}

/// The video being shown now, as a shell reads it each round.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoNow {
    pub source: VideoSource,
    /// The provider the video came from.
    pub provider: ProviderId,
    /// The track has no sound of its own here: the video is all there is
    /// (a YouTube item, a file whose audio the core cannot decode). The
    /// shell keeps the player on screen in every view then.
    pub only: bool,
}

/// How many videos a plan carries at most: enough to step past a refusal or
/// two, few enough that a plugin is not asked for every row.
pub(crate) const MAX_CANDIDATES: usize = 3;

/// How many rows a video search asks for.
pub(crate) const SEARCH_ROWS: usize = 10;

/// The least a search row must score to be taken for the track.
///
/// The duration is left out of the score: a music video is often longer than
/// the song (an intro, an outro, a scene), and the score's duration penalty
/// would push the right video under a wrong one. The duration only breaks
/// ties.
const MATCH_MIN: f64 = 0.82;

/// Weighs search rows against the track: the ones that are it, best first,
/// and how many were not.
pub(crate) fn rank(
    track: &TrackRef,
    rows: Vec<ProviderTrack>,
) -> (Vec<(ProviderTrack, f64)>, usize) {
    let mut kept = Vec::new();
    let mut refused = 0;
    for row in rows {
        let score = crate::identity::fuzzy::similarity(
            &track.artist,
            &track.title,
            None,
            &row.track.artist,
            &row.track.title,
            None,
            None,
        );
        if score >= MATCH_MIN {
            kept.push((row, score));
        } else {
            refused += 1;
        }
    }
    let wanted = track.duration_ms;
    let gap = |row: &ProviderTrack| match (wanted, row.track.duration_ms) {
        (Some(a), Some(b)) if b > 0 => a.abs_diff(b),
        _ => u64::MAX,
    };
    // Best score first; among equals the closer duration. The search's own
    // order (relevance) is kept for the rest — the sort is stable.
    kept.sort_by(|(a, score_a), (b, score_b)| {
        score_b
            .partial_cmp(score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| gap(a).cmp(&gap(b)))
    });
    (kept, refused)
}

/// The text a video provider is searched with.
pub(crate) fn query_for(track: &TrackRef) -> String {
    format!("{} {}", track.artist, track.title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, artist: &str, title: &str, duration_ms: Option<u64>) -> ProviderTrack {
        ProviderTrack {
            id: ProviderTrackId::new(ProviderId::new("youtube"), id),
            track: TrackRef::new(artist, title).with_duration_ms(duration_ms),
        }
    }

    #[test]
    fn youtube_ids_have_one_shape() {
        assert!(is_youtube_id("CCHdMIEGaaM"));
        assert!(is_youtube_id("5NV6Rdv1a3I"));
        assert!(is_youtube_id("a_b-c_d-e_f"));
        assert!(!is_youtube_id("short"));
        assert!(!is_youtube_id("CCHdMIEGaaM?x"));
        assert!(!is_youtube_id("CCHdMIEG/aM"));
        assert!(!is_youtube_id("CCHdMIEGaa\u{e9}"));
        let bad = VideoSource::Youtube {
            video_id: "javascript:".to_owned(),
        };
        let err = bad.check().unwrap_err();
        assert_eq!(err.stage(), Stage::VideoLookup);
    }

    #[test]
    fn the_song_is_kept_and_the_rest_counted() {
        let track = TrackRef::new("Daft Punk", "Get Lucky").with_duration_ms(Some(248_000));
        let rows = vec![
            row(
                "VeauURD-qLk",
                "Music EDM 2022",
                "D.a.f.t P.u.n.k Greatest Hits",
                Some(4_275_000),
            ),
            row("CCHdMIEGaaM", "Daft Punk", "Get Lucky", Some(369_000)),
            row(
                "hoda-sLt6fY",
                "BBC",
                "Anna McLuckie performs 'Get Lucky' by Daft Punk",
                Some(133_000),
            ),
            row("5NV6Rdv1a3I", "Daft Punk", "Get Lucky", Some(249_000)),
        ];
        let (kept, refused) = rank(&track, rows);
        let ids: Vec<&str> = kept.iter().map(|(row, _)| row.id.id.as_str()).collect();
        // Two videos of the song; the one whose length is the song's first.
        assert_eq!(ids, ["5NV6Rdv1a3I", "CCHdMIEGaaM"]);
        assert_eq!(refused, 2);
    }

    /// Measured live (2026-10-02): the artist's other song scored 0.75 and
    /// was taken for "Let It Happen" under the first floor (0.72).
    #[test]
    fn another_song_by_the_artist_is_not_the_song() {
        let track = TrackRef::new("Tame Impala", "Let It Happen");
        let rows = vec![
            row("pFptt7Cargc", "Tame Impala", "Let It Happen", Some(467_000)),
            row(
                "2SUwOgmvzK4",
                "Tame Impala",
                "The Less I Know The Better",
                Some(216_000),
            ),
        ];
        let (kept, refused) = rank(&track, rows);
        let ids: Vec<&str> = kept.iter().map(|(row, _)| row.id.id.as_str()).collect();
        assert_eq!(ids, ["pFptt7Cargc"]);
        assert_eq!(refused, 1);
    }

    #[test]
    fn a_live_take_is_not_the_song() {
        let track = TrackRef::new("Radiohead", "Creep");
        let rows = vec![row("aaaaaaaaaaa", "Radiohead", "Creep (Live)", None)];
        let (kept, refused) = rank(&track, rows);
        assert!(kept.is_empty(), "{kept:?}");
        assert_eq!(refused, 1);
    }

    #[test]
    fn sources_travel_as_tagged_json() {
        let source = VideoSource::Youtube {
            video_id: "CCHdMIEGaaM".to_owned(),
        };
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(json, r#"{"kind":"youtube","video_id":"CCHdMIEGaaM"}"#);
        let back: VideoSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, source);
    }
}
