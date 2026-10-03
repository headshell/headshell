//! Recommendation tests (D-087): the taste, the sittings, the scoring, and
//! a visit end to end over fake sources.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::taste::{self, Reason, Signals, Taste};
use super::*;
use crate::identity::normalize::track_key;
use crate::ids::ProviderTrackId;
use crate::model::{ExportKind, Listen, ListenSource, PlayRule, TrackRef};
use crate::provider::{
    AudioSource, Capabilities, ProviderFuture, ProviderHealth, ProviderInfo, ProviderTrack,
};

const DAY: i64 = 86_400_000;
/// 2026-10-01T00:00:00Z.
const NOW: i64 = 1_790_812_800_000;

fn listen(artist: &str, title: &str, at_ms: i64, ms: u64) -> Listen {
    Listen {
        track: TrackRef::new(artist, title),
        played_at: jiff::Timestamp::from_millisecond(at_ms).unwrap(),
        ms_played: ms,
        source: ListenSource::Import {
            export: ExportKind::SpotifyExtended,
        },
        canonical_id: None,
    }
}

/// A sitting: the songs one after another, four minutes apart.
fn sitting(start_ms: i64, songs: &[(&str, &str)]) -> Vec<Listen> {
    songs
        .iter()
        .enumerate()
        .map(|(i, (artist, title))| listen(artist, title, start_ms + i as i64 * 240_000, 200_000))
        .collect()
}

fn read(listens: &[Listen], since: Option<i64>, genres: &HashMap<String, Vec<String>>) -> Taste {
    taste::read(listens, since, NOW, PlayRule::default(), genres)
}

#[test]
fn a_play_fades_with_age_and_a_skip_takes_back_half() {
    let listens = vec![
        listen("A", "Today", NOW, 200_000),
        listen("A", "Old", NOW - 90 * DAY, 200_000),
        listen("A", "Skipped", NOW, 200_000),
        listen("A", "Skipped", NOW, 5_000),
    ];
    let taste = read(&listens, None, &HashMap::new());
    let score = |title: &str| taste.songs[&track_key("A", title)].score;
    assert!((score("Today") - 1.0).abs() < 1e-9);
    assert!((score("Old") - 0.5).abs() < 1e-6, "one half-life later");
    assert!(
        (score("Skipped") - 0.5).abs() < 1e-9,
        "a play minus half a play"
    );
}

#[test]
fn a_reset_reads_the_taste_from_after_it_without_losing_a_listen() {
    let listens = vec![
        listen("A", "Before", NOW - 10 * DAY, 200_000),
        listen("B", "After", NOW - DAY, 200_000),
    ];
    let taste = read(&listens, Some(NOW - 5 * DAY), &HashMap::new());
    assert_eq!(taste.listens_considered, 1);
    assert_eq!(taste.listens_before_reset, 1);
    assert!(taste.songs.contains_key(&track_key("B", "After")));
    assert!(!taste.songs.contains_key(&track_key("A", "Before")));
}

#[test]
fn the_genre_profile_follows_the_loved_songs() {
    let listens = vec![
        listen("A", "One", NOW, 200_000),
        listen("A", "One", NOW, 200_000),
        listen("B", "Two", NOW, 200_000),
    ];
    let genres = HashMap::from([
        (track_key("A", "One"), vec!["shoegaze".to_owned()]),
        (track_key("B", "Two"), vec!["techno".to_owned()]),
    ]);
    let taste = read(&listens, None, &genres);
    assert_eq!(taste.top_genres(2), vec!["shoegaze", "techno"]);
    assert!((taste.genres["shoegaze"] - 1.0).abs() < 1e-9);
    assert!((taste.genres["techno"] - 0.5).abs() < 1e-9);
    assert_eq!(taste.songs_with_genre, 2);
}

#[test]
fn seeds_are_recent_loves_by_different_artists() {
    let mut listens = Vec::new();
    // An old favourite, played a lot but a year ago.
    for _ in 0..10 {
        listens.push(listen("Old", "Favourite", NOW - 365 * DAY, 200_000));
    }
    for _ in 0..3 {
        listens.push(listen("Band", "First", NOW - DAY, 200_000));
    }
    for _ in 0..2 {
        listens.push(listen("Band", "Second", NOW - DAY, 200_000));
    }
    listens.push(listen("Other", "Song", NOW - DAY, 200_000));
    listens.push(listen("Other", "Song", NOW - DAY, 200_000));
    // Played once: a try, not a love.
    listens.push(listen("Once", "Only", NOW - DAY, 200_000));
    let taste = read(&listens, None, &HashMap::new());

    let seeds = taste.seeds(NOW, 3);
    assert_eq!(
        seeds,
        vec![
            track_key("Band", "First"),
            track_key("Other", "Song"),
            // The month had two artists only: the old favourite fills in.
            track_key("Old", "Favourite"),
        ]
    );
}

