//! Recommendations: songs picked for the listener, from one source at a
//! time (D-087).
//!
//! **Everything that knows the listener stays here.** The taste is read
//! from the history on the machine ([`taste`]); the genres and the "not for
//! me" marks live in the library; nothing of it is sent anywhere. A source
//! is asked only what it would be asked anyway — a search for a song by
//! name, the songs *it* says are like one of *its* tracks (by that track's
//! id), the songs it holds in a genre. MusicBrainz is asked for a song's
//! genres only when the user is online.
//!
//! Three kinds of section:
//!
//! - **because you listened to …** — around a song loved lately: what was
//!   played in the same sittings, and what the source names as like it;
//! - **more <genre>** — the genres played most;
//! - **from artists you love** — their songs not heard yet, where the
//!   source's whole catalog is on the machine (the local files).
//!
//! The same split as the suggestions: [`crate::session::Session::recommend_plan`]
//! reads what is on the machine; [`RecommendPlan::finish`] asks the source
//! and ranks, needing no session — a shell runs it off the thread that
//! plays. What it learned on the way (the genres a server or MusicBrainz
//! named) is written by
//! [`crate::session::Session::remember_recommendation_genres`].

pub(crate) mod genre;
pub(crate) mod taste;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::identity::musicbrainz::MusicBrainzLookup;
use crate::identity::normalize::{normalize_artist, track_key};
use crate::ids::{Mbid, ProviderId};
use crate::library::GenreSource;
use crate::playback::QueueItem;
use crate::provider::{Capabilities, Provider, ProviderTrack};
use crate::suggest::SuggestFailure;

pub use taste::Reason;
use taste::{Signals, Taste, Weights};

/// How many songs the top shelf shows at most — the page's square tiles.
pub const TOP_PICKS: usize = 12;
/// Fewer candidates than this, and there is no shelf: three tiles in a row
/// built for twelve look like a mistake.
const TOP_PICKS_MIN: usize = 4;
/// A section keeps at least this many songs when the shelf takes from it.
const SECTION_KEEPS: usize = 2;
/// How many tiles one artist may have.
const PICKS_PER_ARTIST: usize = 2;

/// How many loved songs get a "because you listened to" section.
pub const SEEDS: usize = 3;
/// How many songs a section offers at most.
pub const PER_SECTION: usize = 10;
/// How many genre sections there are at most.
pub const GENRES: usize = 2;
/// How many similar songs a source is asked for, per seed.
const RELATED_ASK: usize = 25;
/// How many songs in a genre a server is asked for.
const GENRE_ASK: usize = 40;
/// How many songs listened together with a seed are looked up by name at
/// a source whose catalog is not on the machine.
const RESOLVE_ASK: usize = 6;
/// How many search rows a lookup by name reads.
const SEARCH_ROWS: usize = 5;
/// How many songs' genres MusicBrainz is asked for at most in one visit.
/// It answers one request a second; the page must not wait long.
pub(crate) const MUSICBRAINZ_ASK: usize = 5;
/// How many artists "from artists you love" draws on.
const LOVED_ARTISTS: usize = 10;

/// What a section is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SectionKind {
    /// Around a song the listener loves lately.
    BecauseYouListened { artist: String, title: String },
    /// A genre the listener plays most.
    Genre { genre: String },
    /// Songs by the artists played most that are not in the history yet.
    FromArtistsYouLove,
}

/// One song offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecommendItem {
    /// Playable as it is: a track of the source the page is for.
    pub item: QueueItem,
    /// Its genres, folded, as far as they are known.
    pub genres: Vec<String>,
    /// Why it is offered, the strongest reason first.
    pub reasons: Vec<Reason>,
    /// 0–1000; a section is in this order.
    pub score: u32,
}

/// One section of the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecommendSection {
    #[serde(flatten)]
    pub kind: SectionKind,
    pub items: Vec<RecommendItem>,
}

/// What went into the page — partial success is counted, not hidden (K9).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecommendSummary {
    /// Listens the taste was read from.
    pub listens_considered: usize,
    /// Listens left out because they came before the last reset.
    pub listens_before_reset: usize,
    /// Distinct songs in the taste.
    pub songs_in_taste: usize,
    /// Of those, how many have a known genre.
    pub songs_with_genre: usize,
    /// Loved songs the page started from.
    pub seeds: usize,
    /// The genres played most, most first.
    pub top_genres: Vec<String>,
    /// Songs found because they were listened together with a seed.
    pub found_listened_together: usize,
    /// Songs the source named as like a seed.
    pub found_at_source: usize,
    /// Songs found in a genre.
    pub found_by_genre: usize,
    /// Songs found by the loved artists.
    pub found_by_artist: usize,
    /// Left out: marked "not for me".
    pub left_out_dismissed: usize,
    /// Left out: played in the last week.
    pub left_out_recent: usize,
    /// Songs whose genres MusicBrainz named this time.
    pub genres_learned_online: usize,
}

