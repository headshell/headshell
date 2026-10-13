//! The language engine (D-089): every word the app shows comes from a
//! language file.
//!
//! ## Where the words live
//!
//! - **English is built in**: `locales/en.json`, compiled into the core. It
//!   is the source language (K11) and the fallback for every key another
//!   language leaves out, so a missing or broken folder never leaves the
//!   app without words.
//! - **Every other language is a file in the data directory's `locales/`
//!   folder**, one per language, named by its code: `tr.json`, `pt-BR.json`.
//!   The language list is what that folder holds — dropping a file in adds a
//!   language. A file there with the code `en` replaces the built-in text
//!   for the keys it has.
//!
//! ## What a file looks like
//!
//! ```json
//! {
//!   "language": { "code": "tr", "name": "Türkçe", "english_name": "Turkish" },
//!   "strings": {
//!     "settings.language.title": "Dil",
//!     "library.count": { "one": "{count} şarkı", "other": "{count} şarkı" }
//!   }
//! }
//! ```
//!
//! A value is a text, or the plural forms CLDR names (`zero one two few
//! many other`), chosen by the argument named `count`. `{name}` is replaced
//! by the argument of that name.
//!
//! ## What is not silent (K9)
//!
//! A file that can't be read, isn't the shape above or names another code
//! than its file is **listed as a problem** — with the file and the reason
//! — and left out; the app goes on in the languages that work. A language's
//! report counts its missing keys, the keys English doesn't have (a typo,
//! or a key the app dropped) and the texts whose `{placeholders}` differ
//! from English's.
//!
//! ## Two voices for diagnostics
//!
//! A message a developer may be sent — an error chain, `headshell diag`, a
//! warning — is written in the user's language **and** in English, with
//! [`DIAG_SEPARATOR`] between them ([`Translator::diag`]); in English it is
//! written once.

pub mod plural;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
pub use plural::PluralCategory;

/// The source language: the built-in file, and the fallback for every key.
pub const SOURCE_LANGUAGE: &str = "en";

/// What separates the user's language from English in a diagnostic message.
pub const DIAG_SEPARATOR: &str = " ‖ ";

/// The built-in English file.
const ENGLISH: &str = include_str!("../../locales/en.json");

/// A value in a language file: a text, or its plural forms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Text {
    Plain(String),
    Plural(BTreeMap<PluralCategory, String>),
}

/// What a language file says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageMeta {
    /// The BCP 47 code: `tr`, `pt-BR`. Must match the file's name.
    pub code: String,
    /// The language's name in itself: `Türkçe`.
    pub name: String,
    /// The language's name in English: `Turkish`.
    pub english_name: String,
}

/// A language file, as it is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageFile {
    pub language: LanguageMeta,
    pub strings: BTreeMap<String, Text>,
}

/// Where a language comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageOrigin {
    /// Compiled into the app (English).
    BuiltIn,
    /// A file in the languages folder.
    Folder { path: PathBuf },
}

/// One language the app can speak, and how complete it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageInfo {
    pub code: String,
    pub name: String,
    pub english_name: String,
    pub origin: LanguageOrigin,
    /// English's keys this language has.
    pub translated: u32,
    /// English's keys.
    pub total: u32,
    /// Keys English doesn't have: a typo, or a key the app no longer uses.
    pub unknown_keys: Vec<String>,
    /// Keys whose `{placeholders}` differ from English's: they would show
    /// a value in the wrong place, or not at all.
    pub placeholder_mismatches: Vec<String>,
}

/// A file in the languages folder that could not be used, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageProblem {
    pub path: PathBuf,
    pub kind: LanguageProblemKind,
}

/// Why a file in the languages folder could not be used. A kind, not a
/// sentence: the folder is read before the language is known, so the
/// surface words it ([`LanguageProblemKind::key`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LanguageProblemKind {
    /// The folder itself could not be read.
    FolderUnreadable { detail: String },
    /// The file could not be read.
    Unreadable { detail: String },
    /// The file is not JSON of the language file's shape.
    NotALanguageFile { detail: String },
    /// The file names another code than its own name.
    CodeMismatch { code: String, file: String },
    /// The code is not a language code.
    InvalidCode { code: String },
    /// The language has no name.
    NoName,
    /// Another file already gives this language.
    Duplicate { code: String },
}

