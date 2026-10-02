//! The theme loader (PLAN §3.3, D-037/D-038).
//!
//! **Why here and not in `headshell-core`.** The Golden Rule's test is not
//! "will the GUI want this" but "can the core offer this". A theme is a CSS
//! custom property mechanism; the mobile bindings (K7) will not use CSS, and
//! the CLI's `--json` output has no theme. So this is not a core concept but
//! a shell job specific to the webview — and things of this kind are exactly
//! what should stay in the shell.
//!
//! ## The contract
//!
//! A theme is a directory with two files:
//!
//! ```text
//! <data_dir>/themes/<id>/
//!   theme.json   # { "name": ..., "author": ..., "api": 1 }
//!   theme.css    # only :root { --headshell-*: ...; }
//! ```
//!
//! The directory name **is the identity**; the `name` in the manifest is only
//! for display.
//!
//! ## Three rules, all three K9
//!
//! 1. **An `api` mismatch is not silently ignored.** The theme is rejected and
//!    the version it expects is written. "My theme does not show up" should
//!    not be a diagnostic question.
//! 2. **A theme spilling outside `:root` is not rejected, it is flagged**
//!    (D-038). The guarantee only ever holds for the tokens in `:root`; an
//!    extended theme is loaded, but labelled as such in the list.
//! 3. **A lost selection is not swallowed.** If the selected theme was
//!    deleted from disk, it goes back to the default, but says that it did
//!    ([`ActiveTheme::problem`]).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The theme contract version this build supports.
///
/// It goes up when token names or promised class names are removed or change
/// meaning. Adding a new token does not bump it — old themes keep working,
/// they just get the new token's default.
pub const THEME_API: u32 = 1;

/// The built-in reference themes (PLAN §3.4).
///
/// They are embedded at compile time but **have no privileges**: they go
/// through the same validation as a theme on disk. If they were privileged
/// they would not prove the contract is enough, they would only prove
/// themselves.
const BUILTIN: &[Builtin] = &[
    Builtin {
        id: "daylight",
        manifest: include_str!("../themes/daylight/theme.json"),
        css: include_str!("../themes/daylight/theme.css"),
    },
    Builtin {
        id: "contrast",
        manifest: include_str!("../themes/contrast/theme.json"),
        css: include_str!("../themes/contrast/theme.css"),
    },
];

struct Builtin {
    id: &'static str,
    manifest: &'static str,
    css: &'static str,
}

/// The default theme itself: the `:root` block of `style.css` (D-037).
/// The preview takes a token a theme does not write from here — and so does
/// the theme when applied.
const DEFAULT_CSS: &str = include_str!("../ui/style.css");

/// The shape of `theme.json`. Unknown fields are not an error: a theme must
/// not fall over because of a field a later version added.
#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    #[serde(default)]
    author: Option<String>,
    api: u32,
}

/// A loadable theme — the form shown in the list.
#[derive(Debug, Clone, Serialize)]
pub struct ThemeInfo {
    /// The directory name. The selection is stored with this.
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    /// Does it spill outside `:root` (D-038). If `true`, there is no guarantee.
    pub extended: bool,
    /// Did it ship with the app, or did the user put it there.
    pub builtin: bool,
    /// The colour swatches drawn in the theme list (D-072).
    pub preview: ThemePreview,
}

/// The colours a theme shows in the list.
///
/// The values come from the theme's **own** `:root` tokens; a token it does
/// not write takes the default theme's — the same as what it will look like
/// when applied. A conditional value inside an `@media` does not count: the
/// preview shows the one valid in every condition.
///
/// The value is raw CSS text and is not validated: the webview applies it
/// through `style`; if it is invalid the swatch stays empty, the theme does
/// not fall over. `None` means "this token is written nowhere" — its absence
/// from the default theme is a bug, and it is locked down by a test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThemePreview {
    pub bg: Option<String>,
    pub surface: Option<String>,
    pub text: Option<String>,
    pub accent: Option<String>,
}

