//! The core's outer surface.
//!
//! The CLI, the GUI and the mobile bindings call **only** the methods here:
//! every command is a single call, and diagnostics and persistence are taken
//! care of here. Delete a capability from the CLI and the core still offers
//! it — the Golden Rule's test.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::diag::{DiagReport, Recorder, Stage};
use crate::error::{Error, ErrorKind, Result};
use crate::identity::{
    FingerprintLookup, MetadataLookup, OfflineLookup, Resolution, ResolveSummary, Resolver,
};
use crate::ids::ProviderId;
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
        let result = Ok(registry.list());
        self.finish(rec, result, |providers, diag| ProviderListReport {
            providers,
            diag,
        })
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
        let dirs = self.config.music_dirs();
        self.finish(rec, result, move |(summary, write), diag| ScanReport {
            dirs,
            summary,
            write,
            scanned: true,
            reason: "requested".to_owned(),
            diag,
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
            let dirs = self.config.music_dirs();
            return self.finish(rec, Ok(()), move |(), diag| ScanReport {
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
        let dirs = self.config.music_dirs();
        self.finish(rec, result, move |(summary, write), diag| ScanReport {
            dirs,
            summary,
            write,
            scanned: true,
            reason,
            diag,
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
    /// If `all` is false only the first match is taken. An empty result is an
    /// **error**: better than "played it, but no sound".
    ///
    /// # Errors
    /// If there is no provider, the search fails, or nothing matches.
    pub async fn queue_from_search(
        &self,
        registry: &ProviderRegistry,
        query: &str,
        all: bool,
        limit: usize,
    ) -> Result<Vec<QueueItem>> {
        // The **persistent catalog** first: the scan happens once, and the search
        // is answered by FTS without touching the disk. Asking the provider is
        // needed only if the catalog is empty (not scanned yet, or a remote
        // provider).
        let mut items: Vec<QueueItem> = self
            .library
            .search_catalog(query, limit)?
            .into_iter()
            .filter(|hit| {
                // Skip a provider that is still in the catalog but can no longer play.
                registry.get(&hit.id.provider).is_some_and(|provider| {
                    provider
                        .info()
                        .capabilities
                        .contains(crate::provider::Capabilities::STREAM)
                })
            })
            .map(|hit| QueueItem {
                id: hit.id,
                track: hit.track,
            })
            .collect();

        if items.is_empty() {
            let streamers = registry.with_capability(crate::provider::Capabilities::STREAM);
            if streamers.is_empty() {
                return Err(Error::new(
                    Stage::PlaybackResolve,
                    ErrorKind::NotFound {
                        what: "provider that can stream audio".to_owned(),
                    },
                ));
            }
            for provider in streamers {
                let hits = provider.search(query, limit).await?;
                items.extend(hits.into_iter().map(|hit| QueueItem {
                    id: hit.id,
                    track: hit.track,
                }));
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
                player.queue_mut().replace(items.clone());
                return Ok((items, Vec::new(), false));
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
            Ok((items, listens, true))
        }
        .await;

        let report = match result {
            Ok((items, listens, played)) => {
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
                Ok((items, written.inserted, played))
            }
            Err(err) => Err(err),
        };

        let query = options.query;
        self.finish(
            rec,
            report,
            move |(queued, listens_recorded, played), diag| PlayReport {
                query,
                queued,
                listens_recorded,
                played,
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

/// The variable that is the desktop's `--online` (D-076). It takes `1`
/// (online) or `0` (offline); unset or empty is offline.
pub const ONLINE_ENV: &str = "HEADSHELL_ONLINE";

/// The lookup mode [`ONLINE_ENV`] asks for.
///
/// A shell without command-line flags (the desktop) reads its `--online`
/// from here, as the local provider reads `HEADSHELL_MUSIC_DIRS`.
///
/// # Errors
/// If the variable holds anything but `1` or `0`: a `true` or `yes` that went
/// unrecognised would silently leave the user offline (K9).
pub fn lookup_mode_from_env() -> Result<LookupMode> {
    match std::env::var(ONLINE_ENV) {
        Ok(raw) => lookup_mode_of(Some(&raw)),
        Err(std::env::VarError::NotPresent) => lookup_mode_of(None),
        Err(std::env::VarError::NotUnicode(raw)) => lookup_mode_of(Some(&raw.to_string_lossy())),
    }
}

fn lookup_mode_of(raw: Option<&str>) -> Result<LookupMode> {
    match raw.map(str::trim) {
        None | Some("" | "0") => Ok(LookupMode::Offline),
        Some("1") => Ok(LookupMode::Online),
        Some(other) => Err(Error::new(
            Stage::ConfigLoad,
            ErrorKind::InvalidInput {
                detail: format!("{ONLINE_ENV} is `{other}` — it takes 1 (online) or 0 (offline)"),
            },
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_online_variable_takes_one_or_zero_and_says_what_else_it_got() {
        assert_eq!(lookup_mode_of(None).unwrap(), LookupMode::Offline);
        assert_eq!(lookup_mode_of(Some("")).unwrap(), LookupMode::Offline);
        assert_eq!(lookup_mode_of(Some("0")).unwrap(), LookupMode::Offline);
        assert_eq!(lookup_mode_of(Some(" 1 ")).unwrap(), LookupMode::Online);

        let err = lookup_mode_of(Some("true")).unwrap_err();
        assert_eq!(err.stage(), Stage::ConfigLoad);
        let text = err.chain_text();
        assert!(text.contains("HEADSHELL_ONLINE is `true`"), "{text}");
        assert!(text.contains("1 (online) or 0 (offline)"), "{text}");
    }
}
