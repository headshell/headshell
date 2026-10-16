//! The plugin boundary: providers running in embedded QuickJS (K5, D-069).
//!
//! A plugin is the directory `<data_dir>/plugins/<name>/`; it contains a
//! `plugin.json` (see [`manifest`]) and a JS script. The script runs in the
//! engine inside the core — **nothing has to be installed** on the user's
//! machine. In api 1 a plugin was a subprocess written in Python, and that
//! was why "whoever I sent it to had a problem": Windows has no Python,
//! Debian packages `venv` separately, versions don't line up.
//!
//! Plugins do not live in the main repository: they live in the
//! `headshell/plugins` repository, and the app installs and updates them
//! from that repository's index ([`catalog`], D-071). A directory put there
//! by hand is a plugin too; the catalog leaves it alone.
//!
//! ## Lifecycle
//!
//! 1. **Discovery** ([`discover`]) — without starting the engine: the
//!    manifest is read, consent ([`consent`]) is checked, and the protocol
//!    version and the state of the artifacts are measured. `headshell plugin
//!    list` uses only this much.
//! 2. **Start** — **on the first call**, lazily: the plugin's thread is
//!    started, the script is evaluated, and the functions for the declared
//!    capabilities are checked to be exported ([`script`]).
//! 3. **Call** — every call has a time limit; a stuck plugin does not stall
//!    the core.
//! 4. **Failure** — a timeout or a dead thread drops the engine and the next
//!    call restarts it. After [`MAX_STARTS`] attempts it gives up; endless
//!    restarts would hide a crash loop.
//!
//! ## What the boundary holds
//!
//! The identity namespace (a plugin cannot make up another provider's ids),
//! the secret namespace (only its own, D-042), time, memory, **the network**
//! (every request and every redirect is checked against `permissions.net`)
//! and **the file system** (the plugin has no file access). The one gate it
//! does not hold is the tools the engine installs: an artifact like yt-dlp
//! run through `host.tools.run` runs with the user's privileges, and
//! `headshell plugin list` says so ([`host`]).

pub mod artifact;
pub mod catalog;
pub mod consent;
#[cfg(feature = "plugin-engine")]
mod host;
pub mod manifest;
pub mod protocol;
#[cfg(feature = "plugin-engine")]
mod script;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result, io_err};
use crate::ids::{ProviderId, ProviderTrackId};
use crate::net::HttpClient;
use crate::provider::{
    ArtworkImage, AudioSource, Capabilities, Provider, ProviderFuture, ProviderHealth,
    ProviderInfo, ProviderLyrics, ProviderTrack,
};
use crate::secrets::{Secrets, plugin_namespace};

use artifact::ArtifactStore;
use consent::{ConsentStatus, ConsentStore};
use manifest::{MANIFEST_FILE, Permissions, PluginManifest, Requirement};
use protocol::{
    HealthResult, PLUGIN_API, SourceResult, WireArtwork, WireLyrics, WireTrack, export,
};

#[cfg(feature = "plugin-engine")]
use script::ScriptWorker;
#[cfg(not(feature = "plugin-engine"))]
use unavailable::ScriptWorker;

/// Is the permission declaration enforced (D-040 → D-069).
///
/// It was `false` in api 1: the plugin was a separate process running with
/// all of the user's privileges. In api 2 a plugin can only get out through
/// the engine's gates, and the engine checks the declaration at every gate.
/// **The one exception** is the tools the engine installs (yt-dlp): they are
/// separate processes and are not confined — the outputs say so separately.
pub const PERMISSIONS_ENFORCED: bool = true;

/// How many times a plugin is started. Beyond that it gives up and says why
/// — endless restarts would make a crash loop silent.
pub const MAX_STARTS: u32 = 3;

/// Time allowed to evaluate the script. Short: loading cannot go online
/// (the engine refuses it), it only sets itself up.
pub const START_TIMEOUT: Duration = Duration::from_secs(5);

/// Time allowed for one call. A search that needs the network, or yt-dlp's
/// signature solving, can take this long.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// A plugin seen by discovery. Those that cannot run are here too — a plugin
/// skipped silently is a plugin the user thinks is installed (K9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEntry {
    /// Directory name = identity.
    pub name: String,
    pub dir: PathBuf,
    /// Fields from the manifest, if it could be read.
    pub display_name: Option<String>,
    pub version: Option<String>,
    pub api: Option<u32>,
    pub permissions: Permissions,
    /// Consent state (if the manifest could be read).
    pub consent: Option<ConsentStatus>,
    /// The state of the artifacts asked of the engine (D-055, D-069). Measured
    /// **without going online**: is there a release for this platform, is it on
    /// disk, does its hash match.
    #[serde(default)]
    pub requires: Vec<artifact::RequirementStatus>,
    /// Does it give covers (api 3, D-076) — what its manifest declares. `None`
    /// when the manifest could not be read.
    #[serde(default)]
    pub artwork: Option<bool>,
    /// Does it give lyrics (api 4, D-078) — what its manifest declares.
    /// `None` when the manifest could not be read.
    #[serde(default)]
    pub lyrics: Option<bool>,
    /// Does it give videos (api 5, D-086) — what its manifest declares.
    /// `None` when the manifest could not be read.
    #[serde(default)]
    pub video: Option<bool>,
    /// Does it name similar songs (api 6, D-087) — what its manifest
    /// declares. `None` when the manifest could not be read.
    #[serde(default)]
    pub related: Option<bool>,
    /// The links it takes (api 7, D-095) — the templates its manifest names.
    /// `None` when the manifest could not be read.
    #[serde(default)]
    pub links: Option<Vec<String>>,
    /// Why it cannot load, if it cannot — one line, copyable.
    pub problem: Option<String>,
}

