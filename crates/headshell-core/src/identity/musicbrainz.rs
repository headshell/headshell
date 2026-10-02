//! The MusicBrainz metadata source — it feeds the 2nd and 3rd links of the K6
//! chain.
//!
//! Until this file, `MetadataLookup` had no real implementation:
//! [`OfflineLookup`](super::OfflineLookup) always said "not found" and the
//! chain dropped every record without an ISRC to `LocalKey`. So the two middle
//! links of the ISRC → **MBID** → **fuzzy** order did no measurable work
//! (D-045).
//!
//! ## The limits live here, because MusicBrainz has rules
//!
//! - **`User-Agent` is mandatory.** Requests that do not identify the
//!   application get a `403`. We produce a default value, but it can be
//!   changed with [`MusicBrainzLookup::with_user_agent`]; whoever distributes
//!   the app should put their own contact address there.
//! - **One request per second.** That is the average rate for anonymous
//!   clients; exceed it and a `503` comes back. The limiter is in
//!   [`crate::net::RateLimiter`] and **sleeps between calls**; the reasoning
//!   is written there. There is **one** limiter for the public server per
//!   process: the identity chain and the cover lookup (D-076) each hold a
//!   client, and two limiters would let two requests a second through.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::Deserialize;

use super::{Candidate, LengthRange, LookupFuture, MetadataLookup, SearchPage};
use crate::error::Result;
use crate::ids::{Isrc, Mbid};
use crate::net::{HttpClient, HttpHeader, HttpRequest, RateLimiter, encode_query, parse_json};

/// The public MusicBrainz server.
pub const DEFAULT_BASE_URL: &str = "https://musicbrainz.org/ws/2";

/// The shortest time between two requests.
///
/// MusicBrainz's anonymous limit is one request per second. 1100 ms leaves
/// room for clock skew and network jitter — a client that grazes the limit
/// eats a `503` and starts retrying, which is slower overall.
const MIN_INTERVAL: Duration = Duration::from_millis(1100);

/// How many releases a release search asks for (D-076).
///
/// A recording search's page is its caller's to size (D-081); the cover
/// chain's release search is a single page, and this is its size.
const SEARCH_LIMIT: usize = 25;

/// MusicBrainz's largest page: a larger `limit` is answered with 100.
const MAX_PAGE: usize = 100;

/// The lowest score of a search's top tier (D-081).
///
/// MusicBrainz scores relative to the best hit of the whole search (a tail
/// page of `Creep` runs 82 → 32, it does not restart at 100), but rounds
/// with each index replica's own statistics: the same exact `Creep`s scored
/// all 100 on one replica, 34 × 100 and 66 × 99 on another. The first
/// inexact hits scored 94, 93, 92 and 86 (`One More Time, Aerodynamic`,
/// `Roads (live)`, `Şımarık (remix)`, `Archangel (TASO REMIX)`; measured
/// 2026-10-02). 97 sits between the two with room for a point of rounding
/// either way.
const TOP_TIER: u32 = 97;

/// How many times to retry after a `503` (rate limit).
///
/// Counted: endless retries would make a client that exceeds its quota
/// silent — the same reasoning as plugin restarts being counted (§2.1).
const RATE_LIMIT_RETRIES: u32 = 2;

/// A metadata source that connects to MusicBrainz.
///
/// It goes through [`HttpClient`], not straight to HTTP (D-020): tests supply
/// a fake client, mobile supplies its own stack.
pub struct MusicBrainzLookup {
    http: Arc<dyn HttpClient>,
    base_url: String,
    user_agent: String,
    limiter: Arc<RateLimiter>,
    search_limit: usize,
}

/// The limiter every client of the public server shares.
fn shared_limiter() -> Arc<RateLimiter> {
    static LIMITER: OnceLock<Arc<RateLimiter>> = OnceLock::new();
    Arc::clone(LIMITER.get_or_init(|| Arc::new(RateLimiter::new(MIN_INTERVAL))))
}

impl std::fmt::Debug for MusicBrainzLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MusicBrainzLookup")
            .field("base_url", &self.base_url)
            .field("user_agent", &self.user_agent)
            .field("search_limit", &self.search_limit)
            .finish_non_exhaustive()
    }
}

impl MusicBrainzLookup {
    /// A source connecting to the public server.
    #[must_use]
    pub fn new(http: Arc<dyn HttpClient>) -> Self {
        Self {
            http,
            base_url: DEFAULT_BASE_URL.to_owned(),
            user_agent: default_user_agent(),
            limiter: shared_limiter(),
            search_limit: SEARCH_LIMIT,
        }
    }

