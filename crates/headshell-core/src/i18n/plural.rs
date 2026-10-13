//! Which plural form a count takes, per language (D-089).
//!
//! The categories are CLDR's (`zero one two few many other`), so a language
//! file names its forms the way every translation tool does. The rules here
//! are CLDR's for **whole numbers** — the app counts tracks, listens and
//! plays, never fractions — and cover the languages whose rules differ from
//! English's; any other language takes English's (`one` for 1, `other` for
//! the rest). The desktop asks its webview's `Intl.PluralRules` instead,
//! which knows every language; this table is for the CLI.

use serde::{Deserialize, Serialize};

/// A CLDR plural category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

/// The category `count` takes in `language` (a BCP 47 code: `tr`, `pt-BR`).
#[must_use]
pub fn category(language: &str, count: i64) -> PluralCategory {
    let base = language
        .split(['-', '_'])
        .next()
        .unwrap_or(language)
        .to_ascii_lowercase();
    let n = count.unsigned_abs();
    let (i10, i100) = (n % 10, n % 100);
    match base.as_str() {
        // No plural forms at all.
        "ja" | "zh" | "ko" | "vi" | "th" | "id" | "ms" | "lo" | "my" | "km" => {
            PluralCategory::Other
        }
        // 0 and 1 are both singular.
        "fr" | "hi" | "bn" | "fa" | "am" | "zu" => {
            if n <= 1 {
                PluralCategory::One
            } else {
                PluralCategory::Other
            }
        }
        // East Slavic.
        "ru" | "uk" | "be" => {
            if i10 == 1 && i100 != 11 {
                PluralCategory::One
            } else if (2..=4).contains(&i10) && !(12..=14).contains(&i100) {
                PluralCategory::Few
            } else {
                PluralCategory::Many
            }
        }
        "pl" => {
            if n == 1 {
                PluralCategory::One
            } else if (2..=4).contains(&i10) && !(12..=14).contains(&i100) {
                PluralCategory::Few
            } else {
                PluralCategory::Many
            }
        }
        "cs" | "sk" => match n {
            1 => PluralCategory::One,
            2..=4 => PluralCategory::Few,
            _ => PluralCategory::Other,
        },
        "ar" => match (n, i100) {
            (0, _) => PluralCategory::Zero,
            (1, _) => PluralCategory::One,
            (2, _) => PluralCategory::Two,
            (_, 3..=10) => PluralCategory::Few,
            (_, 11..=99) => PluralCategory::Many,
            _ => PluralCategory::Other,
        },
        // English and every language that counts like it (de, es, it, nl,
        // pt, sv, tr, el, fi, …).
        _ => {
            if n == 1 {
                PluralCategory::One
            } else {
                PluralCategory::Other
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_and_its_kind_have_one_and_other() {
        assert_eq!(category("en", 1), PluralCategory::One);
        assert_eq!(category("en", 0), PluralCategory::Other);
        assert_eq!(category("en", 2), PluralCategory::Other);
        assert_eq!(category("tr", 1), PluralCategory::One);
        assert_eq!(category("pt-BR", 5), PluralCategory::Other);
        // An unknown language counts like English rather than failing.
        assert_eq!(category("xx", 1), PluralCategory::One);
    }

    #[test]
    fn the_languages_that_count_differently_follow_cldr() {
        assert_eq!(category("fr", 0), PluralCategory::One);
        assert_eq!(category("ja", 1), PluralCategory::Other);
        assert_eq!(category("ru", 21), PluralCategory::One);
        assert_eq!(category("ru", 11), PluralCategory::Many);
        assert_eq!(category("ru", 23), PluralCategory::Few);
        assert_eq!(category("pl", 22), PluralCategory::Few);
        assert_eq!(category("pl", 12), PluralCategory::Many);
        assert_eq!(category("cs", 3), PluralCategory::Few);
        assert_eq!(category("ar", 0), PluralCategory::Zero);
        assert_eq!(category("ar", 2), PluralCategory::Two);
        assert_eq!(category("ar", 103), PluralCategory::Few);
        assert_eq!(category("ar", 111), PluralCategory::Many);
    }
}
