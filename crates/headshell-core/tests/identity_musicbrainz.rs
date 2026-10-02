//! The identity chain, **against the real MusicBrainz** (Phase 2 §2.3).
//!
//! `identity_accuracy.rs` measures scoring with a synthetic catalog: what is
//! tested there is the logic. What is tested here is something else — that
//! the query is really built and sent, that the response is decoded, and
//! that the chain gets tied to an **authority** without falling back to
//! `LocalKey`. Both tests are needed: one measures the rules, the other that
//! the rules are applied to the right data.
//!
//! **These tests are part of the default run** (the same decision D-043 made
//! for SoundCloud). Two failures are kept apart (K9):
//! - **Not reaching it** is not a failure: without a network the test skips
//!   itself and writes the reason to `stderr`. This is **not limited** to the
//!   TCP probe at the start — a timeout arriving in the middle of the test or
//!   an exhausted `503` retry is skipped too ([`skip_if_unreachable`],
//!   D-046). In a busy run we share the quota with the server's load at that
//!   moment too, and without this distinction a green gate would depend on
//!   the network weather.
//! - **Reaching it and getting the unexpected** fails. `400`, `404`, a parse
//!   error and an unexpected body are on this side: they may be our fault.
//!
//! When it turns red, the first question: what does `curl -A 'x/1 ( y )'
//! 'https://musicbrainz.org/ws/2/recording?query=recording:%22Creep%22&fmt=json'`
//! say? If it works, the fault is ours.
//!
//! **Rate limit.** MusicBrainz gives an anonymous client one request per
//! second. The tests share a single [`MusicBrainzLookup`] so they go through
//! the same limiter in a parallel run too — separate instances would multiply
//! the quota by the number of tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, OnceLock};

use headshell_core::identity::musicbrainz::MusicBrainzLookup;
use headshell_core::identity::{MetadataLookup, ResolveMethod, Resolver};
use headshell_core::ids::{CanonicalKind, Isrc};
use headshell_core::model::TrackRef;

/// A real ISRC and the recording it is tied to (measured from MusicBrainz on
/// 2026-09-01).
///
/// A Turkish one was picked because it kills two birds with one stone: the
/// ISRC link **and** that the query's percent-encoding does not break on
/// multi-byte characters.
const KNOWN_ISRC: &str = "TR0441211603";
const KNOWN_ISRC_TITLE: &str = "Sil Baştan";

/// Can MusicBrainz be reached over TCP.
///
/// Only DNS + connect; it does not go into HTTP. The goal is answering "is
/// there a network", not measuring the service's health — measuring health is
/// the tests' job.
fn musicbrainz_reachable() -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = ("musicbrainz.org", 443).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| {
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5)).is_ok()
    })
}

/// The source all tests share — one limiter, one quota.
///
/// The `User-Agent` is specific to the tests: so MusicBrainz's log shows who
/// spent the quota, and a test run is not written under a real distribution's
/// identity.
fn shared_lookup() -> Option<Arc<dyn MetadataLookup>> {
    static LOOKUP: OnceLock<Option<Arc<dyn MetadataLookup>>> = OnceLock::new();
    LOOKUP
        .get_or_init(|| {
            let http = headshell_core::net::default_http_client().ok()?;
            let lookup = MusicBrainzLookup::new(http).with_user_agent(
                "headshell-tests/0.0.1 ( https://github.com/headshell/headshell )",
            );
            Some(Arc::new(lookup) as Arc<dyn MetadataLookup>)
        })
        .clone()
}

/// **Not reaching** the network is not a failure — turns the result into a
/// skip.
///
/// The TCP probe at the start answers "is there a network", but it cannot see
/// a timeout arriving in the middle of the test or an exhausted `503` retry.
/// MusicBrainz gives an anonymous client one request per second, and in a
/// busy run we share the limit not only with our own quota but with the
/// server's load at that moment. The K9 / D-043 line holds here too: **not
/// reaching it is skipped, reaching it and getting the unexpected fails.**
/// Without this distinction a green gate would depend on the network weather.
///
/// Only transport-layer errors are skipped. A parse error, `400`, `404` or an
/// unexpected body **fails** — they may be our fault.
fn skip_if_unreachable<T>(test: &str, result: headshell_core::Result<T>) -> Option<T> {
    use headshell_core::error::ErrorKind;
    match result {
        Ok(value) => Some(value),
        Err(err) => {
            let unreachable = matches!(
                err.kind(),
                ErrorKind::Network { .. } | ErrorKind::HttpStatus { status: 503, .. }
            );
            assert!(
                unreachable,
                "{test}: unexpected error (not a failure to reach it):\n{}",
                err.chain_text()
            );
            eprintln!(
                "{test}: could not reach MusicBrainz — skipped (network or quota):\n{}",
                err.chain_text()
            );
            None
        }
    }
}