/// The songs recommended from one source.
///
/// Not a recorded run: the desktop asks for it every time the page opens,
/// and `headshell diag`'s last run must stay the user's last command. What
/// could not be asked is in `failures`, with its stage (K9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecommendReport {
    pub provider: ProviderId,
    /// The name the user knows it by.
    pub display_name: String,
    /// The last reset (ms): the taste is read from the listens after it.
    /// `None`: never reset — the whole history counts.
    pub since_ms: Option<i64>,
    /// The best of every section, best first — the shelf of square tiles at
    /// the top of the page. A song here is not in a section too. Empty when
    /// there are too few to fill a shelf.
    pub top_picks: Vec<RecommendItem>,
    pub sections: Vec<RecommendSection>,
    pub failures: Vec<SuggestFailure>,
    pub summary: RecommendSummary,
    /// The genres learned on the way — the source's, MusicBrainz's — for
    /// [`crate::session::Session::remember_recommendation_genres`] to
    /// write. Not part of the answer a shell shows.
    #[serde(skip)]
    pub(crate) learned: Vec<LearnedGenres>,
}

impl RecommendReport {
    /// Nothing to offer.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.top_picks.is_empty() && self.sections.iter().all(|section| section.items.is_empty())
    }
}

/// What `headshell recommend dismiss` answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DismissReport {
    pub artist: String,
    pub title: String,
    /// `false`: it was marked already.
    pub newly_marked: bool,
    pub diag: crate::diag::DiagReport,
}

/// What `headshell recommend reset` answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecommendResetReport {
    /// The taste starts over from here (ms).
    pub since_ms: i64,
    pub forgot: crate::library::RecommendForgetSummary,
    pub diag: crate::diag::DiagReport,
}

/// A song's genres as one source named them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LearnedGenres {
    pub(crate) artist: String,
    pub(crate) title: String,
    pub(crate) genres: Vec<String>,
    pub(crate) source: GenreSource,
}

/// One loved song to start from, with what was played around it.
pub(crate) struct Seed {
    pub(crate) key: String,
    pub(crate) artist: String,
    pub(crate) title: String,
    /// Listened together with it, by key, 0–1.
    pub(crate) neighbours: HashMap<String, f64>,
}

/// A track of the source with its genres, from the catalog on the machine.
pub(crate) struct PoolRow {
    pub(crate) key: String,
    pub(crate) item: QueueItem,
    pub(crate) genres: Vec<String>,
}

/// MusicBrainz, when online: the client (or why there is none) and the
/// songs to ask about.
pub(crate) struct MusicBrainzAsk {
    /// The client, or — in a build without one — the failure to report.
    pub(crate) lookup: std::result::Result<Arc<MusicBrainzLookup>, SuggestFailure>,
    pub(crate) songs: Vec<(String, String, Mbid)>,
}

/// What [`crate::session::Session::recommend_plan`] read; [`Self::finish`]
/// asks the source and ranks.
pub struct RecommendPlan {
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) since_ms: Option<i64>,
    pub(crate) taste: Taste,
    pub(crate) seeds: Vec<Seed>,
    pub(crate) top_genres: Vec<String>,
    /// The source's whole catalog, when it is on the machine; `None`: the
    /// source is asked.
    pub(crate) pool: Option<Vec<PoolRow>>,
    /// Every known genre, by key.
    pub(crate) known_genres: HashMap<String, Vec<String>>,
    pub(crate) dismissed: HashSet<String>,
    pub(crate) musicbrainz: Option<MusicBrainzAsk>,
}

/// The candidates of one section, by key, each with its track and signals.
type Candidates = HashMap<String, (QueueItem, Signals)>;

