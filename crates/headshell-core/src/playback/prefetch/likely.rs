//! The third track (D-077): of the tracks beyond the next two, the one
//! likeliest to be needed soon. "Next" is always the likeliest — the user's
//! rule — and the one after it comes second; this guesses the third.
//!
//! A track beyond the next two is needed before the read-ahead thread has
//! moved on in two ways: the user skips the next two quickly, and the
//! queue's own continuation is needed; or the user picks a track in the
//! queue. The first is weighed with how often the user skips — in this
//! session, and in the history of the next two tracks and their artists; the
//! second with how much the user listened to each track and its artist.
//!
//! Simple on purpose: a guess the report can explain in one sentence ("you
//! skipped 4 of the 5 tracks you left") is worth more than a model nobody
//! can check (K9).

use std::collections::HashMap;

use crate::ids::ProviderTrackId;
use crate::library::ListeningProfile;
use crate::playback::queue::QueueItem;

use super::Habits;

/// The share of tracks taken as skipped before anything is known, and how
/// many observed tracks that guess is worth.
const SKIP_PRIOR: f64 = 0.25;
const SKIP_PRIOR_WEIGHT: f64 = 4.0;

/// The share of moves taken as a pick in the queue before anything is
/// known, and how many observed moves that guess is worth.
const JUMP_PRIOR: f64 = 0.1;
const JUMP_PRIOR_WEIGHT: f64 = 10.0;

/// How many of the session's tracks one track's own history is worth.
const HISTORY_WEIGHT: f64 = 4.0;

/// How much the artist's plays count towards a track, against its own.
const ARTIST_SHARE: f64 = 0.25;

/// The track picked for the third place, and why.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pick {
    /// Its position in play order.
    pub(crate) index: usize,
    pub(crate) why: String,
}

#[expect(
    clippy::cast_precision_loss,
    reason = "listen counts far below 2^52; an estimate either way"
)]
fn float(count: u64) -> f64 {
    count as f64
}

/// How often the user leaves a track before it counts, this session.
fn session_skip_rate(habits: Habits) -> f64 {
    let seen = f64::from(habits.skipped + habits.finished);
    (f64::from(habits.skipped) + SKIP_PRIOR * SKIP_PRIOR_WEIGHT) / (seen + SKIP_PRIOR_WEIGHT)
}

/// How likely a track is to be skipped: its own history if it has one, else
/// its artist's, drawn towards the session's rate.
fn skip_rate(profile: ListeningProfile, session: f64) -> f64 {
    let (plays, skips) = if profile.plays + profile.skips > 0 {
        (profile.plays, profile.skips)
    } else {
        (profile.artist_plays, profile.artist_skips)
    };
    (float(skips) + session * HISTORY_WEIGHT) / (float(plays + skips) + HISTORY_WEIGHT)
}

/// How often the user picks a track in the queue, this session.
fn jump_rate(habits: Habits) -> f64 {
    let moves = f64::from(habits.skipped + habits.finished + habits.jumped + habits.went_back);
    (f64::from(habits.jumped) + JUMP_PRIOR * JUMP_PRIOR_WEIGHT) / (moves + JUMP_PRIOR_WEIGHT)
}

/// How strongly a track pulls a pick: its plays, and a share of its artist's.
fn pull(profile: ListeningProfile) -> f64 {
    1.0 + float(profile.plays) + ARTIST_SHARE * float(profile.artist_plays)
}

fn times(count: u64) -> String {
    if count == 1 {
        "once".to_owned()
    } else {
        format!("{count} times")
    }
}

