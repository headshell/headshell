//! Every word the window shows comes from the language files (D-089).
//!
//! The webview would show a key the bundle lacks as `⟦key⟧` and say nothing
//! else; the compiler doesn't see it either. This file does:
//!
//! 1. Every key the markup names (`data-i18n`, `data-i18n-attr`) and every
//!    literal key the scripts ask for (`t("…")`, `diag("…")`, `english("…")`)
//!    is in the built-in English.
//! 2. The English a keyed element carries in the markup — what shows for
//!    the instant before the words arrive — is the file's English, so the
//!    two can't drift apart.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

const ENGLISH: &str = include_str!("../../headshell-core/locales/en.json");
const INDEX_HTML: &str = include_str!("../ui/index.html");
const SCRIPTS: [(&str, &str); 7] = [
    ("app.js", include_str!("../ui/app.js")),
    ("menu.js", include_str!("../ui/menu.js")),
    ("motion.js", include_str!("../ui/motion.js")),
    ("anchor.js", include_str!("../ui/anchor.js")),
    ("lyrics.js", include_str!("../ui/lyrics.js")),
    ("video.js", include_str!("../ui/video.js")),
    ("words.js", include_str!("../ui/words.js")),
];

/// The built-in English: key → its text (a plural's `other` form).
fn english() -> BTreeMap<String, String> {
    let file: serde_json::Value = serde_json::from_str(ENGLISH).expect("en.json must parse");
    file["strings"]
        .as_object()
        .expect("en.json has no strings")
        .iter()
        .map(|(key, value)| {
            let text = value
                .as_str()
                .or_else(|| value["other"].as_str())
                .unwrap_or_default();
            (key.clone(), text.to_owned())
        })
        .collect()
}

/// The `name="…"` values in the markup, with what follows each tag.
fn attribute_values<'a>(html: &'a str, name: &str) -> Vec<(&'a str, &'a str)> {
    let marker = format!("{name}=\"");
    let mut found = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find(&marker) {
        let after = &rest[start + marker.len()..];
        let end = after.find('"').unwrap();
        found.push((&after[..end], &after[end + 1..]));
        rest = &after[end + 1..];
    }
    found
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn every_key_the_markup_names_is_in_english_and_its_fallback_is_englishs() {
    let english = english();
    let keyed = attribute_values(INDEX_HTML, "data-i18n");
    assert!(!keyed.is_empty(), "the scan found no keyed markup");
    for (key, after) in keyed {
        let text = english
            .get(key)
            .unwrap_or_else(|| panic!("index.html names `{key}`, which en.json lacks"));
        // The element's own text: after its tag closes, up to the next tag.
        let inner = after
            .split_once('>')
            .map(|(_, inner)| inner.split('<').next().unwrap_or_default())
            .unwrap_or_default();
        assert_eq!(
            collapse(inner),
            collapse(text),
            "index.html's fallback for `{key}` is not en.json's English"
        );
    }
    for (pairs, _) in attribute_values(INDEX_HTML, "data-i18n-attr") {
        for pair in pairs.split(';') {
            let key = pair.split('=').nth(1).unwrap_or_default().trim();
            assert!(
                english.contains_key(key),
                "index.html names `{key}`, which en.json lacks"
            );
        }
    }
}

#[test]
fn every_key_the_scripts_ask_for_is_in_english() {
    let english = english();
    let mut asked = 0;
    for (name, source) in SCRIPTS {
        for function in ["t", "diag", "english"] {
            let call = format!("{function}(\"");
            let mut rest = source;
            while let Some(start) = rest.find(&call) {
                let before = rest[..start].chars().next_back();
                let after = &rest[start + call.len()..];
                rest = after;
                // `split("…")` ends in `t("`: only a call of the function
                // itself counts.
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                    continue;
                }
                let key = &after[..after.find('"').unwrap()];
                asked += 1;
                assert!(
                    english.contains_key(key),
                    "{name} asks for `{key}`, which en.json lacks"
                );
            }
        }
    }
    assert!(asked > 0, "the scan found no calls");
}
