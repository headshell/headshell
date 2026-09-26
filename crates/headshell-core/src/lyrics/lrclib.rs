//! LRCLIB (D-078): a free, open, keyless database of synced lyrics
//! (`lrclib.net`, MIT, a Rust server).
//!
//! What it does was read in its source before this was written, not
//! guessed:
//!
//! - `/api/get` matches the artist and title exactly (lower-cased), the
//!   album exactly when given, and the length within **±2 s** when given —
//!   without a length it does not look at length at all. Among several
//!   matches it returns the lowest id, **not** the synced one: it may hand
//!   back unsynced words while a synced version of the same length exists.
//! - `/api/search` returns up to twenty versions with their lengths.
//! - Neither takes an ISRC or an MBID.
//! - A busy server answers `503 ServerOverloaded`; two of four quick
//!   requests got it (measured, D-078).
//!
//! So: the versions are searched, and **the choice is made here**
//! ([`choose`]) — a synced one within [`LENGTH_WINDOW_MS`] of the recording,
//! nearest first; `/api/get` is asked only when the search had no such one
//! (it matches names exactly where the search is fuzzy); a busy answer is
//! asked again twice, and then said (K9). One request a second, as for
//! MusicBrainz.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::Deserialize;

use super::{LENGTH_WINDOW_MS, Lyrics, LyricsFit, lrc};
use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::identity::musicbrainz::default_user_agent;
use crate::identity::normalize;
use crate::model::TrackRef;
use crate::net::{self, HttpClient, HttpHeader, HttpRequest, HttpResponse, RateLimiter};

/// The public server.
pub(crate) const DEFAULT_URL: &str = "https://lrclib.net";

/// One request a second, for every LRCLIB client of the process: the worker
/// and a `lyrics` command share the server's patience.
fn shared_limiter() -> Arc<RateLimiter> {
    static LIMITER: OnceLock<Arc<RateLimiter>> = OnceLock::new();
    Arc::clone(LIMITER.get_or_init(|| Arc::new(RateLimiter::new(Duration::from_secs(1)))))
}

/// The waits before asking a busy server again.
const RETRY_WAITS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

/// One version at LRCLIB.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Record {
    pub(crate) id: i64,
    #[serde(default)]
    pub(crate) track_name: Option<String>,
    #[serde(default)]
    pub(crate) artist_name: Option<String>,
    #[serde(default)]
    pub(crate) album_name: Option<String>,
    /// In seconds, with a fraction.
    #[serde(default)]
    pub(crate) duration: Option<f64>,
    #[serde(default)]
    pub(crate) instrumental: bool,
    #[serde(default)]
    pub(crate) plain_lyrics: Option<String>,
    #[serde(default)]
    pub(crate) synced_lyrics: Option<String>,
}

impl Record {
    fn length_ms(&self) -> Option<i64> {
        let seconds = self.duration.filter(|s| s.is_finite() && *s > 0.0)?;
        // At most an hour (LRCLIB's own limit): the cast cannot overflow.
        Some((seconds.min(86_400.0) * 1_000.0).round() as i64)
    }

    fn has_synced(&self) -> bool {
        !self.instrumental
            && self
                .synced_lyrics
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
    }

    fn has_plain(&self) -> bool {
        !self.instrumental
            && self
                .plain_lyrics
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
    }
}

/// What LRCLIB had for a recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Answer {
    Found {
        lyrics: Lyrics,
        fit: LyricsFit,
        delta_ms: Option<i64>,
        /// LRCLIB's id of the version taken — for a person reading the cache.
        record: i64,
    },
    /// The version of this length is marked as having no words.
    Instrumental { record: i64 },
    /// No version with words. `detail` says what was looked at.
    None { detail: String },
}

/// Which version to take, and how it fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Choice {
    Synced {
        index: usize,
        fit: LyricsFit,
        delta_ms: Option<i64>,
    },
    Instrumental {
        index: usize,
    },
    Plain {
        index: usize,
        fit: LyricsFit,
        delta_ms: Option<i64>,
    },
    None,
}