/// The third track of the plan: of those beyond the playing one and the
/// next two (`queue` in play order), the likeliest to be needed soon.
/// `None` when there is none.
pub(crate) fn third(
    queue: &[QueueItem],
    position: usize,
    profiles: &HashMap<ProviderTrackId, ListeningProfile>,
    habits: Habits,
) -> Option<Pick> {
    let profile = |index: usize| {
        queue
            .get(index)
            .and_then(|item| profiles.get(&item.id))
            .copied()
            .unwrap_or_default()
    };
    let continuation = position + 3;
    let candidates: Vec<usize> = (0..queue.len())
        .filter(|index| !(position..continuation).contains(index))
        .collect();
    if candidates.is_empty() {
        return None;
    }

    let session = session_skip_rate(habits);
    let skip_next = skip_rate(profile(position + 1), session);
    let skip_after = skip_rate(profile(position + 2), session);
    let through = skip_next * skip_after;
    let jump = jump_rate(habits);
    let pulls: Vec<f64> = candidates
        .iter()
        .map(|&index| pull(profile(index)))
        .collect();
    let total: f64 = pulls.iter().sum();

    // The best score; the earlier track in play order wins a tie.
    let mut best: Option<(usize, f64, f64)> = None;
    for (&index, &weight) in candidates.iter().zip(&pulls) {
        let picked = jump * weight / total;
        let continued = if index == continuation { through } else { 0.0 };
        let score = picked + continued;
        if best.is_none_or(|(_, top, _)| score > top) {
            best = Some((index, score, continued));
        }
    }
    let (index, score, continued) = best?;

    let why = if index == continuation && continued * 2.0 >= score {
        let left = habits.skipped + habits.finished;
        if habits.skipped > 0 {
            format!(
                "you skipped {} of the {left} tracks you left this session",
                habits.skipped
            )
        } else if profile(position + 1).skips + profile(position + 1).artist_skips > 0 {
            "you often skip the track after this one".to_owned()
        } else {
            "the queue's own continuation".to_owned()
        }
    } else {
        let picked = profile(index);
        let artist = queue
            .get(index)
            .map_or_else(String::new, |item| item.track.artist.clone());
        if picked.plays > 0 {
            format!("you played it {}", times(picked.plays))
        } else if picked.artist_plays > 0 {
            format!("you played {artist} {}", times(picked.artist_plays))
        } else if habits.jumped > 0 {
            format!(
                "you picked {} this session",
                times(u64::from(habits.jumped))
            )
        } else if index == continuation {
            "the queue's own continuation".to_owned()
        } else if candidates.len() == 1 {
            "the only other track in the queue".to_owned()
        } else {
            format!("track {} of the queue; nothing points elsewhere", index + 1)
        }
    };
    Some(Pick { index, why })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProviderId;
    use crate::model::TrackRef;

    fn queue(len: usize) -> Vec<QueueItem> {
        (0..len)
            .map(|i| QueueItem {
                id: ProviderTrackId::new(ProviderId::new("p"), i.to_string()),
                track: TrackRef::new(format!("Artist {i}"), format!("Track {i}")),
            })
            .collect()
    }

    fn with(
        profiles: &[(usize, ListeningProfile)],
        queue: &[QueueItem],
    ) -> HashMap<ProviderTrackId, ListeningProfile> {
        profiles
            .iter()
            .map(|(index, profile)| (queue[*index].id.clone(), *profile))
            .collect()
    }

    /// Knowing nothing, the third is where the queue goes on.
    #[test]
    fn without_history_the_third_is_the_continuation() {
        let queue = queue(10);
        let pick = third(&queue, 0, &HashMap::new(), Habits::default()).unwrap();
        assert_eq!(pick.index, 3);
        assert_eq!(pick.why, "the queue's own continuation");
    }

    /// A track the user played many times pulls the pick to itself, and the
    /// report says why.
    #[test]
    fn a_track_played_often_is_the_likeliest_pick() {
        let queue = queue(10);
        let loved = ListeningProfile {
            plays: 40,
            ..ListeningProfile::default()
        };
        let pick = third(&queue, 0, &with(&[(7, loved)], &queue), Habits::default()).unwrap();
        assert_eq!(pick.index, 7);
        assert_eq!(pick.why, "you played it 40 times");
    }

    /// Someone skipping through a queue needs its continuation next, however
    /// much they once played a track further down.
    #[test]
    fn a_session_of_skips_keeps_the_continuation_first() {
        let queue = queue(10);
        let loved = ListeningProfile {
            plays: 40,
            ..ListeningProfile::default()
        };
        let habits = Habits {
            skipped: 6,
            finished: 1,
            ..Habits::default()
        };
        let pick = third(&queue, 0, &with(&[(7, loved)], &queue), habits).unwrap();
        assert_eq!(pick.index, 3);
        assert_eq!(
            pick.why,
            "you skipped 6 of the 7 tracks you left this session"
        );
    }

    /// The artist's plays pull too, a share of a track's own.
    #[test]
    fn an_artist_played_often_pulls_its_tracks() {
        let queue = queue(10);
        let artist = ListeningProfile {
            artist_plays: 200,
            ..ListeningProfile::default()
        };
        let pick = third(&queue, 2, &with(&[(8, artist)], &queue), Habits::default()).unwrap();
        assert_eq!(pick.index, 8);
        assert_eq!(pick.why, "you played Artist 8 200 times");
    }

    /// The playing track and the next two are never the third; a queue with
    /// nothing beyond them has none. Tracks already played are candidates.
    #[test]
    fn the_third_is_beyond_the_next_two_or_none() {
        assert_eq!(
            third(&queue(3), 0, &HashMap::new(), Habits::default()),
            None
        );
        let queue = queue(4);
        let loved = ListeningProfile {
            plays: 9,
            ..ListeningProfile::default()
        };
        let pick = third(&queue, 1, &with(&[(0, loved)], &queue), Habits::default()).unwrap();
        assert_eq!(pick.index, 0, "a track already played can be picked again");
        assert_eq!(pick.why, "you played it 9 times");
        let pick = third(&queue, 1, &HashMap::new(), Habits::default()).unwrap();
        assert_eq!(pick.why, "the only other track in the queue");
    }
}
