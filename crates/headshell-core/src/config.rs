//! The data directory and configuration.
//!
//! Path resolution lives in the core: the CLI, the GUI and mobile must find
//! the same directory.
//!
//! ## Platforms (D-070)
//!
//! The data directory lives in each operating system's **own** place; the
//! first version only looked at `HOME`, and since standard Windows does not
//! define `HOME`, it never opened there:
//!
//! | system | default |
//! |---|---|
//! | Linux, BSD and the other Unixes | `$XDG_DATA_HOME/headshell` → `~/.local/share/headshell` |
//! | macOS | `~/Library/Application Support/headshell` |
//! | Windows | `%LOCALAPPDATA%\headshell` |
//!
//! `HEADSHELL_DATA_DIR` comes first on every system; if `XDG_DATA_HOME` is
//! explicitly defined it is honoured on macOS and Windows too — the user put
//! it there on purpose.
//!
//! The environment is read in pure functions ([`resolve_data_dir`],
//! [`resolve_music_folders`]): in Rust 2024 `set_var` is `unsafe` and the
//! workspace says `unsafe_code = "forbid"`, so this way the Windows and macOS
//! branches are tested on every machine.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};

/// Environment variable for giving the data directory by hand (tests and
/// portable installs).
pub const DATA_DIR_ENV: &str = "HEADSHELL_DATA_DIR";

/// Environment variable for giving the music directories by hand. It
/// decides over the folders chosen in the app (D-085).
///
/// The list is written like `PATH`: separated by `:` on Unix and `;` on
/// Windows — so the colon inside a path like `C:\Music` does not count as a
/// separator.
pub const MUSIC_DIRS_ENV: &str = "HEADSHELL_MUSIC_DIRS";

/// Who decided the music folders (D-085).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderSource {
    /// [`MUSIC_DIRS_ENV`], for this run.
    Variable,
    /// The folders chosen in the app, kept in `preferences.json`.
    Saved,
    /// Nothing chosen: the system's own music folder.
    Default,
}

/// The music folders the local provider reads, and who decided them
/// (D-085).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MusicFolders {
    /// What the local provider reads now.
    pub dirs: Vec<PathBuf>,
    /// Who decided `dirs` (K9: a choice that does not hold is said).
    pub source: FolderSource,
    /// The folders chosen in the app; `None`: nothing chosen. An empty list
    /// is a choice too — no local folders.
    pub saved: Option<Vec<PathBuf>>,
    /// The system's own music folder(s): what "nothing chosen" means here.
    pub system: Vec<PathBuf>,
}

/// The paths the core needs to work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    data_dir: PathBuf,
}

impl Config {
    /// Uses the given directory.
    #[must_use]
    pub fn with_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// Finds the data directory from the environment — by this operating
    /// system's rules.
    ///
    /// # Errors
    /// If no candidate is found. Silently falling back to a temporary directory
    /// would mean losing the user's history without them noticing; the error
    /// says which variables were looked at on this system.
    pub fn discover() -> Result<Self> {
        match resolve_data_dir(std::env::consts::OS, &non_empty_env) {
            Some(dir) => Ok(Self::with_data_dir(dir)),
            None => Err(Error::new(
                Stage::ConfigLoad,
                ErrorKind::NotFound {
                    what: format!(
                        "data directory — none of the variables looked at on this system is defined: {}",
                        data_dir_sources(std::env::consts::OS).join(", ")
                    ),
                },
            )),
        }
    }