    /// Another server (your own copy of MusicBrainz, or a test server).
    ///
    /// A trailing `/` is dropped, so no double slash appears when building
    /// `{base}/recording`.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let url = base_url.into();
        self.base_url = url.trim_end_matches('/').to_owned();
        self
    }

    /// Your own `User-Agent`.
    ///
    /// MusicBrainz wants to see the application and **a reachable address**;
    /// format: `application/version ( contact )`. A distribution that does not
    /// set this goes with the default and shares the risk of being throttled.
    #[must_use]
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// The shortest time between requests — on a limiter of this client's
    /// own, no longer the shared one.
    ///
    /// Only for those connecting to their own copy, and for tests; going below
    /// the default on the public server breaks the quota.
    #[must_use]
    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.limiter = Arc::new(RateLimiter::new(interval));
        self
    }

    /// How many releases a release search asks for. A recording search's
    /// page size is its caller's (D-081).
    #[must_use]
    pub fn with_search_limit(mut self, limit: usize) -> Self {
        self.search_limit = limit.clamp(1, 100);
        self
    }

    fn headers(&self) -> Vec<HttpHeader> {
        vec![
            HttpHeader::new("User-Agent", self.user_agent.clone()),
            HttpHeader::new("Accept", "application/json"),
        ]
    }

    /// Makes a GET, respecting the rate limit; `404` is **not an error** but
    /// `None`.
    ///
    /// The distinction between "not found" and "could not talk" starts here
    /// (K9): the first means moving on to the next link of the chain, the second
    /// means stopping and reporting.
    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        what: &str,
    ) -> Result<Option<T>> {
        let request = HttpRequest::get(url).with_headers(self.headers());
        let mut attempt = 0;
        loop {
            self.limiter.acquire();
            let response = self.http.send(&request).await?;

            if response.status == 404 {
                return Ok(None);
            }
            // 503 = quota exceeded. The server says "slow down", not "no".
            if response.status == 503 && attempt < RATE_LIMIT_RETRIES {
                attempt += 1;
                tracing::warn!(url, attempt, "MusicBrainz rate limit: waiting and retrying");
                std::thread::sleep(MIN_INTERVAL);
                continue;
            }
            response.error_for_status(url)?;
            return parse_json::<T>(&response, what).map(Some);
        }
    }

    /// The releases a recording appears on (D-076), with their release
    /// groups. `Ok(None)`: MusicBrainz does not know the recording.
    ///
    /// The identity chain's queries do not ask for releases (`inc` has
    /// `artist-credits` and `isrcs`); the cover lookup needs them, so it asks
    /// separately — one request per album, since covers are kept per album.
    ///
    /// # Errors
    /// If the request fails or the answer does not parse.
    pub(crate) async fn releases_of(&self, recording: &Mbid) -> Result<Option<Vec<Release>>> {
        let url = format!(
            "{}/recording/{}?inc=releases+release-groups&fmt=json",
            self.base_url,
            encode_query(recording.as_str())
        );
        let Some(payload) = self
            .get_json::<RecordingReleases>(&url, "musicbrainz recording releases")
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(collect_releases(payload.releases, recording.as_str())))
    }

    /// The releases titled like `album` by `artist` (D-076): the cover
    /// chain's way in when the recording it matched is not on the album —
    /// a well-known song has dozens of namesake recordings on compilations
    /// and live records, and the one the identity chain picks can be any.
    ///
    /// # Errors
    /// If the request fails or the answer does not parse.
    pub(crate) async fn search_releases(&self, artist: &str, album: &str) -> Result<Vec<Release>> {
        let query = build_release_query(artist, album);
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!(
            "{}/release?query={}&fmt=json&limit={}",
            self.base_url,
            encode_query(&query),
            self.search_limit
        );
        let Some(payload) = self
            .get_json::<ReleaseSearch>(&url, "musicbrainz release search")
            .await?
        else {
            return Ok(Vec::new());
        };
        Ok(collect_releases(payload.releases, &query))
    }
}

/// Turns raw releases into [`Release`]s; the ones with a broken MBID are
/// counted and reported, not dropped silently.
fn collect_releases(raw: Vec<MbRelease>, asked: &str) -> Vec<Release> {
    let mut releases = Vec::with_capacity(raw.len());
    let mut skipped = 0usize;
    for release in raw {
        let Some(id) = Mbid::parse(&release.id) else {
            skipped += 1;
            continue;
        };
        let (group, primary_type, secondary_types) = match release.release_group {
            Some(group) => (
                Mbid::parse(&group.id),
                group.primary_type,
                group.secondary_types,
            ),
            None => (None, None, Vec::new()),
        };
        releases.push(Release {
            id,
            title: release.title,
            official: release.status.as_deref() == Some("Official"),
            date: release.date.filter(|date| !date.is_empty()),
            group,
            primary_type,
            secondary_types,
        });
    }
    if skipped > 0 {
        tracing::warn!(
            asked,
            skipped,
            "skipped releases with an invalid MBID in the MusicBrainz response"
        );
    }
    releases
}