#[test]
fn songs_in_one_sitting_are_close_and_a_long_pause_parts_them() {
    let mut listens = sitting(
        NOW - DAY,
        &[
            ("S", "Seed"),
            ("N", "Next"),
            ("F", "Far"),
            ("X", "Further"),
            ("Y", "Beyond"),
        ],
    );
    // An hour later: a new sitting.
    listens.extend(sitting(NOW - DAY + 3_600_000 * 2, &[("L", "Later")]));
    let rows = taste::listened_together(
        &listens,
        None,
        NOW,
        PlayRule::default(),
        &[track_key("S", "Seed")],
    );
    let row = &rows[&track_key("S", "Seed")];
    assert!(
        (row[&track_key("N", "Next")] - 1.0).abs() < 1e-9,
        "the closest is 1"
    );
    assert!((row[&track_key("F", "Far")] - 0.5).abs() < 1e-9);
    assert!((row[&track_key("X", "Further")] - 1.0 / 3.0).abs() < 1e-9);
    assert!(
        !row.contains_key(&track_key("Y", "Beyond")),
        "past the window"
    );
    assert!(
        !row.contains_key(&track_key("L", "Later")),
        "another sitting"
    );
}

#[test]
fn a_skipped_song_was_not_listened_together() {
    let listens = vec![
        listen("S", "Seed", NOW - DAY, 200_000),
        listen("K", "Skipped", NOW - DAY + 60_000, 3_000),
    ];
    let rows = taste::listened_together(
        &listens,
        None,
        NOW,
        PlayRule::default(),
        &[track_key("S", "Seed")],
    );
    assert!(
        rows.get(&track_key("S", "Seed"))
            .is_none_or(HashMap::is_empty)
    );
}

#[test]
fn the_score_names_its_strongest_reason_first() {
    let taste = Taste {
        genres: HashMap::from([("jazz".to_owned(), 1.0)]),
        ..Taste::default()
    };
    let signals = Signals {
        together: 0.2,
        source_rank: Some(0),
        genres: vec!["jazz".to_owned()],
        artist: "Nobody".to_owned(),
        heard: false,
    };
    let (points, reasons) = taste::score(&taste, &signals, taste::AROUND_A_SONG);
    assert!(points > 0 && points <= 1000);
    assert_eq!(
        reasons,
        vec![
            Reason::SimilarAtSource,
            Reason::SharedGenre,
            Reason::ListenedTogether,
            Reason::NotHeardYet,
        ]
    );
}

/// A source with fixed answers that remembers what it was asked.
struct Fake {
    id: &'static str,
    capabilities: Capabilities,
    plugin: bool,
    catalog: Vec<ProviderTrack>,
    related: Vec<ProviderTrack>,
    fails: bool,
    asked: Mutex<Vec<String>>,
}

impl Fake {
    fn new(id: &'static str, capabilities: Capabilities) -> Self {
        Self {
            id,
            capabilities,
            plugin: false,
            catalog: Vec::new(),
            related: Vec::new(),
            fails: false,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

fn track(provider: &str, id: &str, artist: &str, title: &str, genres: &[&str]) -> ProviderTrack {
    ProviderTrack {
        id: ProviderTrackId::new(ProviderId::new(provider), id),
        track: TrackRef::new(artist, title),
        genres: genres.iter().map(|g| (*g).to_owned()).collect(),
    }
}

impl Provider for Fake {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ProviderId::new(self.id),
            display_name: format!("{} source", self.id),
            capabilities: self.capabilities,
        }
    }

    fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
        Box::pin(std::future::ready(Ok(ProviderHealth {
            id: ProviderId::new(self.id),
            reachable: true,
            track_count: None,
            detail: None,
        })))
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
        self.asked.lock().unwrap().push(format!("search {query}"));
        if self.fails {
            return Box::pin(std::future::ready(Err(crate::Error::new(
                crate::diag::Stage::NetworkRequest,
                crate::ErrorKind::InvalidInput {
                    detail: "unreachable".to_owned(),
                },
            ))));
        }
        let words: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let hits = self
            .catalog
            .iter()
            .filter(|t| {
                let text = format!("{} {}", t.track.artist, t.track.title).to_lowercase();
                words.iter().all(|w| text.contains(w))
            })
            .take(limit)
            .cloned()
            .collect();
        Box::pin(std::future::ready(Ok(hits)))
    }