/// Picks a version for the recording. The rule of D-078, without a network:
///
/// 1. Only versions whose artist and title match the track's, normalised as
///    the identity chain does — `(Remastered)` is the same song; `(Live)` is
///    not. The search is fuzzy; a namesake must not lend its words.
/// 2. With the recording's length: a synced version within the window,
///    nearest first; else one marked instrumental within the window; else
///    the nearest synced one anywhere ([`LyricsFit::Closest`]); else unsynced
///    words, within the window first.
/// 3. Without a length: the first synced version, unmeasured; else the rest
///    in the same order.
///
/// Ties go to the version on the track's album, then to the lowest id: the
/// same answer every time.
pub(crate) fn choose(records: &[Record], track: &TrackRef, duration_ms: Option<u64>) -> Choice {
    let title = normalize::normalize_text(&track.title);
    let artist = normalize::normalize_artist(&track.artist);
    let album = track
        .album
        .as_deref()
        .map(normalize::normalize_text)
        .filter(|album| !album.is_empty());
    let same_song = |record: &Record| {
        let their_title = normalize::normalize_text(record.track_name.as_deref().unwrap_or(""));
        let their_artist = normalize::normalize_artist(record.artist_name.as_deref().unwrap_or(""));
        !title.is_empty() && their_title == title && artist_matches(&their_artist, &artist)
    };
    // On the track's album: its name inside theirs, word for word —
    // `Dummy` is on `Dummy m4a` and `Dummy (Remastered)`.
    let on_album = |record: &Record| {
        album.as_deref().is_some_and(|album| {
            contains_words(
                &normalize::normalize_text(record.album_name.as_deref().unwrap_or("")),
                album,
            )
        })
    };
    let off_album = |record: &Record| album.is_some() && !on_album(record);
    let named: Vec<(usize, &Record)> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| same_song(record))
        .collect();
    // The album guard (as for covers, D-076): a live take can run the album
    // version's length to the second — measured: Portishead's "Sour Times"
    // from a 2013 festival is 251 s, the album's own 254 s — and its lines
    // would be timed for a different performance. With a synced version on
    // the track's album, only the album's versions are weighed; without
    // one, every version is.
    let album_has_synced = named
        .iter()
        .any(|(_, record)| on_album(record) && record.has_synced());
    let candidates: Vec<(usize, &Record)> = named
        .into_iter()
        .filter(|(_, record)| !album_has_synced || on_album(record))
        .collect();

    let Some(duration) = duration_ms.and_then(|ms| i64::try_from(ms).ok()) else {
        let first = |wanted: fn(&Record) -> bool| {
            candidates
                .iter()
                .filter(|(_, record)| wanted(record))
                .min_by_key(|(_, record)| (off_album(record), record.id))
                .map(|(index, _)| *index)
        };
        return if let Some(index) = first(Record::has_synced) {
            Choice::Synced {
                index,
                fit: LyricsFit::Unmeasured,
                delta_ms: None,
            }
        } else if let Some(index) = first(|record| record.instrumental) {
            Choice::Instrumental { index }
        } else if let Some(index) = first(Record::has_plain) {
            Choice::Plain {
                index,
                fit: LyricsFit::Unmeasured,
                delta_ms: None,
            }
        } else {
            Choice::None
        };
    };

    let window = i64::try_from(LENGTH_WINDOW_MS).unwrap_or(i64::MAX);
    // (index, delta) of the nearest version that `wanted`, within the window
    // or anywhere.
    let nearest = |wanted: fn(&Record) -> bool, within: bool| {
        candidates
            .iter()
            .filter(|(_, record)| wanted(record))
            .filter_map(|(index, record)| {
                let delta = record.length_ms()? - duration;
                (!within || delta.abs() <= window).then_some((*index, delta, *record))
            })
            .min_by_key(|(_, delta, record)| (delta.abs(), off_album(record), record.id))
            .map(|(index, delta, _)| (index, delta))
    };
    let fit_of = |delta: i64| {
        if delta.abs() <= window {
            LyricsFit::Length
        } else {
            LyricsFit::Closest
        }
    };

    if let Some((index, delta)) = nearest(Record::has_synced, true) {
        return Choice::Synced {
            index,
            fit: LyricsFit::Length,
            delta_ms: Some(delta),
        };
    }
    if let Some((index, _)) = nearest(|record| record.instrumental, true) {
        return Choice::Instrumental { index };
    }
    if let Some((index, delta)) = nearest(Record::has_synced, false) {
        return Choice::Synced {
            index,
            fit: LyricsFit::Closest,
            delta_ms: Some(delta),
        };
    }
    if let Some((index, delta)) =
        nearest(Record::has_plain, true).or_else(|| nearest(Record::has_plain, false))
    {
        return Choice::Plain {
            index,
            fit: fit_of(delta),
            delta_ms: Some(delta),
        };
    }
    Choice::None
}