impl ThemePreview {
    /// The tokens of `css`, the missing ones from the default theme.
    fn of(css: &str) -> Self {
        let defaults = root_tokens(DEFAULT_CSS);
        let own = root_tokens(css);
        let pick = |token: &str| {
            let name = format!("--headshell-{token}");
            own.get(&name).or_else(|| defaults.get(&name)).cloned()
        };
        Self {
            bg: pick("bg"),
            surface: pick("surface"),
            text: pick("text"),
            accent: pick("accent"),
        }
    }
}

/// A rejected theme and **its reason**. No silent skipping (K9).
#[derive(Debug, Clone, Serialize)]
pub struct RejectedTheme {
    pub id: String,
    pub reason: String,
}

/// The answer of `themes_list`.
#[derive(Debug, Clone, Serialize)]
pub struct ThemeList {
    /// The directory the user puts themes in. Shown even if it does not exist —
    /// the question "where do I put it" should be answered in the interface.
    pub dir: PathBuf,
    /// The contract version this build supports.
    pub api: u32,
    /// The selected theme; the default (the embedded `style.css`) if `None`.
    pub active: Option<String>,
    /// The default theme's colour swatches — it is an option in the list too.
    pub default_preview: ThemePreview,
    pub themes: Vec<ThemeInfo>,
    pub rejected: Vec<RejectedTheme>,
}

/// The theme to apply: the CSS the webview will put in its `<style>` tag.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ActiveTheme {
    pub id: Option<String>,
    pub name: Option<String>,
    /// An empty string = the default theme (no token is overridden).
    pub css: String,
    pub extended: bool,
    /// If the selection could not be applied, the reason. It does not go back
    /// to the default **silently**.
    pub problem: Option<String>,
}

/// The theme directory + the selection file. Holds no state: every call
/// reads the disk.
///
/// On purpose: when a theme author edits the file and refreshes the list, they
/// should see the change. A list held in memory would raise the question "why
/// doesn't it change".
pub struct ThemeStore {
    dir: PathBuf,
    settings: PathBuf,
}

