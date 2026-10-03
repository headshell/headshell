//! Genre names as recommendations compare them (D-087).
//!
//! The same genre arrives written many ways: `Indie Rock`, `indie rock`,
//! `Rock; Indie`, `Hip-Hop/Rap`, ID3v1's `(17)`. They are compared after one
//! folding, done here and only here, so the tags, the servers, the plugins
//! and MusicBrainz all land on the same words.

/// The separators one tag value packs several genres with.
const SEPARATORS: &[char] = &[';', '/', ',', '|', '\0'];

/// The longest a genre may be; a longer "genre" is a comment in the wrong
/// field.
const MAX_LEN: usize = 40;

/// The ID3v1 genre list's first entries, for tags that still carry a
/// number (`(17)`, `17`). Only the common ones: an unknown number is dropped,
/// not guessed.
const ID3V1: &[&str] = &[
    "blues",
    "classic rock",
    "country",
    "dance",
    "disco",
    "funk",
    "grunge",
    "hip-hop",
    "jazz",
    "metal",
    "new age",
    "oldies",
    "other",
    "pop",
    "r&b",
    "rap",
    "reggae",
    "rock",
    "techno",
    "industrial",
    "alternative",
    "ska",
    "death metal",
    "pranks",
    "soundtrack",
    "euro-techno",
    "ambient",
    "trip-hop",
    "vocal",
    "jazz+funk",
    "fusion",
    "trance",
    "classical",
    "instrumental",
    "acid",
    "house",
    "game",
    "sound clip",
    "gospel",
    "noise",
    "alternative rock",
    "bass",
    "soul",
    "punk",
    "space",
    "meditative",
    "instrumental pop",
    "instrumental rock",
    "ethnic",
    "gothic",
    "darkwave",
    "techno-industrial",
    "electronic",
    "pop-folk",
    "eurodance",
    "dream",
    "southern rock",
    "comedy",
    "cult",
    "gangsta",
    "top 40",
    "christian rap",
    "pop/funk",
    "jungle",
    "native american",
    "cabaret",
    "new wave",
    "psychedelic",
    "rave",
    "showtunes",
    "trailer",
    "lo-fi",
    "tribal",
    "acid punk",
    "acid jazz",
    "polka",
    "retro",
    "musical",
    "rock & roll",
    "hard rock",
];

/// Words that say nothing about the music.
const NOISE: &[&str] = &["other", "unknown", "genre", "misc", "none", "various"];

/// Folds raw genre values into the words recommendations compare: lower
/// case, single spaces, split on the usual separators, ID3v1 numbers
/// turned into their names, empty and meaningless entries dropped, each
/// genre once, in the order first seen.
#[must_use]
pub(crate) fn normalize_genres<S: AsRef<str>>(raw: &[S]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in raw {
        for part in value.as_ref().split(SEPARATORS) {
            let Some(genre) = fold(part) else {
                continue;
            };
            if !out.contains(&genre) {
                out.push(genre);
            }
        }
    }
    out
}

fn fold(part: &str) -> Option<String> {
    let trimmed = part.trim();
    // ID3v1: `(17)`, `17`, or `(17)Rock` — the text after the number wins.
    let unwrapped = trimmed
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
        .map_or((None, trimmed), |(number, rest)| {
            (Some(number), rest.trim())
        });
    let text = match unwrapped {
        (Some(number), "") => id3v1(number)?.to_owned(),
        (_, rest) if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() => {
            id3v1(rest)?.to_owned()
        }
        (_, rest) => rest.to_lowercase(),
    };
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.is_empty() || folded.chars().count() > MAX_LEN || NOISE.contains(&folded.as_str()) {
        return None;
    }
    Some(folded)
}

fn id3v1(number: &str) -> Option<&'static str> {
    number
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|index| ID3V1.get(index))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::normalize_genres;

    #[test]
    fn the_same_genre_written_differently_folds_to_one() {
        assert_eq!(
            normalize_genres(&["Indie Rock", "indie  rock", " INDIE ROCK "]),
            vec!["indie rock"]
        );
    }

    #[test]
    fn packed_values_are_split() {
        assert_eq!(
            normalize_genres(&["Rock; Indie", "Hip-Hop/Rap"]),
            vec!["rock", "indie", "hip-hop", "rap"]
        );
    }

    #[test]
    fn id3v1_numbers_become_names_and_unknown_ones_are_dropped() {
        assert_eq!(
            normalize_genres(&["(17)", "13", "(200)"]),
            vec!["rock", "pop"]
        );
        assert_eq!(normalize_genres(&["(17)Rock & Roll"]), vec!["rock & roll"]);
    }

    #[test]
    fn noise_and_overlong_values_are_dropped() {
        let long = "a".repeat(60);
        assert!(normalize_genres(&["Other", "", "  ", long.as_str()]).is_empty());
    }
}