impl PluginEntry {
    /// Can this plugin be started.
    ///
    /// A missing artifact **blocks** loading: starting a plugin without its
    /// artifact would make it fail with an incomprehensible error on its first
    /// search.
    #[must_use]
    pub fn is_loadable(&self) -> bool {
        self.problem.is_none()
            && self.missing_requirements().is_empty()
            && self
                .consent
                .as_ref()
                .is_some_and(consent::ConsentStatus::is_approved)
    }

    /// Artifacts that are not ready. Empty means nothing is missing on the
    /// engine's side.
    #[must_use]
    pub fn missing_requirements(&self) -> Vec<&artifact::RequirementStatus> {
        self.requires
            .iter()
            .filter(|status| !status.state.is_ready())
            .collect()
    }

    /// A one-line status to show the user.
    #[must_use]
    pub fn status_text(&self) -> String {
        if let Some(problem) = &self.problem {
            return problem.clone();
        }
        let missing = self.missing_requirements();
        if !missing.is_empty() {
            let detail = missing
                .iter()
                .map(|status| {
                    format!(
                        "{} {}: {}",
                        status.name,
                        status.version,
                        status.state.describe()
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            // Without platform support, suggesting the install command is wrong advice.
            let unsupported = missing.iter().all(|status| {
                matches!(status.state, artifact::RequirementState::Unsupported { .. })
            });
            return if unsupported {
                format!(
                    "the artifact the engine must install does not exist for this platform ({detail})"
                )
            } else {
                format!(
                    "an artifact the engine must install is missing ({detail}) — `headshell plugin install {}`",
                    self.name
                )
            };
        }
        match &self.consent {
            Some(status) => status.describe(),
            None => "state unknown".to_owned(),
        }
    }
}

/// Discovery summary (K9: how many came, what happened to them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSummary {
    /// Number of plugins seen in the directory.
    pub discovered: usize,
    /// Ready to load (approved, compatible version, artifacts installed).
    pub ready: usize,
    /// Awaiting consent, or asking for new permissions.
    pub awaiting_approval: usize,
    /// Disabled by the user.
    pub disabled: usize,
    /// Protocol version mismatch.
    pub incompatible: usize,
    /// Manifest unreadable or invalid.
    pub broken: usize,
    /// An artifact the engine must install is missing (D-055). Separate from
    /// `ready`: what the user has to do is different — install, not approve.
    #[serde(default)]
    pub needs_install: usize,
}

impl PluginSummary {
    /// Copies the counters into the diagnostics recorder.
    pub fn record_into(&self, recorder: &mut crate::diag::Recorder) {
        let n = |value: usize| i64::try_from(value).unwrap_or(i64::MAX);
        recorder.set("plugins.discovered", n(self.discovered));
        recorder.set("plugins.ready", n(self.ready));
        recorder.set("plugins.awaiting_approval", n(self.awaiting_approval));
        recorder.set("plugins.disabled", n(self.disabled));
        recorder.set("plugins.incompatible", n(self.incompatible));
        recorder.set("plugins.broken", n(self.broken));
        recorder.set("plugins.needs_install", n(self.needs_install));
    }
}

/// Scans the plugin directory. **Starts no plugin.**
///
/// A broken plugin does not stop the scan: its reason is written to
/// [`PluginEntry::problem`] and the rest of the scan continues.
///
/// # Errors
/// If the plugin directory cannot be read (it exists but there is no
/// permission, say) or the consent ledger is corrupt. A **missing**
/// directory is not an error: an empty list.
pub fn discover(config: &Config) -> Result<(Vec<PluginEntry>, PluginSummary)> {
    let dir = config.plugins_dir();
    let consents = ConsentStore::load(&config.plugin_consent_path())?;
    let store = ArtifactStore::new(config);

    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), PluginSummary::default()));
        }
        Err(err) => return Err(io_err(Stage::PluginLoad, &dir, err)),
    };

    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| io_err(Stage::PluginLoad, &dir, err))?;
        let path = entry.path();
        if !path.is_dir() || !path.join(MANIFEST_FILE).exists() {
            continue;
        }
        found.push(describe_plugin(&path, &consents, &store));
    }
    // Directory order depends on the file system; the output must be stable.
    found.sort_by(|a, b| a.name.cmp(&b.name));

    let summary = summarize(&found);
    Ok((found, summary))
}

