//! Diagnostics: every failure says which stage it happened in.
//!
//! This module is the one thing the project inherited from its bash
//! prototype — when something breaks, saying *where* it broke in a single
//! block that can be copied and pasted.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which stage of the work we are in. Every error is tied to a stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Stage {
    /// Resolving the configuration/data directory.
    ConfigLoad,
    /// Opening the export archive, listing its contents.
    ImportRead,
    /// Determining which provider the archive belongs to.
    ImportDetect,
    /// Parsing the records (JSON/CSV → `Listen`).
    ImportParse,
    /// The canonical identity resolution chain.
    IdentityResolve,
    /// Opening the library database/schema migration.
    LibraryOpen,
    /// Writing to the library.
    LibraryWrite,
    /// Reading/searching the library.
    LibraryQuery,
    /// Computing statistics.
    StatsCompute,
    /// Talking to a provider plugin.
    ProviderCall,
    /// Plugin discovery: reading the manifest, validation, permission consent
    /// (Phase 2).
    PluginLoad,
    /// The artifact side of the plugin engine: resolving the declared tools
    /// (yt-dlp) for this platform, downloading, verifying and installing them
    /// (D-055, D-069).
    ///
    /// Separate from `PluginLoad`: "the manifest is broken" and "the manifest is
    /// fine but the artifact it wants is not installed" are different diagnoses
    /// and need different things — one needs the plugin fixed, the other an
    /// install step (K9).
    PluginRuntime,
    /// Starting the plugin: reading the script, evaluating it in QuickJS and
    /// comparing the functions it exports with its declaration (D-069).
    ///
    /// Separate from `ProviderCall`: "the plugin never came up" and "the plugin is
    /// up but said no to this call" are different diagnoses (K9). Its old name
    /// was `PLUGIN_HANDSHAKE`; there is no longer a subprocess to shake hands
    /// with.
    PluginStart,
    /// The plugin catalog: reading and validating the index, downloading plugin
    /// files and putting them in place with their hashes, updating and removing
    /// (D-071).
    ///
    /// Separate from `PluginRuntime`: that one is about **the tools a plugin
    /// wants** (yt-dlp), this one about **the plugin itself**. Not reaching the
    /// network at all stays in `NetworkRequest`: "the catalog is broken" and "I
    /// could not reach the catalog" are different diagnoses (K9).
    PluginCatalog,
    /// The HTTP transport layer: connecting, timeouts, TLS, status codes.
    ///
    /// Separate from `ProviderCall`: "I could not reach the server" and "the
    /// server refused my request" are different problems with different fixes
    /// (K9).
    NetworkRequest,
    /// Producing or writing the Sleeve card.
    SleeveRender,
    /// Finding the source to play (getting an `AudioSource` from a provider).
    PlaybackResolve,
    /// Decoding audio (symphonia): opening the container, setting up the decoder.
    PlaybackDecode,
    /// Audio output (cpal): opening the device, setting up the stream.
    PlaybackOutput,
    /// Reading a cover from where it lives: the audio file's tags, an image
    /// in its folder, the provider or the plugin — and decoding it (D-076).
    ArtworkRead,
    /// Looking a cover up with third parties: the recording's releases at
    /// MusicBrainz and the Cover Art Archive (D-076).
    ///
    /// Separate from `ArtworkRead`: "the file's picture is broken" and "the
    /// archive has none" are different diagnoses (K9). Not reaching the network
    /// at all stays in `NetworkRequest`.
    ArtworkLookup,
    /// The cover cache: the image files and their index in the data directory
    /// (D-076).
    ArtworkStore,
    /// Reading lyrics from where the track lives: a `.lrc` next to the file,
    /// the file's tags, the server or the plugin (D-078).
    LyricsRead,
    /// Looking lyrics up with a third party: LRCLIB (D-078).
    ///
    /// Separate from `LyricsRead`: "the file's `.lrc` is broken" and "LRCLIB
    /// is busy" are different diagnoses (K9). Not reaching the network at all
    /// stays in `NetworkRequest`.
    LyricsLookup,
    /// The lyrics cache and the user's shifts in the data directory (D-078).
    LyricsStore,
}