    fn resolve_source<'a>(
        &'a self,
        _id: &'a ProviderTrackId,
    ) -> ProviderFuture<'a, Option<AudioSource>> {
        Box::pin(std::future::ready(Ok(None)))
    }

    fn related<'a>(
        &'a self,
        id: &'a ProviderTrackId,
        limit: usize,
    ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
        self.asked
            .lock()
            .unwrap()
            .push(format!("related {}", id.id));
        let tracks = self.related.iter().take(limit).cloned().collect();
        Box::pin(std::future::ready(Ok(tracks)))
    }

    fn genre_tracks<'a>(
        &'a self,
        genre: &'a str,
        limit: usize,
    ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
        self.asked.lock().unwrap().push(format!("genre {genre}"));
        let tracks = self
            .catalog
            .iter()
            .filter(|t| {
                crate::recommend::genre::normalize_genres(&t.genres)
                    .iter()
                    .any(|g| g == genre)
            })
            .take(limit)
            .cloned()
            .collect();
        Box::pin(std::future::ready(Ok(tracks)))
    }

    fn is_plugin(&self) -> bool {
        self.plugin
    }
}

/// A plan over `listens`, as the session would build it.
fn plan_for(
    provider: Arc<dyn Provider>,
    listens: &[Listen],
    genres: HashMap<String, Vec<String>>,
    pool: Option<Vec<PoolRow>>,
    dismissed: HashSet<String>,
) -> RecommendPlan {
    let rule = PlayRule::default();
    let taste = taste::read(listens, None, NOW, rule, &genres);
    let seed_keys = taste.seeds(NOW, SEEDS);
    let mut rows = taste::listened_together(listens, None, NOW, rule, &seed_keys);
    let seeds = seed_keys
        .iter()
        .map(|key| {
            let song = &taste.songs[key];
            Seed {
                key: key.clone(),
                artist: song.artist.clone(),
                title: song.title.clone(),
                neighbours: rows.remove(key).unwrap_or_default(),
            }
        })
        .collect();
    let top_genres = taste.top_genres(GENRES);
    RecommendPlan {
        provider,
        since_ms: None,
        taste,
        seeds,
        top_genres,
        pool,
        known_genres: genres,
        dismissed,
        musicbrainz: None,
    }
}

fn pool_row(provider: &str, artist: &str, title: &str, genres: &[&str]) -> PoolRow {
    PoolRow {
        key: track_key(artist, title),
        item: QueueItem {
            id: ProviderTrackId::new(ProviderId::new(provider), format!("/music/{title}.flac")),
            track: TrackRef::new(artist, title),
        },
        genres: genres.iter().map(|g| (*g).to_owned()).collect(),
    }
}

fn titles(section: &RecommendSection) -> Vec<&str> {
    section
        .items
        .iter()
        .map(|item| item.item.track.title.as_str())
        .collect()
}

#[tokio::test]
async fn the_local_files_are_recommended_from_the_catalog_without_asking_anyone() {
    // Two months ago, the seed was played with its neighbours; lately alone.
    let mut listens = sitting(
        NOW - 60 * DAY,
        &[
            ("Seed Band", "Loved"),
            ("Near", "Together"),
            ("Nearish", "Also"),
        ],
    );
    for i in 0..4 {
        listens.push(listen(
            "Seed Band",
            "Loved",
            NOW - 2 * DAY - i * DAY,
            200_000,
        ));
    }
    let genres = HashMap::from([(
        track_key("Seed Band", "Loved"),
        vec!["dream pop".to_owned()],
    )]);
    let pool = vec![
        pool_row("local", "Seed Band", "Loved", &["dream pop"]),
        pool_row("local", "Near", "Together", &[]),
        pool_row("local", "Nearish", "Also", &[]),
        pool_row("local", "Stranger", "In Genre", &["dream pop"]),
        pool_row("local", "Seed Band", "Unheard B-Side", &[]),
        pool_row("local", "Unrelated", "Noise", &["noise"]),
    ];
    let source = Arc::new(Fake::new(
        "local",
        Capabilities::SEARCH | Capabilities::STREAM,
    ));
    let plan = plan_for(
        Arc::clone(&source) as Arc<dyn Provider>,
        &listens,
        genres,
        Some(pool),
        HashSet::new(),
    );
    let report = plan.finish().await;

    assert!(
        source.asked().is_empty(),
        "a catalog on the machine is read, not asked"
    );
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let kinds: Vec<&SectionKind> = report.sections.iter().map(|s| &s.kind).collect();
    assert_eq!(
        kinds,
        vec![
            &SectionKind::BecauseYouListened {
                artist: "Seed Band".to_owned(),
                title: "Loved".to_owned()
            },
            &SectionKind::Genre {
                genre: "dream pop".to_owned()
            },
            &SectionKind::FromArtistsYouLove,
        ]
    );
    assert_eq!(titles(&report.sections[0]), vec!["Together", "Also"]);
    assert_eq!(
        report.sections[0].items[0].reasons[0],
        Reason::ListenedTogether
    );
    // The seed itself is never offered; a song is in one section only.
    assert_eq!(titles(&report.sections[1]), vec!["In Genre"]);
    assert_eq!(titles(&report.sections[2]), vec!["Unheard B-Side"]);
    assert_eq!(report.summary.seeds, 1);
    assert_eq!(report.summary.found_listened_together, 2);
}