fn summarize(entries: &[PluginEntry]) -> PluginSummary {
    let mut summary = PluginSummary {
        discovered: entries.len(),
        ..PluginSummary::default()
    };
    for entry in entries {
        if entry.problem.is_some() {
            if entry.api.is_some_and(|api| api != PLUGIN_API) {
                summary.incompatible += 1;
            } else {
                summary.broken += 1;
            }
            continue;
        }
        if !entry.missing_requirements().is_empty() {
            summary.needs_install += 1;
            continue;
        }
        match &entry.consent {
            Some(ConsentStatus::Approved) => summary.ready += 1,
            Some(ConsentStatus::Disabled) => summary.disabled += 1,
            Some(ConsentStatus::NotAsked | ConsentStatus::NeedsApproval { .. }) => {
                summary.awaiting_approval += 1;
            }
            None => summary.broken += 1,
        }
    }
    summary
}

fn describe_plugin(dir: &Path, consents: &ConsentStore, store: &ArtifactStore) -> PluginEntry {
    let name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let manifest = match PluginManifest::load(dir) {
        Ok(manifest) => manifest,
        Err(err) => {
            let api = match err.kind() {
                ErrorKind::PluginIncompatible { plugin_api, .. } => Some(*plugin_api),
                _ => None,
            };
            let problem = match err.kind() {
                ErrorKind::PluginIncompatible { plugin_api, .. } => format!(
                    "protocol version mismatch: plugin api {plugin_api}, core api \
                     {PLUGIN_API}{}",
                    if *plugin_api == 1 {
                        " — api 1 was the old Python/subprocess plugin; install the plugin's api 2 \
                         (QuickJS) version"
                    } else {
                        ""
                    }
                ),
                _ => err.chain_text().replace('\n', " "),
            };
            return PluginEntry {
                name,
                dir: dir.to_path_buf(),
                display_name: None,
                version: None,
                api,
                permissions: Permissions::default(),
                consent: None,
                requires: Vec::new(),
                artwork: None,
                lyrics: None,
                video: None,
                related: None,
                links: None,
                problem: Some(problem),
            };
        }
    };

    // Artifact state is read from disk; if it cannot be read that is a
    // `problem` too — silently saying "nothing missing" would show a missing
    // artifact as ready.
    let (requires, problem) = match store.statuses(&manifest.requires) {
        Ok(requires) => (requires, None),
        Err(err) => (Vec::new(), Some(err.chain_text().replace('\n', " "))),
    };

    PluginEntry {
        name: name.clone(),
        dir: dir.to_path_buf(),
        display_name: Some(manifest.display_name.clone()),
        version: manifest.version.clone(),
        api: Some(manifest.api),
        consent: Some(consents.status(&name, &manifest.permissions)),
        permissions: manifest.permissions,
        requires,
        artwork: Some(manifest.artwork),
        lyrics: Some(manifest.lyrics),
        video: Some(manifest.video),
        related: Some(manifest.related),
        links: Some(manifest.links.clone()),
        problem,
    }
}

/// Sets up the approved plugins as providers. **No engine is started** —
/// each provider starts its own engine on its first call.
///
/// # Errors
/// If discovery fails or the secret file is corrupt.
pub fn load(config: &Config) -> Result<(Vec<Arc<dyn Provider>>, PluginSummary)> {
    let (entries, summary) = discover(config)?;
    let secrets = Secrets::load(&config.secrets_path())?;

    let mut providers: Vec<Arc<dyn Provider>> = Vec::new();
    for entry in &entries {
        if !entry.is_loadable() {
            tracing::debug!(plugin = %entry.name, status = %entry.status_text(), "plugin not loaded");
            continue;
        }
        // Discovery already read the manifest once; we read it again for setup
        // because `PluginEntry` is a narrow view that can cross to uniffi (K7),
        // not the whole manifest.
        let manifest = match PluginManifest::load(&entry.dir) {
            Ok(manifest) => manifest,
            Err(err) => {
                tracing::warn!(
                    plugin = %entry.name,
                    error = %err.chain_text().replace('\n', " "),
                    "the plugin manifest could not be read after discovery"
                );
                continue;
            }
        };
        providers.push(Arc::new(PluginProvider::from_manifest(
            config, &manifest, &entry.dir, &secrets,
        )?));
    }
    Ok((providers, summary))
}

/// Everything needed to start a plugin's engine.
///
/// Carried by value, because the engine is set up on its own thread and
/// every restart uses the same recipe.
///
/// In a build without the engine, nobody reads most of the fields — the
/// fallback only uses the name to say "no engine". The recipe is built
/// anyway, because the provider's discovery, consent and capability surface
/// is independent of the engine and must stay the same.
#[derive(Clone)]
#[cfg_attr(not(feature = "plugin-engine"), allow(dead_code))]
pub(crate) struct ScriptSpec {
    pub(crate) plugin: String,
    /// Absolute path of the script.
    pub(crate) main: PathBuf,
    /// The name shown in stack traces: the manifest's `main`.
    pub(crate) module_name: String,
    pub(crate) capabilities: Capabilities,
    pub(crate) permissions: Permissions,
    pub(crate) secrets: BTreeMap<String, String>,
    pub(crate) state_dir: PathBuf,
    /// The plugin's HTTP client; if there is none, **why** (K9).
    pub(crate) http: std::result::Result<Arc<dyn HttpClient>, String>,
    pub(crate) store: ArtifactStore,
    pub(crate) requires: Vec<Requirement>,
}

