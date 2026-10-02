//! Canonical identity resolution.
//!
//! **Invariant #5 — the chain is not broken:**
//! ISRC → MusicBrainz ID → fuzzy match (artist+title+duration) → AcoustID
//! fingerprint. Every step returns a confidence score and the step that
//! resolved a record stays in the record; saying "resolved" is not enough,
//! *how* it was resolved must be measurable.

/// AcoustID lookup — only in `fingerprint` builds.
///
/// The module itself has to compress the fingerprint into the form AcoustID
/// expects, and that compressor lives in `rusty-chromaprint`. With the
/// feature off the module **does not exist**; the code that calls the
/// chain's 4th link still compiles because it goes through
/// [`Resolver::resolve_file`], and [`fingerprint::fingerprint_file`] says why
/// the link is missing (K9).
#[cfg(feature = "fingerprint")]
pub mod acoustid;
pub mod fingerprint;
pub mod fuzzy;
pub mod musicbrainz;
pub mod normalize;

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::ids::{CanonicalId, Isrc, Mbid};
use crate::model::TrackRef;

/// Which link of the chain resolved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveMethod {
    /// The ISRC led straight to a recording.
    Isrc,
    /// The metadata search gave an exact (normalised) hit.
    Mbid,
    /// The fuzzy match passed the threshold.
    Fuzzy,
    /// Audio fingerprint (AcoustID) — Phase 2.
    Fingerprint,
    /// The chain came up empty; the identity was derived from a local key.
    ///
    /// Good enough for grouping, worthless as an authority. Once the network is
    /// available, these records should be resolved again.
    LocalKey,
}

impl ResolveMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Isrc => "isrc",
            Self::Mbid => "mbid",
            Self::Fuzzy => "fuzzy",
            Self::Fingerprint => "fingerprint",
            Self::LocalKey => "local_key",
        }
    }
}

impl std::fmt::Display for ResolveMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The resolution result for a single track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub canonical_id: CanonicalId,
    pub method: ResolveMethod,
    /// 0.0–1.0. Low for `LocalKey`: the identity is consistent but has no
    /// authority.
    pub confidence: f64,
    /// Which recording the match went to (if any).
    pub matched: Option<Candidate>,
    /// **How many candidates shared** the top score. `1` = a single winner.
    ///
    /// In a real catalog a tie is the rule, not the exception: a `Radiohead —
    /// Creep` search returns dozens of identical artists and titles, and when
    /// the duration is unknown they all get the same score. Without this number
    /// the output said "100% confidence" — while what actually happened was a
    /// **deterministic but arbitrary** choice among 25 equivalent candidates
    /// (D-045). K9: the weakness of the evidence behind a choice must be
    /// measurable.
    ///
    /// `1` for links that do not use a candidate list (ISRC, `LocalKey`).
    #[serde(default = "one")]
    pub tied_candidates: usize,
    /// The source ranks more candidates as high as the ones read, and they
    /// were left unread (D-081): its top tier did not fit in one
    /// [`WHOLE_PAGE`], or the reading stopped because the rest could only
    /// join a tie already seen. `tied_candidates` is then a lower bound, and
    /// a winner unique among the candidates read is **not** claimed — it was
    /// never compared with the rest.
    ///
    /// `false` for links that do not search.
    #[serde(default)]
    pub unread_equals: bool,
}

/// `serde` default: old records without the field count as a single winner.
const fn one() -> usize {
    1
}

/// The margin within which two scores count as "the same".
///
/// Looking for exact equality in floating point would show two candidates
/// that ran the same computation in a different order as different. Missing
/// a tie is worse than inventing one: a missed tie is reported as "100%
/// confidence".
const SCORE_EPSILON: f64 = 1e-9;

/// The margin by which confidence stays **strictly** below the threshold in a
/// tie.
const AMBIGUITY_MARGIN: f64 = 0.01;

/// The first read of a search (D-081): most searches end on it, so it stays
/// as small as it was before searches were read whole.
const FIRST_PAGE: usize = 25;

/// The second and last read of a search: MusicBrainz's largest page, asked
/// for **from the start** in one request (D-081).
///
/// Not the next page: MusicBrainz answers each address from its own cache,
/// filled by whichever index replica served it, and the replicas order the
/// equally scored hits differently. A first page of 25 and a second page
/// from offset 25 came from different replicas in four reads of five
/// (measured 2026-10-02): 25 hits twice, 25 never. One response is one
/// replica's ranking, whole. A top tier that does not fit in it is not
/// claimed — a winner of the part read is not a winner of the catalog.
pub const WHOLE_PAGE: usize = 100;

/// How far from the query's duration a search narrows, both ways (D-081).
///
/// A recording further than this from the query scores at most 0.7
/// ([`fuzzy`]'s mismatch penalty), below any threshold, so narrowing to it
/// loses no possible winner — and it turns `Creep`'s 149 equally scored
/// recordings into 33, read whole in one response (measured 2026-10-02).
pub const LENGTH_WINDOW_MS: u64 = fuzzy::DURATION_MISMATCH_MS;

/// A scored candidate and all the evidence that tells it apart.
///
/// `gap` travels next to the score because [`fuzzy::similarity`] splits the
/// duration difference into **bands**: 0 s and 2.9 s fall in the same band,
/// both "fit". Right for the band decision, not for breaking ties — that
/// information was being lost.
struct Scored {
    candidate: Candidate,
    score: f64,
    /// The difference between the query's duration and the candidate's. If
    /// either is unknown, [`u64::MAX`]: "I can't claim closeness", it sorts last.
    gap: u64,
}

/// A candidate scored against the query.
fn score(track: &TrackRef, candidate: Candidate) -> Scored {
    let score = fuzzy::similarity(
        &track.artist,
        &track.title,
        track.duration_ms,
        &candidate.artist,
        &candidate.title,
        candidate.duration_ms,
        candidate.disambiguation.as_deref(),
    );
    let gap = duration_gap(track.duration_ms, candidate.duration_ms);
    Scored {
        candidate,
        score,
        gap,
    }
}

/// Sorts the best first, breaking score ties **deterministically**.
///
/// In a real catalog a tie is the rule, not the exception: a `Radiohead —
/// Creep` search returns more than 190 recordings, dozens of which carry
/// exactly the same artist and title, and when the duration is unknown they
/// all get the same score. `max_by` used to surrender to the order
/// MusicBrainz sent, and that order is not stable — the same query gave two
/// different MBIDs in two runs (D-045). For an identity layer that is
/// unacceptable: the same track cannot get a different canonical id
/// tomorrow.
fn sort_scored(scored: &mut [Scored]) {
    scored.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| tiebreak_rank(&right.candidate).cmp(&tiebreak_rank(&left.candidate)))
            // Closeness in duration: evidence that the score's bands swallowed
            // (D-046). The smaller gap wins.
            .then_with(|| left.gap.cmp(&right.gap))
            // Last resort: MBID order. Arbitrary but **stable** — being stable
            // matters more than not being arbitrary.
            .then_with(|| {
                left.candidate
                    .mbid
                    .as_str()
                    .cmp(right.candidate.mbid.as_str())
            })
    });
}