impl LanguageProblemKind {
    /// The language file key that words this problem.
    #[must_use]
    pub const fn key(&self) -> &'static str {
        match self {
            Self::FolderUnreadable { .. } => "locale.problem.folder_unreadable",
            Self::Unreadable { .. } => "locale.problem.unreadable",
            Self::NotALanguageFile { .. } => "locale.problem.not_a_language_file",
            Self::CodeMismatch { .. } => "locale.problem.code_mismatch",
            Self::InvalidCode { .. } => "locale.problem.invalid_code",
            Self::NoName => "locale.problem.no_name",
            Self::Duplicate { .. } => "locale.problem.duplicate",
        }
    }

    /// The arguments [`Self::key`]'s text is filled with.
    #[must_use]
    pub fn args(&self) -> Args {
        match self {
            Self::FolderUnreadable { detail }
            | Self::Unreadable { detail }
            | Self::NotALanguageFile { detail } => Args::new().text("detail", detail),
            Self::CodeMismatch { code, file } => Args::new().text("code", code).text("file", file),
            Self::InvalidCode { code } | Self::Duplicate { code } => Args::new().text("code", code),
            Self::NoName => Args::new(),
        }
    }
}

/// Who decided the language the app speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageSource {
    /// Chosen in the settings, or with `headshell language use`.
    Saved,
    /// The system's language.
    System,
    /// Nothing chosen and the system's language isn't available: English.
    Default,
}

/// The language the app speaks, and everything that decided it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageChoice {
    /// The code the app speaks.
    pub active: String,
    pub source: LanguageSource,
    /// The saved choice; `None`: follow the system.
    pub saved: Option<String>,
    /// The system's language as it was read; `None`: it says none.
    pub system: Option<String>,
    /// A saved language whose file is gone: the app fell back, and says so
    /// rather than silently forgetting the choice (K9).
    pub unavailable: Option<String>,
}

/// Every language the app can speak, the files it couldn't use, and the
/// folder they come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Languages {
    pub folder: PathBuf,
    pub available: Vec<LanguageInfo>,
    pub problems: Vec<LanguageProblem>,
}

/// A loaded language: its description and its words.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub meta: LanguageMeta,
    pub origin: LanguageOrigin,
    pub strings: BTreeMap<String, Text>,
}

/// The built-in English.
///
/// # Errors
/// Only if the file compiled into the app is broken — a test keeps that
/// from shipping, but the core does not panic over it (K8).
pub fn english() -> Result<Catalog> {
    let file: LanguageFile = serde_json::from_str(ENGLISH).map_err(|source| {
        Error::new(
            Stage::LocaleLoad,
            ErrorKind::Json {
                entry: "locales/en.json (built in)".to_owned(),
                source,
            },
        )
    })?;
    Ok(Catalog {
        meta: file.language,
        origin: LanguageOrigin::BuiltIn,
        strings: file.strings,
    })
}