impl ThemeStore {
    /// Sets it up from the data directory.
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("themes"),
            settings: data_dir.join("ui.json"),
        }
    }

    /// All of the loadable and rejected themes.
    ///
    /// # Errors
    /// If the theme directory exists but cannot be read. If the directory **does
    /// not exist** that is not an error: not having put a theme there yet is not
    /// a fault.
    pub fn list(&self) -> Result<ThemeList, String> {
        let mut themes = Vec::new();
        let mut rejected = Vec::new();

        for entry in BUILTIN {
            match load_builtin(entry) {
                Ok((info, _)) => themes.push(info),
                Err(reason) => rejected.push(RejectedTheme {
                    id: entry.id.to_owned(),
                    reason,
                }),
            }
        }

        for id in self.disk_ids()? {
            if BUILTIN.iter().any(|entry| entry.id == id) {
                // We say it rather than silently shadowing: the user should not
                // have to guess which file won.
                rejected.push(RejectedTheme {
                    id: id.clone(),
                    reason: format!(
                        "the same name as a built-in theme ({id}); rename the directory to something else"
                    ),
                });
                continue;
            }
            match self.load_from_disk(&id) {
                Ok((info, _)) => themes.push(info),
                Err(reason) => rejected.push(RejectedTheme { id, reason }),
            }
        }

        themes.sort_by(|a, b| a.name.cmp(&b.name));
        rejected.sort_by(|a, b| a.id.cmp(&b.id));

        Ok(ThemeList {
            dir: self.dir.clone(),
            api: THEME_API,
            active: self.selection()?,
            default_preview: ThemePreview::of(""),
            themes,
            rejected,
        })
    }

    /// Loads the saved selection. Called once at startup.
    ///
    /// # Errors
    /// If the selection file cannot be read (a corrupt JSON included) — then
    /// which file is corrupt must be said, not run away from to the default.
    pub fn active(&self) -> Result<ActiveTheme, String> {
        let Some(id) = self.selection()? else {
            return Ok(ActiveTheme::default());
        };
        match self.load(&id) {
            Ok(theme) => Ok(theme),
            // The theme may have been deleted or broken: we go back to the
            // default, but **with the reason**. A silent return would mean the
            // user never learning why their theme disappeared.
            Err(reason) => Ok(ActiveTheme {
                problem: Some(format!(
                    "the selected theme \"{id}\" could not be applied: {reason}"
                )),
                ..ActiveTheme::default()
            }),
        }
    }

    /// Picks a theme and writes the selection persistently. `None` = go back to
    /// the default.
    ///
    /// The order is deliberate: **validate first, then write.** Saving a theme
    /// that cannot be loaded would start the app with a broken selection next
    /// time.
    ///
    /// # Errors
    /// If the theme cannot be loaded or the selection file cannot be written.
    pub fn select(&self, id: Option<String>) -> Result<ActiveTheme, String> {
        let theme = match &id {
            Some(id) => self.load(id)?,
            None => ActiveTheme::default(),
        };
        self.write_selection(id.as_deref())?;
        Ok(theme)
    }

    /// Loads a single theme (built-in or from disk).
    fn load(&self, id: &str) -> Result<ActiveTheme, String> {
        let (info, css) = match BUILTIN.iter().find(|entry| entry.id == id) {
            Some(entry) => load_builtin(entry)?,
            None => self.load_from_disk(id)?,
        };
        Ok(ActiveTheme {
            id: Some(info.id),
            name: Some(info.name),
            css,
            extended: info.extended,
            problem: None,
        })
    }

    fn load_from_disk(&self, id: &str) -> Result<(ThemeInfo, String), String> {
        let dir = self.dir.join(id);
        let manifest = read_file(&dir.join("theme.json"))?;
        let css = read_file(&dir.join("theme.css"))?;
        parse(id, &manifest, &css, false)
    }

    /// The names of the subdirectories in the theme directory. An empty list if
    /// the directory does not exist.
    fn disk_ids(&self) -> Result<Vec<String>, String> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(format!("could not read {}: {err}", self.dir.display())),
        };

        let mut ids = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|err| format!("could not read {}: {err}", self.dir.display()))?;
            if !entry.path().is_dir() {
                continue;
            }
            match entry.file_name().into_string() {
                Ok(name) => ids.push(name),
                // A directory name that is not UTF-8: we do not swallow it, but
                // since we cannot write its identity we cannot list it either — the
                // error drops the whole directory.
                Err(name) => {
                    return Err(format!(
                        "there is a directory name in {} that is not UTF-8: {name:?}",
                        self.dir.display()
                    ));
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn selection(&self) -> Result<Option<String>, String> {
        Ok(crate::ui_settings::read(&self.settings)?.theme)
    }

    /// Writes the selection; the file's other choices (the debug mode, D-083)
    /// stay as they were.
    fn write_selection(&self, id: Option<&str>) -> Result<(), String> {
        crate::ui_settings::update(&self.settings, |settings| {
            settings.theme = id.map(str::to_owned);
        })
        .map(|_| ())
    }
}

fn load_builtin(entry: &Builtin) -> Result<(ThemeInfo, String), String> {
    parse(entry.id, entry.manifest, entry.css, true)
}

fn read_file(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|err| format!("could not read {}: {err}", path.display()))
}

/// Manifest + CSS → a theme. All of the validation is here.
fn parse(
    id: &str,
    manifest: &str,
    css: &str,
    builtin: bool,
) -> Result<(ThemeInfo, String), String> {
    let manifest: Manifest =
        serde_json::from_str(manifest).map_err(|err| format!("theme.json is corrupt: {err}"))?;

    if manifest.api != THEME_API {
        // K9: we reject it, but we do not leave the question **which version**
        // unanswered.
        return Err(format!(
            "the theme wants version {}, this build offers version {THEME_API}",
            manifest.api
        ));
    }

    Ok((
        ThemeInfo {
            id: id.to_owned(),
            name: manifest.name,
            author: manifest.author,
            extended: !targets_only_root(css),
            builtin,
            preview: ThemePreview::of(css),
        },
        css.to_owned(),
    ))
}

