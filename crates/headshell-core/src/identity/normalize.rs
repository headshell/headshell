//! Text normalisation — the input of fuzzy matching.
//!
//! "Radiohead - Creep (Remastered 2011)" and "radiohead - creep" are the same
//! track. Every rule here is an accuracy decision; if you change one,
//! re-measure the rate on `fixtures/identity/cases.json`.

/// Makes an artist/title string comparable.
///
/// In order: diacritic folding → lower case → parenthesised/dashed suffixes →
/// `feat.` → punctuation → whitespace simplification.
#[must_use]
pub fn normalize_text(input: &str) -> String {
    let folded = fold_diacritics(input);
    let lowered = folded.to_lowercase();
    let without_suffix = strip_edition_suffixes(&lowered);
    let without_feat = strip_featuring(&without_suffix);
    collapse(&strip_punctuation(&without_feat))
}

/// Normalises an artist name; unlike a title, a `the` prefix is dropped.
#[must_use]
pub fn normalize_artist(input: &str) -> String {
    let base = normalize_text(input);
    base.strip_prefix("the ")
        .map_or(base.clone(), ToOwned::to_owned)
}

/// A track's local grouping key: `artist\u{1}title`.
#[must_use]
pub fn track_key(artist: &str, title: &str) -> String {
    format!("{}\u{1}{}", normalize_artist(artist), normalize_text(title))
}

/// Folds Turkish and common Latin diacritics to ASCII.
///
/// By hand rather than with a library: the list is small and we want it to be
/// visible exactly what gets folded.
fn fold_diacritics(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            'ç' | 'Ç' => 'c',
            'ğ' | 'Ğ' => 'g',
            'ı' | 'I' => 'i',
            'İ' => 'i',
            'ö' | 'Ö' => 'o',
            'ş' | 'Ş' => 's',
            'ü' | 'Ü' => 'u',
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => {
                'a'
            }
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'Ó' | 'Ò' | 'Ô' | 'Õ' => 'o',
            'ú' | 'ù' | 'û' | 'Ú' | 'Ù' | 'Û' => 'u',
            'ñ' | 'Ñ' => 'n',
            'ý' | 'ÿ' | 'Ý' => 'y',
            'ø' | 'Ø' => 'o',
            'æ' | 'Æ' => 'a',
            'ß' => 's',
            other => other,
        })
        .collect()
}

/// Labels marking a re-release of the same recording — they are dropped.
///
/// They do not change the recording itself: `Creep` and `Creep (Remastered
/// 2011)` are the same performance.
pub(crate) const REISSUE_MARKERS: [&str; 13] = [
    "remaster",
    "remastered",
    "deluxe",
    "edition",
    "version",
    "mono",
    "stereo",
    "bonus track",
    "radio edit",
    "single version",
    "album version",
    "anniversary",
    "expanded",
];

/// Labels saying the track is **a different recording** — they are not
/// dropped.
///
/// D-009: a live recording must stay apart from the studio one, a remix from
/// the original. These labels used to be in [`REISSUE_MARKERS`], and `Creep
/// (Live at Glastonbury)` was being tied to the studio recording with 100%
/// confidence. Now they both stay in the text and lower the score through
/// [`variant_markers`].
pub(crate) const VARIANT_MARKERS: [&str; 9] = [
    "live",
    "remix",
    "acoustic",
    "instrumental",
    "karaoke",
    "demo",
    "unplugged",
    "cover",
    "reprise",
];

/// The variant labels found in a normalised title (sorted, unique).
///
/// If the two sides' sets differ, the two recordings are not the same
/// performance. The comparison is by **word**: `"live"` occurs inside
/// `"delivered"`, so a substring search produces false positives.
#[must_use]
pub fn variant_markers(normalized_title: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = VARIANT_MARKERS
        .iter()
        .copied()
        .filter(|marker| {
            marker.split_whitespace().all(|word| {
                normalized_title
                    .split_whitespace()
                    .any(|token| token == word)
            })
        })
        .collect();
    found.sort_unstable();
    found
}

