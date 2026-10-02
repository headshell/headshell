//! The same search twice: does MusicBrainz return the same set of
//! candidates?
//!
//! D-046's live run failed the `the_same_query_always_yields_the_same_canonical_id`
//! test. This probe separates the cause: is the scoring unstable, or **the
//! incoming candidate set**? They are entirely different flaws and cannot be
//! told apart without measuring.
//!
//! Measured (2026-09-02): the same query twice in a row returns 25 candidates,
//! and in some runs **the number of shared candidates is zero** — MusicBrainz
//! serves search from several index replicas. Within one replica the order is
//! fixed; between replicas the top 25 are completely different.
//!
//! ```bash
//! cargo run -p headshell-core --features http-client --example mb_stability_probe -- "Artist" "Title"
//! ```

use headshell_core::identity::{MetadataLookup as _, Resolver};
use headshell_core::model::TrackRef;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let artist = args.next().unwrap_or_else(|| "Radiohead".to_owned());
    let title = args.next().unwrap_or_else(|| "Creep".to_owned());
    let duration_ms: Option<u64> = args.next().and_then(|raw| raw.parse().ok());

    let Ok(http) = headshell_core::net::default_http_client() else {
        println!("a build with the `http-client` feature off — the probe cannot run.");
        return;
    };
    let lookup = std::sync::Arc::new(
        headshell_core::identity::musicbrainz::MusicBrainzLookup::new(http)
            .with_user_agent("headshell-tests/0.0.1 ( https://github.com/headshell/headshell )"),
    );
    let resolver = Resolver::new(lookup.clone());
    let track = TrackRef::new(&artist, &title).with_duration_ms(duration_ms);
    println!("query: {artist} — {title} (duration {duration_ms:?})");

    let mut sets: Vec<Vec<String>> = Vec::new();
    for round in 1..=2 {
        match lookup.search_recordings(&artist, &title, None, 25).await {
            Ok(page) => {
                let found = page.candidates;
                println!(
                    "run {round}: {} candidates on the first page, top continues: {}",
                    found.len(),
                    page.top_continues
                );
                // The score and duration of the best five candidates: is the tie real, or
                // does the scoring fail to tell apart what it could?
                let mut scored: Vec<(f64, String, Option<u64>)> = found
                    .iter()
                    .map(|c| {
                        let score = headshell_core::identity::fuzzy::similarity(
                            &artist,
                            &title,
                            duration_ms,
                            &c.artist,
                            &c.title,
                            c.duration_ms,
                            c.disambiguation.as_deref(),
                        );
                        (score, c.mbid.as_str().to_owned(), c.duration_ms)
                    })
                    .collect();
                scored.sort_by(|a, b| b.0.total_cmp(&a.0));
                for (score, mbid, dur) in scored.iter().take(5) {
                    println!("    {score:.4}  {mbid}  duration={dur:?}");
                }
                sets.push(
                    found
                        .iter()
                        .map(|candidate| candidate.mbid.as_str().to_owned())
                        .collect(),
                );
            }
            Err(err) => {
                println!("run {round} failed:\n{}", err.chain_text());
                return;
            }
        }

        // What the chain does with this set: the identity, the method, how many
        // candidates are tied. The real question is "does it give the same
        // answer", and the tie count says at which threshold the refusal rule
        // will hold.
        match resolver.resolve(&track).await {
            Ok(res) => println!(
                "  → {} ({}, confidence {:.3}, tied {}, unread equals {})",
                res.canonical_id,
                res.method,
                res.confidence,
                res.tied_candidates,
                res.unread_equals
            ),
            Err(err) => println!("  → resolution error: {}", err.chain_text()),
        }
    }

    let (first, second) = (&sets[0], &sets[1]);
    let overlap = first.iter().filter(|id| second.contains(id)).count();
    println!("same order?  : {}", first == second);
    println!("shared       : {overlap}");
    println!("1. first three: {:?}", &first[..first.len().min(3)]);
    println!("2. first three: {:?}", &second[..second.len().min(3)]);
}
