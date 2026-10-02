//! The core's outer surface.
//!
//! The CLI, the GUI and the mobile bindings call **only** the methods here:
//! every command is a single call, and diagnostics and persistence are taken
//! care of here. Delete a capability from the CLI and the core still offers
//! it — the Golden Rule's test.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{Config, MusicFolders};
use crate::diag::{DiagReport, Recorder, Stage};
use crate::error::{Error, ErrorKind, Result};
use crate::identity::{
    FingerprintLookup, MetadataLookup, OfflineLookup, Resolution, ResolveSummary, Resolver,
};
use crate::ids::{ProviderId, ProviderTrackId};
use crate::import::{self, ImportSummary};
use crate::library::{
    CatalogStore, CatalogTrack, CatalogWriteSummary, ListenStore, SearchHit, SqliteLibrary,
    WriteSummary,
};
use crate::model::{Listen, PlayRule, TrackRef};
use crate::net::HttpClient;
use crate::playback::QueueItem;
use crate::plugin::artifact::{ArtifactStore, InstallOutcome};
use crate::plugin::catalog::{
    self, CatalogFetch, CatalogPlugin, CatalogSummary, IndexedPlugin, Removed, UpdateOutcome,
    UpdateSummary,
};
use crate::plugin::consent::{ConsentStatus, ConsentStore};
use crate::plugin::manifest::{Permissions, PluginManifest};
use crate::plugin::{PluginEntry, PluginSummary};
use crate::provider::remote::{self, NewServer, RemoteServer, ServerKind, StoredAuth};
use crate::provider::{ProviderHealth, ProviderInfo, ProviderRegistry, ScanSummary};
use crate::secrets::Secrets;
use crate::sleeve::{self, CardSize, SleeveData};
use crate::stats::{self, StatsQuery, StatsReport};

/// The full result of an import command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportReport {
    pub import: ImportSummary,
    pub identity: ResolveSummary,
    pub write: WriteSummary,
    pub diag: DiagReport,
}

/// The result of resolving a single track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveReport {
    pub query: String,
    pub artist: String,
    pub title: String,
    pub resolution: Resolution,
    pub diag: DiagReport,
}

/// Search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchReport {
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub diag: DiagReport,
}

/// Statistics result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatsResponse {
    pub report: StatsReport,
    pub diag: DiagReport,
}

/// Sleeve card result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SleeveResponse {
    pub data: SleeveData,
    pub size: CardSize,
    /// Format, path and byte count, if a file was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written: Option<WrittenCard>,
    pub diag: DiagReport,
}

/// The written card file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WrittenCard {
    pub path: std::path::PathBuf,
    pub bytes: u64,
    pub kind: sleeve::CardFileKind,
}

/// The list of registered providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderListReport {
    pub providers: Vec<ProviderInfo>,
    /// Which of them the play box asks first (D-080) — the music role.
    pub play_from: PlayFrom,
    /// Who plays each role (D-086).
    pub roles: ProviderRoles,
    pub diag: DiagReport,
}

/// Where the play box and its suggestions search first (D-080). The others
/// are asked only when this one finds nothing.
///
/// Since D-086 the same shape says who plays any [`Role`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayFrom {
    /// The provider asked first; `None` when no registered provider can play.
    pub provider: Option<ProviderId>,
    /// The user chose it — for this role, or for the main role this one
    /// follows; `false`: nothing chosen yet, so the registry's first one
    /// that can play — local files.
    pub chosen: bool,
    /// Its answers come from the scanned catalog: at once, without the
    /// network. Otherwise it is asked over its own connection, and the
    /// interface waits for a pause in typing before asking.
    pub instant: bool,
    /// A choice that cannot be kept — the provider is no longer registered
    /// or can no longer play (a server removed, a plugin disabled). Said, not
    /// dropped (K9).
    pub missing: Option<ProviderId>,
    /// Music or video with no choice of its own: the main provider's — or,
    /// when the main one cannot play this role, the registry's first that
    /// can (D-086). Always `false` for the main role.
    #[serde(default)]
    pub follows_main: bool,
}

/// A part a provider plays (D-086).
///
/// The play box searches the **music** provider; a song's video is found at
/// the **video** provider; both follow the **main** provider until the user
/// picks them apart, and changing the main one moves those that follow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Main,
    Music,
    Video,
}

impl Role {
    /// The name the CLI and the JSON use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Music => "music",
            Self::Video => "video",
        }
    }

    /// Reads a role's name.
    ///
    /// # Errors
    /// If it is not one of `main`, `music`, `video`.
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim() {
            "main" => Ok(Self::Main),
            "music" => Ok(Self::Music),
            "video" => Ok(Self::Video),
            other => Err(Error::new(
                Stage::ConfigLoad,
                ErrorKind::InvalidInput {
                    detail: format!("{other:?} is not a role — it is main, music or video"),
                },
            )),
        }
    }

    /// Can a provider with these capabilities play this role? Every role
    /// searches; music and the main role play — sound, or a video for the
    /// sound — and the video role shows videos.
    #[must_use]
    pub fn fits(self, capabilities: crate::provider::Capabilities) -> bool {
        use crate::provider::Capabilities as C;
        capabilities.contains(C::SEARCH)
            && match self {
                Self::Main | Self::Music => {
                    capabilities.contains(C::STREAM) || capabilities.contains(C::VIDEO)
                }
                Self::Video => capabilities.contains(C::VIDEO),
            }
    }
}

/// Who plays each role (D-086).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRoles {
    pub main: PlayFrom,
    pub music: PlayFrom,
    pub video: PlayFrom,
}

/// What `headshell provider use --for <role>` and `provider follow` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RolesReport {
    pub roles: ProviderRoles,
    pub diag: DiagReport,
}

/// What `headshell video` looks a video up for (D-086).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoTarget {
    /// `Artist - Title`.
    Query(String),
    /// A file: its own video first, when the local provider shows videos.
    File(PathBuf),
}

/// What `headshell video` answers: the videos found for a track (D-086).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoReport {
    pub plan: crate::video::VideoPlan,
    pub diag: DiagReport,
}

/// What `headshell provider use` answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayFromReport {
    pub play_from: PlayFrom,
    pub diag: DiagReport,
}

/// Who set the desktop's lookup mode (D-082).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnlineSource {
    /// [`ONLINE_ENV`], for one launch.
    Variable,
    /// The choice saved in `preferences.json`.
    Saved,
    /// Nothing set: offline, as a fresh install is.
    Default,
}

/// The desktop's online choice (D-082), and everything that decides it.
///
/// The CLI does not go by it — every run there chooses with `--online` — so
/// what this says is what a desktop started from here goes by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnlineChoice {
    /// What the desktop goes by.
    pub mode: LookupMode,
    /// Who decided `mode` (K9: a choice that does not hold is said, not
    /// silently overridden).
    pub source: OnlineSource,
    /// The saved choice; `None`: nothing saved.
    pub saved: Option<LookupMode>,
    /// [`ONLINE_ENV`] as this process read it; `None`: unset.
    pub variable: Option<LookupMode>,
}

impl OnlineChoice {
    /// The variable first — it is set for one launch on purpose — then the
    /// saved choice, then offline.
    #[must_use]
    pub const fn decide(saved: Option<LookupMode>, variable: Option<LookupMode>) -> Self {
        let (mode, source) = match (variable, saved) {
            (Some(mode), _) => (mode, OnlineSource::Variable),
            (None, Some(mode)) => (mode, OnlineSource::Saved),
            (None, None) => (LookupMode::Offline, OnlineSource::Default),
        };
        Self {
            mode,
            source,
            saved,
            variable,
        }
    }
}

/// What `headshell online [on|off]` answers (D-082).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnlineReport {
    pub online: OnlineChoice,
    pub diag: DiagReport,
}

/// What `headshell music-dirs [add|remove]` answers (D-085).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MusicFoldersReport {
    pub folders: MusicFolders,
    /// Whether the chosen folders changed: adding a folder already on the
    /// list changes nothing, and says so.
    pub changed: bool,
    pub diag: DiagReport,
}

/// The result of testing a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderTestReport {
    pub info: ProviderInfo,
    pub health: ProviderHealth,
    pub diag: DiagReport,
}

/// The list of installed plugins (Phase 2 §2.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginListReport {
    pub plugins: Vec<PluginEntry>,
    pub summary: PluginSummary,
    /// Are permissions enforced — **no** (D-040), and every output says so.
    /// The field is visible rather than implied so no one relies on a
    /// protection that does not exist.
    pub permissions_enforced: bool,
    pub diag: DiagReport,
}

/// The result of a consent command (`approve`, `disable`, `enable`, `forget`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginConsentReport {
    pub name: String,
    /// Which command ran.
    pub action: String,
    /// The permissions the plugin **declares**.
    pub permissions: Permissions,
    /// The artifacts the engine will download for it (D-055, D-069).
    ///
    /// Kept **apart** from `permissions.net`, on purpose: the engine does the
    /// download, not the plugin. Mixed into the same list, the user would read
    /// "this plugin connects to github.com" — it is the engine that connects,
    /// and what it downloads is pinned by its hash.
    #[serde(default)]
    pub requires: Vec<crate::plugin::manifest::Requirement>,
    /// The platform the artifacts were resolved for — the consent screen shows
    /// this platform's release.
    #[serde(default)]
    pub platform: String,
    /// The state after the command.
    pub status: ConsentStatus,
    pub permissions_enforced: bool,
    pub diag: DiagReport,
}

/// Output of `headshell plugin install` (D-055, D-069, D-071).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInstallReport {
    /// Can the plugin run now: are **all** declared artifacts ready.
    pub ready: bool,
    /// The number of artifacts the manifest declares. `0` is a valid answer:
    /// the plugin needs nothing, it does not mean "didn't look" (K9).
    pub declared: usize,
    /// The platform the artifacts were resolved for (`linux-x86_64`). In api 1
    /// this held the Python the engine had found; in api 2 the interpreter is
    /// embedded and the only thing to choose is the platform's binary.
    pub platform: String,
    /// What was downloaded, if this command fetched the plugin from the catalog
    /// (D-071). `None`: the plugin was already on disk and the catalog was
    /// **not read**.
    #[serde(default)]
    pub fetched: Option<CatalogFetch>,
    /// The permissions the plugin declares — the ones up for approval.
    #[serde(default)]
    pub permissions: Permissions,
    /// Consent state after the install. Coming from the catalog is not consent
    /// (D-040): a freshly installed plugin says `not_asked`.
    pub consent: ConsentStatus,
    pub report: crate::plugin::artifact::InstallReport,
    pub diag: DiagReport,
}

/// Output of `headshell plugin catalog` (D-071).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginCatalogReport {
    /// The catalog that was read.
    pub index: String,
    /// The platform the tool state was measured for.
    pub platform: String,
    pub plugins: Vec<CatalogPlugin>,
    /// Plugins installed **from this catalog** that are no longer listed.
    pub delisted: Vec<String>,
    pub summary: CatalogSummary,
    pub diag: DiagReport,
}

/// One plugin's update result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginUpdate {
    pub name: String,
    pub outcome: UpdateOutcome,
    /// Installing the updated version's tools (D-055). Only filled in for
    /// `updated`.
    #[serde(default)]
    pub tools: Vec<(String, InstallOutcome)>,
    /// Why the tools could not be installed, if they could not. The plugin's
    /// files were updated anyway, and this says so separately: "updated" and
    /// "works" are not the same thing.
    pub tools_error: Option<String>,
    /// Consent state after the update; `needs_approval` if the permissions grew
    /// (D-040).
    pub consent: Option<ConsentStatus>,
}

/// Output of `headshell plugin update` (D-071).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginUpdateReport {
    pub index: String,
    pub platform: String,
    pub plugins: Vec<PluginUpdate>,
    pub summary: UpdateSummary,
    pub diag: DiagReport,
}

/// Output of `headshell plugin remove` (D-071).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginRemoveReport {
    pub name: String,
    pub removed: Removed,
    /// There was a consent record and it was forgotten: a plugin reinstalled
    /// under the same name is asked again from scratch — another plugin must
    /// not inherit the old one's consent.
    pub consent_forgotten: bool,
    /// Key names of the secrets **left** in the plugin's namespace (D-042).
    /// Not deleted: a value the user typed in (a cookie, a key) must not vanish
    /// silently. The values are not in this list.
    pub kept_secrets: Vec<String>,
    pub diag: DiagReport,
}

/// Output of `headshell plugin index` — catalog repository maintenance
/// (D-071).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginIndexReport {
    /// The index file.
    pub path: std::path::PathBuf,
    pub url_template: String,
    pub plugins: Vec<IndexedPlugin>,
    /// Was the index on disk already identical to the generated one.
    pub up_to_date: bool,
    /// Did this command write the file (`--check` never writes).
    pub written: bool,
    pub diag: DiagReport,
}

/// The secret store's **key names** (no values, D-042).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretListReport {
    pub namespaces: std::collections::BTreeMap<String, Vec<String>>,
    pub diag: DiagReport,
}

/// The result of writing or removing a secret. Carries **no** value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretWriteReport {
    pub namespace: String,
    pub key: String,
    pub action: String,
    pub changed: bool,
    pub diag: DiagReport,
}

/// Scan result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanReport {
    /// The root directories scanned. Empty if the user gave no
    /// `HEADSHELL_MUSIC_DIRS`.
    pub dirs: Vec<std::path::PathBuf>,
    pub summary: ScanSummary,
    /// What was written to the persistent catalog: inserted, updated and
    /// dropped rows.
    pub write: CatalogWriteSummary,
    /// Did the scan actually run. `--if-stale` may have skipped it (D-025).
    pub scanned: bool,
    /// Why it ran or why it was skipped — this belongs to diagnostics (K9):
    /// "unchanged" and "couldn't look" are different things.
    pub reason: String,
    pub diag: DiagReport,
}

/// A **secret-free** summary of a registered remote server.
///
/// The token and the API key are **not** in this type: `--json` output can
/// end up in a pipeline, a log or a bug report. Credentials live only in
/// `servers.json` with `0600` permissions (D-021).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSummary {
    pub id: ProviderId,
    pub kind: ServerKind,
    pub url: String,
    pub username: String,
    /// Which credential path: `subsonic_token` or `api_key`.
    pub auth: String,
}

impl ServerSummary {
    fn of(server: &RemoteServer) -> Self {
        Self {
            id: server.id.clone(),
            kind: server.kind,
            url: server.url.clone(),
            username: server.username.clone(),
            auth: match server.auth {
                StoredAuth::SubsonicToken { .. } => "subsonic_token".to_owned(),
                StoredAuth::ApiKey { .. } => "api_key".to_owned(),
            },
        }
    }
}

/// The list of registered servers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerListReport {
    pub servers: Vec<ServerSummary>,
    /// Path of the registry file — "where was it written?" is a diagnostics
    /// question.
    pub path: std::path::PathBuf,
    pub diag: DiagReport,
}

/// Result of adding a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAddReport {
    pub server: ServerSummary,
    /// Was the server contacted and the login verified.
    pub verified: bool,
    /// Observations made while registering (weak entropy, a field that could
    /// not be learned…).
    pub notes: Vec<String>,
    pub diag: DiagReport,
}

/// Result of removing a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRemoveReport {
    pub id: ProviderId,
    pub removed: bool,
    pub remaining: usize,
    pub diag: DiagReport,
}

/// Playback options.
///
/// A separate struct: it crosses `uniffi` as a single record, and adding an
/// option does not break callers' signatures.
///
/// `query` is an **owned** `String`, not a borrow: K7 forbids lifetimes in
/// exported types and `uniffi` cannot express `&str` in a record field. The
/// copy costs one short string per command.
#[derive(Debug, Clone)]
pub struct PlayOptions {
    /// The text to search for.
    pub query: String,
    /// Queue every matching track (false: only the first).
    pub all: bool,
    /// Shuffle the queue.
    pub shuffle: bool,
    /// Show the queue without playing.
    pub dry_run: bool,
    /// The maximum number of search results to take.
    pub limit: usize,
    /// The shell shows videos (D-086): a track that is a video only plays
    /// as one; without, it is passed by and counted.
    pub shows_video: bool,
}

impl PlayOptions {
    /// With default options: play the first match.
    #[must_use]
    pub fn new(query: String) -> Self {
        Self {
            query,
            all: false,
            shuffle: false,
            dry_run: false,
            limit: 100,
            shows_video: false,
        }
    }
}