/// Drops re-release suffixes: `(Remastered 2011)`, `- 2011 Remaster`,
/// `[Deluxe Edition]`.
///
/// Only [`REISSUE_MARKERS`] are dropped. A suffix containing one of the
/// [`VARIANT_MARKERS`] **is kept** — `(Live Version)` contains both "live" and
/// "version", and if it were dropped the live recording would merge with the
/// studio one. Suffixes that are part of the track's real name (`(Reprise)`)
/// are kept too; merging different tracks is worse than a missed match.
fn strip_edition_suffixes(input: &str) -> String {
    let droppable = |text: &str| {
        !VARIANT_MARKERS.iter().any(|m| text.contains(m))
            && REISSUE_MARKERS.iter().any(|m| text.contains(m))
    };

    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open) = rest.find(['(', '[']) {
        let close_char = if rest.as_bytes()[open] == b'(' {
            ')'
        } else {
            ']'
        };
        let Some(close_rel) = rest[open..].find(close_char) else {
            break;
        };
        let close = open + close_rel;
        let inside = &rest[open + 1..close];
        out.push_str(&rest[..open]);
        if !droppable(inside) {
            out.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);

    // Dashed suffixes of the form " - Remastered 2011".
    if let Some(dash) = out.rfind(" - ") {
        if droppable(&out[dash + 3..]) {
            out.truncate(dash);
        }
    }
    out
}

/// Drops `feat. X`, `ft. X`, `featuring X` suffixes.
fn strip_featuring(input: &str) -> String {
    const MARKERS: [&str; 4] = ["feat.", "feat ", "ft.", "featuring "];
    let mut out = input.to_owned();
    for marker in MARKERS {
        if let Some(pos) = out.find(marker) {
            out.truncate(pos);
        }
    }
    out
}

/// Turns everything other than alphanumerics and whitespace into spaces.
fn strip_punctuation(input: &str) -> String {
    input
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect()
}

/// Collapses runs of whitespace into one and trims.
fn collapse(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edition_suffixes_are_dropped() {
        assert_eq!(normalize_text("Creep (Remastered 2011)"), "creep");
        assert_eq!(
            normalize_text("Wish You Were Here - 2011 Remaster"),
            "wish you were here"
        );
        assert_eq!(
            normalize_text("Bohemian Rhapsody [Deluxe Edition]"),
            "bohemian rhapsody"
        );
    }

    #[test]
    fn meaningful_parentheses_survive() {
        assert_eq!(
            normalize_text("Karma Police (Reprise)"),
            "karma police reprise"
        );
    }

    #[test]
    fn variant_labels_are_kept_not_stripped() {
        // D-009: a live/remix recording must not merge with the studio one.
        assert_eq!(
            normalize_text("Creep (Live at Glastonbury)"),
            "creep live at glastonbury"
        );
        assert_eq!(
            normalize_text("Wish You Were Here - Live"),
            "wish you were here live"
        );
        // "Live Version" contains both a variant and a re-release label; the variant
        // wins.
        assert_eq!(normalize_text("Numb (Live Version)"), "numb live version");
        assert_eq!(
            normalize_text("Aerodynamic (Slum Village Remix)"),
            "aerodynamic slum village remix"
        );
    }

    #[test]
    fn variant_markers_match_whole_words_only() {
        assert_eq!(variant_markers(&normalize_text("Creep (Live)")), ["live"]);
        assert_eq!(variant_markers(&normalize_text("Creep")), [] as [&str; 0]);
        // "live" occurs inside "delivered" — a substring search gives a false
        // positive.
        assert_eq!(
            variant_markers(&normalize_text("Delivered")),
            [] as [&str; 0]
        );
        assert_eq!(
            variant_markers(&normalize_text("Numb (Live Acoustic)")),
            ["acoustic", "live"]
        );
    }

    #[test]
    fn featuring_is_dropped() {
        assert_eq!(normalize_text("Numb feat. Jay-Z"), "numb");
        assert_eq!(normalize_text("Otherside ft. Someone"), "otherside");
    }

    #[test]
    fn turkish_characters_fold_to_ascii() {
        assert_eq!(normalize_artist("Şebnem Ferah"), "sebnem ferah");
        assert_eq!(normalize_text("Gündoğdu"), "gundogdu");
        assert_eq!(normalize_text("Işıl Işıl"), "isil isil");
    }

    #[test]
    fn the_prefix_only_drops_for_artists() {
        assert_eq!(normalize_artist("The Beatles"), "beatles");
        assert_eq!(
            normalize_text("The Less I Know The Better"),
            "the less i know the better"
        );
    }

    #[test]
    fn track_key_joins_both_sides() {
        assert_eq!(
            track_key("The Beatles", "Yesterday"),
            "beatles\u{1}yesterday"
        );
        assert_eq!(
            track_key("Radiohead", "Creep (Remastered)"),
            track_key("radiohead", "creep")
        );
    }
}