impl Stage {
    /// A fixed name for logs and reports: `IDENTITY_RESOLVE`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigLoad => "CONFIG_LOAD",
            Self::ImportRead => "IMPORT_READ",
            Self::ImportDetect => "IMPORT_DETECT",
            Self::ImportParse => "IMPORT_PARSE",
            Self::IdentityResolve => "IDENTITY_RESOLVE",
            Self::LibraryOpen => "LIBRARY_OPEN",
            Self::LibraryWrite => "LIBRARY_WRITE",
            Self::LibraryQuery => "LIBRARY_QUERY",
            Self::StatsCompute => "STATS_COMPUTE",
            Self::ProviderCall => "PROVIDER_CALL",
            Self::PluginLoad => "PLUGIN_LOAD",
            Self::PluginRuntime => "PLUGIN_RUNTIME",
            Self::PluginStart => "PLUGIN_START",
            Self::PluginCatalog => "PLUGIN_CATALOG",
            Self::NetworkRequest => "NETWORK_REQUEST",
            Self::SleeveRender => "SLEEVE_RENDER",
            Self::PlaybackResolve => "PLAYBACK_RESOLVE",
            Self::PlaybackDecode => "PLAYBACK_DECODE",
            Self::PlaybackOutput => "PLAYBACK_OUTPUT",
            Self::ArtworkRead => "ARTWORK_READ",
            Self::ArtworkLookup => "ARTWORK_LOOKUP",
            Self::ArtworkStore => "ARTWORK_STORE",
            Self::LyricsRead => "LYRICS_READ",
            Self::LyricsLookup => "LYRICS_LOOKUP",
            Self::LyricsStore => "LYRICS_STORE",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The environment we run in. The first block of an error report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvInfo {
    pub headshell_version: String,
    pub os: String,
    pub arch: String,
    pub data_dir: Option<PathBuf>,
}

impl EnvInfo {
    /// Collects the environment information known at build time.
    #[must_use]
    pub fn collect(data_dir: Option<PathBuf>) -> Self {
        Self {
            headshell_version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            data_dir,
        }
    }
}

/// The diagnostics report of the last run. `headshell diag` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagReport {
    /// Which command ran (`import fixtures/spotify.zip`).
    pub command: String,
    pub started_at: jiff::Timestamp,
    pub finished_at: jiff::Timestamp,
    pub env: EnvInfo,
    /// If the command ended with an error, at which stage.
    pub failed_at: Option<Stage>,
    /// The error chain: the outermost error first, in `source()` order.
    pub error_chain: Vec<String>,
    /// Counts: `records.total`, `identity.by_isrc`, ...
    pub counters: BTreeMap<String, i64>,
    /// Observations that are not errors but are worth recording.
    pub notes: Vec<String>,
}

impl DiagReport {
    /// Did the command finish successfully.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.failed_at.is_none()
    }

    /// A single block that can be copied and pasted. The GUI shows the same
    /// text, which is why the formatting lives here and not in the CLI.
    #[must_use]
    pub fn render(&self) -> String {
        use fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "── headshell diag ───────────────────────────────");
        let _ = writeln!(out, "command   : {}", self.command);
        let _ = writeln!(
            out,
            "result    : {}",
            if self.succeeded() {
                "SUCCEEDED"
            } else {
                "FAILED"
            }
        );
        let _ = writeln!(out, "started   : {}", self.started_at);
        let _ = writeln!(out, "finished  : {}", self.finished_at);
        let _ = writeln!(
            out,
            "version   : headshell {} ({} {})",
            self.env.headshell_version, self.env.os, self.env.arch
        );
        if let Some(dir) = &self.env.data_dir {
            let _ = writeln!(out, "data dir  : {}", dir.display());
        }

        if let Some(stage) = self.failed_at {
            let _ = writeln!(out, "\nSTEP: {stage}");
            for (depth, err) in self.error_chain.iter().enumerate() {
                let _ = writeln!(out, "  {}{}", "  ".repeat(depth), err);
            }
        }

        if !self.counters.is_empty() {
            let _ = writeln!(out, "\ncounts:");
            let width = self.counters.keys().map(String::len).max().unwrap_or(0);
            for (key, value) in &self.counters {
                let _ = writeln!(out, "  {key:<width$} = {value}");
            }
        }

        if !self.notes.is_empty() {
            let _ = writeln!(out, "\nnotes:");
            for note in &self.notes {
                let _ = writeln!(out, "  - {note}");
            }
        }
        let _ = writeln!(out, "────────────────────────────────────────────");
        out
    }
}

/// Collects diagnostic information over a run.
///
/// Operations inside the core write counts to it; when the command ends it
/// becomes a report with [`Recorder::finish`].
#[derive(Debug)]
pub struct Recorder {
    command: String,
    started_at: jiff::Timestamp,
    env: EnvInfo,
    counters: BTreeMap<String, i64>,
    notes: Vec<String>,
}

impl Recorder {
    #[must_use]
    pub fn start(command: impl Into<String>, data_dir: Option<PathBuf>) -> Self {
        Self {
            command: command.into(),
            started_at: jiff::Timestamp::now(),
            env: EnvInfo::collect(data_dir),
            counters: BTreeMap::new(),
            notes: Vec::new(),
        }
    }

    /// Increments a counter. Creates it if missing.
    pub fn count(&mut self, key: impl Into<String>, delta: i64) {
        *self.counters.entry(key.into()).or_insert(0) += delta;
    }

    /// Sets a counter to an absolute value.
    pub fn set(&mut self, key: impl Into<String>, value: i64) {
        self.counters.insert(key.into(), value);
    }

    /// An observation that does not become an error but belongs in the report.
    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    /// The counters collected so far.
    #[must_use]
    pub fn counters(&self) -> &BTreeMap<String, i64> {
        &self.counters
    }