    /// The data directory.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The library database.
    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("library.db")
    }

    /// Registered remote servers and credentials (D-021).
    ///
    /// A file separate from the database: credentials are not library data, and
    /// keeping them in the same file would muddle backup/sharing behaviour.
    /// Written as `0600` on Unix.
    #[must_use]
    pub fn servers_path(&self) -> PathBuf {
        self.data_dir.join("servers.json")
    }

    /// The user's choices that are not library data (D-080): today, the
    /// provider the play box searches first. Deleting it only forgets them.
    #[must_use]
    pub fn preferences_path(&self) -> PathBuf {
        self.data_dir.join("preferences.json")
    }

    /// The namespaced secret store (D-042). Written as `0600` on Unix.
    ///
    /// Separate from `servers.json`, and deliberately so: what lives there is a
    /// **server record** (address + type + user), the secret is one field of that
    /// record. What lives here is the secret itself, its owner given by the
    /// namespace.
    #[must_use]
    pub fn secrets_path(&self) -> PathBuf {
        self.data_dir.join("secrets.json")
    }

    /// The cover cache (D-076): the image files, named by their content hash,
    /// and `index.json`, which says what was found — or not found — for which
    /// album. A cache: deleting it only costs fetching again.
    #[must_use]
    pub fn artwork_dir(&self) -> PathBuf {
        self.data_dir.join("artwork")
    }

    /// Where lyrics looked up at LRCLIB and the user's shifts of lyrics are
    /// kept (D-078). A cache, except the shifts: deleting it costs asking
    /// again, and shifting again.
    #[must_use]
    pub fn lyrics_dir(&self) -> PathBuf {
        self.data_dir.join("lyrics")
    }

    /// The directory plugins live in: `<data_dir>/plugins/<name>/plugin.json`.
    #[must_use]
    pub fn plugins_dir(&self) -> PathBuf {
        self.data_dir.join("plugins")
    }

    /// The plugin permission consent ledger (D-040).
    #[must_use]
    pub fn plugin_consent_path(&self) -> PathBuf {
        self.data_dir.join("plugins.json")
    }

    /// The address of the plugin catalog (D-071).
    ///
    /// `HEADSHELL_PLUGIN_INDEX` if given — a fork, a mirror or a local server for
    /// testing — otherwise the index of the `headshell/plugins` repository. It is
    /// read from the environment on every call, like `music_dirs`.
    #[must_use]
    pub fn plugin_index_url(&self) -> String {
        crate::plugin::catalog::resolve_index_url(&non_empty_env)
    }

    /// A plugin's state directory: the file of `host.storage` and the working
    /// directory of the tools the engine installed (D-069). The plugin itself
    /// cannot touch the file system; the engine uses this directory on its
    /// behalf.
    #[must_use]
    pub fn plugin_state_dir(&self, plugin: &str) -> PathBuf {
        self.plugins_dir().join(plugin).join("state")
    }

    /// The artifacts the plugin engine installs (D-055): `<data_dir>/runtime`.
    ///
    /// **Outside** `plugins_dir`, and on purpose: an artifact does not belong to a
    /// single plugin. If two plugins want the same `yt-dlp` version they share
    /// the same file, and when one plugin is deleted the other's runtime does
    /// not go with it.
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.data_dir.join("runtime")
    }

    /// The last run's diagnostics report.
    #[must_use]
    pub fn last_run_path(&self) -> PathBuf {
        self.data_dir.join("last-run.json")
    }

    /// The music folders the local provider scans (D-085).
    ///
    /// Order: `HEADSHELL_MUSIC_DIRS` → the folders chosen in the app →
    /// `XDG_MUSIC_DIR` → this system's usual music directory
    /// (`%USERPROFILE%\Music` on Windows, `~/Music` on macOS, `~/Müzik` and
    /// `~/Music` elsewhere). If none exists the list is empty — picking a
    /// made-up path would turn "I did not find the user's music" into "I
    /// looked in the wrong place".
    ///
    /// # Errors
    /// If `preferences.json` cannot be read: falling back to the system's
    /// folder in silence would hide the user's choice (K9).
    pub fn music_folders(&self) -> Result<MusicFolders> {
        let saved = crate::preferences::load(&self.preferences_path())?.music_dirs;
        Ok(resolve_music_folders(
            std::env::consts::OS,
            &non_empty_env,
            saved,
        ))
    }

    /// Creates the data directory.
    ///
    /// # Errors
    /// If the directory cannot be created.
    pub fn ensure_data_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .map_err(|source| crate::error::io_err(Stage::ConfigLoad, &self.data_dir, source))
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Where the data directory is found on an operating system, in order — the
/// error message and the documentation use this list.
fn data_dir_sources(os: &str) -> Vec<&'static str> {
    let default: &[&'static str] = match os {
        "windows" => &["LOCALAPPDATA", "USERPROFILE"],
        _ => &["HOME"],
    };
    [DATA_DIR_ENV, "XDG_DATA_HOME"]
        .into_iter()
        .chain(default.iter().copied())
        .collect()
}

/// Resolves the data directory from the given environment. `os` is the value
/// of `std::env::consts::OS` (`"linux"`, `"macos"`, `"windows"`,
/// `"freebsd"`…).
fn resolve_data_dir(os: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(dir) = env(DATA_DIR_ENV) {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = env("XDG_DATA_HOME") {
        return Some(PathBuf::from(dir).join("headshell"));
    }
    match os {
        // `LOCALAPPDATA` is defined in every user session. In the rare case it is
        // not (some service accounts) the same place is built from the profile.
        "windows" => env("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                env("USERPROFILE").map(|home| PathBuf::from(home).join("AppData").join("Local"))
            })
            .map(|base| base.join("headshell")),
        "macos" => env("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("headshell")
        }),
        _ => env("HOME").map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("headshell")
        }),
    }
}