/// The same artist, or one credit inside the other — `Ezhel` and
/// `Ezhel & Ufo361` — word for word, never inside a word.
fn artist_matches(theirs: &str, ours: &str) -> bool {
    if theirs.is_empty() || ours.is_empty() {
        return false;
    }
    theirs == ours || contains_words(theirs, ours) || contains_words(ours, theirs)
}

/// Is `short` inside `long`, word for word?
fn contains_words(long: &str, short: &str) -> bool {
    let long: Vec<&str> = long.split_whitespace().collect();
    let short: Vec<&str> = short.split_whitespace().collect();
    !short.is_empty()
        && long
            .windows(short.len())
            .any(|window| window == short.as_slice())
}

/// The LRCLIB client. It goes through [`HttpClient`] like every other
/// network call (D-020): tests hand it a fake.
pub(crate) struct Lrclib {
    http: Arc<dyn HttpClient>,
    base_url: String,
    user_agent: String,
    limiter: Arc<RateLimiter>,
    retry_waits: Vec<Duration>,
}

impl std::fmt::Debug for Lrclib {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lrclib")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl Lrclib {
    /// The public server, over `http`.
    pub(crate) fn new(http: Arc<dyn HttpClient>) -> Self {
        Self {
            http,
            base_url: DEFAULT_URL.to_owned(),
            user_agent: default_user_agent(),
            limiter: shared_limiter(),
            retry_waits: RETRY_WAITS.to_vec(),
        }
    }

    /// This build's client.
    ///
    /// # Errors
    /// If the build has no HTTP client — said, not turned into "none" (K9).
    pub(crate) fn from_build() -> Result<Self> {
        Ok(Self::new(net::default_http_client()?))
    }

