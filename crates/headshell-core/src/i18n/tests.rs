use super::*;
use crate::test_support::TempDir;

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn turkish(strings: &str) -> String {
    format!(
        r#"{{ "language": {{ "code": "tr", "name": "Türkçe", "english_name": "Turkish" }}, "strings": {{ {strings} }} }}"#
    )
}

#[test]
fn the_built_in_english_reads_and_every_text_has_words() {
    let english = english().unwrap();
    assert_eq!(english.meta.code, SOURCE_LANGUAGE);
    assert_eq!(english.origin, LanguageOrigin::BuiltIn);
    assert!(!english.strings.is_empty());
    for (key, text) in &english.strings {
        match text {
            Text::Plain(s) => assert!(!s.trim().is_empty(), "{key} is empty"),
            Text::Plural(forms) => {
                assert!(
                    forms.contains_key(&PluralCategory::Other),
                    "{key} has no `other` form"
                );
                assert!(
                    forms.values().all(|s| !s.trim().is_empty()),
                    "{key} has an empty form"
                );
            }
        }
    }
}

/// Every problem the folder can have is worded by a key English has, and
/// the key's placeholders are exactly the arguments the problem gives.
#[test]
fn every_problem_kind_is_worded_by_english() {
    let english = english().unwrap();
    let kinds = [
        LanguageProblemKind::FolderUnreadable { detail: "d".into() },
        LanguageProblemKind::Unreadable { detail: "d".into() },
        LanguageProblemKind::NotALanguageFile { detail: "d".into() },
        LanguageProblemKind::CodeMismatch {
            code: "tr".into(),
            file: "de.json".into(),
        },
        LanguageProblemKind::InvalidCode { code: "x".into() },
        LanguageProblemKind::NoName,
        LanguageProblemKind::Duplicate { code: "tr".into() },
    ];
    for kind in kinds {
        // The desktop builds the key from the serialized tag: the two must
        // stay one name.
        let tag = serde_json::to_value(&kind).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(kind.key(), format!("locale.problem.{tag}"));
        let text = english
            .strings
            .get(kind.key())
            .unwrap_or_else(|| panic!("{} is not in en.json", kind.key()));
        let given: BTreeSet<String> = kind.args().values.into_iter().map(|arg| arg.name).collect();
        assert_eq!(placeholders(text), given, "{}", kind.key());
    }
}

#[test]
fn a_folder_that_does_not_exist_holds_no_languages_and_is_no_problem() {
    let dir = TempDir::new("i18n-missing");
    let (catalogs, problems) = scan(&dir.path().join("locales"));
    assert!(catalogs.is_empty());
    assert!(problems.is_empty());
}

