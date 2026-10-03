//! The taste read from the history, and the scoring — pure functions
//! (D-087).
//!
//! Nothing here touches the database, the network or the clock: listens,
//! genres and "now" come in, numbers come out. That is what makes every
//! weight testable, and what keeps the taste on the machine — it is never
//! a value anything outside the core sees.

use std::collections::{HashMap, HashSet};

use crate::identity::normalize::{normalize_artist, track_key};
use crate::model::{Listen, PlayRule};

/// How fast a listen's weight fades: it halves every this many days. A
/// song loved two years ago still counts — a quarter as much as today.
const HALF_LIFE_DAYS: f64 = 90.0;

/// A skip takes back this much of a play's weight.
const SKIP_PENALTY: f64 = 0.5;

/// Two listens further apart than this are not one sitting.
const SESSION_GAP_MS: i64 = 30 * 60 * 1000;

/// How far apart in a sitting two songs still count as listened together.
const WINDOW: usize = 3;

/// A seed is looked for among the songs played this recently first.
const SEED_RECENT_DAYS: i64 = 30;

/// A seed is a song played at least this many times: one play is a try,
/// not a love.
const SEED_MIN_PLAYS: u32 = 2;

/// A song played this recently is not offered again.
pub(crate) const RECENT_DAYS: i64 = 7;

const DAY_MS: f64 = 86_400_000.0;

/// One song as the history knows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Song {
    pub(crate) artist: String,
    pub(crate) title: String,
    /// The faded sum of its plays, minus its skips. Below zero: skipped
    /// more than played.
    pub(crate) score: f64,
    /// Counted plays.
    pub(crate) plays: u32,
    pub(crate) last_ms: i64,
}

/// The taste: what the listener plays, by song, artist and genre.
#[derive(Debug, Clone, Default)]
pub(crate) struct Taste {
    /// By grouping key ([`track_key`]).
    pub(crate) songs: HashMap<String, Song>,
    /// By folded artist name; each the sum of its songs' positive scores,
    /// scaled so the most played artist is 1.
    pub(crate) artists: HashMap<String, f64>,
    /// By folded genre; scaled so the most played genre is 1.
    pub(crate) genres: HashMap<String, f64>,
    /// Songs played in the last [`RECENT_DAYS`].
    pub(crate) recent: HashSet<String>,
    /// Listens the taste was read from.
    pub(crate) listens_considered: usize,
    /// Listens left out because they came before the reset.
    pub(crate) listens_before_reset: usize,
    /// Songs in the taste that have a genre.
    pub(crate) songs_with_genre: usize,
}

/// The weight a listen this old carries.
fn fade(now_ms: i64, at_ms: i64) -> f64 {
    let age_days = (now_ms.saturating_sub(at_ms)).max(0) as f64 / DAY_MS;
    0.5_f64.powf(age_days / HALF_LIFE_DAYS)
}

/// Reads the taste from the history.
///
/// Only listens at or after `since_ms` count — a reset starts the taste
/// over without deleting a listen. A counted play ([`PlayRule`]) adds its
/// faded weight; a skip takes back half of it.
pub(crate) fn read(
    listens: &[Listen],
    since_ms: Option<i64>,
    now_ms: i64,
    rule: PlayRule,
    genres: &HashMap<String, Vec<String>>,
) -> Taste {
    let mut taste = Taste::default();
    for listen in listens {
        let at = listen.played_at.as_millisecond();
        if since_ms.is_some_and(|since| at < since) {
            taste.listens_before_reset += 1;
            continue;
        }
        taste.listens_considered += 1;
        let key = track_key(&listen.track.artist, &listen.track.title);
        let weight = fade(now_ms, at);
        let delta = if listen.counts_as_play(rule) {
            weight
        } else if listen.ms_played > 0 {
            -SKIP_PENALTY * weight
        } else {
            0.0
        };
        let song = taste.songs.entry(key.clone()).or_insert_with(|| Song {
            artist: listen.track.artist.clone(),
            title: listen.track.title.clone(),
            score: 0.0,
            plays: 0,
            last_ms: at,
        });
        song.score += delta;
        if listen.counts_as_play(rule) {
            song.plays = song.plays.saturating_add(1);
        }
        song.last_ms = song.last_ms.max(at);
        if now_ms.saturating_sub(at) <= RECENT_DAYS * DAY_MS as i64 {
            taste.recent.insert(key);
        }
    }

    for (key, song) in &taste.songs {
        if song.score <= 0.0 {
            continue;
        }
        *taste
            .artists
            .entry(normalize_artist(&song.artist))
            .or_insert(0.0) += song.score;
        if let Some(list) = genres.get(key).filter(|list| !list.is_empty()) {
            taste.songs_with_genre += 1;
            let share = song.score / list.len() as f64;
            for genre in list {
                *taste.genres.entry(genre.clone()).or_insert(0.0) += share;
            }
        }
    }
    scale(&mut taste.artists);
    scale(&mut taste.genres);
    taste
}