/// Distance to the query's duration; the worst value if unknown.
///
/// Counting an unknown duration as 0 would mean "matches exactly" — the third
/// time in this module that an unknown would have been counted as a match,
/// and every time it produced a wrong match.
fn duration_gap(query_ms: Option<u64>, candidate_ms: Option<u64>) -> u64 {
    match (query_ms, candidate_ms) {
        (Some(query), Some(candidate)) => query.abs_diff(candidate),
        _ => u64::MAX,
    }
}

/// How many candidates are **truly indistinguishable** from the winner.
///
/// The list is sorted by every sorting criterion, so the indistinguishable
/// ones form a prefix: same score, same [`tiebreak_rank`], same duration gap.
/// The only thing left to separate two candidates equal on all three is the
/// MBID order — and that is not evidence, just a fixed choice.
///
/// Looking only at the score was not enough (D-046): in the live catalog a
/// `Radiohead — Creep` search leaves 9–10 candidates on the same score, and
/// `Şebnem Ferah — Sil Baştan` leaves three candidates — 309, 313 and 315 s
/// long — on the same score. In the first there really is no evidence; in the
/// second there is, and it had been hidden under the bands.
fn count_tied(scored: &[Scored]) -> usize {
    let Some(top) = scored.first() else {
        return 1;
    };
    let top_rank = tiebreak_rank(&top.candidate);
    scored
        .iter()
        .take_while(|entry| {
            (top.score - entry.score).abs() <= SCORE_EPSILON
                && tiebreak_rank(&entry.candidate) == top_rank
                && entry.gap == top.gap
        })
        .count()
        .max(1)
}

/// A candidate recording returned by the metadata source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub mbid: Mbid,
    pub artist: String,
    pub title: String,
    pub duration_ms: Option<u64>,
    pub isrc: Option<Isrc>,
    /// The note that sets a recording apart from its namesakes — MusicBrainz's
    /// `disambiguation` field.
    ///
    /// Information that is not in the title but decides the identity comes from
    /// here: `"live, 1994-05-27: Astoria, London, UK"`. All three `Creep`s in the
    /// catalog are titled `Creep`; which one is the live recording is written
    /// **only** in this field (D-045). [`fuzzy::similarity`] takes it as
    /// `context_b`.
    #[serde(default)]
    pub disambiguation: Option<String>,
}

/// Among candidates with equal scores, which one is the better canonical
/// choice. The larger one wins.
///
/// Two criteria, both answering "which is this song's **default**
/// recording":
/// - A recording **without a disambiguation note** is the default:
///   MusicBrainz writes the note only when a recording has to be told apart
///   from its namesakes. A recording with a note is by definition an
///   exception (live, remix, a different night).
/// - A recording **with a known duration** is preferred over one without: a
///   recording carrying more information has been worked on more, and is
///   therefore more trustworthy.
fn tiebreak_rank(candidate: &Candidate) -> u8 {
    u8::from(candidate.disambiguation.is_none()) * 2 + u8::from(candidate.duration_ms.is_some())
}

/// The highest [`tiebreak_rank`]: no note, a known duration.
const TOP_RANK: u8 = 3;

/// Can no candidate left unread change the outcome (D-081)?
///
/// Only a tie at the ceiling of every sorting criterion is final: the
/// highest score [`fuzzy::similarity`] gives (it is clamped to 1), the
/// [`TOP_RANK`], and the smallest duration gap — `0` when the query has a
/// duration, and without one every gap is the same unknown. An unread
/// candidate can at best equal such a tie, which only makes it larger. A
/// winner that is **unique** among those read is never final: an unread
/// candidate could equal it.
fn settled(scored: &[Scored], query_duration: Option<u64>) -> bool {
    let Some(top) = scored.first() else {
        return false;
    };
    let best_gap = query_duration.is_none() || top.gap == 0;
    count_tied(scored) > 1
        && top.score >= 1.0 - SCORE_EPSILON
        && tiebreak_rank(&top.candidate) == TOP_RANK
        && best_gap
}

/// The first hits of a recording search (D-081).
///
/// A search engine ranks, and returns the head of the ranking. MusicBrainz
/// scores every exact match alike — 149 recordings of `Radiohead — Creep` —
/// and which of them make the head depends on the index replica that
/// answers: three replicas, three different first 25, one of which held a
/// single plain recording (measured). A winner picked from the head is a
/// winner of the head, not of the catalog; `top_continues` is how the source
/// says the head did not hold its whole top tier.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchPage {
    /// The candidates, the source's best first.
    pub candidates: Vec<Candidate>,
    /// The source's top tier goes on past these candidates: more hits ranked
    /// as high as its best were not returned. `false`: the tier ended among
    /// them, or the source holds nothing more.
    pub top_continues: bool,
}

/// A range of recording lengths, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LengthRange {
    pub from_ms: u64,
    pub to_ms: u64,
}

impl LengthRange {
    /// [`LENGTH_WINDOW_MS`] either side of `ms`.
    #[must_use]
    pub const fn around(ms: u64) -> Self {
        Self {
            from_ms: ms.saturating_sub(LENGTH_WINDOW_MS),
            to_ms: ms.saturating_add(LENGTH_WINDOW_MS),
        }
    }

    /// Does a known length fall in the range? An unknown one never does.
    #[must_use]
    pub fn holds(&self, ms: Option<u64>) -> bool {
        ms.is_some_and(|ms| (self.from_ms..=self.to_ms).contains(&ms))
    }
}

/// The summary of one resolution round. The project's most important metric
/// is read from here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveSummary {
    pub total: usize,
    pub by_isrc: usize,
    pub by_mbid: usize,
    pub by_fuzzy: usize,
    pub by_fingerprint: usize,
    pub by_local_key: usize,
}

impl ResolveSummary {
    fn record(&mut self, method: ResolveMethod) {
        self.total += 1;
        match method {
            ResolveMethod::Isrc => self.by_isrc += 1,
            ResolveMethod::Mbid => self.by_mbid += 1,
            ResolveMethod::Fuzzy => self.by_fuzzy += 1,
            ResolveMethod::Fingerprint => self.by_fingerprint += 1,
            ResolveMethod::LocalKey => self.by_local_key += 1,
        }
    }

    /// The share of authoritative resolutions (linked to MusicBrainz).
    ///
    /// This is the target metric of the accuracy set — `LocalKey` does not
    /// count.
    #[must_use]
    pub fn authoritative_ratio(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        let authoritative = self.by_isrc + self.by_mbid + self.by_fuzzy + self.by_fingerprint;
        #[expect(
            clippy::cast_precision_loss,
            reason = "ratio for display; the loss is negligible"
        )]
        {
            authoritative as f64 / self.total as f64
        }
    }

    /// Copies the counters into the diagnostics recorder.
    pub fn record_into(&self, recorder: &mut crate::diag::Recorder) {
        let n = |v: usize| i64::try_from(v).unwrap_or(i64::MAX);
        recorder.set("identity.total", n(self.total));
        recorder.set("identity.by_isrc", n(self.by_isrc));
        recorder.set("identity.by_mbid", n(self.by_mbid));
        recorder.set("identity.by_fuzzy", n(self.by_fuzzy));
        recorder.set("identity.by_fingerprint", n(self.by_fingerprint));
        recorder.set("identity.by_local_key", n(self.by_local_key));
    }
}