    /// Another server, no waits — for tests.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn for_tests(http: Arc<dyn HttpClient>, base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            limiter: Arc::new(RateLimiter::new(Duration::ZERO)),
            retry_waits: vec![Duration::ZERO; RETRY_WAITS.len()],
            ..Self::new(http)
        }
    }

    /// Looks the recording up: the search, then — without a synced version
    /// of its length — the exact query.
    ///
    /// # Errors
    /// If LRCLIB cannot be reached, stays busy, or answers with something
    /// that is not its JSON.
    pub(crate) async fn find(&self, track: &TrackRef, duration_ms: Option<u64>) -> Result<Answer> {
        let mut records = self.search(track).await?;
        let mut choice = choose(&records, track, duration_ms);
        let exact = matches!(
            choice,
            Choice::Synced {
                fit: LyricsFit::Length,
                ..
            } | Choice::Instrumental { .. }
        );
        if !exact && let Some(duration_ms) = duration_ms {
            match self.get(track, duration_ms).await {
                Ok(Some(record)) if !records.iter().any(|known| known.id == record.id) => {
                    records.push(record);
                    choice = choose(&records, track, Some(duration_ms));
                }
                Ok(_) => {}
                // With nothing from the search, the failure is the answer:
                // "could not ask" is not "none" (K9). With something, the
                // search's choice stands and the failure is logged.
                Err(err) if choice == Choice::None => return Err(err),
                Err(err) => tracing::warn!(
                    error = %err.chain_text().replace('\n', " "),
                    "LRCLIB: the exact query failed; the search's choice stands"
                ),
            }
        }
        Ok(answer(&records, choice, track))
    }

    async fn search(&self, track: &TrackRef) -> Result<Vec<Record>> {
        let url = format!(
            "{}/api/search?track_name={}&artist_name={}",
            self.base_url,
            net::encode_query(&track.title),
            net::encode_query(&track.artist)
        );
        let response = self.send(&url).await?;
        parse(&response, &url)
    }

    /// `/api/get` with the length: `None` when it has no version (`404`).
    async fn get(&self, track: &TrackRef, duration_ms: u64) -> Result<Option<Record>> {
        let mut url = format!(
            "{}/api/get?track_name={}&artist_name={}&duration={:.3}",
            self.base_url,
            net::encode_query(&track.title),
            net::encode_query(&track.artist),
            // Seconds, as LRCLIB takes them; `u64` ms is exact in `f64`
            // for any length a song has.
            duration_ms as f64 / 1_000.0
        );
        if let Some(album) = track.album.as_deref().filter(|a| !a.trim().is_empty()) {
            url.push_str(&format!("&album_name={}", net::encode_query(album)));
        }
        let response = self.send(&url).await?;
        if response.status == 404 {
            return Ok(None);
        }
        parse(&response, &url).map(Some)
    }

    /// Sends a GET: one at a time a second, and a busy answer asked again
    /// after each of [`RETRY_WAITS`].
    async fn send(&self, url: &str) -> Result<HttpResponse> {
        let request = HttpRequest::get(url).with_headers(vec![
            HttpHeader::new("User-Agent", self.user_agent.clone()),
            // LRCLIB reads this one first, to tell clients apart.
            HttpHeader::new("Lrclib-Client", self.user_agent.clone()),
        ]);
        let mut waits = self.retry_waits.iter();
        loop {
            self.limiter.acquire();
            let response = self.http.send(&request).await?;
            let busy = matches!(response.status, 429 | 503);
            if busy && let Some(wait) = waits.next() {
                tracing::debug!(
                    status = response.status,
                    ?wait,
                    "LRCLIB is busy; asking again"
                );
                std::thread::sleep(*wait);
                continue;
            }
            if response.status == 404 || response.is_success() {
                return Ok(response);
            }
            let detail = net::clip(&response.text_lossy(), 200);
            return Err(Error::new(
                Stage::LyricsLookup,
                ErrorKind::HttpStatus {
                    url: url.to_owned(),
                    status: response.status,
                    detail: if busy {
                        format!(
                            "LRCLIB stayed busy — asked {} times: {detail}",
                            self.retry_waits.len() + 1
                        )
                    } else {
                        detail
                    },
                },
            ));
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(response: &HttpResponse, url: &str) -> Result<T> {
    serde_json::from_slice(&response.body).map_err(|source| {
        Error::new(
            Stage::LyricsLookup,
            ErrorKind::Json {
                entry: format!("LRCLIB {}", net::without_query(url)),
                source,
            },
        )
    })
}

/// The choice made into lyrics.
fn answer(records: &[Record], choice: Choice, track: &TrackRef) -> Answer {
    let versions = records
        .iter()
        .filter(|record| {
            normalize::normalize_text(record.track_name.as_deref().unwrap_or(""))
                == normalize::normalize_text(&track.title)
        })
        .count();
    match choice {
        Choice::Synced {
            index,
            fit,
            delta_ms,
        } => {
            let record = &records[index];
            let lyrics = lrc::parse(record.synced_lyrics.as_deref().unwrap_or(""));
            Answer::Found {
                lyrics,
                fit,
                delta_ms,
                record: record.id,
            }
        }
        Choice::Plain {
            index,
            fit,
            delta_ms,
        } => {
            let record = &records[index];
            let mut lyrics = lrc::parse(record.plain_lyrics.as_deref().unwrap_or(""));
            // Plain words are plain, even if a line looks like a stamp.
            lyrics.synced = false;
            for line in &mut lyrics.lines {
                line.start_ms = None;
            }
            Answer::Found {
                lyrics,
                fit,
                delta_ms,
                record: record.id,
            }
        }
        Choice::Instrumental { index } => Answer::Instrumental {
            record: records[index].id,
        },
        Choice::None => Answer::None {
            detail: if versions == 0 {
                format!(
                    "LRCLIB has no version of \"{}\" by {}",
                    track.title, track.artist
                )
            } else {
                format!(
                    "LRCLIB has {versions} version(s) titled \"{}\", none by {} with words",
                    track.title, track.artist
                )
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::fake::FakeHttp;

    fn record(id: i64, artist: &str, title: &str, seconds: f64, synced: bool) -> Record {
        Record {
            id,
            track_name: Some(title.to_owned()),
            artist_name: Some(artist.to_owned()),
            album_name: None,
            duration: Some(seconds),
            instrumental: false,
            plain_lyrics: Some("plain words".to_owned()),
            synced_lyrics: synced.then(|| "[00:01.00]synced words".to_owned()),
        }
    }

    fn song() -> TrackRef {
        TrackRef::new("Şebnem Ferah", "Sil Baştan")
    }

    #[test]
    fn the_synced_version_nearest_the_recordings_length_is_taken() {
        // The spread of lengths measured for a real song (D-078).
        let records = vec![
            record(1, "Sebnem Ferah", "Sil Bastan", 198.0, true),
            record(2, "Şebnem Ferah", "Sil Baştan", 305.0, true),
            record(3, "Şebnem Ferah", "Sil Baştan", 313.9, true),
            record(4, "Şebnem Ferah", "Sil Baştan", 312.4, true),
        ];
        assert_eq!(
            choose(&records, &song(), Some(312_000)),
            Choice::Synced {
                index: 3,
                fit: LyricsFit::Length,
                delta_ms: Some(400)
            }
        );
    }

    /// `/api/get`'s trap: the lowest id within the window may be unsynced. A
    /// synced version within the window wins however far down it is.
    #[test]
    fn an_unsynced_version_of_the_right_length_does_not_hide_a_synced_one() {
        let records = vec![
            record(1, "Şebnem Ferah", "Sil Baştan", 312.0, false),
            record(9, "Şebnem Ferah", "Sil Baştan", 313.5, true),
        ];
        assert!(matches!(
            choose(&records, &song(), Some(312_000)),
            Choice::Synced {
                index: 1,
                fit: LyricsFit::Length,
                ..
            }
        ));
    }

    #[test]
    fn a_namesake_or_a_live_take_does_not_lend_its_words() {
        let records = vec![
            record(1, "Another Band", "Sil Baştan", 312.0, true),
            record(2, "Şebnem Ferah", "Sil Baştan (Live)", 312.0, true),
        ];
        assert_eq!(choose(&records, &song(), Some(312_000)), Choice::None);
        // A reissue suffix is the same song.
        let records = vec![record(
            3,
            "Şebnem Ferah",
            "Sil Baştan (Remastered 2011)",
            312.0,
            true,
        )];
        assert!(matches!(
            choose(&records, &song(), Some(312_000)),
            Choice::Synced { index: 0, .. }
        ));
    }

    #[test]
    fn without_a_version_of_its_length_the_nearest_synced_one_is_taken_and_marked() {
        let records = vec![
            record(1, "Şebnem Ferah", "Sil Baştan", 257.0, true),
            record(2, "Şebnem Ferah", "Sil Baştan", 305.0, true),
            record(3, "Şebnem Ferah", "Sil Baştan", 312.0, false),
        ];
        assert_eq!(
            choose(&records, &song(), Some(312_000)),
            Choice::Synced {
                index: 1,
                fit: LyricsFit::Closest,
                delta_ms: Some(-7_000)
            },
            "the nearest synced version beats the unsynced one of the right length (the user's choice)"
        );
    }

    #[test]
    fn an_instrumental_version_of_the_length_is_the_answer() {
        let mut quiet = record(5, "Şebnem Ferah", "Sil Baştan", 312.5, false);
        quiet.instrumental = true;
        quiet.plain_lyrics = None;
        let records = vec![record(1, "Şebnem Ferah", "Sil Baştan", 250.0, true), quiet];
        assert_eq!(
            choose(&records, &song(), Some(312_000)),
            Choice::Instrumental { index: 1 }
        );
    }

    #[test]
    fn the_album_breaks_a_tie_and_a_length_of_zero_is_no_length() {
        let mut on_album = record(7, "Şebnem Ferah", "Sil Baştan", 312.0, true);
        on_album.album_name = Some("Can Kırıkları".to_owned());
        let mut zero = record(1, "Şebnem Ferah", "Sil Baştan", 0.0, true);
        zero.album_name = Some("Can Kırıkları".to_owned());
        let records = vec![
            record(2, "Şebnem Ferah", "Sil Baştan", 312.0, true),
            zero,
            on_album,
        ];
        let track = song().with_album(Some("Can Kırıkları".to_owned()));
        assert!(matches!(
            choose(&records, &track, Some(312_000)),
            Choice::Synced { index: 2, .. }
        ));
    }

    /// Real data (LRCLIB, 2026-09-26): the album version of "Sour Times" is
    /// 254 s there, and two festival takes sit within two seconds of the
    /// album's 251 s. The festival's lines are timed for another
    /// performance: the album's version is taken, marked as the nearest.
    /// Without the guard the 251 s festival take wins by length.
    #[test]
    fn a_live_take_of_the_albums_length_does_not_lend_its_timing() {
        let on = |id, album: &str, seconds| {
            let mut version = record(id, "Portishead", "Sour Times", seconds, true);
            version.album_name = Some(album.to_owned());
            version
        };
        let records = vec![
            on(
                36_877_417,
                "2013-06-22 Southside Festival, Neuhausen, Germany",
                251.0,
            ),
            on(36_877_405, "2011-10-01 ATP Festival, NY, USA", 253.0),
            on(36_934_080, "1001 Songs To Hear Before You Die", 254.0),
            on(34_425_096, "Dummy m4a", 254.0),
        ];
        let track = TrackRef::new("Portishead", "Sour Times").with_album(Some("Dummy".to_owned()));
        assert_eq!(
            choose(&records, &track, Some(251_000)),
            Choice::Synced {
                index: 3,
                fit: LyricsFit::Closest,
                delta_ms: Some(3_000)
            }
        );
        // A track on a compilation, the song's own album absent from LRCLIB:
        // every version is weighed, and the one of its length is taken.
        let compiled = TrackRef::new("Portishead", "Sour Times")
            .with_album(Some("Trip-Hop Classics".to_owned()));
        assert!(matches!(
            choose(&records[2..], &compiled, Some(254_500)),
            Choice::Synced {
                fit: LyricsFit::Length,
                ..
            }
        ));
    }

    #[test]
    fn without_the_recordings_length_nothing_is_measured() {
        let records = vec![
            record(3, "Şebnem Ferah", "Sil Baştan", 312.0, false),
            record(4, "Şebnem Ferah", "Sil Baştan", 250.0, true),
        ];
        assert_eq!(
            choose(&records, &song(), None),
            Choice::Synced {
                index: 1,
                fit: LyricsFit::Unmeasured,
                delta_ms: None
            }
        );
    }

    #[test]
    fn a_credit_inside_another_is_the_same_artist_but_not_inside_a_word() {
        assert!(artist_matches("ezhel ufo361", "ezhel"));
        assert!(artist_matches("ezhel", "ezhel ufo361"));
        assert!(!artist_matches("ezhelx", "ezhel"));
        assert!(!artist_matches("", "ezhel"));
    }

    const SEARCH: &str = r#"[
        {"id":11,"trackName":"Sil Baştan","artistName":"Şebnem Ferah","albumName":"Can Kırıkları",
         "duration":305.0,"instrumental":false,"plainLyrics":"a\nb","syncedLyrics":"[00:01.00]a\n[00:02.00]b"},
        {"id":12,"trackName":"Sil Baştan","artistName":"Şebnem Ferah","albumName":"Can Kırıkları",
         "duration":312.4,"instrumental":false,"plainLyrics":"c","syncedLyrics":null}
    ]"#;

    #[tokio::test]
    async fn the_search_is_enough_when_it_has_a_synced_version_of_the_length() {
        let http = Arc::new(FakeHttp::new().route("/api/search?", SEARCH));
        let lrclib = Lrclib::for_tests(http.clone(), "http://lrclib.test/");
        let answer = lrclib.find(&song(), Some(304_000)).await.unwrap();
        let Answer::Found {
            lyrics,
            fit,
            delta_ms,
            record,
        } = answer
        else {
            panic!("{answer:?}");
        };
        assert_eq!(
            (record, fit, delta_ms),
            (11, LyricsFit::Length, Some(1_000))
        );
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines.len(), 2);
        let asked = http.requests();
        assert_eq!(asked.len(), 1, "no second request");
        assert_eq!(
            asked[0].url,
            "http://lrclib.test/api/search?track_name=Sil%20Ba%C5%9Ftan&artist_name=%C5%9Eebnem%20Ferah"
        );
        let header = |name: &str| {
            asked[0]
                .headers
                .iter()
                .find(|h| h.name == name)
                .map(|h| h.value.clone())
                .unwrap_or_default()
        };
        assert!(header("User-Agent").starts_with("headshell/"));
        assert_eq!(header("Lrclib-Client"), header("User-Agent"));
    }

    #[tokio::test]
    async fn without_one_the_exact_query_is_asked_with_the_length_and_the_album() {
        let exact = r#"{"id":99,"trackName":"Sil Baştan","artistName":"Şebnem Ferah","albumName":"Can Kırıkları",
            "duration":312.0,"instrumental":false,"plainLyrics":"x","syncedLyrics":"[00:03.00]x"}"#;
        let http = Arc::new(
            FakeHttp::new()
                .route("/api/search?", SEARCH)
                .route("/api/get?", exact),
        );
        let lrclib = Lrclib::for_tests(http.clone(), "http://lrclib.test");
        let track = song().with_album(Some("Can Kırıkları".to_owned()));
        let answer = lrclib.find(&track, Some(312_250)).await.unwrap();
        assert!(
            matches!(
                answer,
                Answer::Found {
                    record: 99,
                    fit: LyricsFit::Length,
                    ..
                }
            ),
            "{answer:?}"
        );
        let get = &http.requests()[1].url;
        assert!(get.contains("duration=312.250"), "{get}");
        assert!(
            get.contains("album_name=Can%20K%C4%B1r%C4%B1klar%C4%B1"),
            "{get}"
        );
    }

    #[tokio::test]
    async fn nothing_found_says_what_was_looked_at() {
        let http = Arc::new(FakeHttp::new().route("/api/search?", "[]").route_status(
            "/api/get?",
            404,
            r#"{"code":404,"name":"TrackNotFound"}"#,
        ));
        let answer = Lrclib::for_tests(http, "http://lrclib.test")
            .find(&song(), Some(312_000))
            .await
            .unwrap();
        let Answer::None { detail } = answer else {
            panic!("{answer:?}");
        };
        assert!(detail.contains("no version of \"Sil Baştan\""), "{detail}");
    }

    #[tokio::test]
    async fn a_busy_server_is_asked_again_and_then_said_to_be_busy() {
        let http = Arc::new(FakeHttp::new().route_status(
            "/api/search?",
            503,
            r#"{"message":"The server is busy, please retry in a moment","name":"ServerOverloaded","statusCode":503}"#,
        ));
        let err = Lrclib::for_tests(http.clone(), "http://lrclib.test")
            .find(&song(), Some(312_000))
            .await
            .unwrap_err();
        assert_eq!(http.requests().len(), 3, "asked three times");
        let text = err.chain_text();
        assert!(text.contains("LYRICS_LOOKUP"), "{text}");
        assert!(text.contains("stayed busy"), "{text}");
        assert!(text.contains("503"), "{text}");
    }
}