/// Scales the values so the largest is 1.
fn scale(values: &mut HashMap<String, f64>) {
    let top = values.values().copied().fold(0.0_f64, f64::max);
    if top > 0.0 {
        for value in values.values_mut() {
            *value /= top;
        }
    }
}

impl Taste {
    /// The songs the recommendations start from: the best loved of the
    /// last month, each by a different artist; the best loved of all time
    /// fill in when the month is short. A song played once is not one.
    /// Best first, by key.
    pub(crate) fn seeds(&self, now_ms: i64, count: usize) -> Vec<String> {
        let mut loved: Vec<(&String, &Song)> = self
            .songs
            .iter()
            .filter(|(_, song)| song.score > 0.0 && song.plays >= SEED_MIN_PLAYS)
            .collect();
        // Best first; equal scores by key, so the order never depends on a
        // hash map's.
        loved.sort_by(|a, b| b.1.score.total_cmp(&a.1.score).then_with(|| a.0.cmp(b.0)));
        let recent_from = now_ms.saturating_sub(SEED_RECENT_DAYS * DAY_MS as i64);

        let mut seeds = Vec::new();
        let mut artists = HashSet::new();
        for recent_only in [true, false] {
            for (key, song) in &loved {
                if seeds.len() >= count {
                    return seeds;
                }
                if recent_only && song.last_ms < recent_from {
                    continue;
                }
                if seeds.contains(*key) || !artists.insert(normalize_artist(&song.artist)) {
                    continue;
                }
                seeds.push((*key).clone());
            }
        }
        seeds
    }

    /// The genres played most, most first.
    pub(crate) fn top_genres(&self, count: usize) -> Vec<String> {
        let mut genres: Vec<(&String, &f64)> = self.genres.iter().collect();
        genres.sort_by(|a, b| b.1.total_cmp(a.1).then_with(|| a.0.cmp(b.0)));
        genres
            .into_iter()
            .take(count)
            .map(|(genre, _)| genre.clone())
            .collect()
    }

    /// The artists played most (folded names), most first.
    pub(crate) fn top_artists(&self, count: usize) -> Vec<String> {
        let mut artists: Vec<(&String, &f64)> = self.artists.iter().collect();
        artists.sort_by(|a, b| b.1.total_cmp(a.1).then_with(|| a.0.cmp(b.0)));
        artists
            .into_iter()
            .take(count)
            .map(|(artist, _)| artist.clone())
            .collect()
    }

    /// How close a song's genres are to the taste, 0–1: the mean of the
    /// taste's weight for each of them.
    pub(crate) fn genre_closeness(&self, genres: &[String]) -> f64 {
        if genres.is_empty() {
            return 0.0;
        }
        let sum: f64 = genres
            .iter()
            .map(|genre| self.genres.get(genre).copied().unwrap_or(0.0))
            .sum();
        (sum / genres.len() as f64).clamp(0.0, 1.0)
    }

    /// How loved an artist is, 0–1.
    pub(crate) fn artist_closeness(&self, artist: &str) -> f64 {
        self.artists
            .get(&normalize_artist(artist))
            .copied()
            .unwrap_or(0.0)
    }
}