/// The return type of a metadata call.
///
/// A boxed future instead of `async fn`, because the trait has to be `dyn`
/// compatible (K7 / D-006): a trait carrying `-> impl Future` cannot give an
/// `Arc<dyn MetadataLookup>`. This is a hand-written version of what the
/// `async-trait` macro generates — so the dependency tree does not grow.
pub type LookupFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A metadata source (MusicBrainz, a local catalog, a test fake).
///
/// Everything that goes online is behind this trait; tests never touch the
/// network. The signatures are `async` because the real implementation will
/// speak HTTP — written synchronously today, every caller would have to
/// change tomorrow.
///
/// `Send + Sync`: `uniffi` models this as a **callback interface**
/// (`#[uniffi::export(with_foreign)]`), so it can be implemented on the
/// Kotlin/Swift side too; an object coming from there crosses threads.
pub trait MetadataLookup: Send + Sync {
    /// Recording id from an ISRC.
    fn recording_by_isrc<'a>(&'a self, isrc: &'a Isrc) -> LookupFuture<'a, Option<Candidate>>;

    /// Candidate search by artist+title: the first `limit` hits, the
    /// source's best first, and whether its top tier goes on past them
    /// (D-081). The source may cap `limit` at its largest page.
    ///
    /// With `length`, only recordings whose length lies in it — recordings
    /// of unknown length are left out too.
    fn search_recordings<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
        length: Option<LengthRange>,
        limit: usize,
    ) -> LookupFuture<'a, SearchPage>;
}

/// A fingerprint match: a candidate recording + the service's own
/// confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct FingerprintCandidate {
    pub candidate: Candidate,
    /// AcoustID's **fingerprint overlap** score, 0–1.
    ///
    /// Not text similarity: [`fuzzy::similarity`] compares artist and title,
    /// while this number compares the audio itself. It lives in a separate field
    /// so the two are not seen on the same scale and confused — for a file with
    /// broken tags the text score can be near zero while this one is 0.99, and
    /// this one is right.
    pub score: f64,
}

/// A source that looks up recordings from an audio fingerprint (AcoustID).
///
/// A separate trait from [`MetadataLookup`] because its input is entirely
/// different: that one takes text, this one takes audio. Squeezing them into
/// one trait would add a meaningless method to [`OfflineLookup`], which never
/// goes online.
pub trait FingerprintLookup: Send + Sync {
    /// The recordings that match a fingerprint — strongest match first.
    ///
    /// An empty list is **not an error**: "this audio is not in the database" is
    /// a valid answer, and means moving on to the chain's next step (the local
    /// key). An error is only the case where we **could not ask** (K9).
    fn recordings_by_fingerprint<'a>(
        &'a self,
        fingerprint: &'a fingerprint::Fingerprint,
    ) -> LookupFuture<'a, Vec<FingerprintCandidate>>;
}

/// A source that never goes online. Phase 0's default: the chain falls to
/// the local key.
#[derive(Debug, Clone, Copy, Default)]
pub struct OfflineLookup;

impl MetadataLookup for OfflineLookup {
    fn recording_by_isrc<'a>(&'a self, _isrc: &'a Isrc) -> LookupFuture<'a, Option<Candidate>> {
        Box::pin(std::future::ready(Ok(None)))
    }

    fn search_recordings<'a>(
        &'a self,
        _artist: &'a str,
        _title: &'a str,
        _length: Option<LengthRange>,
        _limit: usize,
    ) -> LookupFuture<'a, SearchPage> {
        Box::pin(std::future::ready(Ok(SearchPage::default())))
    }
}

/// A fixed catalog in memory. For tests and the accuracy set.
#[derive(Debug, Clone, Default)]
pub struct StaticLookup {
    candidates: Vec<Candidate>,
}

impl StaticLookup {
    #[must_use]
    pub fn new(candidates: Vec<Candidate>) -> Self {
        Self { candidates }
    }
}

impl MetadataLookup for StaticLookup {
    fn recording_by_isrc<'a>(&'a self, isrc: &'a Isrc) -> LookupFuture<'a, Option<Candidate>> {
        Box::pin(async move {
            Ok(self
                .candidates
                .iter()
                .find(|c| c.isrc.as_ref() == Some(isrc))
                .cloned())
        })
    }

    /// It cuts its answer like a search engine, but does not rank: every
    /// match counts as the top, so leaving matches out says the top
    /// continues.
    fn search_recordings<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
        length: Option<LengthRange>,
        limit: usize,
    ) -> LookupFuture<'a, SearchPage> {
        Box::pin(async move {
            let key_artist = normalize::normalize_artist(artist);
            let key_title = normalize::normalize_text(title);
            // Behave like a real search engine: filter roughly by the first letters and
            // let the scoring make the real decision.
            let matches: Vec<&Candidate> = self
                .candidates
                .iter()
                .filter(|c| {
                    normalize::normalize_artist(&c.artist) == key_artist
                        || normalize::normalize_text(&c.title) == key_title
                })
                .filter(|c| length.is_none_or(|range| range.holds(c.duration_ms)))
                .collect();
            let candidates: Vec<Candidate> = matches
                .iter()
                .take(limit)
                .map(|&candidate| candidate.clone())
                .collect();
            let top_continues = candidates.len() < matches.len();
            Ok(SearchPage {
                candidates,
                top_continues,
            })
        })
    }
}

/// Resolution thresholds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolveConfig {
    /// The acceptance threshold for a fuzzy match. Below it, it falls to
    /// `LocalKey`.
    pub min_fuzzy_confidence: f64,
    /// A match above this score counts as an "exact hit" (`Mbid`).
    pub exact_match_confidence: f64,
}

impl Default for ResolveConfig {
    fn default() -> Self {
        Self {
            min_fuzzy_confidence: 0.88,
            exact_match_confidence: 0.98,
        }
    }
}

/// The resolver that walks the identity chain.
///
/// The metadata source is `Arc<dyn MetadataLookup>`, not a generic (D-006):
/// `uniffi` cannot express generic parameters, while `Arc<dyn Trait>` crosses
/// as a callback interface.
#[derive(Clone)]
pub struct Resolver {
    lookup: Arc<dyn MetadataLookup>,
    /// The chain's 4th link. `None` = this resolver does not look at
    /// fingerprints.
    ///
    /// Optional because the link depends on two things at once: having a
    /// **file**, and an AcoustID source having been supplied. Imported history
    /// records have no file; filling this field for them would only carry a
    /// useless dependency.
    fingerprint_lookup: Option<Arc<dyn FingerprintLookup>>,
    config: ResolveConfig,
}