    /// Closes the run and produces the report.
    #[must_use]
    pub fn finish(self, outcome: Result<(), &crate::Error>) -> DiagReport {
        let (failed_at, error_chain) = match outcome {
            Ok(()) => (None, Vec::new()),
            Err(err) => (Some(err.stage()), error_chain(err)),
        };
        DiagReport {
            command: self.command,
            started_at: self.started_at,
            finished_at: jiff::Timestamp::now(),
            env: self.env,
            failed_at,
            error_chain,
            counters: self.counters,
            notes: self.notes,
        }
    }
}

/// Unfolds the error's **causes** into a list of plain text.
///
/// The chain deliberately starts at `err.source()`, not at `err` itself:
/// since [`crate::Error`]'s `Display` is `"STEP: {stage}"`, the first link
/// would be a copy of the heading already printed from `failed_at` (D-007).
/// `error_chain` holds only the real causes.
fn error_chain(err: &dyn std::error::Error) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = err.source();
    while let Some(cause) = current {
        chain.push(cause.to_string());
        current = cause.source();
    }
    chain
}

/// Writes the last run's report to disk.
///
/// # Errors
/// If the file cannot be written.
pub fn save_last_run(path: &std::path::Path, report: &DiagReport) -> crate::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|source| crate::error::io_err(Stage::ConfigLoad, parent, source))?;
        }
    }
    let body = serde_json::to_vec_pretty(report).map_err(|source| {
        crate::Error::new(
            Stage::ConfigLoad,
            crate::ErrorKind::Json {
                entry: path.display().to_string(),
                source,
            },
        )
    })?;
    std::fs::write(path, body)
        .map_err(|source| crate::error::io_err(Stage::ConfigLoad, path, source))
}

/// Reads the last run's report.
///
/// If there is no file, `Ok(None)` — no command may have run yet.
///
/// # Errors
/// If the file cannot be read or is corrupt.
pub fn load_last_run(path: &std::path::Path) -> crate::Result<Option<DiagReport>> {
    let body = match std::fs::read(path) {
        Ok(body) => body,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(crate::error::io_err(Stage::ConfigLoad, path, source)),
    };
    serde_json::from_slice(&body).map(Some).map_err(|source| {
        crate::Error::new(
            Stage::ConfigLoad,
            crate::ErrorKind::Json {
                entry: path.display().to_string(),
                source,
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DiagReport {
        DiagReport {
            command: "import fixtures/spotify.zip".to_owned(),
            started_at: jiff::Timestamp::UNIX_EPOCH,
            finished_at: jiff::Timestamp::UNIX_EPOCH,
            env: EnvInfo {
                headshell_version: "0.0.0".to_owned(),
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
                data_dir: None,
            },
            failed_at: Some(Stage::ImportParse),
            // The chain carries only the causes; the "STEP: ..." heading is printed
            // once, from `failed_at` (D-007).
            error_chain: vec![
                "could not parse JSON: Streaming_History_Audio_0.json".to_owned(),
                "unexpected field".to_owned(),
            ],
            counters: BTreeMap::from([("records.total".to_owned(), 12)]),
            notes: vec!["2 records skipped".to_owned()],
        }
    }

    #[test]
    fn render_names_the_stage() {
        let text = sample().render();
        assert!(text.contains("STEP: IMPORT_PARSE"), "{text}");
        assert!(text.contains("records.total = 12"), "{text}");
        assert!(text.contains("FAILED"), "{text}");
    }

    #[test]
    fn error_chain_does_not_repeat_the_stage_header() {
        // D-007: since `Error`'s Display is "STEP: {stage}", the heading would be
        // printed twice if the chain started from it.
        let err = crate::error::io_err(
            Stage::ImportRead,
            "/missing/file.zip",
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        );
        let report = Recorder::start("import /missing/file.zip", None).finish(Err(&err));

        assert_eq!(report.failed_at, Some(Stage::ImportRead));
        assert!(
            !report
                .error_chain
                .iter()
                .any(|line| line.starts_with("STEP:")),
            "the chain must not repeat the heading: {:?}",
            report.error_chain
        );
        assert_eq!(
            report.error_chain,
            vec![
                "file operation failed: /missing/file.zip".to_owned(),
                "no such file".to_owned(),
            ]
        );
        assert_eq!(report.render().matches("STEP: IMPORT_READ").count(), 1);
    }

    #[test]
    fn recorder_counts_and_finishes_clean() {
        let mut rec = Recorder::start("stats", None);
        rec.count("records.total", 3);
        rec.count("records.total", 2);
        rec.set("identity.by_isrc", 4);
        let report = rec.finish(Ok(()));
        assert!(report.succeeded());
        assert_eq!(report.counters["records.total"], 5);
        assert_eq!(report.counters["identity.by_isrc"], 4);
    }

    #[test]
    fn stage_names_are_screaming_snake() {
        assert_eq!(Stage::IdentityResolve.to_string(), "IDENTITY_RESOLVE");
        let json = serde_json::to_string(&Stage::ImportParse).unwrap();
        assert_eq!(json, "\"IMPORT_PARSE\"");
    }
}