/// A provider living in the embedded engine.
pub struct PluginProvider {
    id: ProviderId,
    display_name: String,
    capabilities: Capabilities,
    permissions: Permissions,
    /// The links it takes, read from its manifest (D-095).
    link_templates: Vec<crate::link::LinkTemplate>,
    spec: ScriptSpec,
    start_timeout: Duration,
    call_timeout: Duration,
    /// Who gets the engine next (D-077).
    gate: Gate,
    state: std::sync::Mutex<SessionState>,
}

/// The order in which calls get a plugin's one engine (D-077).
///
/// The engine runs one call at a time. A call made on the user's behalf goes
/// first: it waits only for the call already running, and if that one was
/// made ahead of time ([`crate::provider::ahead_of_time`]) it is asked to
/// give way — a tool it is running is stopped. A call made ahead of time
/// waits while any call for the user waits. Measured before this: pressing
/// "next" while yt-dlp resolved the track after it waited for that whole
/// run, 4–5 s.
#[derive(Default)]
struct Gate {
    state: std::sync::Mutex<GateState>,
    turn: std::sync::Condvar,
}

#[derive(Default)]
struct GateState {
    busy: bool,
    /// The running call's stop flag, when it was made ahead of time.
    running_ahead: Option<Arc<AtomicBool>>,
    /// Calls for the user waiting for their turn.
    waiting_for_user: usize,
}

/// A turn at the engine; it ends when dropped.
struct Turn<'a> {
    gate: &'a Gate,
    /// Raised when a call for the user asks this one to give way. `None`
    /// for a call for the user: nothing stops it.
    cancel: Option<Arc<AtomicBool>>,
}

impl Gate {
    /// Waits for a turn. `None` only if the lock is poisoned.
    fn enter(&self, ahead_of_time: bool) -> Option<Turn<'_>> {
        let mut state = self.state.lock().ok()?;
        if ahead_of_time {
            while state.busy || state.waiting_for_user > 0 {
                state = self.turn.wait(state).ok()?;
            }
            let flag = Arc::new(AtomicBool::new(false));
            state.busy = true;
            state.running_ahead = Some(Arc::clone(&flag));
            return Some(Turn {
                gate: self,
                cancel: Some(flag),
            });
        }
        state.waiting_for_user += 1;
        while state.busy {
            if let Some(flag) = &state.running_ahead {
                flag.store(true, Ordering::Release);
            }
            state = self.turn.wait(state).ok()?;
        }
        state.waiting_for_user -= 1;
        state.busy = true;
        state.running_ahead = None;
        Some(Turn {
            gate: self,
            cancel: None,
        })
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.gate.state.lock() {
            state.busy = false;
            state.running_ahead = None;
        }
        self.gate.turn.notify_all();
    }
}

#[derive(Default)]
struct SessionState {
    worker: Option<ScriptWorker>,
    starts: u32,
    /// A reason not worth retrying (such as a contract violation).
    give_up: Option<String>,
}