#[tokio::test]
async fn a_server_is_asked_for_similar_songs_by_its_own_id_and_its_genres_are_learned() {
    let listens: Vec<Listen> = (0..3)
        .map(|i| listen("Seed Band", "Loved", NOW - DAY - i * DAY, 200_000))
        .collect();
    let mut source = Fake::new(
        "navidrome",
        Capabilities::SEARCH | Capabilities::STREAM | Capabilities::RELATED,
    );
    source.catalog = vec![track("navidrome", "s1", "Seed Band", "Loved", &[])];
    source.related = vec![
        track("navidrome", "s1", "Seed Band", "Loved", &[]),
        track("navidrome", "r1", "Kin", "First Like It", &["Shoegaze"]),
        track("navidrome", "r2", "Kin", "Second Like It", &[]),
        track("navidrome", "r3", "Marked", "Not For Me", &[]),
    ];
    let source = Arc::new(source);
    let plan = plan_for(
        Arc::clone(&source) as Arc<dyn Provider>,
        &listens,
        HashMap::new(),
        None,
        HashSet::from([track_key("Marked", "Not For Me")]),
    );
    let report = plan.finish().await;

    assert_eq!(source.asked(), vec!["search Seed Band Loved", "related s1"]);
    assert_eq!(report.sections.len(), 1);
    let section = &report.sections[0];
    assert_eq!(titles(section), vec!["First Like It", "Second Like It"]);
    assert_eq!(section.items[0].reasons[0], Reason::SimilarAtSource);
    assert_eq!(section.items[0].genres, vec!["shoegaze"]);
    assert_eq!(report.summary.left_out_dismissed, 1);
    assert_eq!(
        report.learned,
        vec![LearnedGenres {
            artist: "Kin".to_owned(),
            title: "First Like It".to_owned(),
            genres: vec!["Shoegaze".to_owned()],
            source: GenreSource::Server,
        }]
    );
}

#[tokio::test]
async fn a_song_played_this_week_is_not_offered_again() {
    let mut listens: Vec<Listen> = (0..3)
        .map(|i| listen("Seed Band", "Loved", NOW - DAY - i * DAY, 200_000))
        .collect();
    listens.push(listen("Kin", "Heard Yesterday", NOW - DAY, 200_000));
    let mut source = Fake::new(
        "navidrome",
        Capabilities::SEARCH | Capabilities::STREAM | Capabilities::RELATED,
    );
    source.catalog = vec![track("navidrome", "s1", "Seed Band", "Loved", &[])];
    source.related = vec![
        track("navidrome", "r1", "Kin", "Heard Yesterday", &[]),
        track("navidrome", "r2", "Kin", "Fresh", &[]),
    ];
    let plan = plan_for(
        Arc::new(source),
        &listens,
        HashMap::new(),
        None,
        HashSet::new(),
    );
    let report = plan.finish().await;
    assert_eq!(titles(&report.sections[0]), vec!["Fresh"]);
    assert_eq!(report.summary.left_out_recent, 1);
}

#[tokio::test]
async fn a_source_that_fails_is_reported_once_and_not_asked_again() {
    let mut listens = Vec::new();
    for (artist, title) in [("A", "One"), ("B", "Two"), ("C", "Three")] {
        listens.push(listen(artist, title, NOW - DAY, 200_000));
        listens.push(listen(artist, title, NOW - DAY, 200_000));
    }
    let mut source = Fake::new(
        "away",
        Capabilities::SEARCH | Capabilities::STREAM | Capabilities::RELATED | Capabilities::GENRE,
    );
    source.fails = true;
    let source = Arc::new(source);
    let plan = plan_for(
        Arc::clone(&source) as Arc<dyn Provider>,
        &listens,
        HashMap::new(),
        None,
        HashSet::new(),
    );
    let report = plan.finish().await;
    assert_eq!(source.asked().len(), 1, "{:?}", source.asked());
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].stage, "NETWORK_REQUEST");
    assert!(report.is_empty());
}