/// The songs listened together with each seed: in the same sitting, at
/// most [`WINDOW`] songs apart, nearer counting more, older fading. Only
/// counted plays take part — a skipped song was not listened to.
///
/// Scaled per seed so its closest neighbour is 1. Only the seeds' rows are
/// built: the whole graph of a large history is never held.
pub(crate) fn listened_together(
    listens: &[Listen],
    since_ms: Option<i64>,
    now_ms: i64,
    rule: PlayRule,
    seeds: &[String],
) -> HashMap<String, HashMap<String, f64>> {
    let wanted: HashSet<&String> = seeds.iter().collect();
    let mut rows: HashMap<String, HashMap<String, f64>> = HashMap::new();
    let mut sitting: Vec<(String, i64)> = Vec::new();
    let mut last_at: Option<i64> = None;

    let mut close = |sitting: &mut Vec<(String, i64)>| {
        for (i, (key, at)) in sitting.iter().enumerate() {
            if !wanted.contains(key) {
                continue;
            }
            let weight = fade(now_ms, *at);
            let low = i.saturating_sub(WINDOW);
            let high = (i + WINDOW).min(sitting.len().saturating_sub(1));
            for (j, (other, _)) in sitting.iter().enumerate().take(high + 1).skip(low) {
                if j == i || other == key {
                    continue;
                }
                let distance = i.abs_diff(j) as f64;
                *rows
                    .entry(key.clone())
                    .or_default()
                    .entry(other.clone())
                    .or_insert(0.0) += weight / distance;
            }
        }
        sitting.clear();
    };

    for listen in listens {
        let at = listen.played_at.as_millisecond();
        if since_ms.is_some_and(|since| at < since) || !listen.counts_as_play(rule) {
            continue;
        }
        if last_at.is_some_and(|last| at.saturating_sub(last) > SESSION_GAP_MS) {
            close(&mut sitting);
        }
        last_at = Some(at);
        sitting.push((track_key(&listen.track.artist, &listen.track.title), at));
    }
    close(&mut sitting);

    for row in rows.values_mut() {
        scale(row);
    }
    rows
}

/// Why a song is offered — every reason that held, the strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Played in the same sittings as a song the listener loves.
    ListenedTogether,
    /// The source itself names it as like a song the listener loves.
    SimilarAtSource,
    /// Its genres are the listener's.
    SharedGenre,
    /// By an artist the listener plays a lot.
    ArtistYouLove,
    /// Not in the listener's history yet.
    NotHeardYet,
}

/// What is known about one candidate.
#[derive(Debug, Clone, Default)]
pub(crate) struct Signals {
    /// Listened together with the seed, 0–1.
    pub(crate) together: f64,
    /// Its place in the source's own list of similar songs, 0 first.
    pub(crate) source_rank: Option<usize>,
    pub(crate) genres: Vec<String>,
    pub(crate) artist: String,
    /// In the history at all.
    pub(crate) heard: bool,
}

/// How much each signal weighs in one kind of section.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Weights {
    pub(crate) together: f64,
    pub(crate) source: f64,
    pub(crate) genre: f64,
    pub(crate) artist: f64,
    pub(crate) unheard: f64,
}

/// "Because you listened to …": what was played around it and what the
/// source says is like it lead.
pub(crate) const AROUND_A_SONG: Weights = Weights {
    together: 0.40,
    source: 0.30,
    genre: 0.15,
    artist: 0.10,
    unheard: 0.05,
};

/// "More <genre>": the genre leads, the new and the loved artists follow.
pub(crate) const IN_A_GENRE: Weights = Weights {
    together: 0.10,
    source: 0.0,
    genre: 0.50,
    artist: 0.20,
    unheard: 0.20,
};

/// "From artists you love": songs of theirs not heard yet.
pub(crate) const BY_LOVED_ARTISTS: Weights = Weights {
    together: 0.0,
    source: 0.0,
    genre: 0.30,
    artist: 0.60,
    unheard: 0.10,
};

/// Scores a candidate, 0–1000, with its reasons strongest first.
pub(crate) fn score(taste: &Taste, signals: &Signals, weights: Weights) -> (u32, Vec<Reason>) {
    let source = signals
        .source_rank
        .map_or(0.0, |rank| 1.0 / (1.0 + rank as f64 * 0.15));
    let parts = [
        (
            Reason::ListenedTogether,
            weights.together * signals.together,
        ),
        (Reason::SimilarAtSource, weights.source * source),
        (
            Reason::SharedGenre,
            weights.genre * taste.genre_closeness(&signals.genres),
        ),
        (
            Reason::ArtistYouLove,
            weights.artist * taste.artist_closeness(&signals.artist),
        ),
        (
            Reason::NotHeardYet,
            if signals.heard { 0.0 } else { weights.unheard },
        ),
    ];
    let total: f64 = parts.iter().map(|(_, part)| part).sum();
    let mut reasons: Vec<(Reason, f64)> =
        parts.into_iter().filter(|(_, part)| *part > 0.02).collect();
    reasons.sort_by(|a, b| b.1.total_cmp(&a.1));
    let scaled = (total.clamp(0.0, 1.0) * 1000.0).round();
    // In range by the clamp above.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let points = scaled as u32;
    (
        points,
        reasons.into_iter().map(|(reason, _)| reason).collect(),
    )
}
