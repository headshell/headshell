//! The user's choices: the provider the play box searches first (D-080) —
//! since D-086 the main provider, with music and video chosen apart from it —
//! whether the desktop goes online (D-082), and the music folders (D-085).
//!
//! A file of its own, `preferences.json`, not the database: a choice is not
//! library data — the line D-021 drew for `servers.json`. A missing file is
//! "nothing chosen yet", not an error; a file that cannot be read is an error
//! that says so (K9), never a silent default that would look like the choice
//! was lost.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result, io_err};
use crate::ids::ProviderId;
use crate::session::LookupMode;

/// The choices, as they are written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    /// The provider the play box and its suggestions ask first; `None`:
    /// nothing chosen, the registry's first one that can play (local files).
    ///
    /// Since D-086 this is the **main** provider: music and video follow it
    /// until each is chosen on its own. The key keeps its name, so a file
    /// written before keeps its choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_from: Option<ProviderId>,
    /// The provider the play box searches, when it is not the main one
    /// (D-086); `None`: it follows the main provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music_from: Option<ProviderId>,
    /// The provider a song's video is found at, when it is not the main one
    /// (D-086); `None`: it follows the main provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_from: Option<ProviderId>,
    /// Whether the desktop asks MusicBrainz and the Cover Art Archive
    /// (D-082); `None`: nothing chosen — offline. The CLI does not read it:
    /// every run there chooses with `--online`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup: Option<LookupMode>,
    /// The music folders chosen in the app (D-085); `None`: nothing chosen —
    /// the system's music folder. An empty list is a choice: no local
    /// folders. `HEADSHELL_MUSIC_DIRS` still decides a run that sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music_dirs: Option<Vec<PathBuf>>,
}

/// Reads the choices; no file is no choice.
///
/// # Errors
/// If the file exists but cannot be read, or is not the JSON written here.
pub fn load(path: &Path) -> Result<Preferences> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Preferences::default()),
        Err(err) => return Err(io_err(Stage::ConfigLoad, path, err)),
    };
    serde_json::from_slice(&raw).map_err(|source| {
        Error::new(
            Stage::ConfigLoad,
            ErrorKind::Json {
                entry: path.display().to_string(),
                source,
            },
        )
    })
}

/// Writes the choices whole or not at all.
///
/// # Errors
/// If the data directory cannot be written.
pub fn save(path: &Path, preferences: &Preferences) -> Result<()> {
    let text = serde_json::to_vec_pretty(preferences).map_err(|source| {
        Error::new(
            Stage::ConfigLoad,
            ErrorKind::Json {
                entry: path.display().to_string(),
                source,
            },
        )
    })?;
    crate::store::write_atomic(path, &text, Stage::ConfigLoad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn no_file_is_no_choice_and_a_choice_comes_back_as_written() {
        let dir = TempDir::new("preferences");
        let path = dir.path().join("preferences.json");
        assert_eq!(load(&path).unwrap(), Preferences::default());

        let chosen = Preferences {
            play_from: Some(ProviderId::new("ytmusic")),
            music_from: None,
            video_from: Some(ProviderId::new("youtube")),
            lookup: Some(LookupMode::Online),
            music_dirs: Some(vec![PathBuf::from("/music")]),
        };
        save(&path, &chosen).unwrap();
        assert_eq!(load(&path).unwrap(), chosen);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""lookup": "online""#), "{text}");
    }

    /// A file written before D-082 has no `lookup`: it reads as nothing
    /// chosen, and the play-from choice in it survives.
    #[test]
    fn a_file_from_before_the_online_choice_still_reads() {
        let dir = TempDir::new("preferences-old");
        let path = dir.path().join("preferences.json");
        std::fs::write(&path, br#"{ "play_from": "local" }"#).unwrap();
        let read = load(&path).unwrap();
        assert_eq!(read.play_from, Some(ProviderId::new("local")));
        assert_eq!(read.lookup, None);
    }

    #[test]
    fn a_broken_file_is_said_not_taken_for_no_choice() {
        let dir = TempDir::new("preferences-broken");
        let path = dir.path().join("preferences.json");
        std::fs::write(&path, b"{ play_from").unwrap();
        let err = load(&path).unwrap_err();
        assert_eq!(err.stage(), Stage::ConfigLoad);
        assert!(
            err.chain_text().contains("preferences.json"),
            "{}",
            err.chain_text()
        );
    }
}