/// A release a recording appears on (D-076).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) id: Mbid,
    pub(crate) title: String,
    /// `status: Official` — not a bootleg or a promo.
    pub(crate) official: bool,
    /// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`: sorts as text.
    pub(crate) date: Option<String>,
    pub(crate) group: Option<Mbid>,
    /// The release group's `primary-type`: `Album`, `Single`, `EP`, …
    pub(crate) primary_type: Option<String>,
    /// Its `secondary-types`: `Compilation`, `Live`, `Soundtrack`, … — empty
    /// for an artist's own studio release.
    pub(crate) secondary_types: Vec<String>,
}

/// This build's default `User-Agent`: the application, its version and
/// where to reach the project. The Cover Art Archive gets the same one
/// (D-076). A distribution with its own contact address sets it with
/// [`MusicBrainzLookup::with_user_agent`].
pub(crate) fn default_user_agent() -> String {
    format!(
        "headshell/{} ( https://github.com/headshell/headshell )",
        env!("CARGO_PKG_VERSION")
    )
}

impl MetadataLookup for MusicBrainzLookup {
    fn recording_by_isrc<'a>(&'a self, isrc: &'a Isrc) -> LookupFuture<'a, Option<Candidate>> {
        Box::pin(async move {
            // `inc=artist-credits`: the recording title alone is not enough to make a
            // candidate; fuzzy scoring wants the artist too.
            let url = format!(
                "{}/isrc/{}?fmt=json&inc=artist-credits+isrcs",
                self.base_url,
                encode_query(isrc.as_str())
            );
            let Some(payload) = self
                .get_json::<IsrcResponse>(&url, "musicbrainz isrc")
                .await?
            else {
                return Ok(None);
            };

            let mut candidates = collect_candidates(payload.recordings, "isrc");
            if candidates.len() > 1 {
                // An ISRC can be tied to more than one recording (the same track's
                // recordings on different releases). We take the first, but the count
                // goes on record: choosing silently would leave "why this MBID?"
                // unanswered later.
                tracing::debug!(
                    isrc = isrc.as_str(),
                    count = candidates.len(),
                    "the ISRC is tied to more than one recording; took the first"
                );
            }
            Ok((!candidates.is_empty()).then(|| candidates.swap_remove(0)))
        })
    }

    fn search_recordings<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
        length: Option<LengthRange>,
        limit: usize,
    ) -> LookupFuture<'a, SearchPage> {
        Box::pin(async move {
            let query = build_query(artist, title);
            if query.is_empty() {
                // An empty query means `400` at MusicBrainz. We say "no candidates"
                // without sending a request; the chain goes down to the fuzzy link
                // with an empty list.
                return Ok(SearchPage::default());
            }
            let query = match length {
                Some(range) => format!("{query} AND {}", length_clause(range)),
                None => query,
            };
            let limit = limit.clamp(1, MAX_PAGE);
            let url = format!(
                "{}/recording?query={}&fmt=json&limit={limit}",
                self.base_url,
                encode_query(&query),
            );
            let Some(payload) = self
                .get_json::<SearchResponse>(&url, "musicbrainz recording search")
                .await?
            else {
                return Ok(SearchPage::default());
            };
            let top_continues = top_continues(&payload, limit);
            Ok(SearchPage {
                candidates: collect_candidates(payload.recordings, "search"),
                top_continues,
            })
        })
    }
}

/// Does the search's top tier go on past the hits returned (D-081)?
///
/// It does when the search has more hits and the last one returned is still
/// in the [`TOP_TIER`]. A hit without a score, or a result without a count
/// that filled its page, is taken as going on: "I don't know" must not read
/// as "it ended" — that would claim a whole tier nobody saw.
fn top_continues(payload: &SearchResponse, limit: usize) -> bool {
    let Some(last) = payload.recordings.last() else {
        return false;
    };
    let returned = payload.recordings.len();
    let more = payload
        .count
        .map_or(returned >= limit, |count| returned < count);
    more && last.score.is_none_or(|score| score >= TOP_TIER)
}

/// The Lucene clause for a length range: MusicBrainz indexes a recording's
/// length in milliseconds as `dur`; one of unknown length has none and
/// falls outside every range.
fn length_clause(range: LengthRange) -> String {
    format!("dur:[{} TO {}]", range.from_ms, range.to_ms)
}

