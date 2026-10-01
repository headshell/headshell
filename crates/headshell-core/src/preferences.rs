//! The user's choices (D-080): today, the provider the play box searches
//! first.
//!
//! A file of its own, `preferences.json`, not the database: a choice is not
//! library data — the line D-021 drew for `servers.json`. A missing file is
//! "nothing chosen yet", not an error; a file that cannot be read is an error
//! that says so (K9), never a silent default that would look like the choice
//! was lost.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result, io_err};
use crate::ids::ProviderId;

/// The choices, as they are written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    /// The provider the play box and its suggestions ask first; `None`:
    /// nothing chosen, the registry's first one that can play (local files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_from: Option<ProviderId>,
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
        };
        save(&path, &chosen).unwrap();
        assert_eq!(load(&path).unwrap(), chosen);
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