#[tokio::test]
async fn a_plugin_has_no_genre_section_and_its_genres_are_learned_as_a_plugins() {
    let listens: Vec<Listen> = (0..3)
        .map(|i| listen("Seed Band", "Loved", NOW - DAY - i * DAY, 200_000))
        .collect();
    let genres = HashMap::from([(track_key("Seed Band", "Loved"), vec!["house".to_owned()])]);
    let mut source = Fake::new(
        "ytmusic",
        Capabilities::SEARCH | Capabilities::STREAM | Capabilities::RELATED,
    );
    source.plugin = true;
    source.catalog = vec![track("ytmusic", "v1", "Seed Band", "Loved", &[])];
    source.related = vec![track("ytmusic", "v2", "Kin", "Radio Pick", &["House"])];
    let source = Arc::new(source);
    let plan = plan_for(
        Arc::clone(&source) as Arc<dyn Provider>,
        &listens,
        genres,
        None,
        HashSet::new(),
    );
    let report = plan.finish().await;
    assert!(
        !source
            .asked()
            .iter()
            .any(|asked| asked.starts_with("genre")),
        "a plugin is never asked by genre: {:?}",
        source.asked()
    );
    assert_eq!(report.sections.len(), 1);
    assert_eq!(report.learned[0].source, GenreSource::Plugin);
}

#[tokio::test]
async fn no_history_is_an_empty_page_not_an_error() {
    let source = Arc::new(Fake::new(
        "local",
        Capabilities::SEARCH | Capabilities::STREAM,
    ));
    let plan = plan_for(
        source,
        &[],
        HashMap::new(),
        Some(Vec::new()),
        HashSet::new(),
    );
    let report = plan.finish().await;
    assert!(report.is_empty());
    assert!(report.failures.is_empty());
    assert_eq!(report.summary.listens_considered, 0);
}

fn scored(artist: &str, title: &str, score: u32) -> RecommendItem {
    RecommendItem {
        item: QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), format!("/music/{title}.flac")),
            track: TrackRef::new(artist, title),
        },
        genres: Vec::new(),
        reasons: Vec::new(),
        score,
    }
}

fn section_of(genre: &str, items: Vec<RecommendItem>) -> RecommendSection {
    RecommendSection {
        kind: SectionKind::Genre {
            genre: genre.to_owned(),
        },
        items,
    }
}

/// The shelf: the best of every section, moved — not copied — at most two
/// by one artist, and every section keeps two.
#[test]
fn the_shelf_takes_the_best_and_leaves_every_section_two() {
    let mut sections = vec![
        section_of(
            "a",
            vec![
                scored("One", "a1", 990),
                scored("One", "a2", 980),
                scored("One", "a3", 970),
                scored("Two", "a4", 500),
                scored("Two", "a5", 400),
            ],
        ),
        section_of(
            "b",
            vec![
                scored("Three", "b1", 900),
                scored("Four", "b2", 800),
                scored("Five", "b3", 700),
                scored("Six", "b4", 100),
            ],
        ),
    ];
    let picks = top_picks(&mut sections);
    let titles: Vec<&str> = picks.iter().map(|p| p.item.track.title.as_str()).collect();
    // a3 is a third by "One"; then section a is down to two, b too.
    assert_eq!(titles, vec!["a1", "a2", "b1", "b2", "a4"]);
    let left: Vec<Vec<&str>> = sections
        .iter()
        .map(|s| {
            s.items
                .iter()
                .map(|i| i.item.track.title.as_str())
                .collect()
        })
        .collect();
    assert_eq!(left, vec![vec!["a3", "a5"], vec!["b3", "b4"]]);
}

#[test]
fn too_few_for_a_shelf_moves_nothing() {
    let mut sections = vec![section_of(
        "a",
        vec![
            scored("One", "a1", 900),
            scored("Two", "a2", 800),
            scored("Three", "a3", 700),
            scored("Four", "a4", 600),
        ],
    )];
    assert!(top_picks(&mut sections).is_empty());
    assert_eq!(sections[0].items.len(), 4);
}