impl std::fmt::Debug for PluginProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginProvider")
            .field("id", &self.id)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl PluginProvider {
    /// Sets it up from the manifest, with this build's plugin HTTP client. The
    /// engine is not started.
    ///
    /// # Errors
    /// If the plugin's state directory cannot be created.
    pub fn from_manifest(
        config: &Config,
        manifest: &PluginManifest,
        dir: &Path,
        secrets: &Secrets,
    ) -> Result<Self> {
        let http = crate::net::plugin_http_client().map_err(|err| {
            format!(
                "plugins cannot go online in this build: {}",
                err.chain_text().replace('\n', " ")
            )
        });
        Self::with_http(config, manifest, dir, secrets, http)
    }

    /// Sets it up from the manifest; the caller supplies the HTTP client. The
    /// engine is not started.
    ///
    /// For tests and for shells that bring their own HTTP stack. The client
    /// given **must not follow redirects**: a client that follows them opens a
    /// way around the permission check ([`host`]).
    ///
    /// # Errors
    /// If the plugin's state directory cannot be created.
    pub fn with_http(
        config: &Config,
        manifest: &PluginManifest,
        dir: &Path,
        secrets: &Secrets,
        http: std::result::Result<Arc<dyn HttpClient>, String>,
    ) -> Result<Self> {
        let state_dir = config.plugin_state_dir(&manifest.name);
        std::fs::create_dir_all(&state_dir)
            .map_err(|err| io_err(Stage::PluginLoad, &state_dir, err))?;

        let (capabilities, unknown) = protocol::parse_capabilities(&manifest.capabilities);
        if !unknown.is_empty() {
            tracing::warn!(
                plugin = %manifest.name,
                unknown = %unknown.join(", "),
                "the manifest contains an unrecognised capability name; ignored"
            );
        }
        let unmapped = capabilities.contains(Capabilities::BROWSE)
            || capabilities.contains(Capabilities::CONTROL);
        if unmapped {
            tracing::warn!(
                plugin = %manifest.name,
                "`browse`/`control` do not map to a function in api 3; the declaration only shows in the list"
            );
        }
        // Covers, lyrics and videos are not capability names but required
        // yes-or-noes (api 3, D-076; api 4, D-078; api 5, D-086).
        let capabilities = if manifest.artwork {
            capabilities | Capabilities::ARTWORK
        } else {
            capabilities
        };
        let capabilities = if manifest.lyrics {
            capabilities | Capabilities::LYRICS
        } else {
            capabilities
        };
        let capabilities = if manifest.video {
            capabilities | Capabilities::VIDEO
        } else {
            capabilities
        };
        // Similar songs: api 6, D-087.
        let capabilities = if manifest.related {
            capabilities | Capabilities::RELATED
        } else {
            capabilities
        };
        // Links: api 7, D-095 — a template named is the yes.
        let link_templates = manifest.link_templates();
        let capabilities = if link_templates.is_empty() {
            capabilities
        } else {
            capabilities | Capabilities::LINKS
        };

        let spec = ScriptSpec {
            plugin: manifest.name.clone(),
            main: manifest.main_path(dir),
            module_name: manifest.main.trim_start_matches("./").to_owned(),
            capabilities,
            permissions: manifest.permissions.normalized(),
            secrets: secrets.namespace(&plugin_namespace(&manifest.name)),
            state_dir,
            http,
            store: ArtifactStore::new(config),
            requires: manifest.requires.clone(),
        };

        Ok(Self {
            id: ProviderId::new(manifest.name.clone()),
            display_name: manifest.display_name.clone(),
            capabilities,
            permissions: manifest.permissions.normalized(),
            link_templates,
            spec,
            start_timeout: START_TIMEOUT,
            call_timeout: CALL_TIMEOUT,
            gate: Gate::default(),
            state: std::sync::Mutex::new(SessionState::default()),
        })
    }

    /// Changes the time limits. For testing only: a test of the timeout must not
    /// wait 20 seconds.
    #[must_use]
    pub fn with_timeouts(mut self, start: Duration, call: Duration) -> Self {
        self.start_timeout = start;
        self.call_timeout = call;
        self
    }

    /// Prepares the engine (starting it if needed) and calls a function.
    ///
    /// A timeout and a dead thread drop the engine: the next call restarts it.
    /// An error the plugin throws does not drop the engine — a rejected request
    /// is not a broken engine (the plugin version of D-023).
    ///
    /// The call waits for its turn at the [`Gate`]: one made on the user's
    /// behalf goes first; one made ahead of time may be asked to give way and
    /// then fails with [`ErrorKind::PluginPreempted`].
    fn call(
        &self,
        function: &'static str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let ahead = crate::provider::ahead_of_time::on_this_thread();
        let turn = self.gate.enter(ahead).ok_or_else(|| {
            Error::new(
                Stage::ProviderCall,
                ErrorKind::PluginCrashed {
                    plugin: self.id.as_str().to_owned(),
                    detail: "the lock ordering the plugin's calls is poisoned".to_owned(),
                },
            )
        })?;
        let mut state = self.state.lock().map_err(|_| {
            Error::new(
                Stage::ProviderCall,
                ErrorKind::PluginCrashed {
                    plugin: self.id.as_str().to_owned(),
                    detail: "the plugin state lock is poisoned (an earlier call panicked)"
                        .to_owned(),
                },
            )
        })?;

        if let Some(reason) = &state.give_up {
            return Err(Error::new(
                Stage::PluginStart,
                ErrorKind::PluginCrashed {
                    plugin: self.id.as_str().to_owned(),
                    detail: reason.clone(),
                },
            ));
        }

        if state.worker.is_none() {
            self.start(&mut state)?;
        }
        let Some(worker) = state.worker.as_mut() else {
            return Err(Error::new(
                Stage::PluginStart,
                ErrorKind::PluginCrashed {
                    plugin: self.id.as_str().to_owned(),
                    detail: "the plugin could not be started".to_owned(),
                },
            ));
        };

        let outcome = worker.call(function, args, self.call_timeout, turn.cancel.clone());
        if let Err(err) = &outcome
            && matches!(
                err.kind(),
                ErrorKind::PluginCrashed { .. } | ErrorKind::PluginTimeout { .. }
            )
        {
            // We do not carry on with the state a timed-out call left half done:
            // the engine is dropped and the next call starts clean.
            state.worker = None;
        }
        // Asked to give way and failed: it gave way — whatever the plugin made
        // of the stopped tool, this is not the track failing (K9).
        let gave_way = turn
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire));
        match outcome {
            Err(err) if gave_way => {
                tracing::debug!(
                    plugin = %self.id,
                    method = function,
                    error = %err.chain_text().replace('\n', " "),
                    "a call made ahead of time gave way"
                );
                Err(Error::new(
                    Stage::ProviderCall,
                    ErrorKind::PluginPreempted {
                        plugin: self.id.as_str().to_owned(),
                        method: function.to_owned(),
                    },
                ))
            }
            outcome => outcome,
        }
    }

    fn start(&self, state: &mut SessionState) -> Result<()> {
        if state.starts >= MAX_STARTS {
            let reason =
                format!("started {MAX_STARTS} times and it fell over every time; giving up");
            state.give_up = Some(reason.clone());
            return Err(Error::new(
                Stage::PluginStart,
                ErrorKind::PluginCrashed {
                    plugin: self.id.as_str().to_owned(),
                    detail: reason,
                },
            ));
        }
        state.starts += 1;

        match ScriptWorker::start(self.spec.clone(), self.start_timeout) {
            Ok(worker) => {
                state.worker = Some(worker);
                Ok(())
            }
            Err(err) => {
                // A contract violation and a missing engine are not fixed by retrying.
                if matches!(
                    err.kind(),
                    ErrorKind::PluginContract { .. } | ErrorKind::Unsupported { .. }
                ) {
                    state.give_up = Some(err.chain_text().replace('\n', " "));
                }
                Err(err)
            }
        }
    }

    /// Shuts the engine down. The next call restarts it.
    pub fn shutdown(&self) {
        match self.state.lock() {
            Ok(mut state) => {
                if let Some(mut worker) = state.worker.take() {
                    worker.shutdown();
                }
            }
            Err(_) => tracing::warn!(plugin = %self.id, "the lock was poisoned during shutdown"),
        }
    }

    fn unsupported(&self, what: &str) -> Error {
        Error::new(
            Stage::ProviderCall,
            ErrorKind::Unsupported {
                provider: self.id.as_str().to_owned(),
                what: what.to_owned(),
                capabilities: self.capabilities.describe(),
            },
        )
    }

    fn contract(&self, method: &str, detail: String) -> Error {
        Error::new(
            Stage::ProviderCall,
            ErrorKind::PluginContract {
                plugin: self.id.as_str().to_owned(),
                method: method.to_owned(),
                detail,
            },
        )
    }

    fn health_now(&self) -> Result<HealthResult> {
        let value = self.call(export::HEALTH, Vec::new())?;
        serde_json::from_value(value).map_err(|err| {
            self.contract(
                export::HEALTH,
                format!("expected `{{ reachable, detail?, track_count? }}`: {err}"),
            )
        })
    }

    fn search_now(&self, query: &str, limit: usize) -> Result<Vec<ProviderTrack>> {
        if !self.capabilities.contains(Capabilities::SEARCH) {
            return Err(self.unsupported("search"));
        }
        let value = self.call(
            export::SEARCH,
            vec![serde_json::json!(query), serde_json::json!(limit)],
        )?;
        self.tracks_from(export::SEARCH, value)
    }

    /// The songs like an item (api 6, D-087). Only the item's id goes in.
    /// The item itself, if the plugin names it, is left out; the answer is
    /// cut to `limit` — a plugin's long list must not become a long queue.
    fn related_now(&self, id: &ProviderTrackId, limit: usize) -> Result<Vec<ProviderTrack>> {
        if !self.capabilities.contains(Capabilities::RELATED) {
            return Err(self.unsupported("similar songs"));
        }
        if id.provider != self.id {
            return Err(Error::new(
                Stage::ProviderCall,
                ErrorKind::InvalidInput {
                    detail: format!(
                        "a {} id cannot be asked of the {} plugin",
                        id.provider, self.id
                    ),
                },
            ));
        }
        let value = self.call(
            export::RELATED,
            vec![serde_json::json!(id.id), serde_json::json!(limit)],
        )?;
        let mut tracks = self.tracks_from(export::RELATED, value)?;
        tracks.retain(|track| track.id != *id);
        tracks.truncate(limit);
        Ok(tracks)
    }

    /// What is at a link that fitted one of its templates (api 7, D-095):
    /// what stood in `{id}` and the template go in, its tracks come out.
    fn link_now(&self, id: &str, template: &str) -> Result<Vec<ProviderTrack>> {
        if !self.capabilities.contains(Capabilities::LINKS) {
            return Err(self.unsupported("links"));
        }
        let value = self.call(
            export::LINK,
            vec![serde_json::json!(id), serde_json::json!(template)],
        )?;
        self.tracks_from(export::LINK, value)
    }

    /// An export's array of tracks as core tracks; malformed ISRCs are
    /// dropped and counted (K9).
    fn tracks_from(&self, name: &str, value: serde_json::Value) -> Result<Vec<ProviderTrack>> {
        let wires: Vec<WireTrack> = serde_json::from_value(value).map_err(|err| {
            self.contract(
                name,
                format!("expected an array of `{{ id, artist, title, … }}`: {err}"),
            )
        })?;

        let mut tracks = Vec::with_capacity(wires.len());
        let mut dropped_isrc = 0usize;
        for wire in wires {
            let (track, dropped) = wire.into_provider_track(&self.id);
            if dropped {
                dropped_isrc += 1;
            }
            tracks.push(track);
        }
        if dropped_isrc > 0 {
            // We count and report, we do not swallow (K9).
            tracing::warn!(
                plugin = %self.id,
                dropped = dropped_isrc,
                "the plugin sent malformed ISRCs; those fields were dropped"
            );
        }
        Ok(tracks)
    }

    fn resolve_now(&self, id: &ProviderTrackId) -> Result<Option<AudioSource>> {
        if !self.capabilities.contains(Capabilities::STREAM) {
            return Err(self.unsupported("resolving sources"));
        }
        if id.provider != self.id {
            return Err(Error::new(
                Stage::PlaybackResolve,
                ErrorKind::InvalidInput {
                    detail: format!(
                        "a {} id cannot be asked of the {} plugin",
                        id.provider, self.id
                    ),
                },
            ));
        }
        let value = self.call(export::RESOLVE_SOURCE, vec![serde_json::json!(id.id)])?;
        let source: SourceResult = serde_json::from_value(value).map_err(|err| {
            self.contract(
                export::RESOLVE_SOURCE,
                format!("expected `{{ kind: \"http_stream\", url, headers }}` or `null`: {err}"),
            )
        })?;
        if let Some(source) = &source
            && let Err(reason) = protocol::check_source(source, &self.permissions)
        {
            return Err(self.contract(export::RESOLVE_SOURCE, reason));
        }
        Ok(source)
    }

    /// The plugin's cover (api 3, D-076): it fetched the image itself,
    /// through `host.http` in binary mode, and hands it over as base64. The
    /// core does not fetch an address the plugin names (K5).
    fn artwork_now(&self, id: &ProviderTrackId, size: u32) -> Result<Option<ArtworkImage>> {
        if !self.capabilities.contains(Capabilities::ARTWORK) {
            return Err(self.unsupported("cover art"));
        }
        if id.provider != self.id {
            return Err(Error::new(
                Stage::ArtworkRead,
                ErrorKind::InvalidInput {
                    detail: format!(
                        "a {} id cannot be asked of the {} plugin",
                        id.provider, self.id
                    ),
                },
            ));
        }
        let value = self.call(
            export::ARTWORK,
            vec![serde_json::json!(id.id), serde_json::json!(size)],
        )?;
        let wire: Option<WireArtwork> = serde_json::from_value(value).map_err(|err| {
            self.contract(
                export::ARTWORK,
                format!("expected `{{ mime?, data }}` with `data` in base64, or `null`: {err}"),
            )
        })?;
        let Some(wire) = wire else {
            return Ok(None);
        };
        let bytes = crate::encoding::base64_decode(&wire.data)
            .ok_or_else(|| self.contract(export::ARTWORK, "`data` is not base64".to_owned()))?;
        Ok(Some(ArtworkImage {
            bytes,
            mime: wire.mime,
            source: crate::artwork::ArtworkSource::Provider,
        }))
    }

    /// The plugin's lyrics (api 4, D-078): the service's own for the track.
    /// `synced` is LRC text, read with the same parser as a `.lrc` file.
    fn lyrics_now(&self, id: &ProviderTrackId) -> Result<Option<ProviderLyrics>> {
        if !self.capabilities.contains(Capabilities::LYRICS) {
            return Err(self.unsupported("lyrics"));
        }
        if id.provider != self.id {
            return Err(Error::new(
                Stage::LyricsRead,
                ErrorKind::InvalidInput {
                    detail: format!(
                        "a {} id cannot be asked of the {} plugin",
                        id.provider, self.id
                    ),
                },
            ));
        }
        let value = self.call(export::LYRICS, vec![serde_json::json!(id.id)])?;
        let wire: Option<WireLyrics> = serde_json::from_value(value).map_err(|err| {
            self.contract(
                export::LYRICS,
                format!(
                    "expected `{{ synced?, plain?, instrumental?, attribution? }}` or `null`: {err}"
                ),
            )
        })?;
        let Some(wire) = wire else {
            return Ok(None);
        };
        fn text(field: &Option<String>) -> Option<&str> {
            field.as_deref().filter(|t| !t.trim().is_empty())
        }
        let mut lyrics = match (text(&wire.synced), text(&wire.plain)) {
            (Some(synced), _) => {
                let lyrics = crate::lyrics::lrc::parse(synced);
                if !lyrics.synced {
                    return Err(self.contract(
                        export::LYRICS,
                        "`synced` has no `[mm:ss.xx]` stamp — unsynced words go in `plain`"
                            .to_owned(),
                    ));
                }
                lyrics
            }
            (None, Some(plain)) => {
                crate::lyrics::Lyrics::unsynced(plain.lines().map(str::to_owned))
            }
            (None, None) if wire.instrumental => crate::lyrics::Lyrics::default(),
            (None, None) => return Ok(None),
        };
        lyrics.attribution = wire.attribution.filter(|a| !a.trim().is_empty());
        Ok(Some(ProviderLyrics {
            lyrics,
            instrumental: wire.instrumental,
            source: crate::lyrics::LyricsSource::Provider,
        }))
    }

    /// The plugin's video for an item (api 5, D-086): a source the app's
    /// own player shows. A plugin cannot hand over a file — the same line
    /// `resolve_source` draws — and a YouTube id must have its shape.
    fn video_now(&self, id: &ProviderTrackId) -> Result<Option<crate::video::VideoSource>> {
        if !self.capabilities.contains(Capabilities::VIDEO) {
            return Err(self.unsupported("videos"));
        }
        if id.provider != self.id {
            return Err(Error::new(
                Stage::VideoLookup,
                ErrorKind::InvalidInput {
                    detail: format!(
                        "a {} id cannot be asked of the {} plugin",
                        id.provider, self.id
                    ),
                },
            ));
        }
        let value = self.call(export::RESOLVE_VIDEO, vec![serde_json::json!(id.id)])?;
        let source: Option<crate::video::VideoSource> =
            serde_json::from_value(value).map_err(|err| {
                self.contract(
                    export::RESOLVE_VIDEO,
                    format!("expected `{{ kind: \"youtube\", video_id }}` or `null`: {err}"),
                )
            })?;
        match &source {
            Some(crate::video::VideoSource::LocalFile { .. }) => Err(self.contract(
                export::RESOLVE_VIDEO,
                "a plugin cannot hand over a file — only the local provider gives `local_file`"
                    .to_owned(),
            )),
            Some(source) => match source.check() {
                Ok(()) => Ok(Some(source.clone())),
                Err(err) => Err(self.contract(export::RESOLVE_VIDEO, err.chain_text())),
            },
            None => Ok(None),
        }
    }
}