/// Says whether the test can run; if it cannot, **writes the reason**.
fn lookup_or_skip(test: &str) -> Option<Arc<dyn MetadataLookup>> {
    let Some(lookup) = shared_lookup() else {
        eprintln!(
            "{test}: a build with the `http-client` feature off — skipped \
             (this is not a failure; `cargo test --workspace` turns it on)"
        );
        return None;
    };
    if !musicbrainz_reachable() {
        eprintln!("{test}: could not reach musicbrainz.org:443 — skipped (taken as no network)");
        return None;
    }
    Some(lookup)
}

/// Does the middle of the chain really work now (D-045's actual question).
#[tokio::test]
async fn the_chain_reaches_an_authority_instead_of_falling_to_a_local_key() {
    let Some(lookup) = lookup_or_skip("the_chain_reaches_an_authority") else {
        return;
    };
    let resolver = Resolver::new(lookup);
    let track = TrackRef::new("Şebnem Ferah", "Sil Baştan").with_duration_ms(Some(309_000));

    let Some(resolution) = skip_if_unreachable(
        "the_chain_reaches_an_authority",
        resolver.resolve(&track).await,
    ) else {
        return;
    };
    eprintln!(
        "resolved: {} ({}, confidence {:.2})",
        resolution.canonical_id, resolution.method, resolution.confidence
    );

    assert_ne!(
        resolution.method,
        ResolveMethod::LocalKey,
        "if we gave the same result as `OfflineLookup`, MusicBrainz never came into play"
    );
    assert_eq!(resolution.canonical_id.kind(), CanonicalKind::Mbid);
    let matched = resolution
        .matched
        .expect("an authoritative match must have a candidate");
    assert_eq!(matched.title, "Sil Baştan");
}

/// The first link: the ISRC must lead straight to the recording.
#[tokio::test]
async fn a_real_isrc_resolves_through_the_first_link() {
    let Some(lookup) = lookup_or_skip("a_real_isrc_resolves") else {
        return;
    };
    let isrc = Isrc::parse(KNOWN_ISRC).expect("the fixed ISRC is well-formed");
    let Some(found) = skip_if_unreachable(
        "a_real_isrc_resolves",
        lookup.recording_by_isrc(&isrc).await,
    ) else {
        return;
    };

    let candidate = found.unwrap_or_else(|| {
        panic!("{KNOWN_ISRC} was not found on MusicBrainz — the recording may have been merged")
    });
    eprintln!(
        "ISRC {KNOWN_ISRC} → {} ({})",
        candidate.mbid, candidate.title
    );
    assert_eq!(candidate.title, KNOWN_ISRC_TITLE);
    assert_eq!(candidate.artist, "Şebnem Ferah");
}

/// Lucene's special characters: unescaped, MusicBrainz returns `400`.
///
/// What is tested with the fake client is the query's *shape*; what is tested
/// here is that MusicBrainz accepts that shape. They are different claims.
#[tokio::test]
async fn a_slash_in_the_artist_name_does_not_break_the_query() {
    let Some(lookup) = lookup_or_skip("a_slash_in_the_artist_name") else {
        return;
    };
    // `400` **fails**: Lucene escaping is our job. Only transport-layer
    // errors are skipped.
    let Some(page) = skip_if_unreachable(
        "a_slash_in_the_artist_name",
        lookup
            .search_recordings("AC/DC", "Back in Black", None, 25)
            .await,
    ) else {
        return;
    };
    let found = page.candidates;

    assert!(
        !found.is_empty(),
        "candidates should have come back for AC/DC"
    );
    assert!(
        found.iter().any(|c| c.artist.contains("AC/DC")),
        "candidates returned: {:?}",
        found.iter().map(|c| &c.artist).collect::<Vec<_>>()
    );
}