/// Turns raw recordings into candidates; what cannot be turned **is counted
/// and reported**.
///
/// Dropping them silently with `filter_map` would be exactly what the "no
/// silent `unwrap_or_default()`" rule forbids: a recording arriving with a
/// broken MBID lowers the accuracy rate for no reason and nobody notices.
fn collect_candidates(recordings: Vec<MbRecording>, source: &str) -> Vec<Candidate> {
    let mut out = Vec::with_capacity(recordings.len());
    let mut skipped = 0usize;
    for recording in recordings {
        match Mbid::parse(&recording.id) {
            Some(mbid) => out.push(Candidate {
                mbid,
                artist: recording.artist_name(),
                title: recording.title,
                duration_ms: recording.length,
                isrc: recording.isrcs.iter().find_map(|raw| Isrc::parse(raw)),
                disambiguation: recording.disambiguation.filter(|note| !note.is_empty()),
            }),
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            source,
            skipped,
            "skipped recordings with an invalid MBID in the MusicBrainz response"
        );
    }
    out
}

/// Builds a Lucene query: `artist:"..." AND recording:"..."`.
///
/// If one of the fields is empty that field is dropped; if both are empty the
/// query comes back empty and the caller sends no request.
fn build_query(artist: &str, title: &str) -> String {
    let mut parts = Vec::new();
    let artist = escape_lucene(artist);
    let title = escape_lucene(title);
    if !artist.is_empty() {
        parts.push(format!("artist:\"{artist}\""));
    }
    if !title.is_empty() {
        parts.push(format!("recording:\"{title}\""));
    }
    parts.join(" AND ")
}

/// Builds a release search: `release:"..." AND artist:"..."`. Without an
/// album there is nothing to search for: the query comes back empty.
fn build_release_query(artist: &str, album: &str) -> String {
    let album = escape_lucene(album);
    if album.is_empty() {
        return String::new();
    }
    let artist = escape_lucene(artist);
    if artist.is_empty() {
        format!("release:\"{album}\"")
    } else {
        format!("release:\"{album}\" AND artist:\"{artist}\"")
    }
}