impl RecommendPlan {
    /// Asks the source — similar songs, songs by name, songs in a genre —
    /// and MusicBrainz when online, then ranks every section.
    ///
    /// A call that fails is in `failures` with its stage and chain (K9),
    /// and the source is not asked again in this visit: a server that did
    /// not answer once will not answer the next call either, and the page
    /// must not wait for each.
    pub async fn finish(self) -> RecommendReport {
        let info = self.provider.info();
        let mut run = Run {
            plan: &self,
            used: self.seeds.iter().map(|seed| seed.key.clone()).collect(),
            failures: Vec::new(),
            learned: Vec::new(),
            summary: RecommendSummary {
                listens_considered: self.taste.listens_considered,
                listens_before_reset: self.taste.listens_before_reset,
                songs_in_taste: self.taste.songs.len(),
                songs_with_genre: self.taste.songs_with_genre,
                seeds: self.seeds.len(),
                top_genres: self.top_genres.clone(),
                ..RecommendSummary::default()
            },
            broken: false,
            learned_source: if self.provider.is_plugin() {
                GenreSource::Plugin
            } else {
                GenreSource::Server
            },
        };

        run.ask_musicbrainz().await;

        let mut sections = Vec::new();
        for seed in &self.seeds {
            let candidates = run.around(seed).await;
            sections.push(run.section(
                SectionKind::BecauseYouListened {
                    artist: seed.artist.clone(),
                    title: seed.title.clone(),
                },
                candidates,
                taste::AROUND_A_SONG,
            ));
        }
        for genre in &self.top_genres {
            let candidates = run.in_genre(genre).await;
            sections.push(run.section(
                SectionKind::Genre {
                    genre: genre.clone(),
                },
                candidates,
                taste::IN_A_GENRE,
            ));
        }
        if self.pool.is_some() {
            let candidates = run.by_loved_artists();
            sections.push(run.section(
                SectionKind::FromArtistsYouLove,
                candidates,
                taste::BY_LOVED_ARTISTS,
            ));
        }
        let top_picks = top_picks(&mut sections);
        sections.retain(|section| !section.items.is_empty());

        RecommendReport {
            provider: info.id,
            display_name: info.display_name,
            since_ms: self.since_ms,
            top_picks,
            sections,
            failures: run.failures,
            summary: run.summary,
            learned: run.learned,
        }
    }
}

/// One visit's working state.
struct Run<'p> {
    plan: &'p RecommendPlan,
    /// Keys already offered (and the seeds): a song is in one section only.
    used: HashSet<String>,
    failures: Vec<SuggestFailure>,
    learned: Vec<LearnedGenres>,
    summary: RecommendSummary,
    /// The source failed once: it is not asked again in this visit.
    broken: bool,
    learned_source: GenreSource,
}