/// What `headshell artwork` looks at (D-076).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkRequest {
    /// A play query: the covers of the tracks it finds, in play order.
    pub query: Option<String>,
    /// Every match instead of the first (`--all`).
    pub all: bool,
    /// A file on disk instead of a query (`--file`): its tags and its folder,
    /// then — online — the third parties.
    pub file: Option<std::path::PathBuf>,
    /// A directory to write the covers to (`--out`): one image per album.
    pub out: Option<std::path::PathBuf>,
}

/// What `headshell lyrics` looks at (D-078).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricsRequest {
    /// A play query: the lyrics of the tracks it finds, in play order.
    pub query: Option<String>,
    /// Every match instead of the first (`--all`).
    pub all: bool,
    /// A file on disk instead of a query (`--file`): its `.lrc` and its tags,
    /// then LRCLIB.
    pub file: Option<std::path::PathBuf>,
    /// Put the lyrics themselves into the report (`--print`).
    pub print: bool,
    /// Keep this shift for every track the report names (`--shift`), in ms;
    /// positive, the lines come sooner. `0` takes a shift back.
    pub shift_ms: Option<i64>,
}

/// The result of a play command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayReport {
    pub query: String,
    /// The tracks that were queued.
    pub queued: Vec<QueueItem>,
    /// Listen records produced by playback (§1.6).
    pub listens_recorded: usize,
    /// Did it actually play (false with `--dry-run`).
    pub played: bool,
    /// Queued tracks that are videos only and were passed by: they play in
    /// the app's window, not here (D-086). With `--dry-run`, the ones that
    /// would be.
    #[serde(default)]
    pub video_only: usize,
    pub diag: DiagReport,
}

/// A session working on an open library.
pub struct Session {
    config: Config,
    library: SqliteLibrary,
}

impl Session {
    /// Opens the library named in the configuration (creating it if needed).
    ///
    /// # Errors
    /// If the data directory cannot be created or the database cannot be
    /// opened.
    pub fn open(config: Config) -> Result<Self> {
        config.ensure_data_dir()?;
        let library = SqliteLibrary::open(config.database_path())?;
        Ok(Self { config, library })
    }