/// Resolves the music folders from the given environment and the folders
/// chosen in the app. The usual directories go on the list only **if they
/// exist**; explicitly given ones go on as they are — if they are missing,
/// the scan says so.
fn resolve_music_folders(
    os: &str,
    env: &dyn Fn(&str) -> Option<String>,
    saved: Option<Vec<PathBuf>>,
) -> MusicFolders {
    let system = system_music_dirs(os, env);
    let (dirs, source) = if let Some(raw) = env(MUSIC_DIRS_ENV) {
        // `split_paths` uses this system's separator: `;` on Windows, `:`
        // elsewhere. Splitting on `:` by hand would cut `C:\Music` in two.
        let dirs = std::env::split_paths(&raw)
            .filter(|path| !path.as_os_str().is_empty())
            .collect();
        (dirs, FolderSource::Variable)
    } else if let Some(saved) = &saved {
        (saved.clone(), FolderSource::Saved)
    } else {
        (system.clone(), FolderSource::Default)
    };
    MusicFolders {
        dirs,
        source,
        saved,
        system,
    }
}

/// The system's own music folder(s): `XDG_MUSIC_DIR`, or the usual one if it
/// exists.
fn system_music_dirs(os: &str, env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    if let Some(dir) = env("XDG_MUSIC_DIR") {
        return vec![PathBuf::from(dir)];
    }
    let candidates: Vec<PathBuf> = match os {
        "windows" => env("USERPROFILE")
            .map(|home| vec![PathBuf::from(home).join("Music")])
            .unwrap_or_default(),
        "macos" => env("HOME")
            .map(|home| vec![PathBuf::from(home).join("Music")])
            .unwrap_or_default(),
        // The default names in the Turkish and English locales.
        _ => env("HOME")
            .map(|home| {
                let home = PathBuf::from(home);
                vec![home.join("Müzik"), home.join("Music")]
            })
            .unwrap_or_default(),
    };
    candidates.into_iter().filter(|dir| dir.is_dir()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake environment: only the given variables are defined.
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        }
    }

    /// Standard Windows does not define `HOME`; that is exactly why the first
    /// version never opened there (D-070).
    #[test]
    fn windows_uses_localappdata_and_does_not_need_home() {
        let env = env_of(&[
            ("LOCALAPPDATA", r"C:\Users\enai\AppData\Local"),
            ("USERPROFILE", r"C:\Users\enai"),
        ]);
        assert_eq!(
            resolve_data_dir("windows", &env),
            Some(PathBuf::from(r"C:\Users\enai\AppData\Local").join("headshell"))
        );

        let only_profile = env_of(&[("USERPROFILE", r"C:\Users\enai")]);
        assert_eq!(
            resolve_data_dir("windows", &only_profile),
            Some(
                PathBuf::from(r"C:\Users\enai")
                    .join("AppData")
                    .join("Local")
                    .join("headshell")
            )
        );

        // `HOME` is not even a candidate on Windows: Git Bash defines it, and the
        // same user would have two separate libraries in two shells.
        let git_bash = env_of(&[("HOME", "/c/Users/enai")]);
        assert_eq!(resolve_data_dir("windows", &git_bash), None);
    }

    #[test]
    fn macos_uses_application_support() {
        let env = env_of(&[("HOME", "/Users/enai")]);
        assert_eq!(
            resolve_data_dir("macos", &env),
            Some(PathBuf::from(
                "/Users/enai/Library/Application Support/headshell"
            ))
        );
    }

    #[test]
    fn linux_and_other_unixes_use_the_xdg_default() {
        let env = env_of(&[("HOME", "/home/enai")]);
        for os in ["linux", "freebsd", "netbsd", "openbsd"] {
            assert_eq!(
                resolve_data_dir(os, &env),
                Some(PathBuf::from("/home/enai/.local/share/headshell")),
                "{os}"
            );
        }
    }

    #[test]
    fn an_explicit_choice_wins_on_every_system() {
        for os in ["linux", "macos", "windows", "freebsd"] {
            let explicit = env_of(&[
                ("HEADSHELL_DATA_DIR", "/chosen"),
                ("XDG_DATA_HOME", "/xdg"),
                ("LOCALAPPDATA", "/local"),
                ("HOME", "/home-dir"),
            ]);
            assert_eq!(
                resolve_data_dir(os, &explicit),
                Some(PathBuf::from("/chosen")),
                "{os}"
            );
            let xdg = env_of(&[
                ("XDG_DATA_HOME", "/xdg"),
                ("LOCALAPPDATA", "/local"),
                ("HOME", "/home-dir"),
            ]);
            assert_eq!(
                resolve_data_dir(os, &xdg),
                Some(PathBuf::from("/xdg/headshell")),
                "{os}: an explicitly defined XDG_DATA_HOME must be honoured"
            );
        }
    }

    #[test]
    fn the_error_names_what_this_system_looked_at() {
        assert_eq!(
            data_dir_sources("windows"),
            vec![
                "HEADSHELL_DATA_DIR",
                "XDG_DATA_HOME",
                "LOCALAPPDATA",
                "USERPROFILE"
            ]
        );
        assert_eq!(
            data_dir_sources("linux"),
            vec!["HEADSHELL_DATA_DIR", "XDG_DATA_HOME", "HOME"]
        );
        assert_eq!(resolve_data_dir("linux", &env_of(&[])), None);
    }

    /// The list is split with this system's own separator; on Windows the colon
    /// of a drive letter is not a separator.
    #[test]
    fn a_music_dir_list_is_split_with_this_systems_separator() {
        let separator = if cfg!(windows) { ';' } else { ':' };
        let first = if cfg!(windows) { r"C:\Music" } else { "/music" };
        let raw = format!("{first}{separator}{separator}/archive");
        let env = env_of(&[("HEADSHELL_MUSIC_DIRS", &raw)]);
        assert_eq!(
            resolve_music_folders(std::env::consts::OS, &env, None).dirs,
            vec![PathBuf::from(first), PathBuf::from("/archive")]
        );
    }

    #[test]
    fn default_music_dirs_follow_the_system_and_must_exist() {
        let home = crate::test_support::TempDir::new("config-music");
        std::fs::create_dir_all(home.join("Music")).unwrap();
        let root = home.to_str().unwrap();

        for (os, key) in [
            ("windows", "USERPROFILE"),
            ("macos", "HOME"),
            ("linux", "HOME"),
        ] {
            let env = env_of(&[(key, root)]);
            assert_eq!(
                resolve_music_folders(os, &env, None).dirs,
                vec![home.join("Music")],
                "{os}"
            );
        }
        // A usual directory that does not exist is not made up.
        let empty = crate::test_support::TempDir::new("config-music-empty");
        let env = env_of(&[("HOME", empty.to_str().unwrap())]);
        assert!(resolve_music_folders("linux", &env, None).dirs.is_empty());
    }

    /// D-085: the variable first, for this run; then the folders chosen in
    /// the app — an empty choice included; then the system's folder. Who
    /// decided is always said.
    #[test]
    fn the_variable_then_the_chosen_folders_then_the_systems() {
        let home = crate::test_support::TempDir::new("config-music-order");
        std::fs::create_dir_all(home.join("Music")).unwrap();
        let root = home.to_str().unwrap();
        let chosen = Some(vec![PathBuf::from("/chosen")]);

        let system = resolve_music_folders("linux", &env_of(&[("HOME", root)]), None);
        assert_eq!(system.source, FolderSource::Default);
        assert_eq!(system.dirs, vec![home.join("Music")]);

        let saved = resolve_music_folders("linux", &env_of(&[("HOME", root)]), chosen.clone());
        assert_eq!(saved.source, FolderSource::Saved);
        assert_eq!(saved.dirs, vec![PathBuf::from("/chosen")]);
        assert_eq!(
            saved.system,
            vec![home.join("Music")],
            "the system's folder is still said"
        );

        let none = resolve_music_folders("linux", &env_of(&[("HOME", root)]), Some(Vec::new()));
        assert_eq!(none.source, FolderSource::Saved);
        assert!(
            none.dirs.is_empty(),
            "an empty choice is not 'nothing chosen'"
        );

        let held = resolve_music_folders(
            "linux",
            &env_of(&[("HOME", root), ("HEADSHELL_MUSIC_DIRS", "/held")]),
            chosen,
        );
        assert_eq!(held.source, FolderSource::Variable);
        assert_eq!(held.dirs, vec![PathBuf::from("/held")]);
        assert_eq!(held.saved, Some(vec![PathBuf::from("/chosen")]));
    }

    #[test]
    fn paths_hang_off_the_data_dir() {
        let config = Config::with_data_dir("/data/headshell");
        assert_eq!(
            config.database_path(),
            PathBuf::from("/data/headshell/library.db")
        );
        assert_eq!(
            config.last_run_path(),
            PathBuf::from("/data/headshell/last-run.json")
        );
        assert_eq!(
            config.servers_path(),
            PathBuf::from("/data/headshell/servers.json")
        );
    }
}