impl Run<'_> {
    fn fail(&mut self, provider: &ProviderId, display_name: &str, err: &crate::Error) {
        self.failures.push(SuggestFailure {
            provider: provider.clone(),
            display_name: display_name.to_owned(),
            stage: err.stage().as_str().to_owned(),
            chain: err.chain_text(),
        });
    }

    fn fail_source(&mut self, err: &crate::Error) {
        let info = self.plan.provider.info();
        self.fail(&info.id, &info.display_name, err);
        self.broken = true;
    }

    /// The genres of a track: the source's own when it names them (and
    /// they are learned), otherwise what the library knows.
    fn genres_of(&mut self, track: &ProviderTrack, key: &str) -> Vec<String> {
        let own = genre::normalize_genres(&track.genres);
        if own.is_empty() {
            return self.plan.known_genres.get(key).cloned().unwrap_or_default();
        }
        self.learned.push(LearnedGenres {
            artist: track.track.artist.clone(),
            title: track.track.title.clone(),
            genres: track.genres.clone(),
            source: self.learned_source,
        });
        own
    }

    fn signals(&self, key: &str, artist: &str, genres: Vec<String>) -> Signals {
        Signals {
            together: 0.0,
            source_rank: None,
            genres,
            artist: artist.to_owned(),
            heard: self.plan.taste.songs.contains_key(key),
        }
    }

    /// MusicBrainz names the genres of the few loved songs that have none
    /// (online only). They shape the next visit's taste.
    async fn ask_musicbrainz(&mut self) {
        let Some(ask) = &self.plan.musicbrainz else {
            return;
        };
        let lookup = match &ask.lookup {
            Ok(lookup) => Arc::clone(lookup),
            Err(failure) => {
                self.failures.push(failure.clone());
                return;
            }
        };
        for (artist, title, mbid) in &ask.songs {
            match lookup.genres_of(mbid).await {
                Ok(found) => {
                    let genres = found.unwrap_or_default();
                    if !genre::normalize_genres(&genres).is_empty() {
                        self.summary.genres_learned_online += 1;
                    }
                    // Learned even when empty: "MusicBrainz names none" is
                    // remembered, so it is not asked again every visit.
                    self.learned.push(LearnedGenres {
                        artist: artist.clone(),
                        title: title.clone(),
                        genres,
                        source: GenreSource::MusicBrainz,
                    });
                }
                Err(err) => {
                    self.fail(&ProviderId::new("musicbrainz"), "MusicBrainz", &err);
                    return;
                }
            }
        }
    }

    /// The source's track for a song known by name, or `None` when it has
    /// none: the same song by its key, else a song of the same artist.
    async fn find(&mut self, artist: &str, title: &str) -> Option<ProviderTrack> {
        if self.broken {
            return None;
        }
        let provider = Arc::clone(&self.plan.provider);
        let hits = match provider
            .search(&format!("{artist} {title}"), SEARCH_ROWS)
            .await
        {
            Ok(hits) => hits,
            Err(err) => {
                self.fail_source(&err);
                return None;
            }
        };
        let key = track_key(artist, title);
        hits.into_iter()
            .find(|hit| track_key(&hit.track.artist, &hit.track.title) == key)
    }

    /// The candidates around one seed.
    async fn around(&mut self, seed: &Seed) -> Candidates {
        let mut candidates = Candidates::new();
        if let Some(pool) = &self.plan.pool {
            let by_key: HashMap<&str, &PoolRow> =
                pool.iter().map(|row| (row.key.as_str(), row)).collect();
            for (key, weight) in &seed.neighbours {
                if let Some(row) = by_key.get(key.as_str()) {
                    let mut signals = self.signals(key, &row.item.track.artist, row.genres.clone());
                    signals.together = *weight;
                    candidates.insert(key.clone(), (row.item.clone(), signals));
                    self.summary.found_listened_together += 1;
                }
            }
            return candidates;
        }

        let provider = Arc::clone(&self.plan.provider);
        let related = provider.info().capabilities.contains(Capabilities::RELATED);
        if related {
            if let Some(found) = self.find(&seed.artist, &seed.title).await {
                if !self.broken {
                    match provider.related(&found.id, RELATED_ASK).await {
                        Ok(tracks) => {
                            for (rank, track) in tracks.into_iter().enumerate() {
                                let key = track_key(&track.track.artist, &track.track.title);
                                let genres = self.genres_of(&track, &key);
                                let mut signals = self.signals(&key, &track.track.artist, genres);
                                signals.source_rank = Some(rank);
                                signals.together =
                                    seed.neighbours.get(&key).copied().unwrap_or(0.0);
                                self.summary.found_at_source += 1;
                                candidates.entry(key).or_insert((
                                    QueueItem {
                                        id: track.id,
                                        track: track.track,
                                    },
                                    signals,
                                ));
                            }
                        }
                        Err(err) => self.fail_source(&err),
                    }
                }
            }
        }

        // The songs listened together, looked up by name: the closest first.
        let mut closest: Vec<(&String, &f64)> = seed
            .neighbours
            .iter()
            .filter(|(key, _)| !candidates.contains_key(*key))
            .collect();
        closest.sort_by(|a, b| b.1.total_cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (key, weight) in closest.into_iter().take(RESOLVE_ASK) {
            if self.plan.dismissed.contains(key) || self.plan.taste.recent.contains(key) {
                continue;
            }
            let Some(song) = self.plan.taste.songs.get(key) else {
                continue;
            };
            let (artist, title) = (song.artist.clone(), song.title.clone());
            let Some(track) = self.find(&artist, &title).await else {
                continue;
            };
            let genres = self.genres_of(&track, key);
            let mut signals = self.signals(key, &track.track.artist, genres);
            signals.together = *weight;
            self.summary.found_listened_together += 1;
            candidates.insert(
                key.clone(),
                (
                    QueueItem {
                        id: track.id,
                        track: track.track,
                    },
                    signals,
                ),
            );
        }
        candidates
    }

    /// The candidates in a genre.
    async fn in_genre(&mut self, wanted: &str) -> Candidates {
        let mut candidates = Candidates::new();
        if let Some(pool) = &self.plan.pool {
            for row in pool
                .iter()
                .filter(|row| row.genres.iter().any(|g| g == wanted))
            {
                let signals = self.signals(&row.key, &row.item.track.artist, row.genres.clone());
                candidates.insert(row.key.clone(), (row.item.clone(), signals));
                self.summary.found_by_genre += 1;
            }
            return candidates;
        }
        let provider = Arc::clone(&self.plan.provider);
        if self.broken || !provider.info().capabilities.contains(Capabilities::GENRE) {
            return candidates;
        }
        match provider.genre_tracks(wanted, GENRE_ASK).await {
            Ok(tracks) => {
                for track in tracks {
                    let key = track_key(&track.track.artist, &track.track.title);
                    let mut genres = self.genres_of(&track, &key);
                    if !genres.iter().any(|g| g == wanted) {
                        genres.push(wanted.to_owned());
                    }
                    let signals = self.signals(&key, &track.track.artist, genres);
                    self.summary.found_by_genre += 1;
                    candidates.entry(key).or_insert((
                        QueueItem {
                            id: track.id,
                            track: track.track,
                        },
                        signals,
                    ));
                }
            }
            Err(err) => self.fail_source(&err),
        }
        candidates
    }

    /// The loved artists' songs not heard yet, from the catalog on the
    /// machine.
    fn by_loved_artists(&mut self) -> Candidates {
        let mut candidates = Candidates::new();
        let Some(pool) = &self.plan.pool else {
            return candidates;
        };
        let loved: HashSet<String> = self
            .plan
            .taste
            .top_artists(LOVED_ARTISTS)
            .into_iter()
            .collect();
        for row in pool {
            if self.plan.taste.songs.contains_key(&row.key)
                || !loved.contains(&normalize_artist(&row.item.track.artist))
            {
                continue;
            }
            let signals = self.signals(&row.key, &row.item.track.artist, row.genres.clone());
            candidates.insert(row.key.clone(), (row.item.clone(), signals));
            self.summary.found_by_artist += 1;
        }
        candidates
    }

    /// Scores, filters and orders one section's candidates.
    fn section(
        &mut self,
        kind: SectionKind,
        candidates: Candidates,
        weights: Weights,
    ) -> RecommendSection {
        let mut scored: Vec<(String, RecommendItem)> = Vec::new();
        for (key, (item, signals)) in candidates {
            if self.used.contains(&key) {
                continue;
            }
            if self.plan.dismissed.contains(&key) {
                self.summary.left_out_dismissed += 1;
                continue;
            }
            if self.plan.taste.recent.contains(&key) {
                self.summary.left_out_recent += 1;
                continue;
            }
            let (score, reasons) = taste::score(&self.plan.taste, &signals, weights);
            if score == 0 {
                continue;
            }
            scored.push((
                key,
                RecommendItem {
                    item,
                    genres: signals.genres,
                    reasons,
                    score,
                },
            ));
        }
        // Best first; equal scores by key, so the order never depends on a
        // hash map's.
        scored.sort_by(|a, b| b.1.score.cmp(&a.1.score).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(PER_SECTION);
        let items = scored
            .into_iter()
            .map(|(key, item)| {
                self.used.insert(key);
                item
            })
            .collect();
        RecommendSection { kind, items }
    }
}

/// Takes the shelf out of the sections: the best scored songs of every
/// section, best first, at most [`PICKS_PER_ARTIST`] by one artist, never
/// leaving a section with fewer than [`SECTION_KEEPS`]. A song moves — it is
/// not shown twice. Too few for a shelf, and nothing moves.
pub(crate) fn top_picks(sections: &mut [RecommendSection]) -> Vec<RecommendItem> {
    let mut ranked: Vec<(u32, usize, usize)> = sections
        .iter()
        .enumerate()
        .flat_map(|(s, section)| {
            section
                .items
                .iter()
                .enumerate()
                .map(move |(i, item)| (item.score, s, i))
        })
        .collect();
    // Best first; equals in the page's order.
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    let mut taken = vec![0_usize; sections.len()];
    let mut by_artist: HashMap<String, usize> = HashMap::new();
    let mut chosen: Vec<(usize, usize)> = Vec::new();
    for (_, s, i) in ranked {
        if chosen.len() >= TOP_PICKS {
            break;
        }
        if sections[s].items.len() - taken[s] <= SECTION_KEEPS {
            continue;
        }
        let artist = normalize_artist(&sections[s].items[i].item.track.artist);
        let count = by_artist.entry(artist).or_insert(0);
        if *count >= PICKS_PER_ARTIST {
            continue;
        }
        *count += 1;
        taken[s] += 1;
        chosen.push((s, i));
    }
    if chosen.len() < TOP_PICKS_MIN {
        return Vec::new();
    }

    let picks = chosen
        .iter()
        .map(|&(s, i)| sections[s].items[i].clone())
        .collect();
    let moved: HashSet<(usize, usize)> = chosen.into_iter().collect();
    for (s, section) in sections.iter_mut().enumerate() {
        let mut index = 0;
        section.items.retain(|_| {
            let keep = !moved.contains(&(s, index));
            index += 1;
            keep
        });
    }
    picks
}
