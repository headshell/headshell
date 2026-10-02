//! `ui.json` — the interface's own choices: the theme (§3.3) and the debug
//! mode (D-083).
//!
//! Neither is a core concept, so neither goes to the core's
//! `preferences.json`. Each writer reads the file, changes its own field and
//! writes it back: two choices in one file must not erase each other.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The shape of `ui.json`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiSettings {
    /// The selected theme; the default (the embedded `style.css`) if `None`.
    #[serde(default)]
    pub theme: Option<String>,
    /// Whether the developer's details are shown (D-083).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub debug: bool,
}

/// Reads the file; a missing file is nothing chosen yet.
///
/// # Errors
/// If the file exists but cannot be read, or is not the JSON written here —
/// which file is corrupt is said, not run away from to the defaults.
pub fn read(path: &Path) -> Result<UiSettings, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(UiSettings::default()),
        Err(err) => return Err(format!("could not read {}: {err}", path.display())),
    };
    serde_json::from_str(&text).map_err(|err| format!("{} is corrupt: {err}", path.display()))
}

/// Changes one field and writes the file back: the other choices stay as they
/// were.
///
/// # Errors
/// If the file cannot be read (see [`read`]) or written.
pub fn update(path: &Path, change: impl FnOnce(&mut UiSettings)) -> Result<UiSettings, String> {
    let mut settings = read(path)?;
    change(&mut settings);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&settings)
        .map_err(|err| format!("could not serialise {}: {err}", path.display()))?;
    fs::write(path, text).map_err(|err| format!("could not write {}: {err}", path.display()))?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    // `expect` is free in tests (CLAUDE.md); not on the production path.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::path::PathBuf;

    /// A temporary directory that deletes itself (D-070), as `theme.rs`'s
    /// tests open theirs.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "headshell-ui-settings-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temporary directory");
        TempDir(dir)
    }

    /// The theme and the debug mode share the file: writing one keeps the
    /// other.
    #[test]
    fn writing_one_choice_keeps_the_other() {
        let dir = temp_dir("keep");
        let path = dir.0.join("ui.json");
        assert_eq!(
            read(&path).expect("no file is no choice"),
            UiSettings::default()
        );

        update(&path, |s| s.theme = Some("daylight".to_owned())).expect("written");
        update(&path, |s| s.debug = true).expect("written");
        let both = read(&path).expect("read back");
        assert_eq!(both.theme.as_deref(), Some("daylight"));
        assert!(both.debug);

        update(&path, |s| s.theme = None).expect("written");
        assert!(
            read(&path).expect("read back").debug,
            "the theme's write kept the debug mode"
        );
    }

    /// A file from before D-083 — only a theme — reads with the debug mode off.
    #[test]
    fn a_file_from_before_the_debug_mode_reads_as_off() {
        let dir = temp_dir("old");
        let path = dir.0.join("ui.json");
        fs::write(&path, r#"{ "theme": "contrast" }"#).expect("written");
        let settings = read(&path).expect("read");
        assert_eq!(settings.theme.as_deref(), Some("contrast"));
        assert!(!settings.debug);
    }
}
