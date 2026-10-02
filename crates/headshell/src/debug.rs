//! The debug mode (D-083): diagnostics, raw data and internal identifiers —
//! the developer's details — are shown only while it is on.
//!
//! Like the theme, it is the interface's own choice, kept in `ui.json`; the
//! core has no part in it, and the CLI — the developer's surface already —
//! has no counterpart. `DEBUG=1` turns it on for one launch, over the saved
//! choice, as `HEADSHELL_ONLINE` does for the online choice (D-082).

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The variable that turns the debug mode on for one launch.
pub const DEBUG_ENV: &str = "DEBUG";

/// What [`DEBUG_ENV`] asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Variable {
    /// Unset or empty: the saved choice decides.
    Unset,
    /// Exactly `1`: on for this launch.
    On,
    /// Anything else. The name is shared — Node's `DEBUG=*`, a Makefile's
    /// `DEBUG=yes` — so it is not refused (the app would not start for a
    /// variable meant for another program); it is left alone and said in the
    /// log, so a `DEBUG=true` that went unrecognised does not stay a mystery.
    Ignored(String),
}

impl Variable {
    /// [`DEBUG_ENV`] as this process has it.
    #[must_use]
    pub fn read() -> Self {
        Self::of(std::env::var_os(DEBUG_ENV).map(|raw| raw.to_string_lossy().into_owned()))
    }

    fn of(raw: Option<String>) -> Self {
        match raw {
            None => Self::Unset,
            Some(raw) => match raw.trim() {
                "" => Self::Unset,
                "1" => Self::On,
                other => Self::Ignored(other.to_owned()),
            },
        }
    }
}

/// The debug mode as the interface needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DebugMode {
    /// Whether the details are shown.
    pub on: bool,
    /// `DEBUG=1` holds this launch on: the switch cannot turn it off.
    pub held: bool,
    /// The saved choice.
    pub saved: bool,
}

/// The saved choice and the variable read at startup.
pub struct DebugSwitch {
    settings: PathBuf,
    held: bool,
}

impl DebugSwitch {
    /// `held`: the variable asked for it ([`Variable::On`]).
    #[must_use]
    pub fn new(data_dir: &Path, held: bool) -> Self {
        Self {
            settings: data_dir.join("ui.json"),
            held,
        }
    }

    /// The mode as it stands.
    ///
    /// # Errors
    /// If `ui.json` cannot be read.
    pub fn mode(&self) -> Result<DebugMode, String> {
        let saved = crate::ui_settings::read(&self.settings)?.debug;
        Ok(self.with(saved))
    }

    /// Saves the choice. While the variable holds the launch the mode stays
    /// on; the choice is kept for a launch without it.
    ///
    /// # Errors
    /// If `ui.json` cannot be read or written.
    pub fn set(&self, on: bool) -> Result<DebugMode, String> {
        let saved =
            crate::ui_settings::update(&self.settings, |settings| settings.debug = on)?.debug;
        Ok(self.with(saved))
    }

    const fn with(&self, saved: bool) -> DebugMode {
        DebugMode {
            on: self.held || saved,
            held: self.held,
            saved,
        }
    }
}

#[cfg(test)]
mod tests {
    // `expect` is free in tests (CLAUDE.md); not on the production path.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// Only exactly `1` turns it on; other programs' values are left alone.
    #[test]
    fn only_one_turns_it_on_and_other_values_are_left_alone() {
        assert_eq!(Variable::of(None), Variable::Unset);
        assert_eq!(Variable::of(Some(String::new())), Variable::Unset);
        assert_eq!(Variable::of(Some(" 1 ".to_owned())), Variable::On);
        assert_eq!(
            Variable::of(Some("*".to_owned())),
            Variable::Ignored("*".to_owned())
        );
        assert_eq!(
            Variable::of(Some("true".to_owned())),
            Variable::Ignored("true".to_owned())
        );
        assert_eq!(
            Variable::of(Some("0".to_owned())),
            Variable::Ignored("0".to_owned())
        );
    }

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "headshell-debug-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temporary directory");
        TempDir(dir)
    }

    /// The saved choice comes back; the variable holds a launch on over it.
    #[test]
    fn the_choice_is_saved_and_the_variable_holds_a_launch_on() {
        let dir = temp_dir("switch");
        let free = DebugSwitch::new(&dir.0, false);
        assert_eq!(
            free.mode().unwrap(),
            DebugMode {
                on: false,
                held: false,
                saved: false
            }
        );
        assert!(free.set(true).unwrap().on);
        assert!(free.mode().unwrap().on, "kept across a restart");

        let held = DebugSwitch::new(&dir.0, true);
        let off = held.set(false).unwrap();
        assert_eq!(
            off,
            DebugMode {
                on: true,
                held: true,
                saved: false
            }
        );
    }
}