/// What D-081 relies on: MusicBrainz scores every exact `Creep` alike — 100,
/// or 99 where a replica's statistics round it down; 149 of them, measured
/// 2026-10-02 — so the first 25 end inside the top tier, and the answer has
/// to say so. If MusicBrainz ever ranks them apart, this fails, and the
/// tier rule must be looked at again.
#[tokio::test]
async fn a_first_page_of_creep_ends_inside_its_top_group() {
    let Some(lookup) = lookup_or_skip("a_first_page_of_creep") else {
        return;
    };
    let Some(page) = skip_if_unreachable(
        "a_first_page_of_creep",
        lookup
            .search_recordings("Radiohead", "Creep", None, 25)
            .await,
    ) else {
        return;
    };
    assert_eq!(page.candidates.len(), 25);
    assert!(
        page.top_continues,
        "the first 25 of 149 alike scored recordings must say the top goes on"
    );
}

/// Does D-045's fix hold on the real catalog.
///
/// On MusicBrainz, a search for "Radiohead — Creep" returns more than 190
/// recordings, and most of the first page are **live recordings**; none of
/// them says "live" in its title, all of them say it in their disambiguation
/// note. The behaviour measured before the fix: the 1994 Astoria recording
/// counted as a "perfect hit" with 1.00 confidence.
///
/// A duration is given (the studio recording is 3:58) because since D-046
/// there are two pieces of evidence that make the distinction, and both must
/// be tested on the real catalog: the disambiguation note **and** closeness
/// in duration.
#[tokio::test]
async fn a_live_take_does_not_win_over_the_studio_take() {
    let Some(lookup) = lookup_or_skip("a_live_take_does_not_win") else {
        return;
    };
    let resolver = Resolver::new(lookup);
    let track = TrackRef::new("Radiohead", "Creep").with_duration_ms(Some(238_000));

    let Some(resolution) =
        skip_if_unreachable("a_live_take_does_not_win", resolver.resolve(&track).await)
    else {
        return;
    };
    // The invariant is this: **a live recording cannot win.** No winner at
    // all does not break this invariant either: two plain recordings of
    // exactly the same length would be a tie, and the right answer to a tie
    // is no authority. Since D-081 the search narrows to the length window
    // (33 recordings, read whole in one response), so on today's catalog the
    // 238 026 ms studio take wins on every replica —
    // `the_same_query_with_a_duration_always_yields_the_same_canonical_id`
    // holds that; this test holds the invariant.
    let Some(matched) = resolution.matched else {
        assert_eq!(
            resolution.method,
            ResolveMethod::LocalKey,
            "with no candidate the method must be the local key"
        );
        eprintln!(
            "chosen: none — {} candidates could not be told apart (unread equals: {}), \
             no authority claimed",
            resolution.tied_candidates, resolution.unread_equals
        );
        return;
    };
    eprintln!(
        "chosen: {} — {:?} ({}, confidence {:.2})",
        matched.mbid, matched.disambiguation, resolution.method, resolution.confidence
    );

    let note = matched.disambiguation.unwrap_or_default().to_lowercase();
    assert!(
        !note.contains("live"),
        "a live recording got ahead of the studio recording: {note:?}"
    );
}