impl std::fmt::Debug for Resolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The source may be implemented in a foreign language; we don't ask for
        // `Debug`.
        f.debug_struct("Resolver")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Resolver {
    #[must_use]
    pub fn new(lookup: Arc<dyn MetadataLookup>) -> Self {
        Self {
            lookup,
            fingerprint_lookup: None,
            config: ResolveConfig::default(),
        }
    }

    #[must_use]
    pub fn with_config(mut self, config: ResolveConfig) -> Self {
        self.config = config;
        self
    }

    /// Wires up the chain's 4th link.
    ///
    /// Used only by [`Self::resolve_file`] — [`Self::resolve`] never sees a file,
    /// so it never touches this source.
    #[must_use]
    pub fn with_fingerprint_lookup(mut self, lookup: Arc<dyn FingerprintLookup>) -> Self {
        self.fingerprint_lookup = Some(lookup);
        self
    }

    /// Runs a single track through the chain.
    ///
    /// # Errors
    /// If the metadata source returns an error. The source saying "not found" is
    /// not an error — the chain moves on to the next link.
    pub async fn resolve(&self, track: &TrackRef) -> Result<Resolution> {
        // Link 1: ISRC.
        if let Some(isrc) = &track.isrc {
            if let Some(candidate) = self.lookup.recording_by_isrc(isrc).await? {
                return Ok(Resolution {
                    canonical_id: CanonicalId::from_mbid(&candidate.mbid),
                    method: ResolveMethod::Isrc,
                    confidence: 1.0,
                    matched: Some(candidate),
                    tied_candidates: 1,
                    unread_equals: false,
                });
            }
            // The source did not know the ISRC, but the ISRC itself is a valid
            // authority.
            return Ok(Resolution {
                canonical_id: CanonicalId::from_isrc(isrc),
                method: ResolveMethod::Isrc,
                confidence: 0.95,
                matched: None,
                tied_candidates: 1,
                unread_equals: false,
            });
        }

        // Links 2 and 3: metadata search + scoring, read as far as it can
        // change the outcome (D-081).
        let (scored, whole) = self.search(track).await?;
        let tied = count_tied(&scored);
        let best = scored.into_iter().next();

        if let Some(Scored {
            candidate, score, ..
        }) = best
        {
            if score >= self.config.min_fuzzy_confidence {
                if tied > 1 {
                    // **No authority is claimed in a tie** (D-046).
                    //
                    // D-045 returned an MBID from here, only clipping the confidence
                    // and saying how many candidates were tied. The live run showed
                    // this was not enough: MusicBrainz serves search from several
                    // index replicas, and the same query run twice in a row can
                    // return 25 candidates each time that **do not overlap at all**.
                    // The deterministic ordering works *within* the set, but the set
                    // is different every time, so the chosen MBID depended on which
                    // replica answered. For an identity layer that is unacceptable.
                    //
                    // Measured: `Radiohead — Creep` (without duration) leaves 9–10
                    // candidates on the same score **and** the same rank in every
                    // run. There is no distinguishing evidence; instead of producing
                    // an identity from evidence that does not exist, we fall to the
                    // local key. What is lost is `authoritative_ratio`; what is
                    // gained is the identity **staying the same** — without the
                    // second, the first is meaningless.
                    tracing::debug!(
                        track = track.display_name(),
                        score,
                        tied,
                        "equivalent candidates could not be told apart; no authority is claimed"
                    );
                    return Ok(self.ambiguous(track, tied, !whole));
                }
                if !whole {
                    // **Nor is it claimed for a winner of the part read** (D-081).
                    // The source ranks more candidates as high as this one and
                    // they were not read, so any of them could equal it. Measured:
                    // one MusicBrainz replica's first page of `Creep` held a single
                    // plain recording among its 25, while the whole top tier holds
                    // 39 — claiming that one made the identity depend on the
                    // replica again, the very thing D-046 ended.
                    tracing::debug!(
                        track = track.display_name(),
                        score,
                        "the source ranks more candidates as high as the ones read; no authority is claimed"
                    );
                    return Ok(self.ambiguous(track, 1, true));
                }
                let method = if score >= self.config.exact_match_confidence {
                    ResolveMethod::Mbid
                } else {
                    ResolveMethod::Fuzzy
                };
                return Ok(Resolution {
                    canonical_id: CanonicalId::from_mbid(&candidate.mbid),
                    method,
                    confidence: score,
                    matched: Some(candidate),
                    tied_candidates: 1,
                    unread_equals: false,
                });
            }
            tracing::debug!(
                track = track.display_name(),
                score,
                threshold = self.config.min_fuzzy_confidence,
                "the best candidate did not pass the threshold"
            );
        }

        // Link 4 (AcoustID) is not called from here and cannot be: the fingerprint
        // looks at the audio itself, and a `TrackRef` has no audio. That link runs
        // through [`Self::resolve_file`], when there is a file.
        Ok(self.ambiguous(track, 1, !whole))
    }

    /// The candidates a search can decide on, scored and sorted, and whether
    /// they hold the source's whole top tier (D-081).
    ///
    /// With a duration the search first narrows to [`LENGTH_WINDOW_MS`]
    /// around it. If that tier is read whole and its best is a plain
    /// recording at the ceiling score, no recording outside it can equal
    /// that — further away it scores at most 0.7, of unknown length it ranks
    /// lower — so the window decides. Otherwise every length is read: a
    /// recording of unknown length may be the best there is.
    async fn search(&self, track: &TrackRef) -> Result<(Vec<Scored>, bool)> {
        if let Some(duration) = track.duration_ms {
            let (scored, whole) = self
                .read(track, Some(LengthRange::around(duration)))
                .await?;
            let decides = scored.first().is_some_and(|top| {
                top.score >= 1.0 - SCORE_EPSILON && tiebreak_rank(&top.candidate) == TOP_RANK
            });
            if !whole || decides {
                return Ok((scored, whole));
            }
        }
        self.read(track, None).await
    }

    /// Reads a search's head; if the top tier goes on past it and the
    /// outcome can still change, the first [`WHOLE_PAGE`] in one response
    /// (D-081). Returns the candidates read, scored and sorted, and whether
    /// they hold the whole top tier.
    async fn read(
        &self,
        track: &TrackRef,
        length: Option<LengthRange>,
    ) -> Result<(Vec<Scored>, bool)> {
        let mut limit = FIRST_PAGE;
        loop {
            let page = self
                .lookup
                .search_recordings(&track.artist, &track.title, length, limit)
                .await?;
            let mut scored: Vec<Scored> = page
                .candidates
                .into_iter()
                .map(|candidate| score(track, candidate))
                .collect();
            sort_scored(&mut scored);
            if !page.top_continues {
                return Ok((scored, true));
            }
            if limit >= WHOLE_PAGE || settled(&scored, track.duration_ms) {
                return Ok((scored, false));
            }
            limit = WHOLE_PAGE;
        }
    }

    /// The case where the chain could not reach an authority: the local key.
    ///
    /// If `tied` is greater than 1 the reason is not "no candidates at all" but
    /// "the candidates could not be told apart" — both produce the same
    /// identity but they are **not the same diagnosis**, and the report must be
    /// able to tell them apart (K9). `unread` says the source ranks more
    /// candidates as high as the ones read (D-081).
    fn ambiguous(&self, track: &TrackRef, tied: usize, unread: bool) -> Resolution {
        let _ = self;
        Resolution {
            canonical_id: CanonicalId::from_local_key(&normalize::track_key(
                &track.artist,
                &track.title,
            )),
            method: ResolveMethod::LocalKey,
            confidence: 0.2,
            matched: None,
            tied_candidates: tied,
            unread_equals: unread,
        }
    }

    /// Runs an **audio file** through all four links of the chain.
    ///
    /// It differs from [`Self::resolve`] in one thing: there is a file. That
    /// gains two things — the metadata is read from the file's own tags (we are
    /// not stuck with what the export gave), and if the text links come up
    /// empty, the **audio** can be asked.
    ///
    /// The order is not broken (K6): first ISRC/MBID/fuzzy from the tags, and
    /// only if all three fall to `LocalKey`, the fingerprint. The fingerprint is
    /// last because it is the most expensive — the whole file is decoded — and
    /// it is not needed when the first three work.
    ///
    /// # Errors
    /// If the file cannot be opened or read, or the fingerprint source **cannot
    /// be asked**. Failing to *produce* a fingerprint (file too short, broken
    /// packets) is not an error: the reason is logged and the chain ends with the
    /// local key — we do not lose what the text links found because of an audio
    /// defect.
    pub async fn resolve_file(&self, path: &Path) -> Result<Resolution> {
        let (track, from_tags) = crate::provider::local::read_track(path)?;
        if !from_tags {
            // No tags: the text links' input was derived from the file name.
            // The chain getting this far and falling to the fingerprint is the
            // **expected** case, not a surprise.
            tracing::debug!(file = %path.display(), "no tags, metadata from the file name");
        }

        let text = self.resolve(&track).await?;
        if text.method != ResolveMethod::LocalKey {
            return Ok(text);
        }

        let Some(lookup) = self.fingerprint_lookup.as_ref() else {
            tracing::debug!(
                file = %path.display(),
                "the chain fell to the local key and no fingerprint source is wired up"
            );
            return Ok(text);
        };

        let print = match fingerprint::fingerprint_file(path) {
            Ok(print) => print,
            Err(err) => {
                // We do not swallow it silently: which file could not give a
                // fingerprint, and why, must stay visible, or "AcoustID never
                // finds a match" gets investigated in the wrong place (K9).
                tracing::warn!(
                    file = %path.display(),
                    error = %err.chain_text(),
                    "could not produce a fingerprint; the chain ends with the local key"
                );
                return Ok(text);
            }
        };

        // The error here **is propagated**, and that is a deliberate distinction:
        // failing to produce a fingerprint is a property of the file, while being
        // unable to ask AcoustID is a configuration or network defect. Swallowing
        // the second would report a setup with no key as "nothing matches".
        let matches = lookup.recordings_by_fingerprint(&print).await?;
        let Some(best) = matches.first() else {
            tracing::debug!(file = %path.display(), "fingerprint not recognised");
            return Ok(text);
        };

        let tied = matches
            .iter()
            .take_while(|found| (best.score - found.score).abs() <= SCORE_EPSILON)
            .count()
            .max(1);
        // The same rule as on the text side (D-045): a choice made among equivalents
        // cannot be reported as an exact hit.
        let confidence = if tied > 1 {
            best.score
                .min(self.config.exact_match_confidence - AMBIGUITY_MARGIN)
        } else {
            best.score
        };

        Ok(Resolution {
            canonical_id: CanonicalId::from_mbid(&best.candidate.mbid),
            method: ResolveMethod::Fingerprint,
            confidence,
            matched: Some(best.candidate.clone()),
            tied_candidates: tied,
            unread_equals: false,
        })
    }

    /// Resolves a set of tracks and returns a summary.
    ///
    /// The same track can appear more than once; repeated resolutions are
    /// cached.
    ///
    /// # Errors
    /// If the metadata source returns an error.
    pub async fn resolve_all(
        &self,
        tracks: &[TrackRef],
    ) -> Result<(Vec<Resolution>, ResolveSummary)> {
        let mut cache: std::collections::HashMap<String, Resolution> =
            std::collections::HashMap::new();
        let mut out = Vec::with_capacity(tracks.len());
        let mut summary = ResolveSummary::default();

        for track in tracks {
            let key = normalize::track_key(&track.artist, &track.title);
            let resolution = if let Some(hit) = cache.get(&key) {
                hit.clone()
            } else {
                let resolved = self.resolve(track).await?;
                cache.insert(key, resolved.clone());
                resolved
            };
            summary.record(resolution.method);
            out.push(resolution);
        }
        Ok((out, summary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(mbid: &str, artist: &str, title: &str, duration_ms: Option<u64>) -> Candidate {
        Candidate {
            mbid: Mbid::parse(mbid).expect("test mbid is valid"),
            artist: artist.to_owned(),
            title: title.to_owned(),
            duration_ms,
            isrc: None,
            disambiguation: None,
        }
    }

    fn catalog() -> StaticLookup {
        StaticLookup::new(vec![
            Candidate {
                isrc: Isrc::parse("GBAYE9200001"),
                ..candidate(
                    "b1a9c0e9-d987-4042-ae91-78d6a3267d69",
                    "Radiohead",
                    "Creep",
                    Some(238_000),
                )
            },
            candidate(
                "c2b8d1f0-1234-4042-ae91-78d6a3267d70",
                "Portishead",
                "Roads",
                Some(303_000),
            ),
        ])
    }

    #[tokio::test]
    async fn isrc_wins_the_chain() {
        let resolver = Resolver::new(Arc::new(catalog()));
        // The metadata is garbage on purpose: with an ISRC, the chain must link up
        // without looking at it.
        let track = TrackRef::new("misspelled artist", "wrong title")
            .with_isrc(Isrc::parse("GBAYE9200001"));
        let res = resolver.resolve(&track).await.unwrap();
        assert_eq!(res.method, ResolveMethod::Isrc);
        assert_eq!(res.confidence, 1.0);
        assert_eq!(res.canonical_id.kind(), crate::ids::CanonicalKind::Mbid);
    }

    #[tokio::test]
    async fn exact_metadata_match_is_reported_as_mbid() {
        let resolver = Resolver::new(Arc::new(catalog()));
        let track =
            TrackRef::new("Radiohead", "Creep (Remastered)").with_duration_ms(Some(238_400));
        let res = resolver.resolve(&track).await.unwrap();
        assert_eq!(res.method, ResolveMethod::Mbid);
        assert!(res.confidence >= 0.98, "{}", res.confidence);
    }

    #[tokio::test]
    async fn below_threshold_falls_back_to_local_key() {
        let resolver = Resolver::new(Arc::new(catalog()));
        let track = TrackRef::new("Radiohead", "Karma Police");
        let res = resolver.resolve(&track).await.unwrap();
        assert_eq!(res.method, ResolveMethod::LocalKey);
        assert_eq!(res.canonical_id.kind(), crate::ids::CanonicalKind::Local);
    }

    #[tokio::test]
    async fn offline_lookup_never_invents_authority() {
        let resolver = Resolver::new(Arc::new(OfflineLookup));
        let track = TrackRef::new("Radiohead", "Creep");
        let res = resolver.resolve(&track).await.unwrap();
        assert_eq!(res.method, ResolveMethod::LocalKey);
    }

    /// **No choice is made** among equivalent candidates (D-046).
    ///
    /// D-045 returned an MBID from here, only clipping the confidence. The live
    /// run showed that was not enough: MusicBrainz serves search from several
    /// index replicas, and the same query run twice in a row can return two
    /// candidate sets that **do not overlap at all**. Being deterministic within
    /// a set cannot keep the identity when the set changes. Without
    /// distinguishing evidence, no authority is claimed.
    #[tokio::test]
    async fn equivalent_candidates_yield_no_authority_at_all() {
        // Same artist, same title, three separate recordings — real MusicBrainz
        // returns exactly this for `Radiohead — Creep`.
        let lookup = StaticLookup::new(vec![
            candidate(
                "cccccccc-1111-4042-ae91-78d6a3267d01",
                "Nirvana",
                "Lithium",
                None,
            ),
            candidate(
                "aaaaaaaa-1111-4042-ae91-78d6a3267d02",
                "Nirvana",
                "Lithium",
                None,
            ),
            candidate(
                "bbbbbbbb-1111-4042-ae91-78d6a3267d03",
                "Nirvana",
                "Lithium",
                None,
            ),
        ]);
        let resolver = Resolver::new(Arc::new(lookup));
        let track = TrackRef::new("Nirvana", "Lithium");

        let res = resolver.resolve(&track).await.unwrap();
        assert_eq!(
            res.tied_candidates, 3,
            "all three candidates must be indistinguishable"
        );
        assert_eq!(
            res.method,
            ResolveMethod::LocalKey,
            "no authority can be claimed without distinguishing evidence"
        );
        assert!(
            res.matched.is_none(),
            "a candidate that was not chosen cannot be reported as the match"
        );
        assert_eq!(
            res.canonical_id.kind(),
            crate::ids::CanonicalKind::Local,
            "the identity must come from the local key: {}",
            res.canonical_id
        );
    }

    /// The **two separate reasons** for falling to the local key must be
    /// distinguishable (K9).
    ///
    /// "No candidates at all" and "the candidates could not be told apart"
    /// produce the same identity but are not the same diagnosis: the first is
    /// solved by better metadata, the second by better **distinguishing**
    /// information (duration, ISRC, fingerprint).
    #[tokio::test]
    async fn an_ambiguous_fallback_is_distinguishable_from_an_empty_one() {
        let empty = Resolver::new(Arc::new(OfflineLookup))
            .resolve(&TrackRef::new("Radiohead", "Creep"))
            .await
            .unwrap();
        assert_eq!(empty.method, ResolveMethod::LocalKey);
        assert_eq!(empty.tied_candidates, 1, "there were no candidates at all");

        let ambiguous = Resolver::new(Arc::new(StaticLookup::new(vec![
            candidate(
                "aaaaaaaa-1111-4042-ae91-78d6a3267d02",
                "Nirvana",
                "Lithium",
                None,
            ),
            candidate(
                "bbbbbbbb-1111-4042-ae91-78d6a3267d03",
                "Nirvana",
                "Lithium",
                None,
            ),
        ])))
        .resolve(&TrackRef::new("Nirvana", "Lithium"))
        .await
        .unwrap();
        assert_eq!(ambiguous.method, ResolveMethod::LocalKey);
        assert_eq!(
            ambiguous.tied_candidates, 2,
            "two candidates could not be told apart"
        );
    }

    /// The same identity must come out even if the server's order changes.
    #[tokio::test]
    async fn candidate_order_does_not_change_the_chosen_id() {
        let entries = [
            candidate(
                "cccccccc-1111-4042-ae91-78d6a3267d01",
                "Nirvana",
                "Lithium",
                None,
            ),
            candidate(
                "aaaaaaaa-1111-4042-ae91-78d6a3267d02",
                "Nirvana",
                "Lithium",
                None,
            ),
            candidate(
                "bbbbbbbb-1111-4042-ae91-78d6a3267d03",
                "Nirvana",
                "Lithium",
                None,
            ),
        ];
        let track = TrackRef::new("Nirvana", "Lithium");

        let forward = Resolver::new(Arc::new(StaticLookup::new(entries.to_vec())))
            .resolve(&track)
            .await
            .unwrap();
        let mut reversed = entries.to_vec();
        reversed.reverse();
        let backward = Resolver::new(Arc::new(StaticLookup::new(reversed)))
            .resolve(&track)
            .await
            .unwrap();

        assert_eq!(
            forward.canonical_id, backward.canonical_id,
            "the choice depended on the order the source sent"
        );
    }

    /// A recording without a note is the default; the duration is the second
    /// criterion.
    #[tokio::test]
    async fn the_plain_recording_wins_over_the_annotated_one() {
        let live = Candidate {
            disambiguation: Some("live, 1994-05-27: Astoria, London, UK".to_owned()),
            // The MBID sorts first alphabetically on purpose: without the note, it
            // would win.
            ..candidate(
                "00000000-1111-4042-ae91-78d6a3267d01",
                "Nirvana",
                "Lithium",
                None,
            )
        };
        let plain = candidate(
            "ffffffff-1111-4042-ae91-78d6a3267d02",
            "Nirvana",
            "Lithium",
            None,
        );
        let resolver = Resolver::new(Arc::new(StaticLookup::new(vec![live, plain])));

        let res = resolver
            .resolve(&TrackRef::new("Nirvana", "Lithium"))
            .await
            .unwrap();
        let matched = res.matched.expect("a candidate must be returned");
        assert_eq!(
            matched.disambiguation, None,
            "the recording with a note should not have been chosen"
        );
    }

    /// A source that serves fixed answers in turn — the n-th request gets
    /// the n-th answer, whatever it asks for — and records what was asked:
    /// the shape of a search without a server (D-081).
    struct ScriptedLookup {
        answers: Vec<SearchPage>,
        asked: std::sync::Mutex<Vec<(Option<LengthRange>, usize)>>,
    }

    impl ScriptedLookup {
        fn new(answers: Vec<(Vec<Candidate>, bool)>) -> Arc<Self> {
            Arc::new(Self {
                answers: answers
                    .into_iter()
                    .map(|(candidates, top_continues)| SearchPage {
                        candidates,
                        top_continues,
                    })
                    .collect(),
                asked: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn asked(&self) -> Vec<(Option<LengthRange>, usize)> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl MetadataLookup for ScriptedLookup {
        fn recording_by_isrc<'a>(&'a self, _isrc: &'a Isrc) -> LookupFuture<'a, Option<Candidate>> {
            Box::pin(std::future::ready(Ok(None)))
        }

        fn search_recordings<'a>(
            &'a self,
            _artist: &'a str,
            _title: &'a str,
            length: Option<LengthRange>,
            limit: usize,
        ) -> LookupFuture<'a, SearchPage> {
            let mut asked = self.asked.lock().unwrap();
            let answer = self.answers.get(asked.len()).cloned().unwrap_or_default();
            asked.push((length, limit));
            Box::pin(std::future::ready(Ok(answer)))
        }
    }

    /// `Radiohead — Creep` recording number `n`: plain (no note, 238 s) or a
    /// live take.
    fn creep(n: usize, plain: bool) -> Candidate {
        let mut c = candidate(
            &format!("00000000-0000-4000-8000-{n:012}"),
            "Radiohead",
            "Creep",
            Some(238_000),
        );
        if !plain {
            c.disambiguation = Some(format!("live, night {n}"));
        }
        c
    }

    /// `head` followed by live takes up to `len` candidates.
    fn answer(head: Vec<Candidate>, len: usize) -> Vec<Candidate> {
        let mut out = head;
        let from = 1000 + out.len();
        out.extend((from..from + len.saturating_sub(out.len())).map(|n| creep(n, false)));
        out
    }

    const NO_LENGTH: Option<LengthRange> = None;

    /// D-081, the replica that turned CI red: its first 25 held a single
    /// plain recording, and the top went on. The first hundred, read in one
    /// response, hold another — a tie the head hid. No authority.
    #[tokio::test]
    async fn the_first_hundred_find_the_equal_the_head_hid() {
        let lookup = ScriptedLookup::new(vec![
            (answer(vec![creep(1, true)], 25), true),
            (answer(vec![creep(1, true), creep(2, true)], 100), true),
        ]);
        let res = Resolver::new(lookup.clone())
            .resolve(&TrackRef::new("Radiohead", "Creep"))
            .await
            .unwrap();

        assert_eq!(res.method, ResolveMethod::LocalKey);
        assert_eq!(res.tied_candidates, 2);
        assert!(res.unread_equals, "the top went on past the hundred");
        assert_eq!(lookup.asked(), vec![(NO_LENGTH, 25), (NO_LENGTH, 100)]);
    }

    /// The other side: the hundred hold the whole tier, with no equal of
    /// the head's winner — it is a winner of the catalog, and claimed.
    #[tokio::test]
    async fn a_winner_of_the_whole_tier_is_claimed() {
        let lookup = ScriptedLookup::new(vec![
            (answer(vec![creep(1, true)], 25), true),
            (answer(vec![creep(1, true)], 60), false),
        ]);
        let res = Resolver::new(lookup.clone())
            .resolve(&TrackRef::new("Radiohead", "Creep"))
            .await
            .unwrap();

        assert_eq!(res.method, ResolveMethod::Mbid);
        assert_eq!(res.matched.unwrap().mbid, creep(1, true).mbid);
        assert!(!res.unread_equals);
        assert_eq!(lookup.asked().len(), 2);
    }

    /// A tier wider than one response: the winner of the part read is not
    /// claimed, and nothing past the hundred is asked for — a next page
    /// would come from another replica's ranking.
    #[tokio::test]
    async fn a_tier_wider_than_one_response_claims_nothing() {
        let lookup = ScriptedLookup::new(vec![
            (answer(vec![creep(1, true)], 25), true),
            (answer(vec![creep(1, true)], 100), true),
            (answer(vec![creep(2, true)], 100), false),
        ]);
        let res = Resolver::new(lookup.clone())
            .resolve(&TrackRef::new("Radiohead", "Creep"))
            .await
            .unwrap();

        assert_eq!(res.method, ResolveMethod::LocalKey);
        assert_eq!(res.tied_candidates, 1);
        assert!(
            res.unread_equals,
            "a winner of the part read is not a winner"
        );
        assert_eq!(lookup.asked(), vec![(NO_LENGTH, 25), (NO_LENGTH, 100)]);
    }

    /// A tie at the ceiling in the head is final: no unread candidate can
    /// break it, so nothing more is asked for.
    #[tokio::test]
    async fn a_tie_at_the_ceiling_stops_the_reading() {
        let lookup = ScriptedLookup::new(vec![(
            answer(vec![creep(1, true), creep(2, true)], 25),
            true,
        )]);
        let res = Resolver::new(lookup.clone())
            .resolve(&TrackRef::new("Radiohead", "Creep"))
            .await
            .unwrap();

        assert_eq!(res.method, ResolveMethod::LocalKey);
        assert_eq!(res.tied_candidates, 2);
        assert_eq!(lookup.asked(), vec![(NO_LENGTH, 25)]);
    }

    /// With a duration the search narrows to the length window, and a plain
    /// recording at the ceiling inside it decides: the closest wins.
    #[tokio::test]
    async fn with_a_duration_the_length_window_decides() {
        let mut near = creep(1, true);
        near.duration_ms = Some(236_000);
        let exact = creep(2, true);
        let lookup = ScriptedLookup::new(vec![(answer(vec![near, exact.clone()], 20), false)]);
        let res = Resolver::new(lookup.clone())
            .resolve(&TrackRef::new("Radiohead", "Creep").with_duration_ms(Some(238_000)))
            .await
            .unwrap();

        assert_eq!(res.matched.unwrap().mbid, exact.mbid);
        assert_eq!(
            lookup.asked(),
            vec![(Some(LengthRange::around(238_000)), 25)]
        );
    }

    /// A window without a plain recording at the ceiling cannot decide: a
    /// recording of unknown length — outside every window — may be the best
    /// there is. Every length is read, and here it is.
    #[tokio::test]
    async fn a_window_that_cannot_decide_falls_back_to_every_length() {
        let mut unknown = creep(5, true);
        unknown.duration_ms = None;
        let lookup = ScriptedLookup::new(vec![
            (answer(Vec::new(), 10), false),
            (answer(vec![unknown.clone()], 12), false),
        ]);
        let res = Resolver::new(lookup.clone())
            .resolve(&TrackRef::new("Radiohead", "Creep").with_duration_ms(Some(238_000)))
            .await
            .unwrap();

        assert_eq!(res.matched.unwrap().mbid, unknown.mbid);
        assert_eq!(
            lookup.asked(),
            vec![(Some(LengthRange::around(238_000)), 25), (NO_LENGTH, 25)]
        );
    }

    /// A window whose tier does not fit in one response decides nothing,
    /// and reading every length would only be wider.
    #[tokio::test]
    async fn a_window_wider_than_one_response_claims_nothing() {
        let window = Some(LengthRange::around(238_000));
        let lookup = ScriptedLookup::new(vec![
            (answer(vec![creep(1, true)], 25), true),
            (answer(vec![creep(1, true)], 100), true),
        ]);
        let res = Resolver::new(lookup.clone())
            .resolve(&TrackRef::new("Radiohead", "Creep").with_duration_ms(Some(238_000)))
            .await
            .unwrap();

        assert_eq!(res.method, ResolveMethod::LocalKey);
        assert!(res.unread_equals);
        assert_eq!(lookup.asked(), vec![(window, 25), (window, 100)]);
    }

    /// The window is the fuzzy scorer's own limit, and holds known lengths
    /// only.
    #[test]
    fn the_length_window_is_the_mismatch_limit_and_holds_known_lengths_only() {
        let range = LengthRange::around(238_000);
        assert_eq!((range.from_ms, range.to_ms), (223_000, 253_000));
        assert!(range.holds(Some(253_000)));
        assert!(!range.holds(Some(253_001)));
        assert!(!range.holds(None));
        assert_eq!(LengthRange::around(5_000).from_ms, 0);
    }

    #[tokio::test]
    async fn summary_counts_every_method() {
        let resolver = Resolver::new(Arc::new(catalog()));
        let tracks = vec![
            TrackRef::new("Radiohead", "Creep").with_duration_ms(Some(238_000)),
            TrackRef::new("Radiohead", "Creep").with_duration_ms(Some(238_000)),
            TrackRef::new("Unknown", "Track"),
        ];
        let (resolutions, summary) = resolver.resolve_all(&tracks).await.unwrap();
        assert_eq!(resolutions.len(), 3);
        assert_eq!(summary.total, 3);
        assert_eq!(
            summary.by_mbid, 2,
            "the same track must get the same result from the cache"
        );
        assert_eq!(summary.by_local_key, 1);
        assert!((summary.authoritative_ratio() - 2.0 / 3.0).abs() < 1e-9);
    }

    /// The chain's 4th link — only in builds that can produce fingerprints.
    #[cfg(feature = "fingerprint")]
    mod chain_with_audio {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        fn fixture(name: &str) -> std::path::PathBuf {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/audio")
                .join(name)
        }

        /// A fingerprint source with fixed answers; counts how often it is asked.
        struct FakeFingerprints {
            answer: Vec<FingerprintCandidate>,
            calls: AtomicUsize,
        }

        impl FakeFingerprints {
            fn new(answer: Vec<FingerprintCandidate>) -> Self {
                Self {
                    answer,
                    calls: AtomicUsize::new(0),
                }
            }
        }

        impl FingerprintLookup for FakeFingerprints {
            fn recordings_by_fingerprint<'a>(
                &'a self,
                _fingerprint: &'a fingerprint::Fingerprint,
            ) -> LookupFuture<'a, Vec<FingerprintCandidate>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(std::future::ready(Ok(self.answer.clone())))
            }
        }

        fn found(mbid: &str, score: f64) -> FingerprintCandidate {
            FingerprintCandidate {
                candidate: candidate(mbid, "Radiohead", "Creep", Some(238_000)),
                score,
            }
        }

        /// When the text links come up empty, the audio is asked and the identity
        /// comes from there.
        #[tokio::test]
        async fn the_fingerprint_link_answers_when_the_text_links_cannot() {
            let prints = Arc::new(FakeFingerprints::new(vec![found(
                "b1a9c0e9-d987-4042-ae91-78d6a3267d69",
                0.99,
            )]));
            let resolver = Resolver::new(Arc::new(catalog()))
                .with_fingerprint_lookup(Arc::clone(&prints) as Arc<dyn FingerprintLookup>);

            // The fixture has no tags and its name matches nothing in the catalog:
            // all three text links are helpless.
            let res = resolver
                .resolve_file(&fixture("fingerprint_sample.flac"))
                .await
                .expect("resolution");

            assert_eq!(res.method, ResolveMethod::Fingerprint);
            assert!((res.confidence - 0.99).abs() < 1e-9);
            assert_eq!(prints.calls.load(Ordering::SeqCst), 1);
        }

        /// The order is not broken: if the tags gave the answer, the audio is never
        /// asked.
        ///
        /// The fingerprint is the most expensive link (the whole file is decoded);
        /// running it needlessly would be a silent cost.
        #[tokio::test]
        async fn a_tagged_file_never_reaches_the_fingerprint_link() {
            let prints = Arc::new(FakeFingerprints::new(vec![found(
                "b1a9c0e9-d987-4042-ae91-78d6a3267d69",
                0.99,
            )]));
            let catalog = StaticLookup::new(vec![candidate(
                "d3c9e2a1-1111-4042-ae91-78d6a3267d71",
                "Test Artist",
                "Sine 440 ünïcode",
                None,
            )]);
            let resolver = Resolver::new(Arc::new(catalog))
                .with_fingerprint_lookup(Arc::clone(&prints) as Arc<dyn FingerprintLookup>);

            let res = resolver
                .resolve_file(&fixture("tagged.flac"))
                .await
                .expect("resolution");

            assert_ne!(res.method, ResolveMethod::Fingerprint);
            assert_eq!(
                prints.calls.load(Ordering::SeqCst),
                0,
                "the audio should not have been asked"
            );
        }

        /// If a fingerprint **cannot be produced**, what the text side found is not
        /// lost.
        ///
        /// The 2-second fixture is below the threshold: the chain ends with the
        /// local key and returns no error.
        #[tokio::test]
        async fn a_file_that_cannot_be_fingerprinted_still_returns_a_local_key() {
            let prints = Arc::new(FakeFingerprints::new(vec![found(
                "b1a9c0e9-d987-4042-ae91-78d6a3267d69",
                0.99,
            )]));
            let resolver = Resolver::new(Arc::new(catalog()))
                .with_fingerprint_lookup(Arc::clone(&prints) as Arc<dyn FingerprintLookup>);

            let res = resolver
                .resolve_file(&fixture("Test Artist - Mp3 Track.mp3"))
                .await
                .expect("a short file must not bring the chain down");

            assert_eq!(res.method, ResolveMethod::LocalKey);
            assert_eq!(
                prints.calls.load(Ordering::SeqCst),
                0,
                "the service must not be asked without a fingerprint"
            );
        }

        /// If the audio was not recognised that is not an error: the chain ends with
        /// the local key.
        #[tokio::test]
        async fn an_unrecognised_fingerprint_is_absence_not_failure() {
            let prints = Arc::new(FakeFingerprints::new(Vec::new()));
            let resolver = Resolver::new(Arc::new(catalog()))
                .with_fingerprint_lookup(prints as Arc<dyn FingerprintLookup>);

            let res = resolver
                .resolve_file(&fixture("fingerprint_sample.flac"))
                .await
                .expect("resolution");

            assert_eq!(res.method, ResolveMethod::LocalKey);
        }

        /// On the fingerprint side too, a tie cannot be reported as an "exact hit"
        /// (D-045's third lesson, this time on the audio side).
        #[tokio::test]
        async fn a_tie_on_the_audio_side_is_reported_and_capped() {
            let prints = Arc::new(FakeFingerprints::new(vec![
                found("b1a9c0e9-d987-4042-ae91-78d6a3267d69", 0.99),
                found("c2b8d1f0-1234-4042-ae91-78d6a3267d70", 0.99),
            ]));
            let resolver = Resolver::new(Arc::new(catalog()))
                .with_fingerprint_lookup(prints as Arc<dyn FingerprintLookup>);

            let res = resolver
                .resolve_file(&fixture("fingerprint_sample.flac"))
                .await
                .expect("resolution");

            assert_eq!(res.tied_candidates, 2);
            assert!(
                res.confidence < ResolveConfig::default().exact_match_confidence,
                "{}",
                res.confidence
            );
        }

        /// If no source is wired up the chain ends after three links — it does not
        /// crash.
        #[tokio::test]
        async fn without_a_fingerprint_source_the_chain_simply_ends_early() {
            let resolver = Resolver::new(Arc::new(catalog()));
            let res = resolver
                .resolve_file(&fixture("fingerprint_sample.flac"))
                .await
                .expect("resolution");
            assert_eq!(res.method, ResolveMethod::LocalKey);
        }

        /// Failing to **ask** the service is not swallowed: a configuration defect
        /// must be visible.
        #[tokio::test]
        async fn a_lookup_failure_is_propagated_not_swallowed() {
            struct Broken;
            impl FingerprintLookup for Broken {
                fn recordings_by_fingerprint<'a>(
                    &'a self,
                    _fingerprint: &'a fingerprint::Fingerprint,
                ) -> LookupFuture<'a, Vec<FingerprintCandidate>> {
                    Box::pin(std::future::ready(Err(crate::error::Error::new(
                        crate::diag::Stage::IdentityResolve,
                        crate::error::ErrorKind::InvalidInput {
                            detail: "no key".to_owned(),
                        },
                    ))))
                }
            }

            let resolver = Resolver::new(Arc::new(catalog()))
                .with_fingerprint_lookup(Arc::new(Broken) as Arc<dyn FingerprintLookup>);
            let err = resolver
                .resolve_file(&fixture("fingerprint_sample.flac"))
                .await
                .unwrap_err();
            assert!(err.chain_text().contains("no key"), "{}", err.chain_text());
        }
    }
}