impl Provider for PluginProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            capabilities: self.capabilities,
        }
    }

    fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
        Box::pin(std::future::ready(Ok(match self.health_now() {
            Ok(health) => ProviderHealth {
                id: self.id.clone(),
                reachable: health.reachable,
                track_count: health.track_count,
                detail: health.detail,
            },
            // Being unreachable is a health **answer** (the same rule as a remote
            // provider): `provider test` must show the reason.
            Err(err) => ProviderHealth {
                id: self.id.clone(),
                reachable: false,
                track_count: None,
                detail: Some(err.chain_text().replace('\n', " ")),
            },
        })))
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
        Box::pin(std::future::ready(self.search_now(query, limit)))
    }

    fn resolve_source<'a>(
        &'a self,
        id: &'a ProviderTrackId,
    ) -> ProviderFuture<'a, Option<AudioSource>> {
        Box::pin(std::future::ready(self.resolve_now(id)))
    }

    fn artwork<'a>(
        &'a self,
        id: &'a ProviderTrackId,
        size: u32,
    ) -> ProviderFuture<'a, Option<ArtworkImage>> {
        Box::pin(std::future::ready(self.artwork_now(id, size)))
    }

    fn lyrics<'a>(&'a self, id: &'a ProviderTrackId) -> ProviderFuture<'a, Option<ProviderLyrics>> {
        Box::pin(std::future::ready(self.lyrics_now(id)))
    }

    fn resolve_video<'a>(
        &'a self,
        id: &'a ProviderTrackId,
    ) -> ProviderFuture<'a, Option<crate::video::VideoSource>> {
        Box::pin(std::future::ready(self.video_now(id)))
    }

    fn related<'a>(
        &'a self,
        id: &'a ProviderTrackId,
        limit: usize,
    ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
        Box::pin(std::future::ready(self.related_now(id, limit)))
    }

    fn link_templates(&self) -> Vec<crate::link::LinkTemplate> {
        self.link_templates.clone()
    }

    fn open_link<'a>(
        &'a self,
        id: &'a str,
        template: &'a str,
    ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
        Box::pin(std::future::ready(self.link_now(id, template)))
    }

    /// A plugin's tracks come from the internet anyway: their lyrics are
    /// looked up at LRCLIB the moment they start (D-078).
    fn is_plugin(&self) -> bool {
        true
    }
}