/// Escapes Lucene's special characters.
///
/// Without escaping, names like `AC/DC` or `Where Is My Mind?` break the
/// query and MusicBrainz returns `400` — so resolution collapses exactly
/// where it is most needed, on odd names.
fn escape_lucene(value: &str) -> String {
    const SPECIAL: &[char] = &[
        '+', '-', '&', '|', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '\\',
        '/',
    ];
    let mut out = String::with_capacity(value.len());
    for ch in value.trim().chars() {
        if SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

// --- Wire format (MusicBrainz ws/2, `fmt=json`) ---------------------------

#[derive(Debug, Deserialize)]
struct IsrcResponse {
    #[serde(default)]
    recordings: Vec<MbRecording>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    /// How many hits the whole search has, past every page.
    #[serde(default)]
    count: Option<usize>,
    #[serde(default)]
    recordings: Vec<MbRecording>,
}

#[derive(Debug, Deserialize)]
struct MbRecording {
    id: String,
    #[serde(default)]
    title: String,
    /// Milliseconds. If MusicBrainz does not know the duration it comes as
    /// `null` — that is information, not zero: [`super::fuzzy`] behaves
    /// differently when it does not know the duration.
    #[serde(default)]
    length: Option<u64>,
    #[serde(rename = "artist-credit", default)]
    artist_credit: Vec<MbArtistCredit>,
    /// The note that sets a recording apart from its namesakes: `"live,
    /// 1994-05-27: Astoria, London, UK"`.
    ///
    /// This field is the **only** marker of live recordings — the title stays
    /// plainly `Creep`. An empty string arrives too (`""`), so it is weeded out
    /// with `filter`: "no note" and "an empty note" are the same thing, but
    /// `Some("")` would push an empty context into scoring.
    #[serde(default)]
    disambiguation: Option<String>,
    #[serde(default)]
    isrcs: Vec<String>,
    /// A search hit's score, 0–100, relative to the best hit of the whole
    /// search ([`TOP_TIER`]). Absent outside searches.
    #[serde(default)]
    score: Option<u32>,
}

impl MbRecording {
    /// Turns the `artist-credit` list into a single string.
    ///
    /// MusicBrainz gives multiple artists piece by piece and carries the joining
    /// phrase (`joinphrase`) separately: `[{name:"Jay-Z", joinphrase:" & "},
    /// {name:"Kanye West"}]` → `Jay-Z & Kanye West`. Taking only the first would
    /// lose half of every duet.
    fn artist_name(&self) -> String {
        let mut out = String::new();
        for credit in &self.artist_credit {
            out.push_str(&credit.name);
            out.push_str(&credit.joinphrase);
        }
        out.trim().to_owned()
    }
}

#[derive(Debug, Deserialize)]
struct RecordingReleases {
    #[serde(default)]
    releases: Vec<MbRelease>,
}

#[derive(Debug, Deserialize)]
struct ReleaseSearch {
    #[serde(default)]
    releases: Vec<MbRelease>,
}

#[derive(Debug, Deserialize)]
struct MbRelease {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(rename = "release-group", default)]
    release_group: Option<MbReleaseGroup>,
}

#[derive(Debug, Deserialize)]
struct MbReleaseGroup {
    id: String,
    #[serde(rename = "primary-type", default)]
    primary_type: Option<String>,
    #[serde(rename = "secondary-types", default)]
    secondary_types: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MbArtistCredit {
    #[serde(default)]
    name: String,
    #[serde(default)]
    joinphrase: String,
}

/// This build's default metadata source.
///
/// # Errors
/// If the `http-client` feature is off: the caller must supply its own
/// client and call [`MusicBrainzLookup::new`].
#[cfg(feature = "http-client")]
pub fn default_musicbrainz_lookup() -> Result<Arc<dyn MetadataLookup>> {
    Ok(Arc::new(MusicBrainzLookup::new(
        crate::net::default_http_client()?,
    )))
}

/// This build's default metadata source.
///
/// # Errors
/// In this build `http-client` is off, so it **always** returns an error. We
/// do not silently fall back to the offline source: the user should know why
/// the chain ended at `LocalKey` (K9).
#[cfg(not(feature = "http-client"))]
pub fn default_musicbrainz_lookup() -> Result<Arc<dyn MetadataLookup>> {
    Err(crate::error::Error::new(
        crate::diag::Stage::IdentityResolve,
        crate::error::ErrorKind::Unsupported {
            provider: "musicbrainz".to_owned(),
            what: "metadata lookup (a build with the `http-client` feature off)".to_owned(),
            capabilities: "NONE".to_owned(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::fake::FakeHttp;

    /// In tests the rate limit is 0: the limiter itself is not what is tested.
    fn lookup(http: FakeHttp) -> MusicBrainzLookup {
        MusicBrainzLookup::new(Arc::new(http))
            .with_base_url("http://mb.test/ws/2")
            .with_min_interval(Duration::ZERO)
    }

    const CREEP_SEARCH: &str = r#"{
        "count": 1,
        "recordings": [{
            "id": "b1a9c0e9-d987-4042-ae91-78d6a3267d69",
            "title": "Creep",
            "length": 238000,
            "artist-credit": [{ "name": "Radiohead", "joinphrase": "" }],
            "isrcs": ["GBAYE9200001"]
        }]
    }"#;

    #[tokio::test]
    async fn a_search_becomes_a_scored_candidate() {
        let http = FakeHttp::new().route("/recording?query=", CREEP_SEARCH);
        let mb = lookup(http);
        let page = mb
            .search_recordings("Radiohead", "Creep", None, 25)
            .await
            .unwrap();
        assert!(!page.top_continues, "a count of 1 has nothing after it");
        let found = page.candidates;

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].artist, "Radiohead");
        assert_eq!(found[0].title, "Creep");
        assert_eq!(found[0].duration_ms, Some(238_000));
        assert_eq!(
            found[0].isrc.as_ref().map(Isrc::as_str),
            Some("GBAYE9200001")
        );
    }

    #[tokio::test]
    async fn an_isrc_lookup_returns_the_first_recording() {
        let body = r#"{
            "isrc": "GBAYE9200001",
            "recordings": [
                { "id": "b1a9c0e9-d987-4042-ae91-78d6a3267d69", "title": "Creep",
                  "length": 238000, "artist-credit": [{ "name": "Radiohead" }] },
                { "id": "63f1a2c3-1111-4042-ae91-78d6a3267d01", "title": "Creep (live)",
                  "length": 251000, "artist-credit": [{ "name": "Radiohead" }] }
            ]
        }"#;
        let http = FakeHttp::new().route("/isrc/", body);
        let mb = lookup(http);
        let isrc = Isrc::parse("GBAYE9200001").unwrap();
        let found = mb.recording_by_isrc(&isrc).await.unwrap().unwrap();
        assert_eq!(found.mbid.as_str(), "b1a9c0e9-d987-4042-ae91-78d6a3267d69");
    }

    /// K9: "not found" is not an error — the chain must move on to the next link.
    #[tokio::test]
    async fn an_unknown_isrc_is_absence_not_failure() {
        let http = FakeHttp::new().route_status("/isrc/", 404, r#"{"error":"Not Found"}"#);
        let mb = lookup(http);
        let isrc = Isrc::parse("GBAYE9200001").unwrap();
        assert_eq!(mb.recording_by_isrc(&isrc).await.unwrap(), None);
    }

    /// ...but "could not talk" is an error, and it says its stage.
    #[tokio::test]
    async fn a_server_error_is_reported_not_swallowed() {
        let http = FakeHttp::new().route_status("/recording?query=", 500, "broken");
        let mb = lookup(http);
        let err = mb
            .search_recordings("Radiohead", "Creep", None, 25)
            .await
            .unwrap_err();
        let text = err.chain_text();
        assert!(text.starts_with("STEP: NETWORK_REQUEST"), "{text}");
        assert!(text.contains("500"), "{text}");
    }

    #[tokio::test]
    async fn the_request_carries_a_user_agent_musicbrainz_accepts() {
        let http = Arc::new(FakeHttp::new().route("/recording?query=", CREEP_SEARCH));
        let mb = MusicBrainzLookup::new(http.clone())
            .with_base_url("http://mb.test/ws/2")
            .with_min_interval(Duration::ZERO);
        mb.search_recordings("Radiohead", "Creep", None, 25)
            .await
            .unwrap();

        let sent = http.requests();
        let agent = sent[0]
            .headers
            .iter()
            .find(|h| h.name == "User-Agent")
            .expect("a User-Agent must be sent: MusicBrainz returns 403 otherwise");
        assert!(agent.value.starts_with("headshell/"), "{}", agent.value);
        assert!(
            agent.value.contains('('),
            "contact address: {}",
            agent.value
        );
    }

    /// A search answer of `count` hits whose first ones score `scores`.
    fn search_page(count: usize, scores: &[u32]) -> String {
        let recordings: Vec<String> = scores
            .iter()
            .enumerate()
            .map(|(index, score)| {
                format!(
                    r#"{{ "id": "00000000-0000-4000-8000-{index:012}", "title": "Creep",
                         "score": {score}, "length": 238000,
                         "artist-credit": [{{ "name": "Radiohead" }}] }}"#
                )
            })
            .collect();
        format!(
            r#"{{ "count": {count}, "offset": 0, "recordings": [{}] }}"#,
            recordings.join(",")
        )
    }

    /// `limit` hits of `scores` from a search of `count`: does the top go on?
    async fn continues(count: usize, scores: &[u32], limit: usize) -> bool {
        let body = search_page(count, scores);
        lookup(FakeHttp::new().route("/recording?query=", &body))
            .search_recordings("Radiohead", "Creep", None, limit)
            .await
            .unwrap()
            .top_continues
    }

    /// D-081: `Creep` has 149 exact hits, and the first 25 cut through them.
    /// The answer says so — the measured shape of the failure.
    #[tokio::test]
    async fn a_head_ending_inside_the_top_tier_says_the_top_continues() {
        let body = search_page(192, &[100; 25]);
        let http = Arc::new(FakeHttp::new().route("/recording?query=", &body));
        let mb = MusicBrainzLookup::new(http.clone())
            .with_base_url("http://mb.test/ws/2")
            .with_min_interval(Duration::ZERO);
        let page = mb
            .search_recordings("Radiohead", "Creep", None, 25)
            .await
            .unwrap();

        assert_eq!(page.candidates.len(), 25);
        assert!(page.top_continues);
        let url = &http.requests()[0].url;
        assert!(url.ends_with("&fmt=json&limit=25"), "{url}");
    }

    /// A replica rounds the same exact hits to 99: still the top tier.
    #[tokio::test]
    async fn a_tier_rounded_to_99_still_continues() {
        let mut scores = vec![100; 10];
        scores.extend([99; 15]);
        assert!(continues(192, &scores, 25).await);
    }

    /// The tier ended among the hits: what comes after is inexact.
    #[tokio::test]
    async fn a_head_where_the_top_tier_ends_says_the_top_does_not_continue() {
        let mut scores = vec![100; 20];
        scores.extend([94, 92, 87, 86, 80]);
        assert!(!continues(148, &scores, 25).await);
    }

    /// An answer holding every hit of the search has nothing after it.
    #[tokio::test]
    async fn an_answer_holding_every_hit_says_the_top_does_not_continue() {
        assert!(!continues(25, &[100; 25], 25).await);
    }

    /// "I don't know" is not "it ended": a hit without a score, or a result
    /// without a count that filled its page, is taken as going on.
    #[tokio::test]
    async fn an_unknown_score_or_count_is_taken_as_going_on() {
        let no_score = r#"{ "count": 192, "recordings": [
            { "id": "00000000-0000-4000-8000-000000000001", "title": "Creep",
              "artist-credit": [{ "name": "Radiohead" }] } ] }"#;
        let page = lookup(FakeHttp::new().route("/recording?query=", no_score))
            .search_recordings("Radiohead", "Creep", None, 1)
            .await
            .unwrap();
        assert!(page.top_continues, "a hit without a score");

        let no_count = search_page(0, &[100, 100]).replace(r#""count": 0, "#, "");
        let page = lookup(FakeHttp::new().route("/recording?query=", &no_count))
            .search_recordings("Radiohead", "Creep", None, 2)
            .await
            .unwrap();
        assert!(page.top_continues, "a full page without a count");
    }

    /// MusicBrainz answers a larger `limit` with 100; asking for more would
    /// only make the count look short.
    #[tokio::test]
    async fn a_page_is_never_asked_larger_than_musicbrainz_gives() {
        let http = Arc::new(FakeHttp::new().route("/recording?query=", CREEP_SEARCH));
        let mb = MusicBrainzLookup::new(http.clone())
            .with_base_url("http://mb.test/ws/2")
            .with_min_interval(Duration::ZERO);
        mb.search_recordings("Radiohead", "Creep", None, 500)
            .await
            .unwrap();
        let url = &http.requests()[0].url;
        assert!(url.ends_with("&limit=100"), "{url}");
    }

    /// A length range becomes MusicBrainz's `dur` clause, in milliseconds.
    #[tokio::test]
    async fn a_length_range_narrows_the_query_to_dur() {
        let http = Arc::new(FakeHttp::new().route("/recording?query=", CREEP_SEARCH));
        let mb = MusicBrainzLookup::new(http.clone())
            .with_base_url("http://mb.test/ws/2")
            .with_min_interval(Duration::ZERO);
        mb.search_recordings("Radiohead", "Creep", Some(LengthRange::around(238_000)), 25)
            .await
            .unwrap();
        let url = &http.requests()[0].url;
        assert!(
            url.contains(&encode_query(
                r#"recording:"Creep" AND dur:[223000 TO 253000]"#
            )),
            "{url}"
        );
    }

    /// Odd names must not break the query — unescaped, MusicBrainz returns
    /// `400`.
    #[test]
    fn lucene_special_characters_are_escaped() {
        let query = build_query("AC/DC", "Where Is My Mind?");
        assert!(query.contains(r#"artist:"AC\/DC""#), "{query}");
        assert!(
            query.contains(r#"recording:"Where Is My Mind\?""#),
            "{query}"
        );
    }

    #[test]
    fn an_empty_query_is_not_sent_at_all() {
        assert_eq!(build_query("", ""), "");
        assert_eq!(build_query("  ", "  "), "");
        assert_eq!(build_query("", "Creep"), r#"recording:"Creep""#);
    }

    #[test]
    fn multi_artist_credits_are_joined_with_their_phrases() {
        let recording: MbRecording = serde_json::from_str(
            r#"{ "id": "b1a9c0e9-d987-4042-ae91-78d6a3267d69", "title": "Otis",
                 "artist-credit": [
                   { "name": "JAY-Z", "joinphrase": " & " },
                   { "name": "Kanye West" }
                 ] }"#,
        )
        .unwrap();
        assert_eq!(recording.artist_name(), "JAY-Z & Kanye West");
    }

    /// A broken MBID is not dropped silently: it is counted and reported.
    #[test]
    fn invalid_mbids_are_counted_not_silently_dropped() {
        let recordings: Vec<MbRecording> = serde_json::from_str(
            r#"[
                { "id": "not-a-uuid", "title": "Broken" },
                { "id": "b1a9c0e9-d987-4042-ae91-78d6a3267d69", "title": "Creep" }
            ]"#,
        )
        .unwrap();
        let candidates = collect_candidates(recordings, "test");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].title, "Creep");
    }

    /// If the duration is unknown it must stay `None` — not zero (the scoring
    /// distinction).
    #[test]
    fn a_missing_length_stays_unknown_rather_than_zero() {
        let recordings: Vec<MbRecording> = serde_json::from_str(
            r#"[{ "id": "b1a9c0e9-d987-4042-ae91-78d6a3267d69", "title": "Creep" }]"#,
        )
        .unwrap();
        let candidates = collect_candidates(recordings, "test");
        assert_eq!(candidates[0].duration_ms, None);
    }

    /// A rate-limit `503` means "slow down", not "no": it is tried once more.
    #[tokio::test]
    async fn a_rate_limited_response_is_retried() {
        // The fake client returns the first matching route; we cannot define
        // two different routes and have them returned in turn, so what is tested
        // here is that retrying is *counted*: if the 503 persists, an error must
        // come back.
        let http = Arc::new(FakeHttp::new().route_status("/recording?query=", 503, "slow down"));
        let mb = MusicBrainzLookup::new(http.clone())
            .with_base_url("http://mb.test/ws/2")
            .with_min_interval(Duration::ZERO);
        let err = mb
            .search_recordings("Radiohead", "Creep", None, 25)
            .await
            .unwrap_err();

        assert!(err.chain_text().contains("503"), "{}", err.chain_text());
        assert_eq!(
            http.requests().len(),
            (RATE_LIMIT_RETRIES + 1) as usize,
            "first attempt + {RATE_LIMIT_RETRIES} retries"
        );
    }

    /// The identity chain and the cover lookup (D-076) each build a client;
    /// together they must stay under the public server's one request a
    /// second — one limiter, not two.
    #[test]
    fn every_client_of_the_public_server_shares_one_limiter() {
        let a = MusicBrainzLookup::new(Arc::new(FakeHttp::new()));
        let b = MusicBrainzLookup::new(Arc::new(FakeHttp::new()));
        assert!(Arc::ptr_eq(&a.limiter, &b.limiter));
        let own =
            MusicBrainzLookup::new(Arc::new(FakeHttp::new())).with_min_interval(Duration::ZERO);
        assert!(
            !Arc::ptr_eq(&a.limiter, &own.limiter),
            "a custom interval is a limiter of its own"
        );
    }

    #[tokio::test]
    async fn a_recordings_releases_are_read_with_their_groups() {
        let http = Arc::new(FakeHttp::new().route(
            "/recording/b1a9c0e9-d987-4042-ae91-78d6a3267d69?inc=releases+release-groups",
            r#"{"id":"b1a9c0e9-d987-4042-ae91-78d6a3267d69","releases":[
                {"id":"6c8c3d3a-5e2b-4b8e-8f39-f0c7f41d4a01","title":"Pablo Honey","status":"Official",
                 "date":"1993-02-22","release-group":{"id":"0b2e8f0e-3a2c-3d8c-9a5e-5e7a4b2d7f11",
                 "primary-type":"Album","secondary-types":[]}},
                {"id":"not-an-mbid","title":"Broken"},
                {"id":"77c2f5d4-1b1f-4b4a-9c3e-2a1e5d6f7a8b","title":"Creep (live)","status":"Bootleg","date":""}
            ]}"#,
        ));
        let mb = MusicBrainzLookup::new(http.clone())
            .with_base_url("http://mb.test/ws/2")
            .with_min_interval(Duration::ZERO);
        let recording = Mbid::parse("b1a9c0e9-d987-4042-ae91-78d6a3267d69").unwrap();
        let releases = mb.releases_of(&recording).await.unwrap().unwrap();
        assert_eq!(releases.len(), 2, "the broken MBID is skipped (and logged)");
        assert_eq!(releases[0].title, "Pablo Honey");
        assert!(releases[0].official);
        assert_eq!(releases[0].date.as_deref(), Some("1993-02-22"));
        assert!(releases[0].group.is_some());
        assert_eq!(releases[0].primary_type.as_deref(), Some("Album"));
        assert!(releases[0].secondary_types.is_empty());
        assert!(!releases[1].official);
        assert_eq!(releases[1].date, None, "an empty date is no date");
        let url = &http.requests()[0].url;
        assert!(url.contains("inc=releases+release-groups"), "{url}");

        let unknown = MusicBrainzLookup::new(Arc::new(FakeHttp::new().route_status(
            "/recording/",
            404,
            "",
        )))
        .with_base_url("http://mb.test/ws/2")
        .with_min_interval(Duration::ZERO);
        assert_eq!(
            unknown.releases_of(&recording).await.unwrap(),
            None,
            "a 404 is `None`"
        );
    }

    #[tokio::test]
    async fn releases_are_searched_by_album_and_artist() {
        let http = Arc::new(FakeHttp::new().route(
            "/release?query=",
            r#"{"releases":[
                {"id":"4b4b9c42-2c3e-4d0f-8d11-2d8a0f5c9a01","score":100,"title":"Dummy","status":"Official",
                 "date":"1994-08-22","release-group":{"id":"76df3287-6cda-33eb-8e9a-044b5e15ffdd",
                 "primary-type":"Album"}},
                {"id":"not-an-mbid","title":"Dummy"}
            ]}"#,
        ));
        let mb = MusicBrainzLookup::new(http.clone())
            .with_base_url("http://mb.test/ws/2")
            .with_min_interval(Duration::ZERO);
        let releases = mb.search_releases("Portishead", "Dummy").await.unwrap();
        assert_eq!(releases.len(), 1, "the broken MBID is skipped (and logged)");
        assert_eq!(releases[0].title, "Dummy");
        assert_eq!(releases[0].primary_type.as_deref(), Some("Album"));
        assert!(releases[0].secondary_types.is_empty(), "absent is none");
        let url = &http.requests()[0].url;
        assert!(url.contains("/release?query="), "{url}");
        assert!(url.contains("Dummy") && url.contains("Portishead"), "{url}");

        assert!(
            mb.search_releases("Portishead", "  ")
                .await
                .unwrap()
                .is_empty(),
            "no album, no request"
        );
        assert_eq!(http.requests().len(), 1);
        assert_eq!(
            build_release_query("AC/DC", "Back in Black"),
            r#"release:"Back in Black" AND artist:"AC\/DC""#
        );
    }
}