/// The `--headshell-*` tokens written in the outermost `:root` blocks.
///
/// The same brace counting as `targets_only_root`: the CSS is not parsed
/// (D-038). A later block overrides an earlier one — the browser's order. The
/// inside of another selector or of an `@media` is not read; the preview
/// shows the unconditional value.
fn root_tokens(css: &str) -> std::collections::BTreeMap<String, String> {
    let css = strip_comments(css);
    let mut tokens = std::collections::BTreeMap::new();
    let mut depth = 0_u32;
    let mut selector = String::new();
    let mut in_root = false;
    let mut declaration = String::new();

    for ch in css.chars() {
        match ch {
            '{' => {
                if depth == 0 {
                    in_root = selector.split_whitespace().collect::<String>() == ":root";
                    selector.clear();
                } else if in_root {
                    declaration.push(ch);
                }
                depth += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if in_root {
                        take_declaration(&mut declaration, &mut tokens);
                    }
                    in_root = false;
                } else if in_root {
                    declaration.push(ch);
                }
            }
            // A rule outside any block, like `@import url(…);`, must not get mixed into
            // the selector.
            ';' if depth == 0 => selector.clear(),
            ';' if depth == 1 && in_root => take_declaration(&mut declaration, &mut tokens),
            _ if depth == 0 => selector.push(ch),
            _ if in_root => declaration.push(ch),
            _ => {}
        }
    }
    tokens
}

/// Reads a `--headshell-name: value` declaration; skips it if it is not a
/// token.
fn take_declaration(
    declaration: &mut String,
    tokens: &mut std::collections::BTreeMap<String, String>,
) {
    if let Some((name, value)) = declaration.split_once(':') {
        let name = name.trim();
        let value = value.trim();
        let value = value
            .strip_suffix("!important")
            .map_or(value, str::trim_end);
        if name.starts_with("--headshell-") && !value.is_empty() {
            tokens.insert(name.to_owned(), value.to_owned());
        }
    }
    declaration.clear();
}

/// Does the CSS only write into the `:root` block?
///
/// D-038: "A simple check that, without parsing CSS, only looks for any
/// selector other than the `:root { ... }` block is enough." That is exactly
/// what is done here — the brace depth is counted, and every selector at the
/// outermost level must be `:root`. A wrapper like `@media` also counts as
/// "extended": changing tokens conditionally is outside the guarantee too.
///
/// The chance of a false positive is an accepted trade-off: a `{` character
/// inside a string can throw the count off. The price is a wrong **label**,
/// not a broken theme — we do not reject (D-038), we only flag.
fn targets_only_root(css: &str) -> bool {
    let css = strip_comments(css);
    let mut depth = 0_u32;
    let mut selector = String::new();

    for ch in css.chars() {
        match ch {
            '{' => {
                if depth == 0 && selector.split_whitespace().collect::<String>() != ":root" {
                    return false;
                }
                depth += 1;
                selector.clear();
            }
            '}' => {
                depth = depth.saturating_sub(1);
                selector.clear();
            }
            _ if depth == 0 => selector.push(ch),
            _ => {}
        }
    }

    // A leftover outside any block: at-rules ending with a semicolon, like
    // `@import url(...)`, are outside the contract too.
    selector.trim().is_empty()
}