/// An ambiguous query without a duration must get **no** authority at all
/// (D-046).
///
/// `Radiohead — Creep`, with no duration given, leaves 39 candidates with
/// the same score and the same rank on the real catalog. Any rule that picks
/// among them is arbitrary, and since MusicBrainz serves searches from
/// several index replicas it is **unstable across runs** too. The right
/// answer: do not make up an identity.
///
/// D-081: this test turned CI red when one replica's first page of 25 held a
/// single plain recording, and the resolver took it for a winner. The
/// resolver now reads the first hundred while the top tier goes on and the
/// outcome can still change, so every replica ends at the same tie.
///
/// This test's sibling, `a_live_take_does_not_win_over_the_studio_take`,
/// asks the same query with a duration, and there an authority is expected —
/// together the two show which evidence unlocks the rule.
#[tokio::test]
async fn an_ambiguous_query_without_duration_claims_no_authority() {
    let Some(lookup) = lookup_or_skip("an_ambiguous_query_without_duration") else {
        return;
    };
    let resolver = Resolver::new(lookup);
    let track = TrackRef::new("Radiohead", "Creep");

    let Some(resolution) = skip_if_unreachable(
        "an_ambiguous_query_without_duration",
        resolver.resolve(&track).await,
    ) else {
        return;
    };
    eprintln!(
        "query without a duration: {} ({}, tied {}, unread equals {})",
        resolution.canonical_id,
        resolution.method,
        resolution.tied_candidates,
        resolution.unread_equals
    );

    assert_eq!(
        resolution.method,
        ResolveMethod::LocalKey,
        "an authority was claimed without distinguishing evidence"
    );
    assert!(
        resolution.tied_candidates > 1,
        "the reason must be 'could not be told apart', not 'no candidate': {}",
        resolution.tied_candidates
    );
}

/// The same query must give the same canonical identity on every run.
///
/// This test **failed in D-045's live run and gave rise to the fix**:
/// MusicBrainz returns more than 190 `Creep` recordings, dozens of them
/// carry exactly the same artist and title, and when the duration is unknown
/// they all get the same score. The choice was left to the order the server
/// sent, and that order is not fixed — two runs gave two different MBIDs. At
/// the identity layer that meant the same track getting a different identity
/// tomorrow.
///
/// The two calls are made **separately** on purpose: [`Resolver`] uses a
/// cache within a single call, whereas what is tested is two separate network
/// responses arriving at the same result.
#[tokio::test]
async fn the_same_query_always_yields_the_same_canonical_id() {
    let Some(lookup) = lookup_or_skip("the_same_query_always_yields_the_same_id") else {
        return;
    };
    let resolver = Resolver::new(lookup);
    let track = TrackRef::new("Radiohead", "Creep");

    let name = "the_same_query_always_yields_the_same_id";
    let (Some(first), Some(second)) = (
        skip_if_unreachable(name, resolver.resolve(&track).await),
        skip_if_unreachable(name, resolver.resolve(&track).await),
    ) else {
        return;
    };

    assert_eq!(
        first.canonical_id, second.canonical_id,
        "the same query gave two different identities: the choice among equally \
         scored candidates must have depended on the server's order"
    );
}

/// The same, with a duration (D-081): reading the search's later pages made
/// this flip between an MBID and the local key — the pages came from
/// different replicas, and only some reads were whole. The length window
/// reads one response, whole, every time.
#[tokio::test]
async fn the_same_query_with_a_duration_always_yields_the_same_canonical_id() {
    let name = "the_same_query_with_a_duration";
    let Some(lookup) = lookup_or_skip(name) else {
        return;
    };
    let resolver = Resolver::new(lookup);
    let track = TrackRef::new("Radiohead", "Creep").with_duration_ms(Some(238_000));

    let (Some(first), Some(second)) = (
        skip_if_unreachable(name, resolver.resolve(&track).await),
        skip_if_unreachable(name, resolver.resolve(&track).await),
    ) else {
        return;
    };
    eprintln!(
        "with a duration: {} ({}), then {} ({})",
        first.canonical_id, first.method, second.canonical_id, second.method
    );
    assert_eq!(first.canonical_id, second.canonical_id);
    assert!(
        !first.unread_equals,
        "the length window must hold Creep's tier whole"
    );
}

/// No authority is made up: a track without a counterpart must stay at
/// `LocalKey`.
#[tokio::test]
async fn nonsense_never_invents_an_authority() {
    let Some(lookup) = lookup_or_skip("nonsense_never_invents_an_authority") else {
        return;
    };
    let resolver = Resolver::new(lookup);
    let track = TrackRef::new("Zzqx Vlorbnak Ensemble", "Hgggrmphl Suite No. 41");

    let Some(resolution) = skip_if_unreachable(
        "nonsense_never_invents_an_authority",
        resolver.resolve(&track).await,
    ) else {
        return;
    };
    assert_eq!(
        resolution.method,
        ResolveMethod::LocalKey,
        "a track without a match must not be tied to an authority: {} (confidence {:.2})",
        resolution.canonical_id,
        resolution.confidence
    );
}
