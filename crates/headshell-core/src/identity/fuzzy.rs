//! The fuzzy match score — the third link of the identity chain.
//!
//! The input is normalised artist+title+duration; the output is a confidence
//! between 0.0 and 1.0. The score's weights live here in one place so they
//! can be tuned on the accuracy set.

use crate::identity::normalize::{
    VARIANT_MARKERS, normalize_artist, normalize_text, variant_markers,
};

/// The title is more distinctive than the artist: one artist has 300 tracks.
const TITLE_WEIGHT: f64 = 0.6;
/// The artist's weight.
const ARTIST_WEIGHT: f64 = 0.4;
/// Durations closer than this count as "the same" (different masters, a
/// margin for silence).
const DURATION_MATCH_MS: u64 = 3_000;
/// Durations further apart than this are seriously suspect: a radio edit / a
/// long version.
pub(crate) const DURATION_MISMATCH_MS: u64 = 15_000;
/// The amount a duration agreement adds to the score.
const DURATION_BONUS: f64 = 0.08;
/// A duration conflict multiplies the score by this ratio.
const DURATION_PENALTY: f64 = 0.7;
/// If the artist similarity is below this, it cannot be the same recording.
///
/// Different artists with the same title are very common (covers, tributes,
/// karaoke, same-named tracks). Since the title weight alone is 0.6, even a
/// completely unrelated artist could get close to the threshold (D-009).
const ARTIST_MIN_SIMILARITY: f64 = 0.7;
/// A score below the artist floor is multiplied by this ratio.
const ARTIST_MISMATCH_PENALTY: f64 = 0.5;
/// A score whose variant labels do not agree is multiplied by this ratio.
///
/// `Creep` and `Creep (Live)` are the same song but not the same
/// **recording**.
const VARIANT_MISMATCH_PENALTY: f64 = 0.5;
/// Words shorter than this do not count as distinctive.
///
/// Connectives like `at`, `in`, `de`, `ve` may be missing from either text,
/// and their absence says nothing. `nyc` (3) is distinctive, `at` (2) is not.
const DETAIL_MIN_LEN: usize = 3;

/// The similarity between two track descriptions.
///
/// If `duration_*` is unknown the duration component does not apply —
/// counting an unknown duration as agreement produces wrong matches.
///
/// On **top** of text similarity there are two separate rules; both catch the
/// case "the title looks alike but this is not the same recording":
/// - the artist floor ([`ARTIST_MIN_SIMILARITY`]) — covers/tributes/karaoke,
/// - a variant mismatch — live/remix/acoustic recordings.
///
/// # `context_b` — the variant information may not be in the title
///
/// Asymmetric, and on purpose: `a` is the user's record, and whatever they
/// have is in the title. `b` comes from a metadata catalog, and MusicBrainz
/// marks live recordings **not in the title** but in the `disambiguation`
/// field — of three separate `Creep`s in the catalog two are live, and all
/// three are titled plainly `Creep`. When this field was not read, the
/// measured result was this (D-045, on a real response): the 1994 Astoria
/// recording, because its duration was 12 s close to the studio recording,
/// counted as a **"perfect hit" with 1.00 confidence**. That is, the chain was
/// tying itself to the wrong recording exactly where it looked most sure.
#[must_use]
pub fn similarity(
    artist_a: &str,
    title_a: &str,
    duration_a_ms: Option<u64>,
    artist_b: &str,
    title_b: &str,
    duration_b_ms: Option<u64>,
    context_b: Option<&str>,
) -> f64 {
    let artist_sim = strsim::jaro_winkler(&normalize_artist(artist_a), &normalize_artist(artist_b));
    let (norm_a, norm_b) = (normalize_text(title_a), normalize_text(title_b));
    let title_sim = strsim::jaro_winkler(&norm_a, &norm_b);
    let base = ARTIST_WEIGHT.mul_add(artist_sim, TITLE_WEIGHT * title_sim);

    let mut scored = match (duration_a_ms, duration_b_ms) {
        (Some(a), Some(b)) => {
            let diff = a.abs_diff(b);
            if diff <= DURATION_MATCH_MS {
                base + DURATION_BONUS
            } else if diff >= DURATION_MISMATCH_MS {
                base * DURATION_PENALTY
            } else {
                base
            }
        }
        _ => base,
    };

    if artist_sim < ARTIST_MIN_SIMILARITY {
        scored *= ARTIST_MISMATCH_PENALTY;
    }
    let markers_a = variant_markers(&norm_a);
    let markers_b = merged_variant_markers(&norm_b, context_b);
    if markers_a != markers_b {
        scored *= VARIANT_MISMATCH_PENALTY;
    } else if !markers_a.is_empty() && unmatched_detail(&norm_a, &norm_b, context_b) {
        // Both are live recordings, but from different nights: `Creep (Live at
        // Glastonbury)` and `Creep` + `live, 1994-05-27: Astoria, London, UK` carry
        // the same marker but are not the same performance (D-010: the canonical
        // identity is at the recording level). This distinction was invisible
        // without real live recordings in the catalog; it showed up once they were
        // added (D-045).
        scored *= VARIANT_MISMATCH_PENALTY;
    }
    scored.clamp(0.0, 1.0)
}