/// Reads the languages folder. A folder that doesn't exist holds no
/// languages; it is not an error.
#[must_use]
pub(crate) fn scan(folder: &Path) -> (Vec<Catalog>, Vec<LanguageProblem>) {
    let mut catalogs = Vec::new();
    let mut problems = Vec::new();
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (catalogs, problems),
        Err(err) => {
            problems.push(LanguageProblem {
                path: folder.to_path_buf(),
                kind: LanguageProblemKind::FolderUnreadable {
                    detail: err.to_string(),
                },
            });
            return (catalogs, problems);
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    paths.sort();
    let mut seen = BTreeSet::new();
    for path in paths {
        match read_file(&path) {
            Ok(catalog) => {
                if seen.insert(catalog.meta.code.to_ascii_lowercase()) {
                    catalogs.push(catalog);
                } else {
                    let code = catalog.meta.code;
                    problems.push(LanguageProblem {
                        path,
                        kind: LanguageProblemKind::Duplicate { code },
                    });
                }
            }
            Err(kind) => problems.push(LanguageProblem { path, kind }),
        }
    }
    (catalogs, problems)
}

/// Reads one language file; why it can't be used otherwise.
fn read_file(path: &Path) -> std::result::Result<Catalog, LanguageProblemKind> {
    let raw = std::fs::read(path).map_err(|err| LanguageProblemKind::Unreadable {
        detail: err.to_string(),
    })?;
    let file: LanguageFile =
        serde_json::from_slice(&raw).map_err(|err| LanguageProblemKind::NotALanguageFile {
            detail: err.to_string(),
        })?;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    if !stem.eq_ignore_ascii_case(&file.language.code) {
        return Err(LanguageProblemKind::CodeMismatch {
            code: file.language.code,
            file: format!("{stem}.json"),
        });
    }
    if !valid_code(&file.language.code) {
        return Err(LanguageProblemKind::InvalidCode {
            code: file.language.code,
        });
    }
    if file.language.name.trim().is_empty() {
        return Err(LanguageProblemKind::NoName);
    }
    Ok(Catalog {
        meta: file.language,
        origin: LanguageOrigin::Folder {
            path: path.to_path_buf(),
        },
        strings: file.strings,
    })
}

/// A BCP 47 code as far as the app needs: a 2–3 letter language and
/// optional subtags (`pt-BR`, `zh-Hant`).
fn valid_code(code: &str) -> bool {
    let mut parts = code.split('-');
    let Some(language) = parts.next() else {
        return false;
    };
    (2..=3).contains(&language.len())
        && language.chars().all(|c| c.is_ascii_alphabetic())
        && parts.all(|part| {
            (2..=8).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// The `{placeholders}` a text uses, in every plural form.
fn placeholders(text: &Text) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut scan = |s: &str| {
        let mut rest = s;
        while let Some(start) = rest.find('{') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('}') else { break };
            let name = &after[..end];
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                found.insert(name.to_owned());
            }
            rest = &after[end + 1..];
        }
    };
    match text {
        Text::Plain(s) => scan(s),
        Text::Plural(forms) => forms.values().for_each(|s| scan(s)),
    }
    found
}

/// How a language measures against English.
#[must_use]
pub fn describe(catalog: &Catalog, english: &Catalog) -> LanguageInfo {
    let total = english.strings.len();
    let mut translated = 0usize;
    let mut unknown_keys = Vec::new();
    let mut placeholder_mismatches = Vec::new();
    for (key, text) in &catalog.strings {
        match english.strings.get(key) {
            Some(source) => {
                translated += 1;
                if placeholders(text) != placeholders(source) {
                    placeholder_mismatches.push(key.clone());
                }
            }
            None => unknown_keys.push(key.clone()),
        }
    }
    LanguageInfo {
        code: catalog.meta.code.clone(),
        name: catalog.meta.name.clone(),
        english_name: catalog.meta.english_name.clone(),
        origin: catalog.origin.clone(),
        translated: u32::try_from(translated).unwrap_or(u32::MAX),
        total: u32::try_from(total).unwrap_or(u32::MAX),
        unknown_keys,
        placeholder_mismatches,
    }
}

/// The languages the app can speak: English, then the folder's, by code.
/// A folder file with the code `en` takes the built-in English's place in
/// the list.
///
/// # Errors
/// Only if the built-in English is broken.
pub(crate) fn languages(folder: &Path) -> Result<(Languages, Vec<Catalog>)> {
    let english = english()?;
    let (found, problems) = scan(folder);
    let mut catalogs = Vec::with_capacity(found.len() + 1);
    if !found
        .iter()
        .any(|catalog| catalog.meta.code.eq_ignore_ascii_case(SOURCE_LANGUAGE))
    {
        catalogs.push(english.clone());
    }
    catalogs.extend(found);
    catalogs.sort_by(|a, b| {
        let source = |c: &Catalog| !c.meta.code.eq_ignore_ascii_case(SOURCE_LANGUAGE);
        source(a)
            .cmp(&source(b))
            .then_with(|| a.meta.code.cmp(&b.meta.code))
    });
    let available = catalogs
        .iter()
        .map(|catalog| describe(catalog, &english))
        .collect();
    Ok((
        Languages {
            folder: folder.to_path_buf(),
            available,
            problems,
        },
        catalogs,
    ))
}

/// The system's language, from the environment: `LANGUAGE`'s first entry,
/// then `LC_ALL`, `LC_MESSAGES`, `LANG` — the order gettext reads them.
/// `C` and `POSIX` say no language. `tr_TR.UTF-8` reads as `tr-TR`.
#[must_use]
pub fn system_language() -> Option<String> {
    system_language_from(|name| std::env::var(name).ok())
}

fn system_language_from(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(var)
        .find_map(|value| {
            let first = value.split(':').next().unwrap_or_default();
            let tag = first
                .split(['.', '@'])
                .next()
                .unwrap_or_default()
                .replace('_', "-");
            (!tag.is_empty() && tag != "C" && tag != "POSIX").then_some(tag)
        })
}

/// The available code that answers `wanted`: the same code, then its
/// language without the region (`tr-TR` → `tr`).
#[must_use]
pub fn best_match(wanted: &str, available: &[LanguageInfo]) -> Option<String> {
    let find = |code: &str| {
        available
            .iter()
            .find(|info| info.code.eq_ignore_ascii_case(code))
            .map(|info| info.code.clone())
    };
    find(wanted).or_else(|| find(wanted.split('-').next().unwrap_or(wanted)))
}

/// Decides the language: the saved one, the system's, English.
#[must_use]
pub fn choose(
    saved: Option<&str>,
    system: Option<&str>,
    available: &[LanguageInfo],
) -> LanguageChoice {
    let saved_match = saved.and_then(|code| best_match(code, available));
    let system_match = system.and_then(|code| best_match(code, available));
    let (active, source) = match (&saved_match, &system_match) {
        (Some(code), _) => (code.clone(), LanguageSource::Saved),
        (None, Some(code)) => (code.clone(), LanguageSource::System),
        (None, None) => (SOURCE_LANGUAGE.to_owned(), LanguageSource::Default),
    };
    LanguageChoice {
        active,
        source,
        saved: saved.map(str::to_owned),
        system: system.map(str::to_owned),
        unavailable: saved.filter(|_| saved_match.is_none()).map(str::to_owned),
    }
}

/// One named argument of a text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextArg {
    pub name: String,
    pub value: String,
}