#[test]
fn a_file_that_cannot_be_used_is_listed_with_its_reason_not_skipped() {
    let dir = TempDir::new("i18n-problems");
    let folder = dir.path();
    write(folder, "tr.json", &turkish(r#""a": "b""#));
    write(folder, "de.json", "{ not json");
    write(
        folder,
        "fr.json",
        r#"{ "language": { "code": "es", "name": "Español", "english_name": "Spanish" }, "strings": {} }"#,
    );
    write(
        folder,
        "x.json",
        r#"{ "language": { "code": "x", "name": "X", "english_name": "X" }, "strings": {} }"#,
    );
    write(
        folder,
        "it.json",
        r#"{ "language": { "code": "it", "name": " ", "english_name": "Italian" }, "strings": {} }"#,
    );
    write(folder, "README.txt", "not a language");

    let (catalogs, problems) = scan(folder);
    let codes: Vec<&str> = catalogs.iter().map(|c| c.meta.code.as_str()).collect();
    assert_eq!(codes, ["tr"]);

    let kind_of = |file: &str| {
        problems
            .iter()
            .find(|p| p.path.file_name().and_then(|n| n.to_str()) == Some(file))
            .map(|p| p.kind.clone())
            .unwrap_or_else(|| panic!("{file} was not listed: {problems:?}"))
    };
    assert!(matches!(
        kind_of("de.json"),
        LanguageProblemKind::NotALanguageFile { .. }
    ));
    assert_eq!(
        kind_of("fr.json"),
        LanguageProblemKind::CodeMismatch {
            code: "es".into(),
            file: "fr.json".into()
        }
    );
    assert_eq!(
        kind_of("x.json"),
        LanguageProblemKind::InvalidCode { code: "x".into() }
    );
    assert_eq!(kind_of("it.json"), LanguageProblemKind::NoName);
    // Not a `.json`: not a language file at all, so not a problem either.
    assert_eq!(problems.len(), 4, "{problems:?}");
}

#[test]
fn a_language_is_measured_against_english() {
    let english = Catalog {
        meta: LanguageMeta {
            code: "en".into(),
            name: "English".into(),
            english_name: "English".into(),
        },
        origin: LanguageOrigin::BuiltIn,
        strings: BTreeMap::from([
            ("hello".into(), Text::Plain("Hello, {name}".into())),
            ("bye".into(), Text::Plain("Bye".into())),
            ("songs".into(), Text::Plain("{count} songs".into())),
        ]),
    };
    let dir = TempDir::new("i18n-describe");
    let path = write(
        dir.path(),
        "tr.json",
        &turkish(r#""hello": "Merhaba, {ad}", "bye": "Hoşça kal", "typo": "?""#),
    );
    let catalog = read_file(&path).unwrap();
    let info = describe(&catalog, &english);
    assert_eq!((info.translated, info.total), (2, 3));
    assert_eq!(info.unknown_keys, ["typo"]);
    assert_eq!(info.placeholder_mismatches, ["hello"]);
}

#[test]
fn the_list_starts_with_english_and_a_folder_en_takes_its_place() {
    let dir = TempDir::new("i18n-list");
    write(dir.path(), "tr.json", &turkish(""));
    let (listed, catalogs) = languages(dir.path()).unwrap();
    let codes: Vec<&str> = listed.available.iter().map(|l| l.code.as_str()).collect();
    assert_eq!(codes, ["en", "tr"]);
    assert_eq!(listed.available[0].origin, LanguageOrigin::BuiltIn);
    assert_eq!(catalogs.len(), 2);

    write(
        dir.path(),
        "en.json",
        r#"{ "language": { "code": "en", "name": "English (mine)", "english_name": "English" }, "strings": {} }"#,
    );
    let (listed, _) = languages(dir.path()).unwrap();
    assert_eq!(listed.available.len(), 2);
    assert_eq!(listed.available[0].name, "English (mine)");
    assert!(matches!(
        listed.available[0].origin,
        LanguageOrigin::Folder { .. }
    ));
}

#[test]
fn the_system_language_is_read_the_way_gettext_reads_it() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    };
    assert_eq!(
        system_language_from(env(&[("LANG", "tr_TR.UTF-8")])),
        Some("tr-TR".into())
    );
    assert_eq!(
        system_language_from(env(&[("LANGUAGE", "de:en"), ("LANG", "tr_TR.UTF-8")])),
        Some("de".into())
    );
    assert_eq!(
        system_language_from(env(&[("LC_ALL", "C"), ("LANG", "C.UTF-8")])),
        None
    );
    assert_eq!(
        system_language_from(env(&[("LANG", "sr_RS@latin")])),
        Some("sr-RS".into())
    );
    assert_eq!(system_language_from(env(&[])), None);
}

fn info(code: &str) -> LanguageInfo {
    LanguageInfo {
        code: code.into(),
        name: code.into(),
        english_name: code.into(),
        origin: LanguageOrigin::BuiltIn,
        translated: 0,
        total: 0,
        unknown_keys: Vec::new(),
        placeholder_mismatches: Vec::new(),
    }
}

#[test]
fn a_region_falls_back_to_its_language() {
    let available = [info("en"), info("tr"), info("pt-BR")];
    assert_eq!(best_match("tr-TR", &available), Some("tr".into()));
    assert_eq!(best_match("PT-br", &available), Some("pt-BR".into()));
    assert_eq!(best_match("de", &available), None);
}

#[test]
fn the_saved_choice_wins_then_the_system_then_english_and_a_lost_one_is_said() {
    let available = [info("en"), info("tr")];

    let choice = choose(Some("tr"), Some("de-DE"), &available);
    assert_eq!(
        (choice.active.as_str(), choice.source),
        ("tr", LanguageSource::Saved)
    );
    assert_eq!(choice.unavailable, None);

    let choice = choose(None, Some("tr-TR"), &available);
    assert_eq!(
        (choice.active.as_str(), choice.source),
        ("tr", LanguageSource::System)
    );

    let choice = choose(None, Some("de-DE"), &available);
    assert_eq!(
        (choice.active.as_str(), choice.source),
        ("en", LanguageSource::Default)
    );

    // The saved file is gone: the system decides, and the loss is said.
    let choice = choose(Some("fr"), Some("tr"), &available);
    assert_eq!(
        (choice.active.as_str(), choice.source),
        ("tr", LanguageSource::System)
    );
    assert_eq!(choice.unavailable, Some("fr".into()));
}

fn translator(strings: &str) -> Translator {
    let dir = TempDir::new("i18n-translator");
    let path = write(dir.path(), "tr.json", &turkish(strings));
    Translator::new(&read_file(&path).unwrap()).unwrap()
}

#[test]
fn a_text_is_filled_and_a_missing_one_falls_back_to_english() {
    let tr = translator(r#""cli.language.folder": "Dil klasörü: {folder}""#);
    assert_eq!(
        tr.text("cli.language.folder", &Args::new().text("folder", "/x")),
        "Dil klasörü: /x"
    );
    // Not translated: English's words.
    assert_eq!(
        tr.text("cli.language.problems", &Args::new()),
        tr.english_text("cli.language.problems", &Args::new())
    );
    // Not in English either: visible, not silently empty.
    assert_eq!(tr.text("no.such.key", &Args::new()), "⟦no.such.key⟧");
    // An argument not given stays as written.
    assert_eq!(
        tr.text("cli.language.folder", &Args::new()),
        "Dil klasörü: {folder}"
    );
}

/// A translation that lost or renamed a placeholder would show `{dosya}`
/// or drop the value: English's words stand in, and the report counts it.
#[test]
fn a_text_with_other_placeholders_than_english_is_not_shown() {
    let tr = translator(r#""cli.language.folder": "klasör: {dosya}""#);
    let args = Args::new().text("folder", "/x");
    assert_eq!(
        tr.text("cli.language.folder", &args),
        tr.english_text("cli.language.folder", &args)
    );
}

#[test]
fn count_picks_the_plural_form() {
    let en = Translator::english().unwrap();
    let one = en.text("cli.language.unknown_keys", &Args::new().number("count", 1));
    let many = en.text("cli.language.unknown_keys", &Args::new().number("count", 3));
    assert_eq!(one, "1 key English doesn't have");
    assert_eq!(many, "3 keys English doesn't have");
}

#[test]
fn a_diagnostic_speaks_both_languages_and_english_once() {
    let en = Translator::english().unwrap();
    let args = Args::new().text("code", "fr").text("name", "English");
    let english = en.diag("locale.unavailable", &args);
    assert!(!english.contains(DIAG_SEPARATOR), "{english}");

    let tr = translator(
        r#""locale.unavailable": "Seçilen dil, {code}, artık yok; {name} konuşuluyor.""#,
    );
    let both = tr.diag("locale.unavailable", &args);
    assert_eq!(
        both,
        format!("Seçilen dil, fr, artık yok; English konuşuluyor.{DIAG_SEPARATOR}{english}")
    );

    // Not translated: the two voices would say the same thing — once.
    assert_eq!(
        tr.diag("cli.language.problems", &Args::new()),
        en.diag("cli.language.problems", &Args::new())
    );
}

/// The bundle keeps the language's own words apart from English's, so the
/// page knows which texts fell back — and a text with broken placeholders
/// is not among the language's own.
#[test]
fn the_bundle_carries_the_languages_own_words_and_english_whole() {
    let dir = TempDir::new("i18n-bundle");
    let path = write(
        dir.path(),
        "tr.json",
        &turkish(
            r#""cli.language.problems": "Kullanılamayanlar:", "cli.language.folder": "klasör: {dosya}""#,
        ),
    );
    let bundle = LocaleBundle::new(&read_file(&path).unwrap()).unwrap();
    assert_eq!(bundle.code, "tr");
    assert_eq!(
        bundle.strings.keys().collect::<Vec<_>>(),
        ["cli.language.problems"]
    );
    assert_eq!(bundle.english, english().unwrap().strings);

    let english_bundle = LocaleBundle::new(&english().unwrap()).unwrap();
    assert!(english_bundle.strings.is_empty());
}