/// Does a distinguishing detail the user gave find a counterpart in the
/// candidate?
///
/// If the user wrote `Live at Glastonbury`, `glastonbury` is a claim, and if
/// it does not occur in the candidate's text (title **or** disambiguation
/// note) the candidate is not that recording. If they only wrote `Live`
/// there is no claim — then the best live candidate is accepted.
///
/// One-directional, and on purpose: an extra detail the candidate carries
/// (`1994-05-27`) means the user did not know it, not a contradiction.
fn unmatched_detail(norm_a: &str, norm_b: &str, context_b: Option<&str>) -> bool {
    let haystack = match context_b {
        Some(context) => format!("{norm_b} {}", normalize_text(context)),
        None => norm_b.to_owned(),
    };
    let known: Vec<&str> = haystack.split_whitespace().collect();
    norm_a
        .split_whitespace()
        .filter(|word| word.chars().count() >= DETAIL_MIN_LEN)
        // The variant labels themselves are not details but a classification.
        .filter(|word| !VARIANT_MARKERS.contains(word))
        .any(|word| !known.contains(&word))
}

/// The candidate's variant markers: those in its title **and** those in the
/// context field.
///
/// The union is taken; the context does not replace the title: a recording
/// may carry both the title `Creep (Acoustic)` and the note `live, 2003`.
fn merged_variant_markers(normalized_title: &str, context: Option<&str>) -> Vec<&'static str> {
    let mut markers = variant_markers(normalized_title);
    if let Some(context) = context {
        for marker in variant_markers(&normalize_text(context)) {
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
        markers.sort_unstable();
    }
    markers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_tracks_score_at_the_top() {
        let score = similarity(
            "Radiohead",
            "Creep",
            Some(238_000),
            "radiohead",
            "Creep (Remastered)",
            Some(238_500),
            None,
        );
        assert!(score > 0.99, "score {score}");
    }

    #[test]
    fn different_tracks_by_same_artist_score_low() {
        let score = similarity(
            "Radiohead",
            "Creep",
            Some(238_000),
            "Radiohead",
            "Karma Police",
            Some(261_000),
            None,
        );
        assert!(score < 0.75, "score {score}");
    }

    #[test]
    fn duration_mismatch_pulls_the_score_down() {
        let same = similarity(
            "Daft Punk",
            "Aerodynamic",
            None,
            "Daft Punk",
            "Aerodynamic",
            None,
            None,
        );
        let mismatched = similarity(
            "Daft Punk",
            "Aerodynamic",
            Some(212_000),
            "Daft Punk",
            "Aerodynamic",
            Some(420_000),
            None,
        );
        assert!(mismatched < same, "should be {mismatched} < {same}");
    }

    #[test]
    fn live_recording_does_not_match_the_studio_take() {
        // D-009: a live recording is a separate recording; it must not merge even if
        // the duration is unknown.
        let studio = similarity("Radiohead", "Creep", None, "Radiohead", "Creep", None, None);
        let live = similarity(
            "Radiohead",
            "Creep (Live at Glastonbury)",
            None,
            "Radiohead",
            "Creep",
            None,
            None,
        );
        assert!(studio > 0.99, "studio score {studio}");
        assert!(live < 0.7, "live score {live}");
    }

    #[test]
    fn two_live_takes_are_not_penalised_against_each_other() {
        // The penalty is for a variant *mismatch*; two live recordings are not
        // penalised against each other.
        let score = similarity(
            "Radiohead",
            "Creep (Live)",
            None,
            "Radiohead",
            "Creep - Live",
            None,
            None,
        );
        assert!(score > 0.88, "score {score}");
    }

    #[test]
    fn a_different_artist_cannot_win_on_the_title_alone() {
        // D-009: cover/tribute/karaoke — the title identical, the artist foreign.
        let score = similarity(
            "Karaoke Version",
            "Creep",
            Some(238_000),
            "Radiohead",
            "Creep",
            Some(238_000),
            None,
        );
        assert!(score < 0.7, "score {score}");
    }

    /// D-045: in the real catalog, the **only** marker of a live recording is the
    /// disambiguation note.
    #[test]
    fn a_live_take_marked_only_in_the_note_loses_to_the_studio_take() {
        // Both candidates are titled plainly `Creep`; the difference is only in the
        // note.
        let studio = similarity("Radiohead", "Creep", None, "Radiohead", "Creep", None, None);
        let live = similarity(
            "Radiohead",
            "Creep",
            None,
            "Radiohead",
            "Creep",
            None,
            Some("live, 1994-05-27: Astoria, London, UK"),
        );
        assert!(studio > 0.99, "studio score {studio}");
        assert!(
            live < studio,
            "when the note was not read both got 1.00: should be {live} < {studio}"
        );
    }

    /// Two different live recordings are not the same recording (D-010).
    #[test]
    fn two_different_live_nights_do_not_merge() {
        let score = similarity(
            "Radiohead",
            "Creep (Live at Glastonbury)",
            None,
            "Radiohead",
            "Creep",
            None,
            Some("live, 1994-05-27: Astoria, London, UK"),
        );
        assert!(score < 0.7, "different nights must not merge: {score}");
    }

    /// But if the user only said "Live", they made no claim.
    #[test]
    fn a_bare_live_query_accepts_the_best_live_candidate() {
        let score = similarity(
            "Radiohead",
            "Creep (Live)",
            None,
            "Radiohead",
            "Creep",
            None,
            Some("live, 1994-05-27: Astoria, London, UK"),
        );
        assert!(score > 0.88, "it must pass the threshold: {score}");
    }

    /// A detail the candidate additionally knows is not a contradiction: the
    /// user just does not know it.
    #[test]
    fn extra_detail_on_the_candidate_side_is_not_a_contradiction() {
        let score = similarity(
            "Pink Floyd",
            "Wish You Were Here - Live",
            None,
            "Pink Floyd",
            "Wish You Were Here",
            None,
            Some("live, 1994-10-20: Earls Court, London"),
        );
        assert!(score > 0.88, "score {score}");
    }

    #[test]
    fn unknown_duration_is_not_treated_as_agreement() {
        // If a perfectly matching pair were picked the score would already hit 1.0
        // and the bonus would be invisible; so we shift the title slightly on
        // purpose.
        let unknown = similarity(
            "Daft Punk",
            "Aerodynamic",
            None,
            "Daft Punk",
            "Aerodynamik",
            Some(212_000),
            None,
        );
        let agreeing = similarity(
            "Daft Punk",
            "Aerodynamic",
            Some(212_000),
            "Daft Punk",
            "Aerodynamik",
            Some(212_500),
            None,
        );
        assert!(unknown < agreeing, "should be {unknown} < {agreeing}");
    }
}