    /// The configuration in use.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Imports an export archive, resolves identities and writes to the
    /// library.
    ///
    /// The metadata source is supplied by the caller; in Phase 0 it is called
    /// with [`OfflineLookup`], and the signature does not change once the
    /// network arrives. `Arc<dyn _>`, not a generic — K7 (`uniffi` cannot
    /// express generics).
    ///
    /// # Errors
    /// If the archive cannot be read or recognised, the resolution source
    /// fails, or writing fails. The error carries the stage it happened in.
    pub async fn import_archive(
        &mut self,
        path: &Path,
        lookup: Arc<dyn MetadataLookup>,
    ) -> Result<ImportReport> {
        let mut rec = Recorder::start(
            format!("import {}", path.display()),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = self.import_inner(path, lookup, &mut rec).await;
        self.finish(rec, result, |(import, identity, write), diag| {
            ImportReport {
                import,
                identity,
                write,
                diag,
            }
        })
    }

    async fn import_inner(
        &mut self,
        path: &Path,
        lookup: Arc<dyn MetadataLookup>,
        rec: &mut Recorder,
    ) -> Result<(ImportSummary, ResolveSummary, WriteSummary)> {
        let mut archive = open_archive(path)?;
        let outcome = import::import(archive.as_mut())?;
        outcome.summary.record_into(rec);

        let tracks: Vec<TrackRef> = outcome
            .listens
            .iter()
            .map(|listen| listen.track.clone())
            .collect();
        let resolver = Resolver::new(lookup);
        let (resolutions, identity) = resolver.resolve_all(&tracks).await?;
        identity.record_into(rec);

        let mut listens = outcome.listens;
        for (listen, resolution) in listens.iter_mut().zip(&resolutions) {
            listen.canonical_id = Some(resolution.canonical_id.clone());
        }

        let write = self.library.insert_listens(&listens)?;
        write.record_into(rec);

        // Also write the resolutions onto the track rows, so `search` and later
        // runs know which method resolved them.
        let mut seen = std::collections::HashSet::new();
        for (listen, resolution) in listens.iter().zip(&resolutions) {
            let key =
                crate::identity::normalize::track_key(&listen.track.artist, &listen.track.title);
            if seen.insert(key.clone()) {
                self.library.set_resolution(&key, resolution)?;
            }
        }

        Ok((outcome.summary, identity, write))
    }

    /// Runs a single `"Artist - Title"` query through the identity chain.
    ///
    /// # Errors
    /// If the query is malformed or the metadata source fails.
    pub async fn resolve_track(
        &self,
        query: &str,
        lookup: Arc<dyn MetadataLookup>,
    ) -> Result<ResolveReport> {
        let mut rec = Recorder::start(
            format!("resolve {query:?}"),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = async {
            let track = TrackRef::parse_query(query)?;
            let resolution = Resolver::new(lookup).resolve(&track).await?;
            rec.set(
                "identity.confidence_pct",
                (resolution.confidence * 100.0) as i64,
            );
            // Ties are part of the diagnosis: "97% confidence" and "97% confidence,
            // 10 candidates tied" do not describe the same run (K9).
            rec.set(
                "identity.tied_candidates",
                i64::try_from(resolution.tied_candidates).unwrap_or(i64::MAX),
            );
            rec.note(format!("method: {}", resolution.method));
            Ok((track, resolution))
        }
        .await;

        let query = query.to_owned();
        self.finish(rec, result, move |(track, resolution), diag| {
            ResolveReport {
                query,
                artist: track.artist,
                title: track.title,
                resolution,
                diag,
            }
        })
    }

    /// Runs an **audio file** through the identity chain.
    ///
    /// Unlike [`Self::resolve_track`], the chain's 4th link can run too: the
    /// metadata is read from the file's own tags, and the fingerprint is asked
    /// if the text links come up empty.
    ///
    /// If `fingerprint_lookup` is `None` the chain ends after three links —
    /// this is **a configuration, not a defect**: if the user did not want to
    /// go online, AcoustID is not asked either.
    ///
    /// # Errors
    /// If the file cannot be read or a source fails.
    pub async fn resolve_file(
        &self,
        path: &Path,
        lookup: Arc<dyn MetadataLookup>,
        fingerprint_lookup: Option<Arc<dyn FingerprintLookup>>,
    ) -> Result<ResolveReport> {
        let label = path.display().to_string();
        let mut rec = Recorder::start(
            format!("resolve --file {label:?}"),
            Some(self.config.data_dir().to_path_buf()),
        );
        // Whether the link was wired up is part of the diagnosis: "no match found"
        // without a fingerprint source and not finding one with a source are not
        // the same run (K9).
        rec.set(
            "identity.fingerprint_lookup",
            i64::from(fingerprint_lookup.is_some()),
        );

        let result = async {
            let mut resolver = Resolver::new(lookup);
            if let Some(fingerprint_lookup) = fingerprint_lookup {
                resolver = resolver.with_fingerprint_lookup(fingerprint_lookup);
            }
            let (track, _) = crate::provider::local::read_track(path)?;
            let resolution = resolver.resolve_file(path).await?;
            rec.set(
                "identity.confidence_pct",
                (resolution.confidence * 100.0) as i64,
            );
            rec.set(
                "identity.tied_candidates",
                i64::try_from(resolution.tied_candidates).unwrap_or(i64::MAX),
            );
            rec.note(format!("method: {}", resolution.method));
            Ok((track, resolution))
        }
        .await;

        self.finish(rec, result, move |(track, resolution), diag| {
            ResolveReport {
                query: label,
                artist: track.artist,
                title: track.title,
                resolution,
                diag,
            }
        })
    }

    /// This setup's AcoustID source — the key is read from the secret store.
    ///
    /// Returning `None` means "no network was requested". A **missing** key is
    /// not `None`: the source is still built, and its first call returns an
    /// error that says what to do (K9 — "didn't want to" and "can't" are
    /// different).
    ///
    /// # Errors
    /// If the secret store cannot be read or this build has no HTTP client.
    #[cfg(feature = "fingerprint")]
    pub fn fingerprint_lookup_for(
        &self,
        mode: LookupMode,
    ) -> Result<Option<Arc<dyn FingerprintLookup>>> {
        use crate::identity::acoustid::{AcoustIdLookup, SECRET_KEY, SECRET_NAMESPACE};

        if mode == LookupMode::Offline {
            return Ok(None);
        }
        let secrets = Secrets::load(&self.config.secrets_path())?;
        let key = secrets
            .namespace(SECRET_NAMESPACE)
            .get(SECRET_KEY)
            .cloned()
            .unwrap_or_default();
        let lookup = AcoustIdLookup::new(crate::net::default_http_client()?).with_api_key(key);
        Ok(Some(Arc::new(lookup)))
    }

    /// This setup's AcoustID source.
    ///
    /// The `fingerprint` feature is off in this build: the chain has no 4th
    /// link and `None` says so. So that the caller does not silently see "no
    /// match", [`crate::identity::fingerprint::fingerprint_file`] also reports
    /// the same condition as an error.
    ///
    /// # Errors
    /// Never errors in this build; it returns `Result` so the signature matches
    /// the build that has the feature.
    #[cfg(not(feature = "fingerprint"))]
    pub fn fingerprint_lookup_for(
        &self,
        _mode: LookupMode,
    ) -> Result<Option<Arc<dyn FingerprintLookup>>> {
        Ok(None)
    }

    /// Full-text search over the library.
    ///
    /// The play count shown is the listens that pass `rule` — exactly the same
    /// calculation as `stats` (D-008). The raw event count is only written to
    /// the diagnostics record (`search.listen_events`).
    ///
    /// # Errors
    /// If the query is empty or on a database error.
    pub fn search(&self, query: &str, limit: usize, rule: PlayRule) -> Result<SearchReport> {
        let mut rec = Recorder::start(
            format!("library search {query:?}"),
            Some(self.config.data_dir().to_path_buf()),
        );
        let n = |v: usize| i64::try_from(v).unwrap_or(i64::MAX);
        let result = self.library.search(query, limit, rule).map(|outcome| {
            rec.set("search.hits", n(outcome.hits.len()));
            rec.set(
                "search.play_count",
                n(outcome.hits.iter().map(|hit| hit.play_count).sum()),
            );
            rec.set("search.listen_events", n(outcome.listen_events));
            outcome.hits
        });
        let query = query.to_owned();
        self.finish(rec, result, move |hits, diag| SearchReport {
            query,
            hits,
            diag,
        })
    }

    /// Builds statistics from the listens in the library.
    ///
    /// # Errors
    /// If the library cannot be read.
    pub fn stats(&self, query: StatsQuery) -> Result<StatsResponse> {
        let mut rec = Recorder::start(
            "stats".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = self.library.all_listens().map(|listens| {
            let report = stats::compute(&listens, query);
            report.record_into(&mut rec);
            report
        });
        self.finish(rec, result, |report, diag| StatsResponse { report, diag })
    }

    /// Builds a shareable Sleeve card and optionally writes it to a file.
    ///
    /// With `out`, writes SVG or PNG according to the extension; without it,
    /// only the data is returned (for JSON output or another consumer).
    ///
    /// # Errors
    /// If the library cannot be read, or rasterisation or writing the file
    /// fails.
    pub fn sleeve(
        &self,
        query: StatsQuery,
        size: CardSize,
        out: Option<&Path>,
    ) -> Result<SleeveResponse> {
        let mut rec = Recorder::start(
            format!(
                "sleeve{}",
                query.year.map_or(String::new(), |y| format!(" {y}"))
            ),
            Some(self.config.data_dir().to_path_buf()),
        );

        let result = self.library.all_listens().and_then(|listens| {
            let report = stats::compute(&listens, query);
            report.record_into(&mut rec);
            let data = sleeve::card_data(&report, &listens);
            rec.set(
                "sleeve.hours",
                i64::try_from(data.plays).unwrap_or(i64::MAX),
            );
            rec.set(
                "sleeve.discoveries",
                i64::try_from(data.discoveries.len()).unwrap_or(i64::MAX),
            );
            rec.set(
                "sleeve.timeline_years",
                i64::try_from(data.by_year.len()).unwrap_or(i64::MAX),
            );

            let written = match out {
                Some(path) => {
                    let (kind, bytes) = sleeve::write_card(&data, size, path)?;
                    rec.set(
                        "sleeve.bytes_written",
                        i64::try_from(bytes).unwrap_or(i64::MAX),
                    );
                    rec.note(format!("output: {}", path.display()));
                    Some(WrittenCard {
                        path: path.to_owned(),
                        bytes,
                        kind,
                    })
                }
                None => None,
            };

            Ok((data, written))
        });

        self.finish(rec, result, |(data, written), diag| SleeveResponse {
            data,
            size,
            written,
            diag,
        })
    }

    /// Lists the providers.
    ///
    /// # Errors
    /// Does not error today; it returns `Result` so the signature does not
    /// change once providers move onto the network (Phase 2).
    pub fn providers(&self, registry: &ProviderRegistry) -> Result<ProviderListReport> {
        let mut rec = Recorder::start(
            "provider list".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );
        rec.set(
            "provider.count",
            i64::try_from(registry.len()).unwrap_or(i64::MAX),
        );
        let result = self.roles(registry).map(|roles| (registry.list(), roles));
        self.finish(rec, result, |(providers, roles), diag| ProviderListReport {
            providers,
            play_from: roles.music.clone(),
            roles,
            diag,
        })
    }

    /// Where the play box searches first (D-080): the music role's provider
    /// (D-086) — the main one unless music was chosen on its own. A choice
    /// that could not be kept is named in `missing`.
    ///
    /// # Errors
    /// If `preferences.json` cannot be read, or the catalog cannot say
    /// whether the provider was scanned.
    pub fn play_from(&self, registry: &ProviderRegistry) -> Result<PlayFrom> {
        Ok(self.roles(registry)?.music)
    }

    /// Who plays each role (D-086).
    ///
    /// - **main:** the user's choice while that provider is registered and
    ///   can play; otherwise the registry's first one that can play sound —
    ///   local files.
    /// - **music:** its own choice; otherwise the main provider.
    /// - **video:** its own choice; otherwise the main provider when it can
    ///   show videos, and the registry's first one that can when it cannot.
    ///
    /// A choice that cannot be kept — the provider gone, or no longer fit
    /// for the role — falls back the same way and is named in `missing`.
    ///
    /// # Errors
    /// If `preferences.json` cannot be read, or the catalog cannot say
    /// whether a provider was scanned.
    pub fn roles(&self, registry: &ProviderRegistry) -> Result<ProviderRoles> {
        let saved = crate::preferences::load(&self.config.preferences_path())?;
        let fits = |id: &ProviderId, role: Role| {
            registry
                .get(id)
                .is_some_and(|provider| role.fits(provider.info().capabilities))
        };

        let (main, main_chosen, main_missing) = match saved.play_from {
            Some(id) if fits(&id, Role::Main) => (Some(id), true, None),
            saved => (
                registry
                    .with_capability(crate::provider::Capabilities::STREAM)
                    .first()
                    .map(|provider| provider.info().id),
                false,
                saved,
            ),
        };

        // Following the main provider, music carries its lost choice too: the
        // play box searches where the main one fell back to, and says why.
        let (music, music_chosen, music_follows, music_missing) = match saved.music_from {
            Some(id) if fits(&id, Role::Music) => (Some(id), true, false, None),
            saved => (
                main.clone(),
                main_chosen,
                true,
                saved.or(main_missing.clone()),
            ),
        };

        let (video, video_chosen, video_follows, video_missing) = match saved.video_from {
            Some(id) if fits(&id, Role::Video) => (Some(id), true, false, None),
            saved => match main.clone().filter(|id| fits(id, Role::Video)) {
                Some(id) => (Some(id), main_chosen, true, saved),
                None => (
                    registry
                        .all()
                        .iter()
                        .map(|provider| provider.info())
                        .find(|info| Role::Video.fits(info.capabilities))
                        .map(|info| info.id),
                    false,
                    true,
                    saved,
                ),
            },
        };

        let entry = |provider: Option<ProviderId>,
                     chosen: bool,
                     follows_main: bool,
                     missing: Option<ProviderId>|
         -> Result<PlayFrom> {
            let instant = match &provider {
                Some(id) => self.library.last_scanned_at_ms(id)?.is_some(),
                None => false,
            };
            Ok(PlayFrom {
                provider,
                chosen,
                instant,
                missing,
                follows_main,
            })
        };
        Ok(ProviderRoles {
            main: entry(main, main_chosen, false, main_missing)?,
            music: entry(music, music_chosen, music_follows, music_missing)?,
            video: entry(video, video_chosen, video_follows, video_missing)?,
        })
    }

    /// Chooses where the play box searches first (D-080): the **main**
    /// provider since D-086 — music and video move with it unless each was
    /// chosen on its own. Only a registered provider that can play can be
    /// chosen; the choice is kept in `preferences.json`, and the CLI's
    /// `play` follows it as the desktop's play box does.
    ///
    /// # Errors
    /// If no such provider is registered, it cannot play, or the choice
    /// cannot be written.
    pub fn set_play_from(
        &self,
        registry: &ProviderRegistry,
        id: &ProviderId,
    ) -> Result<PlayFromReport> {
        let rec = Recorder::start(
            format!("provider use {id}"),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = self
            .store_role(registry, Role::Main, Some(id))
            .map(|roles| roles.music);
        self.finish(rec, result, |play_from, diag| PlayFromReport {
            play_from,
            diag,
        })
    }

    /// Chooses who plays a role (D-086); `None` lets music or video follow
    /// the main provider again, and lets the main one fall back to the
    /// registry's first that can play.
    ///
    /// # Errors
    /// If no such provider is registered, it cannot play the role, or the
    /// choice cannot be written.
    pub fn set_role(
        &self,
        registry: &ProviderRegistry,
        role: Role,
        id: Option<&ProviderId>,
    ) -> Result<RolesReport> {
        let command = match id {
            Some(id) => format!("provider use {id} --for {}", role.as_str()),
            None => format!("provider follow {}", role.as_str()),
        };
        let rec = Recorder::start(command, Some(self.config.data_dir().to_path_buf()));
        let result = self.store_role(registry, role, id);
        self.finish(rec, result, |roles, diag| RolesReport { roles, diag })
    }

    /// Checks a role's new provider and writes the choice.
    fn store_role(
        &self,
        registry: &ProviderRegistry,
        role: Role,
        id: Option<&ProviderId>,
    ) -> Result<ProviderRoles> {
        if let Some(id) = id {
            let Some(provider) = registry.get(id) else {
                let known: Vec<String> = registry
                    .list()
                    .into_iter()
                    .map(|info| info.id.to_string())
                    .collect();
                return Err(Error::new(
                    Stage::ProviderCall,
                    ErrorKind::NotFound {
                        what: format!(
                            "provider {:?} (registered: {})",
                            id.as_str(),
                            known.join(", ")
                        ),
                    },
                ));
            };
            let info = provider.info();
            if !role.fits(info.capabilities) {
                let what = match role {
                    Role::Main => "playing — so it cannot be the one played from",
                    Role::Music => "playing — so it cannot be the one music is played from",
                    Role::Video => "showing videos — so it cannot be the one videos come from",
                };
                return Err(Error::new(
                    Stage::ProviderCall,
                    ErrorKind::Unsupported {
                        provider: info.id.to_string(),
                        what: what.to_owned(),
                        capabilities: info.capabilities.describe(),
                    },
                ));
            }
        }
        let path = self.config.preferences_path();
        let mut preferences = crate::preferences::load(&path)?;
        let slot = match role {
            Role::Main => &mut preferences.play_from,
            Role::Music => &mut preferences.music_from,
            Role::Video => &mut preferences.video_from,
        };
        *slot = id.cloned();
        crate::preferences::save(&path, &preferences)?;
        self.roles(registry)
    }

    /// The videos for a track (D-086). Its own video comes first, whoever is
    /// chosen for videos: a video file is its own video, and so is a plugin
    /// item that has one. Otherwise the video role's provider is searched for
    /// the song.
    ///
    /// # Errors
    /// If the track has no video of its own and no provider can show videos
    /// — "I could not look" — or the video provider's search fails. Finding
    /// nothing is not an error: the plan's candidates are empty and its
    /// counts say why (K9).
    pub async fn video_for(
        &self,
        registry: &ProviderRegistry,
        item: &QueueItem,
    ) -> Result<crate::video::VideoPlan> {
        // The track's own video: the same file, the same plugin item.
        if let Some(own) = registry.get(&item.id.provider)
            && own
                .info()
                .capabilities
                .contains(crate::provider::Capabilities::VIDEO)
            && let Some(source) = own.resolve_video(&item.id).await?
        {
            source.check()?;
            return Ok(crate::video::VideoPlan {
                track: item.track.clone(),
                provider: item.id.provider.clone(),
                own: true,
                candidates: vec![crate::video::VideoCandidate {
                    id: item.id.clone(),
                    track: item.track.clone(),
                    score: 1.0,
                    source,
                }],
                refused: 0,
                without_video: 0,
            });
        }

        let roles = self.roles(registry)?;
        let Some(provider) = roles
            .video
            .provider
            .as_ref()
            .and_then(|id| registry.get(id))
        else {
            return Err(Error::new(
                Stage::VideoLookup,
                ErrorKind::NotFound {
                    what: "a provider that can show videos (choose one: `headshell provider use <name> --for video`)".to_owned(),
                },
            ));
        };
        let video_provider = provider.info().id;

        let query = crate::video::query_for(&item.track);
        let catalog = self
            .library
            .search_catalog(&query, crate::video::SEARCH_ROWS)?;
        let rows = self
            .hits_from(&provider, &query, crate::video::SEARCH_ROWS, &catalog)
            .await?
            .into_iter()
            .filter(|row| row.id != item.id)
            .map(|row| crate::provider::ProviderTrack {
                id: row.id,
                track: row.track,
            })
            .collect();
        let (ranked, refused) = crate::video::rank(&item.track, rows);

        let mut candidates = Vec::new();
        let mut without_video = 0;
        for (row, score) in ranked {
            if candidates.len() >= crate::video::MAX_CANDIDATES {
                break;
            }
            match provider.resolve_video(&row.id).await {
                Ok(Some(source)) if source.check().is_ok() => {
                    candidates.push(crate::video::VideoCandidate {
                        id: row.id,
                        track: row.track,
                        score,
                        source,
                    });
                }
                Ok(Some(_) | None) => without_video += 1,
                // One row that cannot be asked does not hide the others; it
                // is counted with them and said in the log (K9).
                Err(err) => {
                    tracing::warn!(
                        provider = %video_provider,
                        row = %row.id,
                        error = %err.chain_text().replace('\n', " "),
                        "a video match could not be asked for its video"
                    );
                    without_video += 1;
                }
            }
        }
        Ok(crate::video::VideoPlan {
            track: item.track.clone(),
            provider: video_provider,
            own: false,
            candidates,
            refused,
            without_video,
        })
    }

    /// `headshell video`: the videos for a track the user names — a file
    /// (its own video first) or `Artist - Title`.
    ///
    /// # Errors
    /// As [`Session::video_for`]; and if the text is not `Artist - Title`.
    pub async fn video_report(
        &self,
        registry: &ProviderRegistry,
        target: VideoTarget,
    ) -> Result<VideoReport> {
        let label = match &target {
            VideoTarget::Query(query) => format!("video {query:?}"),
            VideoTarget::File(path) => format!("video --file {}", path.display()),
        };
        let mut rec = Recorder::start(label, Some(self.config.data_dir().to_path_buf()));
        let result = async {
            let item = match target {
                VideoTarget::Query(query) => QueueItem {
                    id: ProviderTrackId::new(ProviderId::new("query"), query.clone()),
                    track: TrackRef::parse_query(&query)?,
                },
                VideoTarget::File(path) => {
                    let path = std::fs::canonicalize(&path)
                        .map_err(|err| crate::error::io_err(Stage::VideoLookup, &path, err))?;
                    let (track, _) = crate::provider::local::read_track(&path)?;
                    QueueItem {
                        id: ProviderTrackId::new(ProviderId::new("local"), path.to_string_lossy()),
                        track,
                    }
                }
            };
            self.video_for(registry, &item).await
        }
        .await;
        if let Ok(plan) = &result {
            rec.set(
                "video.candidates",
                i64::try_from(plan.candidates.len()).unwrap_or(i64::MAX),
            );
            rec.set(
                "video.refused",
                i64::try_from(plan.refused).unwrap_or(i64::MAX),
            );
            rec.set(
                "video.without_video",
                i64::try_from(plan.without_video).unwrap_or(i64::MAX),
            );
        }
        self.finish(rec, result, |plan, diag| VideoReport { plan, diag })
    }

    /// Whether the desktop goes online (D-082): [`ONLINE_ENV`] when it is
    /// set — for one launch — otherwise the choice saved with
    /// [`Self::set_online`], otherwise offline. The CLI does not go by it.
    ///
    /// # Errors
    /// If `preferences.json` cannot be read, or the variable holds anything
    /// but `1` or `0`.
    pub fn online_choice(&self) -> Result<OnlineChoice> {
        let saved = crate::preferences::load(&self.config.preferences_path())?.lookup;
        Ok(OnlineChoice::decide(saved, lookup_mode_variable()?))
    }

    /// [`Self::online_choice`] as a report: `headshell online`.
    ///
    /// # Errors
    /// As [`Self::online_choice`].
    pub fn online(&self) -> Result<OnlineReport> {
        let rec = Recorder::start(
            "online".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = self.online_choice();
        self.finish(rec, result, |online, diag| OnlineReport { online, diag })
    }

    /// Saves whether the desktop goes online (D-082): `headshell online
    /// on|off`, and the desktop's settings. [`ONLINE_ENV`] still decides a
    /// launch that sets it, and the report says so.
    ///
    /// # Errors
    /// If `preferences.json` cannot be read or written, or the variable
    /// holds anything but `1` or `0`.
    pub fn set_online(&self, mode: LookupMode) -> Result<OnlineReport> {
        let word = match mode {
            LookupMode::Online => "on",
            LookupMode::Offline => "off",
        };
        let rec = Recorder::start(
            format!("online {word}"),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = (|| {
            let path = self.config.preferences_path();
            let mut preferences = crate::preferences::load(&path)?;
            preferences.lookup = Some(mode);
            crate::preferences::save(&path, &preferences)?;
            self.online_choice()
        })();
        self.finish(rec, result, |online, diag| OnlineReport { online, diag })
    }

    /// The music folders and who decided them (D-085): `headshell
    /// music-dirs`.
    ///
    /// # Errors
    /// If `preferences.json` cannot be read.
    pub fn music_folders(&self) -> Result<MusicFoldersReport> {
        let rec = Recorder::start(
            "music-dirs".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = self.config.music_folders();
        self.finish(rec, result, |folders, diag| MusicFoldersReport {
            folders,
            changed: false,
            diag,
        })
    }

    /// Adds a music folder to the chosen ones (D-085). With nothing chosen
    /// yet the system's folder stays on the list: adding is adding. The
    /// folder is read at the next scan.
    ///
    /// # Errors
    /// If the folder does not exist or is not a folder, or `preferences.json`
    /// cannot be read or written.
    pub fn add_music_folder(&self, dir: &Path) -> Result<MusicFoldersReport> {
        let rec = Recorder::start(
            format!("music-dirs add {}", dir.display()),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = (|| {
            let dir = absolute_folder(dir)?;
            match std::fs::metadata(&dir) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(folder_error(format!("{} is not a folder", dir.display()))),
                Err(err) => {
                    return Err(folder_error(format!(
                        "{} cannot be opened: {err}",
                        dir.display()
                    )));
                }
            }
            let mut chosen = self.chosen_folders()?;
            let changed = !chosen.contains(&dir);
            if changed {
                chosen.push(dir);
                self.save_folders(chosen)?;
            }
            Ok((self.config.music_folders()?, changed))
        })();
        self.finish(rec, result, |(folders, changed), diag| MusicFoldersReport {
            folders,
            changed,
            diag,
        })
    }

    /// Takes a folder off the chosen ones (D-085); its songs leave the
    /// catalog at the next scan. Taking the last one off leaves no local
    /// folders — a choice, not "nothing chosen".
    ///
    /// # Errors
    /// If the folder is not on the list (the list is said), or
    /// `preferences.json` cannot be read or written.
    pub fn remove_music_folder(&self, dir: &Path) -> Result<MusicFoldersReport> {
        let rec = Recorder::start(
            format!("music-dirs remove {}", dir.display()),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = (|| {
            let dir = absolute_folder(dir)?;
            let mut chosen = self.chosen_folders()?;
            let Some(index) = chosen.iter().position(|each| *each == dir) else {
                let list: Vec<String> = chosen
                    .iter()
                    .map(|each| each.display().to_string())
                    .collect();
                return Err(Error::new(
                    Stage::ConfigLoad,
                    ErrorKind::NotFound {
                        what: format!(
                            "music folder {} (the folders: {})",
                            dir.display(),
                            if list.is_empty() {
                                "none".to_owned()
                            } else {
                                list.join(", ")
                            }
                        ),
                    },
                ));
            };
            chosen.remove(index);
            self.save_folders(chosen)?;
            Ok((self.config.music_folders()?, true))
        })();
        self.finish(rec, result, |(folders, changed), diag| MusicFoldersReport {
            folders,
            changed,
            diag,
        })
    }

    /// The folders a change starts from: the chosen ones, or the system's
    /// when nothing is chosen — never the variable's, which holds one run.
    fn chosen_folders(&self) -> Result<Vec<PathBuf>> {
        let folders = self.config.music_folders()?;
        Ok(folders.saved.unwrap_or(folders.system))
    }

    fn save_folders(&self, dirs: Vec<PathBuf>) -> Result<()> {
        let path = self.config.preferences_path();
        let mut preferences = crate::preferences::load(&path)?;
        preferences.music_dirs = Some(dirs);
        crate::preferences::save(&path, &preferences)
    }

    /// Tests a provider: is it up, how many tracks does it see.
    ///
    /// # Errors
    /// If the provider is not registered or the health check fails.
    pub async fn test_provider(
        &self,
        registry: &ProviderRegistry,
        id: &ProviderId,
    ) -> Result<ProviderTestReport> {
        let mut rec = Recorder::start(
            format!("provider test {id}"),
            Some(self.config.data_dir().to_path_buf()),
        );

        let result = async {
            let provider = registry.get(id).ok_or_else(|| {
                Error::new(
                    Stage::ProviderCall,
                    ErrorKind::NotFound {
                        what: format!(
                            "provider: {id} (registered: {})",
                            registry
                                .list()
                                .iter()
                                .map(|info| info.id.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    },
                )
            })?;
            let info = provider.info();
            let health = provider.health().await?;
            Ok((info, health))
        }
        .await;

        if let Ok((_, health)) = &result {
            rec.set("provider.reachable", i64::from(health.reachable));
            if let Some(count) = health.track_count {
                rec.set(
                    "provider.track_count",
                    i64::try_from(count).unwrap_or(i64::MAX),
                );
            }
            if let Some(detail) = &health.detail {
                rec.note(detail.clone());
            }
        }

        self.finish(rec, result, |(info, health), diag| ProviderTestReport {
            info,
            health,
            diag,
        })
    }

    /// Registers a remote server (§1.3, D-019/D-021).
    ///
    /// The password is **not stored**: Subsonic derives a salt/token, Jellyfin
    /// exchanges it for an access key. If `spec.verify` is on, the server is
    /// contacted before saving — better to say now that the password is wrong
    /// than to discover a week later that "it doesn't play".
    ///
    /// # Errors
    /// If a server with the same name exists, the address or credentials are
    /// missing, verification fails, or the registry file cannot be written.
    pub async fn add_server(
        &self,
        spec: NewServer,
        http: Arc<dyn crate::net::HttpClient>,
    ) -> Result<ServerAddReport> {
        let mut rec = Recorder::start(
            format!("provider add {} {}", spec.kind, spec.id),
            Some(self.config.data_dir().to_path_buf()),
        );

        let path = self.config.servers_path();
        let verify = spec.verify;
        let result = async {
            let mut servers = remote::load_servers(&path)?;
            if spec.id.as_str() == "local" {
                return Err(Error::new(
                    Stage::ConfigLoad,
                    ErrorKind::InvalidInput {
                        detail: "the name `local` belongs to the local file provider".to_owned(),
                    },
                ));
            }
            if servers.iter().any(|existing| existing.id == spec.id) {
                // Silently overwriting would change the user's working entry without them
                // noticing.
                return Err(Error::new(
                    Stage::ConfigLoad,
                    ErrorKind::InvalidInput {
                        detail: format!(
                            "a server named {} is already registered; first run `headshell provider remove {}`",
                            spec.id, spec.id
                        ),
                    },
                ));
            }

            let (server, notes) = remote::prepare_server(&spec, http).await?;
            let summary = ServerSummary::of(&server);
            servers.push(server);
            remote::save_servers(&path, &servers)?;
            Ok((summary, notes))
        }
        .await;

        if let Ok((_, notes)) = &result {
            rec.set("server.verified", i64::from(verify));
            for note in notes {
                rec.note(note.clone());
            }
        }

        self.finish(rec, result, move |(server, notes), diag| ServerAddReport {
            server,
            verified: verify,
            notes,
            diag,
        })
    }

    /// Lists the registered remote servers (without credentials).
    ///
    /// # Errors
    /// If the registry file cannot be read or is corrupt.
    pub fn list_servers(&self) -> Result<ServerListReport> {
        let mut rec = Recorder::start(
            "provider servers".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );
        let path = self.config.servers_path();
        let result = remote::load_servers(&path).map(|servers| {
            rec.set(
                "server.count",
                i64::try_from(servers.len()).unwrap_or(i64::MAX),
            );
            servers.iter().map(ServerSummary::of).collect::<Vec<_>>()
        });
        self.finish(rec, result, move |servers, diag| ServerListReport {
            servers,
            path,
            diag,
        })
    }

    /// Removes a remote server entry.
    ///
    /// A name that is not registered is an **error**: saying "removed" while
    /// doing nothing would hide a typo.
    ///
    /// # Errors
    /// If the name is not registered or the file cannot be written.
    pub fn remove_server(&self, id: &ProviderId) -> Result<ServerRemoveReport> {
        let mut rec = Recorder::start(
            format!("provider remove {id}"),
            Some(self.config.data_dir().to_path_buf()),
        );
        let path = self.config.servers_path();
        let result = (|| {
            let mut servers = remote::load_servers(&path)?;
            let before = servers.len();
            servers.retain(|server| &server.id != id);
            if servers.len() == before {
                return Err(Error::new(
                    Stage::ConfigLoad,
                    ErrorKind::NotFound {
                        what: format!("registered server: {id}"),
                    },
                ));
            }
            remote::save_servers(&path, &servers)?;
            rec.set(
                "server.remaining",
                i64::try_from(servers.len()).unwrap_or(i64::MAX),
            );
            Ok(servers.len())
        })();

        let id = id.clone();
        self.finish(rec, result, move |remaining, diag| ServerRemoveReport {
            id,
            removed: true,
            remaining,
            diag,
        })
    }

    /// Scans the local music directories and refreshes the index.
    ///
    /// # Errors
    /// If a root directory cannot be read. Errors on individual files are not
    /// errors; they are counted in the summary (K9).
    pub async fn scan_providers(&mut self, registry: &ProviderRegistry) -> Result<ScanReport> {
        let mut rec = Recorder::start(
            "provider scan".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );

        let result = Self::scan_inner(&mut self.library, registry).await;

        if let Ok((summary, write)) = &result {
            summary.record_into(&mut rec);
            let n = |v: usize| i64::try_from(v).unwrap_or(i64::MAX);
            rec.set("catalog.inserted", n(write.inserted));
            rec.set("catalog.updated", n(write.updated));
            rec.set("catalog.removed", n(write.removed));
            rec.set("catalog.unchanged", n(write.unchanged));
        }
        let result = result.and_then(|scanned| Ok((self.config.music_folders()?.dirs, scanned)));
        self.finish(rec, result, move |(dirs, (summary, write)), diag| {
            ScanReport {
                dirs,
                summary,
                write,
                scanned: true,
                reason: "requested".to_owned(),
                diag,
            }
        })
    }

    /// Scans only if **stale** (D-025).
    ///
    /// Asks the provider a cheap question: could the catalog have changed since
    /// the last scan? The local provider answers from directory timestamps;
    /// no full scan happens. If the answer is "don't know", we **scan** —
    /// skipping because we don't know would make a file the user added
    /// invisible.
    ///
    /// This is not directory watching, it is a poll that looks when triggered:
    /// no `notify` dependency was added and the behaviour is the same on every
    /// platform.
    ///
    /// # Errors
    /// If the staleness question or the scan fails.
    pub async fn scan_providers_if_stale(
        &mut self,
        registry: &ProviderRegistry,
    ) -> Result<ScanReport> {
        let mut rec = Recorder::start(
            "provider scan --if-stale".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );

        let mut reasons = Vec::new();
        let mut stale = false;
        for provider in registry.all() {
            let id = provider.info().id;
            let Some(last) = self.library.last_scanned_at_ms(&id)? else {
                reasons.push(format!("{id}: never scanned"));
                stale = true;
                continue;
            };
            match provider.catalog_changed_since(last).await? {
                Some(true) => {
                    reasons.push(format!("{id}: changed"));
                    stale = true;
                }
                Some(false) => reasons.push(format!("{id}: unchanged")),
                // "Don't know" is not enough to skip.
                None => {
                    reasons.push(format!("{id}: unknown"));
                    stale = true;
                }
            }
        }
        let reason = reasons.join(", ");
        rec.note(format!("staleness: {reason}"));

        if !stale {
            rec.set("scan.skipped", 1);
            let dirs = self.config.music_folders().map(|folders| folders.dirs);
            return self.finish(rec, dirs, move |dirs, diag| ScanReport {
                dirs,
                summary: ScanSummary::default(),
                write: CatalogWriteSummary::default(),
                scanned: false,
                reason,
                diag,
            });
        }

        let result = Self::scan_inner(&mut self.library, registry).await;
        if let Ok((summary, write)) = &result {
            summary.record_into(&mut rec);
            let n = |v: usize| i64::try_from(v).unwrap_or(i64::MAX);
            rec.set("catalog.inserted", n(write.inserted));
            rec.set("catalog.updated", n(write.updated));
            rec.set("catalog.removed", n(write.removed));
            rec.set("catalog.unchanged", n(write.unchanged));
        }
        let result = result.and_then(|scanned| Ok((self.config.music_folders()?.dirs, scanned)));
        self.finish(rec, result, move |(dirs, (summary, write)), diag| {
            ScanReport {
                dirs,
                summary,
                write,
                scanned: true,
                reason,
                diag,
            }
        })
    }

    /// Runs the scan and writes the result to the **persistent catalog**.
    ///
    /// `&mut SqliteLibrary` is a separate parameter to avoid a library borrow
    /// conflict; it could not be called through `self`.
    async fn scan_inner(
        library: &mut SqliteLibrary,
        registry: &ProviderRegistry,
    ) -> Result<(ScanSummary, CatalogWriteSummary)> {
        let mut total = ScanSummary::default();
        let mut write_total = CatalogWriteSummary::default();
        let mut scanned = 0usize;

        for provider in registry.all() {
            let info = provider.info();
            // Timestamps: an unchanged file's tags are not read again.
            let known = library.catalog_stamps(&info.id)?;

            let Some(scan) = provider.scan_catalog(&known).await? else {
                continue;
            };
            scanned += 1;
            total.files_seen += scan.summary.files_seen;
            total.audio_files += scan.summary.audio_files;
            total.indexed += scan.summary.indexed;
            total.tag_fallback += scan.summary.tag_fallback;
            total.failed += scan.summary.failed;
            total.unreadable_dirs += scan.summary.unreadable_dirs;
            total.unchanged += scan.summary.unchanged;

            // Metadata of unchanged rows does not come from the scan; we keep what is
            // in the catalog. Otherwise every scan would delete them.
            let mut rows = Vec::with_capacity(scan.tracks.len());
            for entry in scan.tracks {
                match entry.track {
                    Some(track) => rows.push(CatalogTrack {
                        id: entry.id,
                        track,
                        from_tags: entry.from_tags,
                        mtime_ms: entry.mtime_ms,
                    }),
                    None => {
                        if let Some(existing) = library.catalog_get(&entry.id)? {
                            rows.push(existing);
                        }
                    }
                }
            }

            let write = library.replace_catalog(&info.id, &rows)?;
            write_total.inserted += write.inserted;
            write_total.updated += write.updated;
            write_total.removed += write.removed;
            write_total.unchanged += write.unchanged;
        }

        if scanned == 0 {
            return Err(Error::new(
                Stage::ProviderCall,
                ErrorKind::NotFound {
                    what: "provider with a scannable catalog".to_owned(),
                },
            ));
        }
        Ok((total, write_total))
    }

    /// Turns a search into a playable queue.
    ///
    /// The provider chosen to play from is asked first (D-080); the others
    /// only when it finds nothing, one after another in the registry's order
    /// — local files, servers, plugins. A provider whose catalog was scanned
    /// answers from the catalog (FTS, without touching the disk or the
    /// network); one never scanned is asked by its own search.
    ///
    /// If `all` is false only the first match is taken. An empty result is an
    /// **error**: better than "played it, but no sound".
    ///
    /// # Errors
    /// If there is no provider, the chosen one's search fails, or nothing
    /// matches anywhere.
    pub async fn queue_from_search(
        &self,
        registry: &ProviderRegistry,
        query: &str,
        all: bool,
        limit: usize,
    ) -> Result<Vec<QueueItem>> {
        let play_from = self.play_from(registry)?;
        let order = search_order(registry, play_from.provider.as_ref());
        let Some((first, others)) = order.split_first() else {
            return Err(Error::new(
                Stage::PlaybackResolve,
                ErrorKind::NotFound {
                    what: "provider that can stream audio".to_owned(),
                },
            ));
        };
        let catalog = self.library.search_catalog(query, limit)?;

        // The chosen one's failure is the answer (K9): "could not reach it" is
        // not a reason to play something else instead.
        let mut items = self.hits_from(first, query, limit, &catalog).await?;
        if items.is_empty() {
            // One that fails among the others does not hide what the rest
            // found; when none found anything, its failure is the answer.
            let mut failure = None;
            for provider in others {
                match self.hits_from(provider, query, limit, &catalog).await {
                    Ok(hits) => items.extend(hits),
                    Err(err) => {
                        tracing::warn!(
                            provider = %provider.info().id,
                            error = %err.chain_text().replace('\n', " "),
                            "a provider asked after the chosen one could not be searched"
                        );
                        failure.get_or_insert(err);
                    }
                }
            }
            if items.is_empty()
                && let Some(err) = failure
            {
                return Err(err);
            }
        }

        if items.is_empty() {
            return Err(Error::new(
                Stage::PlaybackResolve,
                ErrorKind::NotFound {
                    what: format!(
                        "track matching {query:?} (if the index is empty: `headshell provider scan`)"
                    ),
                },
            ));
        }
        if !all {
            items.truncate(1);
        }
        Ok(items)
    }

    /// One provider's answer: its rows in the scanned catalog, at once — or,
    /// for one never scanned, its own search.
    async fn hits_from(
        &self,
        provider: &Arc<dyn crate::provider::Provider>,
        query: &str,
        limit: usize,
        catalog: &[CatalogTrack],
    ) -> Result<Vec<QueueItem>> {
        if let Some(rows) = self.scanned_rows(provider, catalog)? {
            return Ok(rows);
        }
        Ok(provider
            .search(query, limit)
            .await?
            .into_iter()
            .map(|hit| QueueItem {
                id: hit.id,
                track: hit.track,
            })
            .collect())
    }

    /// A scanned provider's rows among the catalog's matches; `None` for one
    /// never scanned — its own search is the answer then.
    fn scanned_rows(
        &self,
        provider: &Arc<dyn crate::provider::Provider>,
        catalog: &[CatalogTrack],
    ) -> Result<Option<Vec<QueueItem>>> {
        let id = provider.info().id;
        if self.library.last_scanned_at_ms(&id)?.is_none() {
            return Ok(None);
        }
        Ok(Some(
            catalog
                .iter()
                .filter(|hit| hit.id.provider == id)
                .map(|hit| QueueItem {
                    id: hit.id.clone(),
                    track: hit.track.clone(),
                })
                .collect(),
        ))
    }

    /// What the play box offers while it is typed in (D-080): the chosen
    /// provider's matches, ranked as `crate::suggest` says; when it has none,
    /// every other provider's, each a section of its own.
    ///
    /// Not a recorded run — it is asked at every pause in typing, and the
    /// last run `headshell diag` shows must stay the user's last command. A
    /// provider that could not be asked is in `failures`, with its stage and
    /// chain (K9); the others are still asked.
    ///
    /// # Errors
    /// If `preferences.json` or the catalog cannot be read.
    pub async fn suggest(
        &self,
        registry: &ProviderRegistry,
        query: &str,
    ) -> Result<crate::suggest::SuggestReport> {
        Ok(self.suggest_plan(registry, query)?.finish().await)
    }

    /// The first half of [`Session::suggest`]: who is asked, in order, with
    /// the catalog's answers read. The rest — a server's, a plugin's search —
    /// is [`crate::suggest::SuggestPlan::finish`], which needs no session: the
    /// desktop asks those off the thread that plays.
    ///
    /// # Errors
    /// If `preferences.json` or the catalog cannot be read.
    pub fn suggest_plan(
        &self,
        registry: &ProviderRegistry,
        query: &str,
    ) -> Result<crate::suggest::SuggestPlan> {
        let play_from = self.play_from(registry)?;
        let mut steps = Vec::new();
        if !query.trim().is_empty() {
            let catalog = self.library.search_catalog(query, SUGGEST_CATALOG_ROWS)?;
            for provider in search_order(registry, play_from.provider.as_ref()) {
                let catalog = self.scanned_rows(&provider, &catalog)?;
                steps.push(crate::suggest::SuggestStep { provider, catalog });
            }
        }
        Ok(crate::suggest::SuggestPlan {
            query: query.to_owned(),
            play_from,
            steps,
        })
    }

    /// A player for exactly these tracks — what a picked suggestion plays,
    /// with no search in between (D-080).
    ///
    /// # Errors
    /// If there is no track, or the first one cannot be played.
    pub async fn player_from_items(
        &self,
        registry: &ProviderRegistry,
        items: Vec<QueueItem>,
        shows_video: bool,
    ) -> Result<crate::playback::Player> {
        let mut player = crate::playback::Player::new(registry.clone());
        player.set_shows_video(shows_video);
        player.play_items(items).await?;
        Ok(player)
    }

    /// Searches, queues, plays and writes the listen records.
    ///
    /// Waits until playback ends — the whole flow is here so the CLI does not
    /// have to write a loop (Golden Rule). The GUI later builds its own loop
    /// with [`Session::queue_from_search`] and its own `Player` instead; both
    /// use the same core parts.
    ///
    /// # Errors
    /// If nothing matches, the provider cannot play, or the audio pipeline
    /// cannot be set up.
    pub async fn play(
        &mut self,
        registry: &ProviderRegistry,
        options: PlayOptions,
    ) -> Result<PlayReport> {
        let mut rec = Recorder::start(
            format!("play {:?}", options.query),
            Some(self.config.data_dir().to_path_buf()),
        );

        let result = async {
            let items = self
                .queue_from_search(registry, &options.query, options.all, options.limit)
                .await?;

            let mut player = crate::playback::Player::new(registry.clone());
            if options.shuffle {
                player.queue_mut().set_shuffle(true);
            }

            if options.dry_run {
                let video_only = items
                    .iter()
                    .filter(|item| video_only_provider(registry, &item.id.provider))
                    .count();
                player.queue_mut().replace(items.clone());
                return Ok((items, Vec::new(), false, video_only));
            }

            player.play_items(items.clone()).await?;

            // Run until the queue ends. The poll interval is independent of the
            // anchor: the position is computed on the consumer's side (D-015); here
            // we only ask "did the track end".
            //
            // The sleep is `std::thread::sleep`: the core does not choose an async
            // runtime (PLAN convention), so `tokio::time` cannot be used here. Audio
            // plays on its own thread, so this wait does not interrupt the sound;
            // only this call blocks.
            loop {
                player.tick().await?;
                if player.state() == crate::playback::PlayState::Stopped {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            player.stop();

            let listens = player.take_listens();
            Ok((items, listens, true, player.video_only_skipped()))
        }
        .await;

        let report = match result {
            Ok((items, listens, played, video_only)) => {
                // Scrobbles are written here: into the same table as imported data
                // (§1.6).
                let written = self.record_listens(&listens)?;
                rec.set(
                    "play.queued",
                    i64::try_from(items.len()).unwrap_or(i64::MAX),
                );
                rec.set(
                    "play.listens_recorded",
                    i64::try_from(written.inserted).unwrap_or(i64::MAX),
                );
                rec.set(
                    "play.video_only",
                    i64::try_from(video_only).unwrap_or(i64::MAX),
                );
                Ok((items, written.inserted, played, video_only))
            }
            Err(err) => Err(err),
        };

        let query = options.query;
        self.finish(
            rec,
            report,
            move |(queued, listens_recorded, played, video_only), diag| PlayReport {
                query,
                queued,
                listens_recorded,
                played,
                video_only,
                diag,
            },
        )
    }

    /// Builds a ready [`crate::playback::Player`] from a search result.
    ///
    /// Unlike `Session::play` it **does not wait**. The caller runs its own
    /// loop (the TUI's draw loop, the GUI's timer), calls `tick()` and writes
    /// finished listens with [`Session::record_listens`].
    ///
    /// # Errors
    /// If nothing matches or the first track cannot be played.
    pub async fn player_from_search(
        &self,
        registry: &ProviderRegistry,
        options: PlayOptions,
    ) -> Result<crate::playback::Player> {
        let items = self
            .queue_from_search(registry, &options.query, options.all, options.limit)
            .await?;

        let mut player = crate::playback::Player::new(registry.clone());
        player.set_shows_video(options.shows_video);
        if options.shuffle {
            player.queue_mut().set_shuffle(true);
        }
        if options.dry_run {
            player.queue_mut().replace(items);
        } else {
            player.play_items(items).await?;
        }
        Ok(player)
    }

    /// The covers of the tracks a query finds — or of a file — walked through
    /// the chain in play order, waiting for each (D-076). What the playing
    /// session does in the background for its queue, as a command.
    ///
    /// An album is looked up once: its other tracks get the same answer. The
    /// covers go into the cache the interface reads; `out` also writes one
    /// image per album there.
    ///
    /// # Errors
    /// If neither a query nor a file is given, nothing matches the query, the
    /// file's tags cannot be read, the cache cannot be opened or written, or
    /// online was asked for in a build without an HTTP client. A cover that
    /// is not found, or a link that fails, is not an error: it is in the
    /// report, with the reason (K9).
    pub async fn artwork(
        &mut self,
        registry: &ProviderRegistry,
        request: ArtworkRequest,
        mode: LookupMode,
    ) -> Result<crate::artwork::ArtworkReport> {
        use crate::artwork::{
            ArtworkItem, ArtworkReport, ArtworkSummary, Chain, Links, Online, Outcome, cache,
            key_for,
        };

        let subject = match (&request.file, &request.query) {
            (Some(path), _) => path.display().to_string(),
            (None, Some(query)) => query.clone(),
            (None, None) => {
                return Err(Error::new(
                    Stage::ArtworkRead,
                    ErrorKind::InvalidInput {
                        detail: "a query or --file must be given".to_owned(),
                    },
                ));
            }
        };
        let mut rec = Recorder::start(
            format!("artwork {subject:?}"),
            Some(self.config.data_dir().to_path_buf()),
        );

        let result = async {
            let online = match mode {
                LookupMode::Offline => None,
                LookupMode::Online => Some(Online::from_build()?),
            };
            let store = Arc::new(std::sync::Mutex::new(cache::ArtworkCache::open(
                &self.config.artwork_dir(),
            )?));
            let chain = Chain::new(
                Links {
                    registry: registry.clone(),
                    online,
                },
                Arc::clone(&store),
            );

            let item = |index: usize, provider: ProviderId, track: &TrackRef, outcome: Outcome| {
                ArtworkItem {
                    index,
                    key: outcome.key,
                    provider,
                    artist: track.artist.clone(),
                    title: track.title.clone(),
                    album: track.album.clone(),
                    status: outcome.status,
                    notes: outcome.notes,
                }
            };
            let mut items = Vec::new();
            if let Some(path) = &request.file {
                let (track, _) = crate::provider::local::read_track(path)?;
                let outcome = chain.resolve_file(path, &track);
                items.push(item(0, ProviderId::new("local"), &track, outcome));
            } else if let Some(query) = &request.query {
                let queue = self
                    .queue_from_search(
                        registry,
                        query,
                        request.all,
                        PlayOptions::new(String::new()).limit,
                    )
                    .await?;
                let mut asked: std::collections::HashMap<crate::artwork::ArtworkKey, Outcome> =
                    std::collections::HashMap::new();
                for (index, queued) in queue.iter().enumerate() {
                    let key = key_for(&queued.track);
                    let outcome = match asked.get(&key) {
                        Some(outcome) => outcome.clone(),
                        None => {
                            let outcome = chain.resolve(&queued.id, &queued.track);
                            asked.insert(key, outcome.clone());
                            outcome
                        }
                    };
                    items.push(item(
                        index,
                        queued.id.provider.clone(),
                        &queued.track,
                        outcome,
                    ));
                }
            }

            let written = match &request.out {
                Some(dir) => {
                    let store = store.lock().map_err(|_| {
                        Error::new(
                            Stage::ArtworkStore,
                            ErrorKind::Artwork {
                                detail: "the cover cache lock is poisoned".to_owned(),
                            },
                        )
                    })?;
                    crate::artwork::write_covers(&store, &items, dir)?
                }
                None => Vec::new(),
            };
            Ok((items, written, chain.online()))
        }
        .await;

        if let Ok((items, written, _)) = &result {
            let summary = ArtworkSummary::of(items);
            let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
            rec.set("artwork.tracks", count(summary.tracks));
            rec.set("artwork.found", count(summary.found()));
            rec.set("artwork.not_found", count(summary.not_found));
            rec.set(
                "artwork.not_checked_offline",
                count(summary.not_checked_offline),
            );
            rec.set("artwork.failed", count(summary.failed));
            rec.set("artwork.written", count(written.len()));
            for failed in items.iter().filter(|item| !item.notes.is_empty()) {
                rec.note(format!(
                    "{} — {}: {}",
                    failed.artist,
                    failed.title,
                    failed.notes.join("; ")
                ));
            }
        }
        self.finish(rec, result, move |(items, written, online), diag| {
            ArtworkReport {
                subject,
                online,
                summary: ArtworkSummary::of(&items),
                items,
                written,
                diag: Some(diag),
            }
        })
    }

    /// The lyrics of the tracks a query finds — or of a file — walked
    /// through the chain in play order, waiting for each (D-078). The
    /// command is the user asking: LRCLIB is asked for every track whose
    /// own lyrics do not settle it, as the lyrics view asks for the playing
    /// one.
    ///
    /// # Errors
    /// If neither a query nor a file is given, nothing matches the query, the
    /// file's tags cannot be read for its metadata, or the cache cannot be
    /// opened or the shift written. Lyrics not found, or a link that fails,
    /// is not an error: it is in the report, with the reason (K9).
    pub async fn lyrics(
        &mut self,
        registry: &ProviderRegistry,
        request: LyricsRequest,
    ) -> Result<crate::lyrics::LyricsReport> {
        use crate::lyrics::{
            Ask, Chain, Gathered, Links, LyricsItem, LyricsReport, LyricsSummary, cache, decide,
        };

        let subject = match (&request.file, &request.query) {
            (Some(path), _) => path.display().to_string(),
            (None, Some(query)) => query.clone(),
            (None, None) => {
                return Err(Error::new(
                    Stage::LyricsRead,
                    ErrorKind::InvalidInput {
                        detail: "a query or --file must be given".to_owned(),
                    },
                ));
            }
        };
        let mut rec = Recorder::start(
            format!("lyrics {subject:?}"),
            Some(self.config.data_dir().to_path_buf()),
        );

        let result = async {
            let store = Arc::new(std::sync::Mutex::new(cache::LyricsCache::open(
                &self.config.lyrics_dir(),
            )?));
            let chain = Chain::new(Links::from_build(registry.clone()), Arc::clone(&store));

            let tracks: Vec<(ProviderTrackId, TrackRef)> = match (&request.file, &request.query) {
                (Some(path), _) => {
                    let (track, _) = crate::provider::local::read_track(path)?;
                    // Absolute, as the local provider names its files: a
                    // shift kept here holds when the file plays from the
                    // library.
                    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.clone());
                    vec![(
                        ProviderTrackId::new(ProviderId::new("local"), absolute.to_string_lossy()),
                        track,
                    )]
                }
                (None, Some(query)) => self
                    .queue_from_search(
                        registry,
                        query,
                        request.all,
                        PlayOptions::new(String::new()).limit,
                    )
                    .await?
                    .into_iter()
                    .map(|item| (item.id, item.track))
                    .collect(),
                (None, None) => Vec::new(),
            };

            let mut items = Vec::with_capacity(tracks.len());
            for (index, (id, track)) in tracks.iter().enumerate() {
                let ask = Ask {
                    id,
                    track,
                    duration_ms: track.duration_ms,
                    file: request.file.as_deref(),
                    requested: true,
                };
                let mut gathered = Gathered::default();
                chain.gather(&ask, &mut gathered);
                let outcome = decide(&gathered);
                let mut item = LyricsItem::pending(index, id, track, track.duration_ms);
                if let Some(shift) = request.shift_ms {
                    store
                        .lock()
                        .map_err(|_| {
                            Error::new(
                                Stage::LyricsStore,
                                ErrorKind::Lyrics {
                                    detail: "the lyrics cache lock is poisoned".to_owned(),
                                },
                            )
                        })?
                        .set_offset(&item.key, shift)?;
                }
                item.offset_ms = chain.offset(&item.key);
                item.status = outcome.status;
                item.lyrics = outcome.lyrics.filter(|_| request.print);
                item.notes = outcome.notes;
                items.push(item);
            }
            Ok(items)
        }
        .await;

        if let Ok(items) = &result {
            let summary = LyricsSummary::of(items);
            let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
            rec.set("lyrics.tracks", count(summary.tracks));
            rec.set("lyrics.found", count(summary.found()));
            rec.set("lyrics.synced", count(summary.synced));
            rec.set("lyrics.closest", count(summary.closest));
            rec.set("lyrics.instrumental", count(summary.instrumental));
            rec.set("lyrics.not_found", count(summary.not_found));
            rec.set("lyrics.failed", count(summary.failed));
            for item in items {
                if let crate::lyrics::LyricsStatus::Failed { chain } = &item.status {
                    rec.note(format!(
                        "{} — {}: {}",
                        item.artist,
                        item.title,
                        chain.replace('\n', " ")
                    ));
                }
                if !item.notes.is_empty() {
                    rec.note(format!(
                        "{} — {}: {}",
                        item.artist,
                        item.title,
                        item.notes.join("; ")
                    ));
                }
            }
        }
        self.finish(rec, result, move |items, diag| LyricsReport {
            subject,
            summary: LyricsSummary::of(&items),
            items,
            diag: Some(diag),
        })
    }

    /// Makes the first tracks a play query finds ready, the way a playing
    /// session reads its queue ahead (D-077): the address of each resolved
    /// and the head of its stream fetched, one after another, each said how
    /// it went. The track `play` would start, then the next two.
    ///
    /// # Errors
    /// If nothing matches the query or no provider can stream. A track that
    /// cannot be made ready is not an error of the command: it is in the
    /// report, with the stage it failed at (K9).
    pub async fn prefetch(
        &mut self,
        registry: &ProviderRegistry,
        query: &str,
        all: bool,
    ) -> Result<crate::playback::PrefetchReport> {
        use crate::playback::prefetch::{self, PlanItem};
        use crate::playback::{PlanReason, PrefetchCounters, PrefetchReport, PrefetchSummary};

        let mut rec = Recorder::start(
            format!("prefetch {query:?}"),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = async {
            let queue = self
                .queue_from_search(registry, query, all, PlayOptions::new(String::new()).limit)
                .await?;
            let reasons = [PlanReason::First, PlanReason::Next, PlanReason::AfterNext];
            let plan: Vec<PlanItem> = queue
                .into_iter()
                .zip(reasons)
                .enumerate()
                .map(|(index, (item, reason))| PlanItem {
                    item,
                    index,
                    reason,
                    why: None,
                })
                .collect();
            Ok(prefetch::prepare_now(registry, &plan))
        }
        .await;

        if let Ok(items) = &result {
            let summary = PrefetchSummary::of(items);
            let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
            rec.set("prefetch.planned", count(summary.planned));
            rec.set("prefetch.ready", count(summary.ready));
            rec.set("prefetch.with_head", count(summary.with_head));
            rec.set("prefetch.failed", count(summary.failed));
            for item in items {
                if let crate::playback::PrefetchStatus::Failed { chain } = &item.status {
                    rec.note(format!(
                        "{} — {}: {}",
                        item.artist,
                        item.title,
                        chain.replace('\n', " ")
                    ));
                }
            }
        }
        let subject = query.to_owned();
        self.finish(rec, result, move |items, diag| PrefetchReport {
            subject,
            summary: PrefetchSummary::of(&items),
            items,
            counters: PrefetchCounters::default(),
            habits: crate::playback::Habits::default(),
            diag: Some(diag),
        })
    }

    /// How each queued track, and its artist, was listened to in the history
    /// (D-077): what reading ahead guesses its third track with. A track
    /// whose history cannot be read is left out — the guess goes on without
    /// it, and the log says why.
    pub(crate) fn listening_profiles(
        &self,
        items: &[crate::playback::QueueItem],
        rule: PlayRule,
    ) -> std::collections::HashMap<crate::ids::ProviderTrackId, crate::library::ListeningProfile>
    {
        let mut profiles = std::collections::HashMap::new();
        for item in items {
            match self
                .library
                .listening_profile(&item.track.artist, &item.track.title, rule)
            {
                Ok(profile) => {
                    profiles.insert(item.id.clone(), profile);
                }
                Err(err) => {
                    tracing::warn!(
                        track = item.track.display_name(),
                        error = %err.chain_text().replace('\n', " "),
                        "reading ahead: the track's history could not be read"
                    );
                }
            }
        }
        profiles
    }

    /// Writes the listen records of played tracks to the library (§1.6).
    ///
    /// They go into the **same table** as imported data: past and present
    /// become one timeline.
    ///
    /// # Errors
    /// If writing fails.
    pub fn record_listens(&mut self, listens: &[Listen]) -> Result<WriteSummary> {
        if listens.is_empty() {
            return Ok(WriteSummary::default());
        }
        self.library.insert_listens(listens)
    }

    /// Lists the installed plugins (Phase 2 §2.1). **Starts no process.**
    ///
    /// # Errors
    /// If the plugin directory cannot be read or the consent ledger is corrupt.
    pub fn plugins(&self) -> Result<PluginListReport> {
        let mut rec = Recorder::start(
            "plugin list".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = crate::plugin::discover(&self.config);
        if let Ok((entries, summary)) = &result {
            summary.record_into(&mut rec);
            for entry in entries.iter().filter(|entry| !entry.is_loadable()) {
                rec.note(format!("{}: {}", entry.name, entry.status_text()));
            }
        }
        self.finish(rec, result, |(plugins, summary), diag| PluginListReport {
            plugins,
            summary,
            permissions_enforced: crate::plugin::PERMISSIONS_ENFORCED,
            diag,
        })
    }

    /// Installs a plugin (D-055, D-069, D-071).
    ///
    /// If the plugin is **not** on disk, it is first downloaded from the catalog
    /// ([`crate::plugin::catalog`]): files are verified by sha256, the
    /// downloaded manifest is compared with what the index showed, and it is
    /// put in place together with its origin record. Then — and only this, if
    /// the plugin was already on disk — the tools it declares are installed by
    /// the engine.
    ///
    /// **It goes online, and does so without waiting for `--online`.** That
    /// flag exists to prevent implicit network access ("importing an export
    /// never silently connects anyone to the network"); here the download is
    /// the command itself, not a side effect. A user who typed `install` asked
    /// for the download.
    ///
    /// For a plugin already on disk the catalog is **not read** — this command
    /// does not update ([`Self::update_plugins`]) — and installed artifacts
    /// never go to the network. An installed plugin awaits consent: coming from
    /// the catalog is not consent (D-040).
    ///
    /// # Errors
    /// If the name is invalid, the plugin is neither on disk nor in the
    /// catalog, the catalog cannot be read, a file's hash does not match, the
    /// manifest is corrupt, this build has no HTTP client, or the files cannot
    /// be written. Failing to reach an **artifact** is not an error: it is
    /// reported in [`InstallOutcome`], because "unreachable", "orphaned" and
    /// "hash mismatch" are different diagnoses (K9).
    pub async fn install_plugin(
        &self,
        name: &str,
        http: Arc<dyn HttpClient>,
    ) -> Result<PluginInstallReport> {
        let mut rec = Recorder::start(
            format!("plugin install {name}"),
            Some(self.config.data_dir().to_path_buf()),
        );

        let store = ArtifactStore::new(&self.config);
        let result = async {
            crate::plugin::manifest::validate_local_name(name).map_err(|detail| {
                Error::new(Stage::PluginLoad, ErrorKind::InvalidInput { detail })
            })?;
            let dir = self.config.plugins_dir().join(name);
            let missing = matches!(
                std::fs::symlink_metadata(&dir),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound
            );
            let fetched = if missing {
                let index = self.config.plugin_index_url();
                let catalog = catalog::fetch(http.as_ref(), &index).await?;
                Some(catalog::install(&self.config, http.as_ref(), &catalog, name).await?)
            } else {
                None
            };

            let manifest = PluginManifest::load(&dir)?;
            let outcomes = install_artifacts(&store, &manifest)?;
            let consent = ConsentStore::load(&self.config.plugin_consent_path())?
                .status(name, &manifest.permissions);
            Ok((manifest, outcomes, fetched, consent))
        }
        .await;

        rec.note(format!("platform: {}", store.platform()));
        if let Ok((_, outcomes, fetched, consent)) = &result {
            match fetched {
                Some(fetched) => rec.note(format!(
                    "downloaded from the catalog: {name} {} ← {}",
                    fetched.version, fetched.index
                )),
                None => rec.note("plugin was already on disk; catalog not read".to_owned()),
            }
            for (name, outcome) in outcomes {
                rec.note(format!("{name}: {}", outcome.describe()));
            }
            rec.note(format!("consent: {}", consent.describe()));
        }

        let plugin = name.to_owned();
        let platform = store.platform().to_owned();
        self.finish(
            rec,
            result,
            move |(manifest, outcomes, fetched, consent), diag| {
                let report = crate::plugin::artifact::InstallReport { plugin, outcomes };
                PluginInstallReport {
                    ready: report.is_ready(),
                    declared: manifest.requires.len(),
                    platform,
                    fetched,
                    permissions: manifest.permissions,
                    consent,
                    report,
                    diag,
                }
            },
        )
    }

    /// Reads the plugin catalog and shows it against this machine (D-071):
    /// whether each plugin is installed, whether it has an update, why it
    /// cannot be installed.
    ///
    /// Goes online **only** to read the catalog; installs nothing. The address
    /// is [`Config::plugin_index_url`].
    ///
    /// # Errors
    /// If the catalog cannot be reached (`NETWORK_REQUEST`), or the index is
    /// missing or unreadable (`PLUGIN_CATALOG`). A broken **entry** is not an
    /// error: it is written in that entry's `problem`.
    pub async fn plugin_catalog(&self, http: Arc<dyn HttpClient>) -> Result<PluginCatalogReport> {
        let mut rec = Recorder::start(
            "plugin catalog".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );
        let index = self.config.plugin_index_url();
        rec.note(format!("catalog: {index}"));

        let result = match catalog::fetch(http.as_ref(), &index).await {
            Ok(catalog) => catalog.survey(&self.config),
            Err(err) => Err(err),
        };
        if let Ok(survey) = &result {
            survey.summary.record_into(&mut rec);
            for plugin in &survey.plugins {
                if let Some(problem) = &plugin.problem {
                    rec.note(format!("{}: {problem}", plugin.name));
                }
            }
            for name in &survey.delisted {
                rec.note(format!(
                    "{name}: installed from this catalog but no longer listed"
                ));
            }
        }

        let platform = crate::plugin::artifact::current_platform();
        self.finish(rec, result, move |survey, diag| PluginCatalogReport {
            index,
            platform,
            plugins: survey.plugins,
            delisted: survey.delisted,
            summary: survey.summary,
            diag,
        })
    }

    /// Brings plugins installed from the catalog to the catalog's version
    /// (D-071).
    ///
    /// With `name`, only that plugin, and if that cannot be done (installed by
    /// hand, changed locally, not in the catalog) it is an **error** — the user
    /// asked explicitly. Without it, every installed plugin whose name is in
    /// the catalog; one failing does not stop the others, and each result is
    /// written separately (K9).
    ///
    /// An update touches **only** a plugin that has an origin record and whose
    /// files match that record. The new version's tools are installed; a tool
    /// change needs no consent but is written in the report. If the permissions
    /// grew, the plugin awaits consent again (D-040).
    ///
    /// # Errors
    /// If the catalog cannot be read; with `name`, if that plugin cannot be
    /// updated.
    pub async fn update_plugins(
        &self,
        name: Option<&str>,
        http: Arc<dyn HttpClient>,
    ) -> Result<PluginUpdateReport> {
        let command = match name {
            Some(name) => format!("plugin update {name}"),
            None => "plugin update".to_owned(),
        };
        let mut rec = Recorder::start(command, Some(self.config.data_dir().to_path_buf()));
        let index = self.config.plugin_index_url();
        let platform = crate::plugin::artifact::current_platform();
        let store = ArtifactStore::new(&self.config);
        rec.note(format!("catalog: {index}"));

        let result = async {
            let catalog = catalog::fetch(http.as_ref(), &index).await?;
            let names = match name {
                Some(name) => vec![name.to_owned()],
                None => catalog.update_candidates(&self.config)?,
            };
            let mut plugins = Vec::new();
            for plugin in names {
                let outcome =
                    catalog::update(&self.config, http.as_ref(), &catalog, &plugin, &platform)
                        .await;
                let outcome = match (outcome, name.is_some()) {
                    (Ok(UpdateOutcome::Skipped { reason }), true) => {
                        return Err(Error::new(
                            Stage::PluginCatalog,
                            ErrorKind::PluginCatalog {
                                index: index.clone(),
                                detail: format!("{plugin} not updated: {reason}"),
                            },
                        ));
                    }
                    (Err(err), true) => return Err(err),
                    (Err(err), false) => UpdateOutcome::Failed {
                        error: err.chain_text().replace('\n', " "),
                    },
                    (Ok(outcome), _) => outcome,
                };
                let mut update = PluginUpdate {
                    name: plugin.clone(),
                    outcome,
                    tools: Vec::new(),
                    tools_error: None,
                    consent: None,
                };
                if matches!(update.outcome, UpdateOutcome::Updated { .. }) {
                    let manifest = PluginManifest::load(&self.config.plugins_dir().join(&plugin))?;
                    match install_artifacts(&store, &manifest) {
                        Ok(tools) => update.tools = tools,
                        Err(err) if name.is_some() => return Err(err),
                        Err(err) => update.tools_error = Some(err.chain_text().replace('\n', " ")),
                    }
                    update.consent = Some(
                        ConsentStore::load(&self.config.plugin_consent_path())?
                            .status(&plugin, &manifest.permissions),
                    );
                }
                plugins.push(update);
            }
            Ok(plugins)
        }
        .await;

        if let Ok(plugins) = &result {
            UpdateSummary::of(plugins.iter().map(|plugin| &plugin.outcome)).record_into(&mut rec);
            for plugin in plugins {
                rec.note(format!("{}: {}", plugin.name, plugin.outcome.describe()));
                if let UpdateOutcome::Updated { tools_changed, .. } = &plugin.outcome {
                    for change in tools_changed {
                        rec.note(format!(
                            "{}: tool changed — {} (no consent needed, D-071)",
                            plugin.name,
                            change.describe()
                        ));
                    }
                }
            }
        }

        self.finish(rec, result, move |plugins, diag| PluginUpdateReport {
            index,
            platform,
            summary: UpdateSummary::of(plugins.iter().map(|plugin| &plugin.outcome)),
            plugins,
            diag,
        })
    }

    /// Removes a plugin: deletes its directory (with the `state/` inside) and
    /// forgets its consent (D-071).
    ///
    /// Consent is forgotten **first**: if the directory cannot be deleted the
    /// plugin is left without consent — the reverse order could leave a plugin
    /// that is approved but half deleted. Secrets and the tools the engine
    /// installed are not deleted; the names of the remaining secrets are in the
    /// report. If the directory is a symbolic link, only the link is removed.
    ///
    /// # Errors
    /// If the name is invalid, the plugin is not installed, the consent ledger
    /// or the secret file is corrupt, or the directory cannot be deleted.
    pub fn remove_plugin(&self, name: &str) -> Result<PluginRemoveReport> {
        let mut rec = Recorder::start(
            format!("plugin remove {name}"),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = (|| {
            catalog::installed_dir(&self.config, name)?;
            let path = self.config.plugin_consent_path();
            let mut consents = ConsentStore::load(&path)?;
            let consent_forgotten = consents.forget(name);
            if consent_forgotten {
                consents.save(&path)?;
            }
            let removed = catalog::remove(&self.config, name)?;
            let kept_secrets: Vec<String> = Secrets::load(&self.config.secrets_path())?
                .namespace(&crate::secrets::plugin_namespace(name))
                .into_keys()
                .collect();
            Ok((removed, consent_forgotten, kept_secrets))
        })();

        if let Ok((removed, consent_forgotten, kept_secrets)) = &result {
            rec.note(format!("removed: {}", removed.path.display()));
            if let Some(target) = &removed.link_target {
                rec.note(format!(
                    "was a link; its target was not touched: {}",
                    target.display()
                ));
            }
            rec.note(format!(
                "consent: {}",
                if *consent_forgotten {
                    "forgotten"
                } else {
                    "had no record"
                }
            ));
            if !kept_secrets.is_empty() {
                rec.note(format!("remaining secrets: {}", kept_secrets.join(", ")));
            }
        }

        let name = name.to_owned();
        self.finish(
            rec,
            result,
            move |(removed, consent_forgotten, kept_secrets), diag| PluginRemoveReport {
                name,
                removed,
                consent_forgotten,
                kept_secrets,
                diag,
            },
        )
    }

    /// Generates or checks the catalog repository's index (D-071).
    ///
    /// For catalog **maintenance**: `dir` is a copy of `headshell/plugins`. Every
    /// plugin goes through the core's own manifest validation — the same rule
    /// that installation applies — and the files' hashes are computed. Without
    /// `url_template` the one in the existing `index.json` is used, so every
    /// build writes the same addresses.
    ///
    /// With `check` nothing is written; if the index is out of date, an error
    /// says **what** differs (the catalog repository's CI).
    ///
    /// # Errors
    /// If the template is missing or invalid, a plugin is invalid, the index is
    /// out of date with `check`, or a file cannot be read or written.
    pub fn build_plugin_index(
        &self,
        dir: &Path,
        url_template: Option<&str>,
        check: bool,
    ) -> Result<PluginIndexReport> {
        let mut rec = Recorder::start(
            format!(
                "plugin index {}{}",
                dir.display(),
                if check { " --check" } else { "" }
            ),
            Some(self.config.data_dir().to_path_buf()),
        );
        let index_path = dir.join(catalog::INDEX_FILE);
        let origin = index_path.display().to_string();
        let result = (|| {
            let template = match url_template {
                Some(template) => template.to_owned(),
                None => catalog::read_url_template(&index_path)?.ok_or_else(|| {
                    Error::new(
                        Stage::PluginCatalog,
                        ErrorKind::PluginCatalog {
                            index: origin.clone(),
                            detail:
                                "no address template: pass `--url-template` on the first build; \
                                     later builds use the template in index.json"
                                    .to_owned(),
                        },
                    )
                })?,
            };
            let built = catalog::build_index(dir, &template)?;
            let existing = match std::fs::read_to_string(&index_path) {
                Ok(text) => Some(text),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => {
                    return Err(crate::error::io_err(Stage::PluginCatalog, &index_path, err));
                }
            };
            let up_to_date = existing.as_deref() == Some(built.json.as_str());
            if check && !up_to_date {
                let differences = match &existing {
                    Some(old) => catalog::index_differences(old, &built.json),
                    None => vec!["no index.json".to_owned()],
                };
                return Err(Error::new(
                    Stage::PluginCatalog,
                    ErrorKind::PluginCatalog {
                        index: origin.clone(),
                        detail: format!(
                            "index.json is out of date — regenerate it with `headshell plugin index {}`: \
                             {}",
                            dir.display(),
                            differences.join("; ")
                        ),
                    },
                ));
            }
            let written = !check && !up_to_date;
            if written {
                catalog::write_index(dir, &built.json)?;
            }
            Ok((template, built.plugins, up_to_date, written))
        })();

        if let Ok((_, plugins, up_to_date, written)) = &result {
            rec.set(
                "index.plugins",
                i64::try_from(plugins.len()).unwrap_or(i64::MAX),
            );
            for plugin in plugins {
                rec.note(format!("{} {}", plugin.name, plugin.version));
            }
            rec.note(if *written {
                "index.json written".to_owned()
            } else if *up_to_date {
                "index.json already up to date".to_owned()
            } else {
                "index.json not written".to_owned()
            });
        }

        self.finish(
            rec,
            result,
            move |(url_template, plugins, up_to_date, written), diag| PluginIndexReport {
                path: index_path,
                url_template,
                plugins,
                up_to_date,
                written,
                diag,
            },
        )
    }

    /// Approves the permissions a plugin **declares** (D-040).
    ///
    /// The approved set is read from the manifest: the caller cannot make up
    /// its own permission list, the user only says yes to what the plugin asks
    /// for.
    ///
    /// # Errors
    /// If the plugin is not found, its manifest is corrupt, or the ledger
    /// cannot be written.
    pub fn approve_plugin(&self, name: &str) -> Result<PluginConsentReport> {
        self.consent_command(name, "approve", |store, name, requested| {
            store.approve(name, requested, jiff::Timestamp::now());
            Ok(())
        })
    }

    /// Disables a plugin; the consent record is kept.
    ///
    /// # Errors
    /// If the plugin is not found or was never approved.
    pub fn disable_plugin(&self, name: &str) -> Result<PluginConsentReport> {
        self.consent_command(name, "disable", |store, name, _| {
            missing_consent(store.disable(name), name)
        })
    }

    /// Re-enables a disabled plugin.
    ///
    /// # Errors
    /// If the plugin is not found or was never approved.
    pub fn enable_plugin(&self, name: &str) -> Result<PluginConsentReport> {
        self.consent_command(name, "enable", |store, name, _| {
            missing_consent(store.enable(name), name)
        })
    }

    /// Forgets consent entirely: the plugin is asked again from scratch next
    /// time.
    ///
    /// # Errors
    /// If the plugin is not found or was never approved.
    pub fn forget_plugin(&self, name: &str) -> Result<PluginConsentReport> {
        self.consent_command(name, "forget", |store, name, _| {
            missing_consent(store.forget(name), name)
        })
    }

    fn consent_command(
        &self,
        name: &str,
        action: &str,
        apply: impl FnOnce(&mut ConsentStore, &str, &Permissions) -> Result<()>,
    ) -> Result<PluginConsentReport> {
        let mut rec = Recorder::start(
            format!("plugin {action} {name}"),
            Some(self.config.data_dir().to_path_buf()),
        );

        let action = action.to_owned();
        let platform = crate::plugin::artifact::current_platform();
        let result = (|| {
            let dir = self.config.plugins_dir().join(name);
            let manifest = crate::plugin::manifest::PluginManifest::load(&dir)?;
            let path = self.config.plugin_consent_path();
            let mut store = ConsentStore::load(&path)?;
            apply(&mut store, name, &manifest.permissions)?;
            store.save(&path)?;
            let status = store.status(name, &manifest.permissions);
            Ok((manifest, status))
        })();

        if let Ok((manifest, status)) = &result {
            rec.note(format!(
                "declared permissions: {}",
                manifest.permissions.describe()
            ));
            for requirement in &manifest.requires {
                match requirement.asset_for(&platform) {
                    Some(asset) => rec.note(format!(
                        "the engine will download: {} {} ({platform}) ← {} (sha256 {})",
                        requirement.name, requirement.version, asset.url, asset.sha256
                    )),
                    None => rec.note(format!(
                        "{} {}: no release for this platform ({platform})",
                        requirement.name, requirement.version
                    )),
                }
            }
            rec.note(format!("state: {}", status.describe()));
        }

        self.finish(rec, result, move |(manifest, status), diag| {
            PluginConsentReport {
                name: manifest.name,
                action,
                permissions: manifest.permissions,
                requires: manifest.requires,
                platform,
                status,
                permissions_enforced: crate::plugin::PERMISSIONS_ENFORCED,
                diag,
            }
        })
    }

    /// Lists the **key names** in the secret store (D-042).
    ///
    /// No values are returned: this output can go to a pipeline with `--json`
    /// and into the diagnostics report.
    ///
    /// # Errors
    /// If the secret file is corrupt.
    pub fn secrets(&self) -> Result<SecretListReport> {
        let mut rec = Recorder::start(
            "secret list".to_owned(),
            Some(self.config.data_dir().to_path_buf()),
        );
        let result = Secrets::load(&self.config.secrets_path()).map(|secrets| secrets.describe());
        if let Ok(namespaces) = &result {
            rec.set(
                "secrets.namespaces",
                i64::try_from(namespaces.len()).unwrap_or(i64::MAX),
            );
        }
        self.finish(rec, result, |namespaces, diag| SecretListReport {
            namespaces,
            diag,
        })
    }

    /// Writes a secret.
    ///
    /// # Errors
    /// If the secret file cannot be read or written.
    pub fn set_secret(&self, namespace: &str, key: &str, value: &str) -> Result<SecretWriteReport> {
        self.secret_command(namespace, key, "set", |secrets| {
            secrets.set(namespace, key, value);
            Ok(true)
        })
    }

    /// Removes a secret.
    ///
    /// # Errors
    /// If the secret file cannot be read or written, or the key does not exist.
    pub fn remove_secret(&self, namespace: &str, key: &str) -> Result<SecretWriteReport> {
        self.secret_command(namespace, key, "remove", |secrets| {
            if secrets.remove(namespace, key) {
                Ok(true)
            } else {
                Err(Error::new(
                    Stage::ConfigLoad,
                    ErrorKind::NotFound {
                        what: format!("secret: {namespace} / {key}"),
                    },
                ))
            }
        })
    }

    fn secret_command(
        &self,
        namespace: &str,
        key: &str,
        action: &str,
        apply: impl FnOnce(&mut Secrets) -> Result<bool>,
    ) -> Result<SecretWriteReport> {
        // The command line goes into diagnostics; **the value does not** (D-042).
        let rec = Recorder::start(
            format!("secret {action} {namespace} {key}"),
            Some(self.config.data_dir().to_path_buf()),
        );
        let namespace = namespace.to_owned();
        let key = key.to_owned();
        let action = action.to_owned();

        let path = self.config.secrets_path();
        let result = (|| {
            let mut secrets = Secrets::load(&path)?;
            let changed = apply(&mut secrets)?;
            secrets.save(&path)?;
            Ok(changed)
        })();

        self.finish(rec, result, move |changed, diag| SecretWriteReport {
            namespace,
            key,
            action,
            changed,
            diag,
        })
    }

    /// The last run's diagnostics report.
    ///
    /// # Errors
    /// If the report file is corrupt.
    pub fn last_diag(&self) -> Result<Option<DiagReport>> {
        crate::diag::load_last_run(&self.config.last_run_path())
    }

    /// Closes a command: writes the diagnostics report to disk and wraps the
    /// result.
    ///
    /// The report is written on success and on failure alike — `headshell diag`
    /// is needed most when something has blown up.
    fn finish<T, R>(
        &self,
        rec: Recorder,
        result: Result<T>,
        wrap: impl FnOnce(T, DiagReport) -> R,
    ) -> Result<R> {
        let report = match &result {
            Ok(_) => rec.finish(Ok(())),
            Err(err) => rec.finish(Err(err)),
        };
        if let Err(write_err) = crate::diag::save_last_run(&self.config.last_run_path(), &report) {
            // If diagnostics could not be written, do not hide the real error; just
            // log it.
            tracing::warn!(error = %write_err.chain_text(), "could not write the diagnostics report");
        }
        result.map(|value| wrap(value, report))
    }
}

/// The shared error for a plugin with no record in the consent ledger.
///
/// `disable`/`enable`/`forget` must not silently succeed on a plugin that
/// was never approved: the user would think they did something (K9).
fn missing_consent(found: bool, name: &str) -> Result<()> {
    if found {
        Ok(())
    } else {
        Err(Error::new(
            Stage::PluginLoad,
            ErrorKind::PluginNotApproved {
                plugin: name.to_owned(),
                detail: "no record in the consent ledger — first run `headshell plugin approve`"
                    .to_owned(),
            },
        ))
    }
}

/// Installs the tools a plugin declares with the engine (D-055). If it asks
/// for none, no network access happens and an empty list is returned.
fn install_artifacts(
    store: &ArtifactStore,
    manifest: &PluginManifest,
) -> Result<Vec<(String, InstallOutcome)>> {
    let mut outcomes = Vec::new();
    if !manifest.requires.is_empty() {
        let source = crate::plugin::artifact::default_artifact_source()?;
        for requirement in &manifest.requires {
            let outcome = store.install(source.as_ref(), requirement)?;
            outcomes.push((requirement.name.clone(), outcome));
        }
    }
    Ok(outcomes)
}

/// Decides from the path whether it is a zip or a directory.
fn open_archive(path: &Path) -> Result<Box<dyn import::ExportArchive>> {
    let meta = std::fs::metadata(path)
        .map_err(|source| crate::error::io_err(Stage::ImportRead, path, source))?;
    if meta.is_dir() {
        Ok(Box::new(import::DirArchive::open(path)?))
    } else if meta.is_file() {
        Ok(Box::new(import::ZipArchive::open(path)?))
    } else {
        Err(Error::new(
            Stage::ImportRead,
            ErrorKind::InvalidInput {
                detail: format!("{} is neither a file nor a directory", path.display()),
            },
        ))
    }
}

/// A folder as it is kept: absolute, so a CLI run from another directory
/// means the same folder (D-085).
fn absolute_folder(dir: &Path) -> Result<PathBuf> {
    std::path::absolute(dir)
        .map_err(|err| folder_error(format!("{} cannot be made absolute: {err}", dir.display())))
}

fn folder_error(detail: String) -> Error {
    Error::new(Stage::ConfigLoad, ErrorKind::InvalidInput { detail })
}

/// Where the identity chain gets its metadata source.
///
/// Going online is **an explicit choice**: `headshell` is a tool that works
/// fully without a network, and importing an export must not silently
/// connect anyone to MusicBrainz. The choice lives in the core so the GUI
/// and mobile offer the same two options under the same names (Golden
/// Rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupMode {
    /// No network: the chain goes no further than ISRC; the rest falls to
    /// `LocalKey`.
    #[default]
    Offline,
    /// Asks MusicBrainz — the chain's 2nd and 3rd links run.
    ///
    /// **It is slow, unavoidably:** MusicBrainz gives an anonymous client one
    /// request per second, so an export of thousands of tracks takes hours.
    /// Fine for a single-track `resolve`, not for a bulk import.
    Online,
}

/// The metadata source for the given mode.
///
/// The return type is `Arc<dyn MetadataLookup>`, not a concrete type
/// (D-006): callers — CLI, GUI, mobile — should not see a signature change
/// when we swap the source.
///
/// # Errors
/// [`LookupMode::Online`] was requested but this build has no HTTP client
/// (the `http-client` feature is off). We do not silently fall back to
/// offline: the user said they want the network and should know why they
/// cannot have it (K9).
pub fn lookup_for(mode: LookupMode) -> Result<Arc<dyn MetadataLookup>> {
    match mode {
        LookupMode::Offline => Ok(default_lookup()),
        LookupMode::Online => crate::identity::musicbrainz::default_musicbrainz_lookup(),
    }
}

/// The default metadata source: no network.
#[must_use]
pub fn default_lookup() -> Arc<dyn MetadataLookup> {
    Arc::new(OfflineLookup)
}

/// The variable that decides the desktop's lookup mode for one launch,
/// over the saved choice (D-076, D-082). It takes `1` (online) or `0`
/// (offline); unset or empty leaves it to the saved choice.
pub const ONLINE_ENV: &str = "HEADSHELL_ONLINE";

/// The lookup mode [`ONLINE_ENV`] asks for in this process; `None`: it is
/// unset or empty.
///
/// # Errors
/// If the variable holds anything but `1` or `0`: a `true` or `yes` that went
/// unrecognised would silently leave the user offline (K9).
pub fn lookup_mode_variable() -> Result<Option<LookupMode>> {
    match std::env::var(ONLINE_ENV) {
        Ok(raw) => lookup_variable_of(Some(&raw)),
        Err(std::env::VarError::NotPresent) => lookup_variable_of(None),
        Err(std::env::VarError::NotUnicode(raw)) => {
            lookup_variable_of(Some(&raw.to_string_lossy()))
        }
    }
}

fn lookup_variable_of(raw: Option<&str>) -> Result<Option<LookupMode>> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some("0") => Ok(Some(LookupMode::Offline)),
        Some("1") => Ok(Some(LookupMode::Online)),
        Some(other) => Err(Error::new(
            Stage::ConfigLoad,
            ErrorKind::InvalidInput {
                detail: format!("{ONLINE_ENV} is `{other}` — it takes 1 (online) or 0 (offline)"),
            },
        )),
    }
}

/// How many catalog rows a suggestion ranks: enough for the lists
/// `crate::suggest` offers after duplicates are folded.
const SUGGEST_CATALOG_ROWS: usize = 60;

/// Are `id`'s tracks videos only — it shows videos and plays no sound of
/// its own (D-086)?
fn video_only_provider(registry: &ProviderRegistry, id: &ProviderId) -> bool {
    use crate::provider::Capabilities as C;
    registry.get(id).is_some_and(|provider| {
        let caps = provider.info().capabilities;
        caps.contains(C::VIDEO) && !caps.contains(C::STREAM)
    })
}

/// Who is asked for a text, in order (D-080): `first` — the provider chosen
/// to play from — then every other one that can play sound, in the
/// registry's order (local files, servers, plugins).
///
/// A provider whose tracks are videos only (D-086) is asked only when it is
/// the one chosen: a search typed for songs does not turn into a list of
/// videos unasked.
fn search_order(
    registry: &ProviderRegistry,
    first: Option<&ProviderId>,
) -> Vec<Arc<dyn crate::provider::Provider>> {
    let mut order = registry.with_capability(crate::provider::Capabilities::STREAM);
    if let Some(first) = first {
        if let Some(at) = order
            .iter()
            .position(|provider| &provider.info().id == first)
        {
            let chosen = order.remove(at);
            order.insert(0, chosen);
        } else if let Some(chosen) = registry.get(first) {
            order.insert(0, chosen);
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        AudioSource, Capabilities, Provider, ProviderFuture, ProviderHealth, ProviderTrack,
    };
    use crate::test_support::TempDir;

    /// A provider with a fixed list, answering its own search — the way a
    /// server or a plugin does, never scanned. It notes what it was asked.
    struct Listed {
        id: &'static str,
        capabilities: Capabilities,
        tracks: Vec<(&'static str, &'static str)>,
        fails: bool,
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl Listed {
        fn new(id: &'static str, tracks: Vec<(&'static str, &'static str)>) -> Arc<Self> {
            Arc::new(Self {
                id,
                capabilities: Capabilities::SEARCH | Capabilities::STREAM,
                tracks,
                fails: false,
                asked: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn failing(id: &'static str) -> Arc<Self> {
            Arc::new(Self {
                id,
                capabilities: Capabilities::SEARCH | Capabilities::STREAM,
                tracks: Vec::new(),
                fails: true,
                asked: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Provider for Listed {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: ProviderId::new(self.id),
                display_name: format!("{} server", self.id),
                capabilities: self.capabilities,
            }
        }
        fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
            Box::pin(async move {
                Ok(ProviderHealth {
                    id: ProviderId::new(self.id),
                    reachable: true,
                    track_count: None,
                    detail: None,
                })
            })
        }
        fn search<'a>(
            &'a self,
            query: &'a str,
            _limit: usize,
        ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
            Box::pin(async move {
                self.asked.lock().unwrap().push(query.to_owned());
                if self.fails {
                    return Err(Error::new(
                        Stage::NetworkRequest,
                        ErrorKind::Network {
                            url: format!("https://{}.example", self.id),
                            detail: "connection refused".to_owned(),
                        },
                    ));
                }
                let wanted = query.to_lowercase();
                Ok(self
                    .tracks
                    .iter()
                    .filter(|(artist, title)| {
                        format!("{artist} {title}").to_lowercase().contains(&wanted)
                    })
                    .map(|(artist, title)| ProviderTrack {
                        id: ProviderTrackId::new(ProviderId::new(self.id), *title),
                        track: TrackRef::new(*artist, *title),
                    })
                    .collect())
            })
        }
        fn resolve_source<'a>(
            &'a self,
            _id: &'a ProviderTrackId,
        ) -> ProviderFuture<'a, Option<AudioSource>> {
            Box::pin(async move { Ok(None) })
        }
    }

    /// A session whose catalog holds `local`'s scanned tracks, and a
    /// registry of `local` (answering from that catalog) and `others`.
    fn session_with(
        dir: &TempDir,
        local: &[(&str, &str)],
        others: &[Arc<Listed>],
    ) -> (Session, ProviderRegistry) {
        let mut session = Session::open(Config::with_data_dir(dir.path())).unwrap();
        let rows: Vec<CatalogTrack> = local
            .iter()
            .map(|(artist, title)| CatalogTrack {
                id: ProviderTrackId::new(ProviderId::new("local"), format!("/music/{title}.flac")),
                track: TrackRef::new(*artist, *title),
                from_tags: true,
                mtime_ms: Some(1),
            })
            .collect();
        session
            .library
            .replace_catalog(&ProviderId::new("local"), &rows)
            .unwrap();
        let mut registry = ProviderRegistry::new();
        registry.register(Listed::new("local", Vec::new()));
        for other in others {
            registry.register(Arc::clone(other) as Arc<dyn Provider>);
        }
        (session, registry)
    }

    fn titles(items: &[QueueItem]) -> Vec<&str> {
        items.iter().map(|item| item.track.title.as_str()).collect()
    }

    #[tokio::test]
    async fn with_nothing_chosen_the_catalog_answers_and_the_others_only_when_it_has_nothing() {
        let dir = TempDir::new("play-from-default");
        let server = Listed::new("srv", vec![("Radiohead", "Reckoner")]);
        let (session, registry) = session_with(
            &dir,
            &[("Tame Impala", "Let It Happen")],
            &[Arc::clone(&server)],
        );

        let from = session.play_from(&registry).unwrap();
        assert_eq!(from.provider, Some(ProviderId::new("local")));
        assert!(
            !from.chosen && from.instant && from.missing.is_none(),
            "{from:?}"
        );

        let items = session
            .queue_from_search(&registry, "let it", true, 100)
            .await
            .unwrap();
        assert_eq!(titles(&items), ["Let It Happen"]);
        assert!(
            server.asked().is_empty(),
            "the server was asked although local files had it"
        );

        let items = session
            .queue_from_search(&registry, "reckoner", true, 100)
            .await
            .unwrap();
        assert_eq!(titles(&items), ["Reckoner"]);
        assert_eq!(server.asked(), ["reckoner"]);
    }

    #[tokio::test]
    async fn the_chosen_provider_is_asked_first_and_its_answer_wins() {
        let dir = TempDir::new("play-from-chosen");
        let server = Listed::new("srv", vec![("Tame Impala", "Let It Happen (Live)")]);
        let (session, registry) = session_with(
            &dir,
            &[("Tame Impala", "Let It Happen")],
            &[Arc::clone(&server)],
        );

        let report = session
            .set_play_from(&registry, &ProviderId::new("srv"))
            .unwrap();
        let from = report.play_from;
        assert_eq!(from.provider, Some(ProviderId::new("srv")));
        assert!(
            from.chosen && !from.instant,
            "a server is asked over its connection: {from:?}"
        );

        let items = session
            .queue_from_search(&registry, "let it", true, 100)
            .await
            .unwrap();
        assert_eq!(titles(&items), ["Let It Happen (Live)"]);
        // The choice outlives the session: it is in the data directory.
        let again = Session::open(Config::with_data_dir(dir.path())).unwrap();
        assert_eq!(
            again.play_from(&registry).unwrap().provider,
            Some(ProviderId::new("srv"))
        );
    }

    #[tokio::test]
    async fn a_choice_that_cannot_be_kept_is_named_not_dropped() {
        let dir = TempDir::new("play-from-missing");
        let (session, registry) = session_with(&dir, &[("Tame Impala", "Let It Happen")], &[]);
        crate::preferences::save(
            &session.config.preferences_path(),
            &crate::preferences::Preferences {
                play_from: Some(ProviderId::new("ytmusic")),
                lookup: None,
                ..Default::default()
            },
        )
        .unwrap();

        let from = session.play_from(&registry).unwrap();
        assert_eq!(from.provider, Some(ProviderId::new("local")));
        assert!(!from.chosen);
        assert_eq!(from.missing, Some(ProviderId::new("ytmusic")));
        // Searching still works, from local files.
        let items = session
            .queue_from_search(&registry, "let it", false, 100)
            .await
            .unwrap();
        assert_eq!(titles(&items), ["Let It Happen"]);
    }

    #[test]
    fn only_a_registered_provider_that_can_play_can_be_chosen() {
        let dir = TempDir::new("play-from-refused");
        let mute = Arc::new(Listed {
            id: "mute",
            capabilities: Capabilities::SEARCH,
            tracks: Vec::new(),
            fails: false,
            asked: std::sync::Mutex::new(Vec::new()),
        });
        let (session, registry) = session_with(&dir, &[], &[mute]);

        let err = session
            .set_play_from(&registry, &ProviderId::new("nope"))
            .unwrap_err();
        let text = err.chain_text();
        assert!(
            text.contains("\"nope\"") && text.contains("local") && text.contains("mute"),
            "{text}"
        );

        let err = session
            .set_play_from(&registry, &ProviderId::new("mute"))
            .unwrap_err();
        assert!(
            err.chain_text().contains("cannot do this"),
            "{}",
            err.chain_text()
        );
        // Nothing was written by either refusal.
        assert!(!session.config.preferences_path().exists());
    }

    #[tokio::test]
    async fn the_chosen_ones_failure_is_the_answer_not_a_reason_to_play_something_else() {
        let dir = TempDir::new("play-from-failing");
        let down = Listed::failing("down");
        let (session, registry) = session_with(
            &dir,
            &[("Tame Impala", "Let It Happen")],
            &[Arc::clone(&down)],
        );
        session
            .set_play_from(&registry, &ProviderId::new("down"))
            .unwrap();

        let err = session
            .queue_from_search(&registry, "let it", false, 100)
            .await
            .unwrap_err();
        assert_eq!(err.stage(), Stage::NetworkRequest);
        assert!(
            err.chain_text().contains("connection refused"),
            "{}",
            err.chain_text()
        );
    }

    #[tokio::test]
    async fn after_the_chosen_one_a_failure_does_not_hide_what_the_others_found() {
        let dir = TempDir::new("play-from-fallback");
        let down = Listed::failing("down");
        let server = Listed::new("srv", vec![("Radiohead", "Reckoner")]);
        let (session, registry) = session_with(
            &dir,
            &[("Tame Impala", "Let It Happen")],
            &[Arc::clone(&down), Arc::clone(&server)],
        );

        let items = session
            .queue_from_search(&registry, "reckoner", false, 100)
            .await
            .unwrap();
        assert_eq!(titles(&items), ["Reckoner"]);
        assert_eq!(down.asked(), ["reckoner"]);

        // Found nowhere: the failure is the answer, not "not found" (K9).
        let err = session
            .queue_from_search(&registry, "nowhere", false, 100)
            .await
            .unwrap_err();
        assert_eq!(err.stage(), Stage::NetworkRequest);
    }

    #[tokio::test]
    async fn suggestions_come_from_the_chosen_provider_alone_when_it_has_something() {
        let dir = TempDir::new("suggest-chosen");
        let server = Listed::new("srv", vec![("Tame Impala", "Let It Happen (Live)")]);
        let (session, registry) = session_with(
            &dir,
            &[
                ("Tame Impala", "Let It Happen"),
                ("Ray LaMontagne", "Let It Be Me"),
            ],
            &[Arc::clone(&server)],
        );

        let report = session.suggest(&registry, "let it").await.unwrap();
        assert_eq!(report.sections.len(), 1, "{report:?}");
        let section = &report.sections[0];
        assert_eq!(section.provider, ProviderId::new("local"));
        assert!(!section.fallback);
        assert_eq!(titles(&section.tracks), ["Let It Happen", "Let It Be Me"]);
        assert_eq!(section.artists[0].artist, "Tame Impala");
        assert!(
            server.asked().is_empty(),
            "nobody else is asked when the chosen one answers"
        );

        // Nothing typed: nobody is asked at all.
        let report = session.suggest(&registry, "   ").await.unwrap();
        assert!(report.sections.is_empty() && report.failures.is_empty());
    }

    #[tokio::test]
    async fn when_the_chosen_has_nothing_every_other_answer_is_a_named_section() {
        let dir = TempDir::new("suggest-fallback");
        let down = Listed::failing("down");
        let server = Listed::new("srv", vec![("Radiohead", "Reckoner")]);
        let (session, registry) = session_with(
            &dir,
            &[("Tame Impala", "Let It Happen")],
            &[Arc::clone(&down), Arc::clone(&server)],
        );

        let report = session.suggest(&registry, "reck").await.unwrap();
        assert_eq!(report.sections.len(), 1, "{report:?}");
        let section = &report.sections[0];
        assert_eq!(section.provider, ProviderId::new("srv"));
        assert_eq!(section.display_name, "srv server");
        assert!(
            section.fallback,
            "it was asked because the chosen one had nothing"
        );
        assert_eq!(titles(&section.tracks), ["Reckoner"]);
        // The provider that could not be asked is said, with its stage.
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].provider, ProviderId::new("down"));
        assert_eq!(report.failures[0].stage, "NETWORK_REQUEST");
    }

    #[test]
    fn the_online_variable_takes_one_or_zero_and_says_what_else_it_got() {
        assert_eq!(lookup_variable_of(None).unwrap(), None);
        assert_eq!(lookup_variable_of(Some("")).unwrap(), None);
        assert_eq!(
            lookup_variable_of(Some("0")).unwrap(),
            Some(LookupMode::Offline)
        );
        assert_eq!(
            lookup_variable_of(Some(" 1 ")).unwrap(),
            Some(LookupMode::Online)
        );

        let err = lookup_variable_of(Some("true")).unwrap_err();
        assert_eq!(err.stage(), Stage::ConfigLoad);
        let text = err.chain_text();
        assert!(text.contains("HEADSHELL_ONLINE is `true`"), "{text}");
        assert!(text.contains("1 (online) or 0 (offline)"), "{text}");
    }

    /// D-082: the variable holds one launch over the saved choice; nothing
    /// set is offline, and the report says who decided.
    #[test]
    fn the_variable_overrides_the_saved_choice_and_nothing_set_is_offline() {
        use LookupMode::{Offline, Online};
        let cases = [
            (None, None, Offline, OnlineSource::Default),
            (Some(Online), None, Online, OnlineSource::Saved),
            (Some(Offline), None, Offline, OnlineSource::Saved),
            (Some(Online), Some(Offline), Offline, OnlineSource::Variable),
            (None, Some(Online), Online, OnlineSource::Variable),
        ];
        for (saved, variable, mode, source) in cases {
            let choice = OnlineChoice::decide(saved, variable);
            assert_eq!(
                (choice.mode, choice.source),
                (mode, source),
                "saved {saved:?}, variable {variable:?}"
            );
            assert_eq!((choice.saved, choice.variable), (saved, variable));
        }
    }

    /// D-085: adding keeps what was there — the system's folder when nothing
    /// was chosen — and adding twice changes nothing; taking a folder off
    /// that is not there is an error that lists the folders; the last one
    /// off leaves an empty choice. (What the variable of whoever runs the
    /// tests says is not asserted: only the saved choice.)
    #[test]
    fn music_folders_are_added_and_taken_off_the_chosen_ones() {
        let dir = TempDir::new("music-folders");
        let config = Config::with_data_dir(dir.path().join("data"));
        let session = Session::open(config).unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let first = session.add_music_folder(&a).unwrap();
        assert!(first.changed);
        let saved = first.folders.saved.clone().unwrap();
        assert_eq!(saved.last(), Some(&a));
        for system in &first.folders.system {
            assert!(
                saved.contains(system),
                "the system's folder stays: {system:?}"
            );
        }
        assert!(
            !session.add_music_folder(&a).unwrap().changed,
            "already there"
        );

        let err = session.remove_music_folder(&b).unwrap_err();
        assert_eq!(err.stage(), Stage::ConfigLoad);
        assert!(
            err.chain_text().contains(&a.display().to_string()),
            "{}",
            err.chain_text()
        );

        let missing = session
            .add_music_folder(&dir.path().join("nowhere"))
            .unwrap_err();
        assert!(
            missing.chain_text().contains("cannot be opened"),
            "{}",
            missing.chain_text()
        );

        let mut left = session.music_folders().unwrap().folders.saved.unwrap();
        for folder in left.clone() {
            left = session
                .remove_music_folder(&folder)
                .unwrap()
                .folders
                .saved
                .unwrap();
        }
        assert!(
            left.is_empty(),
            "the last one off leaves an empty choice, not none"
        );
    }

    /// The choice is kept in `preferences.json` beside the play-from one,
    /// which it leaves alone. (The mode itself is not asserted: it follows
    /// the variable of whoever runs the tests.)
    #[test]
    fn the_online_choice_is_saved_beside_the_others() {
        let dir = TempDir::new("online-choice");
        let config = Config::with_data_dir(dir.path());
        let path = config.preferences_path();
        let session = Session::open(config).unwrap();
        crate::preferences::save(
            &path,
            &crate::preferences::Preferences {
                play_from: Some(ProviderId::new("local")),
                lookup: None,
                ..Default::default()
            },
        )
        .unwrap();

        let report = session.set_online(LookupMode::Online).unwrap();
        assert_eq!(report.online.saved, Some(LookupMode::Online));
        assert_eq!(report.diag.failed_at, None);
        let kept = crate::preferences::load(&path).unwrap();
        assert_eq!(kept.lookup, Some(LookupMode::Online));
        assert_eq!(kept.play_from, Some(ProviderId::new("local")));

        let off = session.set_online(LookupMode::Offline).unwrap();
        assert_eq!(off.online.saved, Some(LookupMode::Offline));
        assert_eq!(
            session.online().unwrap().online.saved,
            Some(LookupMode::Offline)
        );
    }

    /// A video service for the tests (D-086): it searches loosely — every
    /// row comes back, as a real video search's do — and each row is a
    /// YouTube video whose id is made from its title.
    struct Videos {
        tracks: Vec<(&'static str, &'static str)>,
    }

    impl Videos {
        fn new(tracks: Vec<(&'static str, &'static str)>) -> Arc<Self> {
            Arc::new(Self { tracks })
        }

        fn video_id(title: &str) -> String {
            let mut id: String = title
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .take(11)
                .collect();
            while id.len() < 11 {
                id.push('-');
            }
            id
        }
    }

    impl Provider for Videos {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: ProviderId::new("tube"),
                display_name: "Tube".to_owned(),
                capabilities: Capabilities::SEARCH | Capabilities::VIDEO,
            }
        }
        fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
            Box::pin(async move {
                Ok(ProviderHealth {
                    id: ProviderId::new("tube"),
                    reachable: true,
                    track_count: None,
                    detail: None,
                })
            })
        }
        fn search<'a>(
            &'a self,
            _query: &'a str,
            _limit: usize,
        ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
            Box::pin(async move {
                Ok(self
                    .tracks
                    .iter()
                    .map(|(artist, title)| ProviderTrack {
                        id: ProviderTrackId::new(ProviderId::new("tube"), *title),
                        track: TrackRef::new(*artist, *title),
                    })
                    .collect())
            })
        }
        fn resolve_source<'a>(
            &'a self,
            _id: &'a ProviderTrackId,
        ) -> ProviderFuture<'a, Option<AudioSource>> {
            Box::pin(async move { Ok(None) })
        }
        fn resolve_video<'a>(
            &'a self,
            id: &'a ProviderTrackId,
        ) -> ProviderFuture<'a, Option<crate::video::VideoSource>> {
            Box::pin(async move {
                // "No video" rows exist too: the plan counts them.
                if id.id.contains("audio only") {
                    return Ok(None);
                }
                Ok(Some(crate::video::VideoSource::Youtube {
                    video_id: Self::video_id(&id.id),
                }))
            })
        }
    }

    fn with_videos(registry: &mut ProviderRegistry, videos: &Arc<Videos>) {
        registry.register(Arc::clone(videos) as Arc<dyn Provider>);
    }

    /// Music and video follow the main provider until each is chosen on its
    /// own; the video role skips a main provider that shows no videos
    /// (D-086).
    #[test]
    fn roles_follow_the_main_provider_until_chosen_apart() {
        let dir = TempDir::new("roles");
        let server = Listed::new("server", Vec::new());
        let (session, mut registry) = session_with(&dir, &[], &[Arc::clone(&server)]);
        with_videos(&mut registry, &Videos::new(Vec::new()));
        let id = |name: &str| Some(ProviderId::new(name));

        let roles = session.roles(&registry).unwrap();
        assert_eq!(roles.main.provider, id("local"));
        assert!(!roles.main.chosen);
        assert_eq!(roles.music.provider, id("local"));
        assert!(roles.music.follows_main);
        // The main provider shows no videos: the first that does.
        assert_eq!(roles.video.provider, id("tube"));
        assert!(roles.video.follows_main);

        // The main one moves: the music follows it.
        let roles = session
            .set_role(&registry, Role::Main, Some(&ProviderId::new("server")))
            .unwrap()
            .roles;
        assert_eq!(roles.main.provider, id("server"));
        assert!(roles.main.chosen);
        assert_eq!(roles.music.provider, id("server"));
        assert!(roles.music.follows_main && roles.music.chosen);
        assert_eq!(roles.video.provider, id("tube"));

        // Chosen apart, a role stays where it was put…
        session
            .set_role(&registry, Role::Music, Some(&ProviderId::new("tube")))
            .unwrap();
        let roles = session
            .set_role(&registry, Role::Main, Some(&ProviderId::new("local")))
            .unwrap()
            .roles;
        assert_eq!(roles.music.provider, id("tube"));
        assert!(!roles.music.follows_main);
        // …and the play box searches it.
        assert_eq!(session.play_from(&registry).unwrap().provider, id("tube"));

        // …until it follows again.
        let roles = session
            .set_role(&registry, Role::Music, None)
            .unwrap()
            .roles;
        assert_eq!(roles.music.provider, id("local"));
        assert!(roles.music.follows_main);

        // A provider that shows no videos cannot be the video one.
        let err = session
            .set_role(&registry, Role::Video, Some(&ProviderId::new("server")))
            .unwrap_err();
        assert_eq!(err.stage(), Stage::ProviderCall);
        assert!(
            err.chain_text().contains("showing videos"),
            "{}",
            err.chain_text()
        );
    }

    /// The song's video is found by searching the video provider; the rows
    /// that are other songs, and the ones without a video, are counted.
    #[tokio::test]
    async fn the_video_is_the_songs_and_the_rest_are_counted() {
        let dir = TempDir::new("video-for");
        let (session, mut registry) = session_with(&dir, &[], &[]);
        with_videos(
            &mut registry,
            &Videos::new(vec![
                ("Music EDM", "Greatest Hits of the Year"),
                ("Daft Punk", "Get Lucky audio only"),
                ("Daft Punk", "Get Lucky"),
            ]),
        );
        let item = QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), "/music/Get Lucky.flac"),
            track: TrackRef::new("Daft Punk", "Get Lucky"),
        };

        let plan = session.video_for(&registry, &item).await.unwrap();
        assert_eq!(plan.provider, ProviderId::new("tube"));
        assert!(!plan.own);
        let ids: Vec<&str> = plan.candidates.iter().map(|c| c.id.id.as_str()).collect();
        assert_eq!(ids, ["Get Lucky"]);
        assert_eq!(
            plan.candidates[0].source,
            crate::video::VideoSource::Youtube {
                video_id: "Get_Lucky--".to_owned()
            }
        );
        assert_eq!(plan.refused, 1, "the greatest-hits video is not the song");
        // "Get Lucky audio only" scored as the song (the title is close), but
        // had no video when asked.
        assert_eq!(plan.without_video, 1);

        // A track of the video provider's own is its own video: no search.
        let own = QueueItem {
            id: ProviderTrackId::new(ProviderId::new("tube"), "Get Lucky"),
            track: TrackRef::new("Daft Punk", "Get Lucky"),
        };
        let plan = session.video_for(&registry, &own).await.unwrap();
        assert!(plan.own);
        assert_eq!(plan.candidates.len(), 1);
    }

    /// No provider shows videos: "I could not look", not "there is none"
    /// (K9).
    #[tokio::test]
    async fn no_video_provider_is_an_error_that_says_what_to_do() {
        let dir = TempDir::new("video-none");
        let (session, registry) = session_with(&dir, &[], &[]);
        let item = QueueItem {
            id: ProviderTrackId::new(ProviderId::new("local"), "/music/x.flac"),
            track: TrackRef::new("A", "B"),
        };
        let err = session.video_for(&registry, &item).await.unwrap_err();
        assert_eq!(err.stage(), Stage::VideoLookup);
        assert!(
            err.chain_text().contains("--for video"),
            "{}",
            err.chain_text()
        );
    }
}