/// Removes `/* ... */` comments. So a `{` in a comment does not throw the
/// count off.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            // An unclosed comment: all the rest is a comment.
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    // `expect` is free in tests (CLAUDE.md); not on the production path.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn store(dir: &Path) -> ThemeStore {
        ThemeStore::new(dir)
    }

    /// A temporary directory that deletes itself (D-070). The name carries the
    /// process id: two runs at the same time must not delete and reopen the same
    /// directory.
    struct TempDir(PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "headshell-theme-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temporary directory");
        TempDir(dir)
    }

    fn write_theme(root: &Path, id: &str, manifest: &str, css: &str) {
        let dir = root.join("themes").join(id);
        fs::create_dir_all(&dir).expect("theme directory");
        fs::write(dir.join("theme.json"), manifest).expect("manifest");
        fs::write(dir.join("theme.css"), css).expect("css");
    }

    // ————————————————— scope scan (D-038)

    #[test]
    fn a_root_only_theme_is_not_marked_extended() {
        assert!(targets_only_root(":root { --headshell-bg: #fff; }"));
        assert!(targets_only_root(
            "/* comment { } */\n:root {\n  --headshell-bg: #fff;\n}\n"
        ));
    }

    #[test]
    fn a_theme_that_targets_a_class_is_marked_extended() {
        assert!(!targets_only_root(
            ":root { --headshell-bg: #fff; }\n.topbar { border: 0; }"
        ));
    }

    #[test]
    fn a_media_query_counts_as_extended() {
        // Changing tokens conditionally is outside the guarantee too: the
        // contract says "these tokens have these values", not "it depends".
        assert!(!targets_only_root(
            "@media (prefers-color-scheme: light) { :root { --headshell-bg: #fff; } }"
        ));
    }

    #[test]
    fn a_dangling_at_rule_counts_as_extended() {
        assert!(!targets_only_root(
            "@import url(other.css);\n:root { --headshell-bg: #fff; }"
        ));
    }

    // ————————————————— manifest validation

    #[test]
    fn a_theme_from_a_future_api_is_rejected_with_both_versions() {
        let manifest = format!(r#"{{"name":"Future","api":{}}}"#, THEME_API + 1);
        let err = parse("future", &manifest, ":root {}", false).expect_err("must be rejected");
        // It must say which version it wants **and** what we offer (K9).
        assert!(err.contains(&(THEME_API + 1).to_string()), "{err}");
        assert!(err.contains(&THEME_API.to_string()), "{err}");
    }

    #[test]
    fn a_broken_manifest_is_rejected_not_ignored() {
        let err =
            parse("broken", "{ this is not json", ":root {}", false).expect_err("must be rejected");
        assert!(err.contains("theme.json"), "{err}");
    }

    #[test]
    fn an_unknown_manifest_field_does_not_break_a_theme() {
        // A theme must not fall over because of a field a later version added.
        let manifest = r#"{"name":"Example","api":1,"homepage":"https://example"}"#;
        let (info, _) = parse("example", manifest, ":root {}", false).expect("must load");
        assert_eq!(info.name, "Example");
    }

    // ————————————————— reference themes (PLAN §3.4)

    #[test]
    fn both_reference_themes_load_and_stay_inside_the_contract() {
        assert_eq!(
            BUILTIN.len(),
            2,
            "§3.4 asks for at least two reference themes"
        );
        for entry in BUILTIN {
            let (info, css) = load_builtin(entry).expect("a reference theme must load");
            assert_eq!(info.id, entry.id);
            assert!(info.builtin);
            // A reference theme cannot be extended: it has to prove the contract
            // **is enough**, not that it gets exceeded.
            assert!(!info.extended, "{} spills outside :root", entry.id);
            assert!(
                css.contains("--headshell-bg"),
                "{} writes no tokens",
                entry.id
            );
        }
    }

    #[test]
    fn the_two_reference_themes_differ_on_more_than_color() {
        // "Two themes" is not met by two palettes; the token set's axis other
        // than colour must be tested too (radius, duration).
        let contrast = BUILTIN
            .iter()
            .find(|entry| entry.id == "contrast")
            .expect("the contrast theme");
        assert!(contrast.css.contains("--headshell-radius-sm: 0"));
        assert!(contrast.css.contains("--headshell-duration: 0ms"));
    }

    // ————————————————— directory scan and selection

    #[test]
    fn an_absent_theme_directory_is_not_an_error() {
        let root = temp_dir("no-dir");
        let list = store(&root).list().expect("list");
        assert_eq!(list.active, None);
        // The built-in ones still show up.
        assert_eq!(list.themes.len(), BUILTIN.len());
        assert!(list.rejected.is_empty());
    }

    #[test]
    fn a_rejected_theme_appears_in_the_list_with_its_reason() {
        let root = temp_dir("rejected");
        write_theme(&root, "future", r#"{"name":"Future","api":99}"#, ":root {}");
        let list = store(&root).list().expect("list");
        assert!(!list.themes.iter().any(|t| t.id == "future"));
        let rejected = list
            .rejected
            .iter()
            .find(|t| t.id == "future")
            .expect("must be listed with its reason");
        assert!(rejected.reason.contains("99"), "{}", rejected.reason);
    }

    #[test]
    fn a_disk_theme_that_shadows_a_builtin_is_rejected_by_name() {
        let root = temp_dir("shadow");
        write_theme(&root, "daylight", r#"{"name":"Fake","api":1}"#, ":root {}");
        let list = store(&root).list().expect("list");
        // The built-in one can still be loaded; the one shadowing it does not
        // silently win.
        assert_eq!(list.themes.iter().filter(|t| t.id == "daylight").count(), 1);
        assert!(list.themes.iter().any(|t| t.id == "daylight" && t.builtin));
        assert!(list.rejected.iter().any(|t| t.id == "daylight"));
    }

    #[test]
    fn an_extended_theme_loads_but_is_flagged() {
        // D-038: neither reject nor let it pass — flag it.
        let root = temp_dir("extended");
        write_theme(
            &root,
            "wide",
            r#"{"name":"Wide","api":1}"#,
            ":root { --headshell-bg: #000; }\n.topbar { border: 0; }",
        );
        let list = store(&root).list().expect("list");
        let info = list
            .themes
            .iter()
            .find(|t| t.id == "wide")
            .expect("must load");
        assert!(info.extended);
    }

    #[test]
    fn selecting_a_theme_survives_a_restart() {
        let root = temp_dir("select");
        store(&root)
            .select(Some("daylight".to_owned()))
            .expect("must be selected");
        // A new store = a new startup: the selection is read from disk.
        let active = store(&root).active().expect("must be read");
        assert_eq!(active.id.as_deref(), Some("daylight"));
        assert!(active.css.contains("--headshell-bg"));
        assert_eq!(active.problem, None);
    }

    #[test]
    fn selecting_none_returns_to_the_default_theme() {
        let root = temp_dir("default");
        let store = store(&root);
        store
            .select(Some("contrast".to_owned()))
            .expect("must be selected");
        let active = store.select(None).expect("must go back to the default");
        assert_eq!(active.id, None);
        assert!(active.css.is_empty());
        assert_eq!(store.list().expect("list").active, None);
    }

    #[test]
    fn a_theme_that_cannot_load_is_never_written_as_the_selection() {
        let root = temp_dir("no-write");
        let store = store(&root);
        store
            .select(Some("daylight".to_owned()))
            .expect("must be selected");
        store
            .select(Some("missing".to_owned()))
            .expect_err("must fail");
        // Validate first, then write: the failed selection did not break the old
        // one.
        assert_eq!(
            store.active().expect("must be read").id.as_deref(),
            Some("daylight")
        );
    }

    #[test]
    fn a_selection_pointing_at_a_missing_theme_falls_back_out_loud() {
        let root = temp_dir("missing");
        write_theme(&root, "gone", r#"{"name":"Gone","api":1}"#, ":root {}");
        let store = store(&root);
        store
            .select(Some("gone".to_owned()))
            .expect("must be selected");
        fs::remove_dir_all(root.join("themes").join("gone")).expect("must be deleted");

        let active = store.active().expect("must go back to the default");
        assert_eq!(active.id, None);
        // Had the return been silent, the user would never have learned why their
        // theme disappeared (K9).
        let problem = active.problem.expect("the reason must be given");
        assert!(problem.contains("gone"), "{problem}");
    }

    #[test]
    fn a_broken_settings_file_is_reported_not_swallowed() {
        let root = temp_dir("broken-settings");
        fs::write(root.join("ui.json"), "{ broken").expect("must be written");
        let err = store(&root).active().expect_err("must fail");
        assert!(err.contains("ui.json"), "{err}");
    }

    // ————————————————— preview colours (D-072)

    #[test]
    fn the_default_preview_reads_every_color_it_draws_from_the_stylesheet() {
        // The `:root` of `style.css` is really read: all fourteen tokens
        // must show up. If a token were renamed or the block became
        // unreadable, the default theme's swatch would stay empty.
        let tokens = root_tokens(DEFAULT_CSS);
        assert!(tokens.len() >= 14, "{tokens:?}");
        let preview = ThemePreview::of("");
        for (token, value) in [
            ("bg", &preview.bg),
            ("surface", &preview.surface),
            ("text", &preview.text),
            ("accent", &preview.accent),
        ] {
            assert_eq!(
                value.as_ref(),
                tokens.get(&format!("--headshell-{token}")),
                "{token}"
            );
            assert!(value.is_some(), "the default theme has no `{token}`");
        }
    }

    #[test]
    fn a_theme_inherits_the_tokens_it_does_not_write() {
        // The README's "shortest working theme": the accent only. The preview
        // must show what it will look like when applied — the new accent on the
        // default background.
        let preview = ThemePreview::of(":root {\n  --headshell-accent: #7aa2f7;\n}\n");
        let defaults = ThemePreview::of("");
        assert_eq!(preview.accent.as_deref(), Some("#7aa2f7"));
        assert_eq!(preview.bg, defaults.bg);
        assert_eq!(preview.text, defaults.text);
    }

    #[test]
    fn conditional_and_foreign_rules_do_not_reach_the_preview() {
        // A value inside `@media` is conditional, a value inside `.topbar` is only
        // for that element: neither is the theme's "colour".
        let css = "@media (prefers-color-scheme: light) { :root { --headshell-bg: #ffffff; } }\n\
                   .topbar { --headshell-accent: red; }\n\
                   :root { --headshell-text: #111111; }";
        let preview = ThemePreview::of(css);
        let defaults = ThemePreview::of("");
        assert_eq!(preview.bg, defaults.bg);
        assert_eq!(preview.accent, defaults.accent);
        assert_eq!(preview.text.as_deref(), Some("#111111"));
    }

    #[test]
    fn comments_imports_and_important_do_not_confuse_the_preview() {
        let css = "@import url(other.css);\n\
                   :root {\n  /* --headshell-bg: #000000; */\n  --headshell-surface: #abcdef !important;\n}\n\
                   :root { --headshell-accent: #010203; }";
        let preview = ThemePreview::of(css);
        // The value in the comment was not read…
        assert_eq!(preview.bg, ThemePreview::of("").bg);
        // …`!important` did not get mixed into the value (the webview writes it
        // into `style`, and `!important` would be invalid there)…
        assert_eq!(preview.surface.as_deref(), Some("#abcdef"));
        // …and a rule outside any block did not hide the following `:root`.
        assert_eq!(preview.accent.as_deref(), Some("#010203"));
    }

    #[test]
    fn the_list_carries_each_themes_own_colors() {
        let root = temp_dir("preview");
        let list = store(&root).list().expect("list");
        assert_eq!(list.default_preview, ThemePreview::of(""));
        let daylight = list
            .themes
            .iter()
            .find(|t| t.id == "daylight")
            .expect("built-in theme");
        // The light theme shows its own background, not the default's dark one.
        assert_ne!(daylight.preview.bg, list.default_preview.bg);
        assert!(daylight.preview.bg.is_some());
    }
}
