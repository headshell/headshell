//! The window's side of music videos (D-086): what a video does when the
//! player is left, and where a local video file is served from.
//!
//! Neither is a core concept. Which video, whose clock, what the listen is —
//! that is the core's (`headshell_core::video`, `Player::show_video`); this is
//! the window showing it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::video_server::VideoServer;

/// What a video does when the user leaves the player for another view.
///
/// YouTube's player must not play where it cannot be seen, so a video does
/// not just go on behind the library: it either gives the sound back to the
/// music, or moves to a small window in the corner that stays on screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoAway {
    /// The lid opens and the music goes on from the same second; a video
    /// that is all the track has pauses.
    #[default]
    Music,
    /// The video goes on in a small window in the corner.
    Corner,
}

/// The video choices and the file server.
pub struct VideoDesk {
    settings: PathBuf,
    /// Started on the first local video; a failure is kept and said each
    /// time it is asked for, not retried in a loop.
    server: OnceLock<Result<VideoServer, String>>,
}

impl VideoDesk {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            settings: data_dir.join("ui.json"),
            server: OnceLock::new(),
        }
    }

    /// The saved choice.
    ///
    /// # Errors
    /// If `ui.json` cannot be read.
    pub fn away(&self) -> Result<VideoAway, String> {
        Ok(crate::ui_settings::read(&self.settings)?
            .video_away
            .unwrap_or_default())
    }

    /// Saves the choice.
    ///
    /// # Errors
    /// If `ui.json` cannot be read or written.
    pub fn set_away(&self, away: VideoAway) -> Result<VideoAway, String> {
        Ok(crate::ui_settings::update(&self.settings, |settings| {
            settings.video_away = Some(away);
        })?
        .video_away
        .unwrap_or_default())
    }

    /// Are music videos on? Off, the window behaves as if they did not
    /// exist: no switch, no video settings, and a track that is a video only
    /// is passed by as the CLI passes it.
    ///
    /// # Errors
    /// If `ui.json` cannot be read.
    pub fn enabled(&self) -> Result<bool, String> {
        Ok(!crate::ui_settings::read(&self.settings)?.videos_off)
    }

    /// Saves whether music videos are on.
    ///
    /// # Errors
    /// If `ui.json` cannot be read or written.
    pub fn set_enabled(&self, on: bool) -> Result<bool, String> {
        let settings = crate::ui_settings::update(&self.settings, |settings| {
            settings.videos_off = !on;
        })?;
        if !on {
            self.stop_serving();
        }
        Ok(!settings.videos_off)
    }

    /// The address the page plays `path` from; it is the only file served
    /// from now on.
    ///
    /// # Errors
    /// If the server could not start.
    pub fn serve(&self, path: &Path) -> Result<String, String> {
        match self.server.get_or_init(VideoServer::start) {
            Ok(server) => server.show(path),
            Err(err) => Err(err.clone()),
        }
    }

    /// Serves nothing any more: the video is gone.
    pub fn stop_serving(&self) {
        if let Some(Ok(server)) = self.server.get() {
            server.hide();
        }
    }
}