/// The arguments a text is filled with. A plain record (K7): the builder
/// methods are a convenience for Rust callers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Args {
    pub values: Vec<TextArg>,
    /// The number that picks the plural form; set by the argument named
    /// `count`.
    pub count: Option<i64>,
}

impl Args {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A text argument: `{name}` is replaced by `value`.
    #[must_use]
    pub fn text(mut self, name: &str, value: &str) -> Self {
        self.values.push(TextArg {
            name: name.to_owned(),
            value: value.to_owned(),
        });
        self
    }

    /// A number argument. The one named `count` also picks the plural form.
    #[must_use]
    pub fn number(mut self, name: &str, value: i64) -> Self {
        if name == "count" {
            self.count = Some(value);
        }
        self.values.push(TextArg {
            name: name.to_owned(),
            value: value.to_string(),
        });
        self
    }
}

/// Speaks one language, falling back to English key by key.
#[derive(Debug, Clone)]
pub struct Translator {
    code: String,
    chosen: Arc<BTreeMap<String, Text>>,
    english: Arc<BTreeMap<String, Text>>,
}

impl Translator {
    /// English alone.
    ///
    /// # Errors
    /// Only if the built-in English is broken.
    pub fn english() -> Result<Self> {
        let english = Arc::new(english()?.strings);
        Ok(Self {
            code: SOURCE_LANGUAGE.to_owned(),
            chosen: Arc::clone(&english),
            english,
        })
    }

    /// `catalog`, falling back to the built-in English (which a folder `en`
    /// file overrides key by key).
    ///
    /// # Errors
    /// Only if the built-in English is broken.
    pub fn new(catalog: &Catalog) -> Result<Self> {
        let mut english = english()?.strings;
        let usable = usable(catalog, &english);
        if catalog.meta.code.eq_ignore_ascii_case(SOURCE_LANGUAGE) {
            english.extend(usable.clone());
        }
        Ok(Self {
            code: catalog.meta.code.clone(),
            chosen: Arc::new(usable),
            english: Arc::new(english),
        })
    }

