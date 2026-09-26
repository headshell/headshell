//! Typed core errors.
//!
//! Every error carries a [`Stage`] — the answer to "where did it break?" lives
//! in the error itself, not somewhere in the log.

use std::path::PathBuf;

use crate::diag::Stage;

/// The one error type the core returns.
#[derive(Debug, thiserror::Error)]
#[error("STEP: {stage}")]
pub struct Error {
    stage: Stage,
    #[source]
    kind: ErrorKind,
}

impl Error {
    #[must_use]
    pub fn new(stage: Stage, kind: ErrorKind) -> Self {
        Self { stage, kind }
    }

    /// The stage the error happened in.
    #[must_use]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// The kind of error.
    #[must_use]
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// `STEP: X` + the chain of causes, one per line. The CLI and the GUI show the
    /// same text.
    #[must_use]
    pub fn chain_text(&self) -> String {
        let mut out = format!("STEP: {}", self.stage);
        let mut current: Option<&dyn std::error::Error> = Some(&self.kind);
        while let Some(err) = current {
            out.push_str("\n  → ");
            out.push_str(&err.to_string());
            current = err.source();
        }
        out
    }
}

/// What broke.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ErrorKind {
    #[error("file operation failed: {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not read the archive: {path}")]
    Archive {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },

    #[error("could not parse JSON: {entry}")]
    Json {
        /// Path inside the archive, or a file name.
        entry: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("database error")]
    Database {
        #[source]
        source: rusqlite::Error,
    },

    #[error("unrecognised export format: {detail}")]
    UnsupportedExport { detail: String },

    #[error("record {index} in {entry} is corrupt: {detail}")]
    MalformedRecord {
        entry: String,
        index: usize,
        detail: String,
    },

    #[error("not found: {what}")]
    NotFound { what: String },

    #[error("invalid input: {detail}")]
    InvalidInput { detail: String },

    #[error("could not produce the card: {detail}")]
    CardRender { detail: String },

    /// A cover image that cannot be used: not an image, a format we do not
    /// read, too large, or it did not decode (D-076). Its `detail` says which.
    #[error("unusable cover image: {detail}")]
    Artwork { detail: String },

    /// Lyrics that cannot be used, or a lyrics service that answered with
    /// something it should not have: a `.lrc` that is not UTF-8, a `SYLT`
    /// frame that counts MPEG frames, LRCLIB busy (D-078). Its `detail` says
    /// which.
    #[error("lyrics: {detail}")]
    Lyrics { detail: String },

    /// The provider does not have this capability.
    ///
    /// "I can't" and "no results" are different things; the latter is an empty
    /// list, the former is this error (K9).
    #[error("{provider} cannot do this: {what} (capabilities: {capabilities})")]
    Unsupported {
        provider: String,
        what: String,
        capabilities: String,
    },

    #[error("audio pipeline error: {detail}")]
    Audio { detail: String },

    /// A transport-layer error: could not connect, timeout, TLS, DNS.
    ///
    /// Separate from an application-layer error (`RemoteApi`): "I could not reach
    /// the server" and "the server said no" are different diagnoses (K9).
    #[error("network request failed: {url} ({detail})")]
    Network { url: String, detail: String },

    /// The server returned a status code outside 2xx.
    #[error("the server returned HTTP {status}: {url} — {detail}")]
    HttpStatus {
        url: String,
        status: u16,
        detail: String,
    },

    /// A plugin manifest could not be read or is invalid (Phase 2, §2.1).
    #[error("invalid plugin manifest: {path} — {detail}")]
    PluginManifest { path: PathBuf, detail: String },

    /// A step of the plugin engine stumbled (D-055, D-069).
    ///
    /// `step` says **at which step** — platform matching, artifact download, hash
    /// verification, reading the script. The engine's failures must not all come
    /// out as the same sentence: "no yt-dlp", "yt-dlp could not be downloaded" and
    /// "the downloaded yt-dlp's hash did not match" are three separate diagnoses
    /// with three different fixes (K9).
    #[error("plugin engine — {step}: {detail}")]
    PluginRuntime { step: String, detail: String },

    /// The plugin catalog refused something (D-071): the index is corrupt, an
    /// entry is invalid, a downloaded file's hash does not match, or the requested
    /// operation cannot be done to this plugin (installed by hand, changed
    /// locally).
    ///
    /// `index` says which catalog is meant: the user may have picked another
    /// catalog with `HEADSHELL_PLUGIN_INDEX`, and which one the error came from
    /// must not be left to guesswork (K9).
    #[error("plugin catalog ({index}): {detail}")]
    PluginCatalog { index: String, detail: String },

    /// The plugin's protocol version does not match the core's.
    ///
    /// This error is half of Phase 2's "counts as done" criterion: an
    /// incompatible plugin **is not loaded**, the core does not crash, and the
    /// user sees what does not match.
    #[error(
        "the {plugin} plugin cannot talk to this version: plugin api {plugin_api}, core api {host_api}"
    )]
    PluginIncompatible {
        plugin: String,
        plugin_api: u32,
        host_api: u32,
    },

    /// The plugin's permissions have not been approved, or the consent was
    /// withdrawn.
    #[error("the {plugin} plugin is not approved: {detail}")]
    PluginNotApproved { plugin: String, detail: String },

    /// The plugin's thread could not be started or fell over (D-069).
    ///
    /// This is not an error thrown by the JS — that is
    /// [`ErrorKind::PluginThrew`]. This is the case where the engine itself cannot
    /// carry the plugin: the thread could not be opened, QuickJS could not be set
    /// up, or the plugin fell over so many times that the engine gave up.
    #[error("the {plugin} plugin is not running: {detail}")]
    PluginCrashed { plugin: String, detail: String },

    /// The plugin did not answer within the time allowed.
    ///
    /// A timeout is separate from a crash: a hung plugin is a different problem
    /// from a dead one and is fixed differently (K9).
    #[error("the {plugin} plugin did not answer the {method} call within {seconds} s")]
    PluginTimeout {
        plugin: String,
        method: String,
        seconds: u64,
    },

    /// The plugin's code threw an error — the engine is fine, the plugin refused
    /// the work.
    ///
    /// `location` is the first line of the JS stack (`main.js:42:7`): "where" is as
    /// much a part of the diagnosis as "why" (K9), and without it the plugin
    /// author has to hunt for the error in their own code.
    #[error("the {plugin} plugin failed on the {method} call: {message}{location}")]
    PluginThrew {
        plugin: String,
        method: String,
        message: String,
        /// Empty, or of the form ` (main.js:42:7)`.
        location: String,
    },

    /// The plugin broke the contract: it does not export a function it declared,
    /// or it returned a value in an unexpected shape.
    ///
    /// Separate from [`ErrorKind::PluginThrew`]: "the plugin said no" and "the
    /// plugin's code cannot get along with the engine" are different diagnoses.
    /// The user fixes the first by waiting or by configuring; only the plugin
    /// author can fix the second (K9).
    #[error("the {plugin} plugin breaks the contract ({method}): {detail}")]
    PluginContract {
        plugin: String,
        method: String,
        detail: String,
    },

    /// A call made ahead of time was stopped so that one made on the user's
    /// behalf could run (D-077).
    ///
    /// Not a failure: nothing is wrong with the plugin or the track. Separate
    /// from [`ErrorKind::PluginThrew`] so that what read ahead can count it as
    /// "gave way" and ask again later, instead of as "this track fails" (K9).
    #[error(
        "the {plugin} plugin's {method} call, made ahead of time, gave way to one the user asked for"
    )]
    PluginPreempted { plugin: String, method: String },

    /// The server returned HTTP 200 but the body holds an error (as Subsonic
    /// does).
    #[error("{server} refused the request: {message} (code {code}, endpoint: {endpoint})")]
    RemoteApi {
        server: String,
        endpoint: String,
        code: i64,
        message: String,
    },
}

/// The core result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Wraps an `io::Error` together with its path and stage.
pub(crate) fn io_err(stage: Stage, path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::new(
        stage,
        ErrorKind::Io {
            path: path.into(),
            source,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_text_starts_with_stage_and_lists_causes() {
        let err = io_err(
            Stage::ImportRead,
            "/missing/file.zip",
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        );
        let text = err.chain_text();
        assert!(text.starts_with("STEP: IMPORT_READ"), "{text}");
        assert!(text.contains("/missing/file.zip"), "{text}");
        assert!(text.contains("no such file"), "{text}");
        assert_eq!(err.stage(), Stage::ImportRead);
    }
}