impl Drop for PluginProvider {
    fn drop(&mut self) {
        // If the provider is dropped, no thread or secret file is left behind.
        self.shutdown();
    }
}

/// Runs a future to completion on the calling thread.
///
/// The core does not set up a runtime (convention: the caller chooses the
/// runtime), but some places are synchronous: `host.http` on the plugin's
/// thread, the install command, and the cover chain on its worker's thread
/// (D-076). `HttpClient::send` is `async`. The gap between them closes here.
///
/// The wait is **not busy**: if the future is not ready the thread sleeps
/// and the waker wakes it. The `ureq` client is ready on the first poll
/// anyway; a shell that supplies its own asynchronous client (mobile) is
/// waited for without burning the CPU.
pub(crate) fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    use std::pin::pin;
    use std::task::{Context, Poll, Wake, Waker};

    struct ThreadWaker(std::thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// The fallback for a build without the engine: the same surface, every
/// start an error.
///
/// In this build plugins are still discovered, listed and approved — the
/// user should see what is installed. They just **cannot run**, and the
/// first call says so (K9).
#[cfg(not(feature = "plugin-engine"))]
mod unavailable {
    use std::time::Duration;

    use super::ScriptSpec;
    use crate::diag::Stage;
    use crate::error::{Error, ErrorKind, Result};

    #[derive(Debug)]
    pub(crate) struct ScriptWorker;

    fn missing(plugin: &str) -> Error {
        Error::new(
            Stage::PluginStart,
            ErrorKind::Unsupported {
                provider: plugin.to_owned(),
                what: "running plugins (a build with the `plugin-engine` feature off)".to_owned(),
                capabilities: "NONE".to_owned(),
            },
        )
    }

    impl ScriptWorker {
        pub(crate) fn start(spec: ScriptSpec, _timeout: Duration) -> Result<Self> {
            Err(missing(&spec.plugin))
        }

        pub(crate) fn call(
            &mut self,
            _function: &'static str,
            _args: Vec<serde_json::Value>,
            _timeout: Duration,
            _cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        ) -> Result<serde_json::Value> {
            Err(missing("?"))
        }

        pub(crate) fn shutdown(&mut self) {}
    }
}

#[cfg(test)]
mod tests;