    /// A translator with no words: every text comes back as `⟦key⟧`. Only
    /// for when even the built-in English can't be read — a surface still
    /// runs, and the gap shows.
    #[must_use]
    pub fn fallback() -> Self {
        let empty = Arc::new(BTreeMap::new());
        Self {
            code: SOURCE_LANGUAGE.to_owned(),
            chosen: Arc::clone(&empty),
            english: empty,
        }
    }

    /// The code this translator speaks.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Whether this translator speaks the source language.
    #[must_use]
    pub fn is_source(&self) -> bool {
        self.code.eq_ignore_ascii_case(SOURCE_LANGUAGE)
    }

    /// The text for `key` in this language; English where it has none. A
    /// key English doesn't have either comes back as `⟦key⟧` — visible, so
    /// the gap is seen and a test can catch it.
    #[must_use]
    pub fn text(&self, key: &str, args: &Args) -> String {
        match self.chosen.get(key) {
            Some(text) => render(text, &self.code, args),
            None => self.english_text(key, args),
        }
    }

    /// The text for `key` in English.
    #[must_use]
    pub fn english_text(&self, key: &str, args: &Args) -> String {
        match self.english.get(key) {
            Some(text) => render(text, SOURCE_LANGUAGE, args),
            None => format!("⟦{key}⟧"),
        }
    }

    /// A diagnostic message: this language, [`DIAG_SEPARATOR`], English —
    /// or English once, when that is the language.
    #[must_use]
    pub fn diag(&self, key: &str, args: &Args) -> String {
        let english = self.english_text(key, args);
        if self.is_source() {
            return english;
        }
        let own = self.text(key, args);
        if own == english {
            english
        } else {
            format!("{own}{DIAG_SEPARATOR}{english}")
        }
    }
}

/// A language's texts the app will show: a text whose `{placeholders}`
/// differ from English's would put a value in the wrong place or drop it,
/// so English's words stand in for it — the language's report counts it
/// ([`LanguageInfo::placeholder_mismatches`]).
fn usable(catalog: &Catalog, english: &BTreeMap<String, Text>) -> BTreeMap<String, Text> {
    catalog
        .strings
        .iter()
        .filter(|(key, text)| {
            english
                .get(*key)
                .is_none_or(|source| placeholders(text) == placeholders(source))
        })
        .map(|(key, text)| (key.clone(), text.clone()))
        .collect()
}

/// Fills a text: the plural form `count` picks, then `{name}` by name.
fn render(text: &Text, language: &str, args: &Args) -> String {
    let template = match text {
        Text::Plain(s) => s.as_str(),
        Text::Plural(forms) => {
            let wanted = plural::category(language, args.count.unwrap_or(0));
            forms
                .get(&wanted)
                .or_else(|| forms.get(&PluralCategory::Other))
                .or_else(|| forms.values().next())
                .map_or("", String::as_str)
        }
    };
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let value = after.find('}').and_then(|end| {
            let name = &after[..end];
            args.values
                .iter()
                .rev()
                .find(|arg| arg.name == name)
                .map(|arg| (arg.value.as_str(), end))
        });
        match value {
            Some((value, end)) => {
                out.push_str(value);
                rest = &after[end + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What the desktop's interface is given: the active language's own words,
/// and English whole — the fallback for every key the language leaves out,
/// and the second voice of a diagnostic message.
///
/// Kept apart rather than merged, so the page knows which words fell back:
/// those it marks `lang="en"`, or the language's rules would apply to
/// English text (a Turkish uppercase turns "listening" into "LİSTENİNG").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocaleBundle {
    pub code: String,
    pub name: String,
    /// The language's own texts; empty for English.
    pub strings: BTreeMap<String, Text>,
    pub english: BTreeMap<String, Text>,
}

impl LocaleBundle {
    /// The bundle for `catalog`.
    ///
    /// # Errors
    /// Only if the built-in English is broken.
    pub fn new(catalog: &Catalog) -> Result<Self> {
        let translator = Translator::new(catalog)?;
        let strings = if translator.is_source() {
            BTreeMap::new()
        } else {
            translator.chosen.as_ref().clone()
        };
        Ok(Self {
            code: catalog.meta.code.clone(),
            name: catalog.meta.name.clone(),
            strings,
            english: translator.english.as_ref().clone(),
        })
    }
}

#[cfg(test)]
mod tests;
