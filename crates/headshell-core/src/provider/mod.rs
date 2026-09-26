//! Providers: where the audio comes from.
//!
//! A provider can be the local disk, a Subsonic server, SoundCloud or (from
//! Phase 2) a plugin. The core does not know which one it is; it only sees
//! the [`Provider`] trait.
//!
//! ## Why capability flags are a must
//!
//! Providers cannot all do the same things. The local disk offers search and
//! streaming but cannot be remote-controlled; Spotify (Phase 2, a separate
//! package) will be `CONTROL` only — it gives no metadata and streams no
//! audio, it only says "play this". If we assumed the trait were uniform, the
//! abstraction would collapse at the first remote player. That is why
//! capabilities are **asked at runtime**, not assumed at compile time.
//!
//! ## The `uniffi` constraint (K7)
//!
//! No generic parameters, lifetimes or closures in public signatures.
//! `Arc<dyn Provider>` can be modelled as a callback interface; async
//! functions return boxed futures like `LookupFuture` so the trait stays
//! `dyn` compatible (the same route as `MetadataLookup` in D-006).

pub mod local;
pub mod remote;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::ids::{ProviderId, ProviderTrackId};
use crate::model::TrackRef;

/// The return value of a provider call.
///
/// A boxed future instead of `async fn`: the trait has to be `dyn`
/// compatible (K7 / D-006). The same reasoning as `MetadataLookup`.
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// What a provider can do.
///
/// A bit mask; `uniffi` carries it as a `u32`. A capability called without
/// asking for the flag returns [`crate::ErrorKind::Unsupported`] — not a
/// silent empty result, because "I can't" and "no results" are different
/// things (K9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capabilities(u32);

impl Capabilities {
    /// Can search for tracks by text.
    pub const SEARCH: Self = Self(1 << 0);
    /// Can browse the catalog (artist → album → track).
    pub const BROWSE: Self = Self(1 << 1);
    /// Can provide a playable audio source.
    pub const STREAM: Self = Self(1 << 2);
    /// Can control a remote player (like Spotify Connect).
    pub const CONTROL: Self = Self(1 << 3);
    /// Can hand over a track's cover art (D-076).
    pub const ARTWORK: Self = Self(1 << 4);
    /// Can hand over a track's lyrics (D-078).
    pub const LYRICS: Self = Self(1 << 5);

    /// No capability at all.
    pub const NONE: Self = Self(0);

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Combines two capabilities.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Does it contain all of `wanted`?
    #[must_use]
    pub const fn contains(self, wanted: Self) -> bool {
        (self.0 & wanted.0) == wanted.0
    }

    /// A human-readable list: `SEARCH|STREAM`.
    #[must_use]
    pub fn describe(self) -> String {
        let all = [
            (Self::SEARCH, "SEARCH"),
            (Self::BROWSE, "BROWSE"),
            (Self::STREAM, "STREAM"),
            (Self::CONTROL, "CONTROL"),
            (Self::ARTWORK, "ARTWORK"),
            (Self::LYRICS, "LYRICS"),
        ];
        let names: Vec<&str> = all
            .iter()
            .filter(|(flag, _)| self.contains(*flag))
            .map(|(_, name)| *name)
            .collect();
        if names.is_empty() {
            "NONE".to_owned()
        } else {
            names.join("|")
        }
    }
}

impl std::ops::BitOr for Capabilities {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::fmt::Display for Capabilities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// A playable audio source.
///
/// In Phase 1 there is only the local file. Remote streaming (Phase 1.3) and
/// plugin streaming (Phase 2) add variants here — **K3: no variant relays
/// audio through a server**, every one is a source the client fetches
/// itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioSource {
    /// A local file system path.
    LocalFile { path: std::path::PathBuf },
    /// A stream to be fetched over HTTP(S) (Subsonic/Jellyfin).
    ///
    /// The client fetches it **itself**; no `headshell` server steps in between.
    HttpStream {
        url: String,
        /// Headers that must be added to the request (authentication).
        headers: Vec<HttpHeader>,
    },
}

/// A single HTTP header. It is defined in the transport layer (`net`) and
/// re-exported here: `AudioSource` carries it, and callers should not have to
/// learn two paths.
pub use crate::net::HttpHeader;

/// Calls made **ahead of time** (D-077): reading the queue ahead, covers.
///
/// A provider that does one call at a time — a plugin's engine — puts a
/// call made on the user's behalf first, and stops a tool a call made ahead
/// of time is running for it. Which one a call is, is a property of the
/// thread it is made on: the background workers mark their threads once,
/// and nothing in the provider's signature has to carry it.
pub(crate) mod ahead_of_time {
    use std::cell::Cell;

    thread_local! {
        static MARKED: Cell<bool> = const { Cell::new(false) };
    }

    /// Marks the calling thread: every provider call made on it from now
    /// on is made ahead of time.
    pub(crate) fn mark_this_thread() {
        MARKED.with(|marked| marked.set(true));
    }

    /// Is the calling thread a background worker's.
    pub(crate) fn on_this_thread() -> bool {
        MARKED.with(Cell::get)
    }
}

/// A track returned by a provider.
///
/// [`TrackRef`] metadata + the provider's own id. The canonical identity is
/// **not here**: the identity chain produces it (K6); a provider cannot claim
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTrack {
    pub id: ProviderTrackId,
    pub track: TrackRef,
}

/// A cover as a provider hands it over: the image bytes, before the core
/// checks and resizes them (D-076).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtworkImage {
    pub bytes: Vec<u8>,
    /// What the source says the bytes are (`image/jpeg`). Only a hint: the
    /// core reads the bytes themselves to decide.
    pub mime: Option<String>,
    /// Where the provider found it. The summary counts a picture in the
    /// file's tags apart from one in its folder and one a server sent (K9).
    pub source: crate::artwork::ArtworkSource,
}

/// A track's lyrics as the provider hands them over (D-078): parsed, but not
/// yet weighed against the other links of the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLyrics {
    /// The lines; synced when the provider knows when each is sung.
    pub lyrics: crate::lyrics::Lyrics,
    /// The provider says the track has no words. The lines are empty then.
    pub instrumental: bool,
    /// Where it found them: a `.lrc` next to the file, the file's tags, or
    /// the server or plugin itself. The chain treats the file's own lyrics
    /// apart — they win even unsynced (D-078).
    pub source: crate::lyrics::LyricsSource,
}

/// A provider's identity and capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: ProviderId,
    /// The name shown to the user ("Local files").
    pub display_name: String,
    pub capabilities: Capabilities,
}

/// A provider's health. `headshell provider test <name>` prints this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderHealth {
    pub id: ProviderId,
    pub reachable: bool,
    /// How many tracks are visible (if known). For diagnostics — K9.
    pub track_count: Option<usize>,
    /// A human-readable explanation if there is a problem.
    pub detail: Option<String>,
}

/// Anything that provides an audio source.
///
/// A call without the capability flag must return
/// [`crate::ErrorKind::Unsupported`] — not a silent empty result.
pub trait Provider: Send + Sync {
    /// Identity and capabilities. Synchronous: it must be known without making a
    /// call.
    fn info(&self) -> ProviderInfo;

    /// Is the provider up, and how many tracks does it see.
    fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth>;

    /// Search by text. Needs the `SEARCH` flag.
    fn search<'a>(&'a self, query: &'a str, limit: usize)
    -> ProviderFuture<'a, Vec<ProviderTrack>>;

    /// Gives a track's playable source. Needs the `STREAM` flag.
    fn resolve_source<'a>(
        &'a self,
        id: &'a ProviderTrackId,
    ) -> ProviderFuture<'a, Option<AudioSource>>;

    /// Scans its catalog and produces rows **ready to be written to the
    /// persistent store**.
    ///
    /// `known` is the previously seen `reference → stamp` map. The provider does
    /// not re-read the metadata of items whose stamp has not changed, and leaves
    /// the [`ScannedItem::track`] field `None` — the caller keeps that row in the
    /// catalog as it is. That is what makes scanning a large library cheap.
    ///
    /// The default implementation returns `None`: most providers (remote APIs,
    /// remote control) have no local catalog to scan.
    ///
    /// A trait method instead of a downcast: it is called through
    /// `Arc<dyn Provider>`, and plugins (Phase 2) can implement it in their own
    /// way.
    fn scan_catalog<'a>(
        &'a self,
        known: &'a std::collections::HashMap<String, i64>,
    ) -> ProviderFuture<'a, Option<CatalogScan>> {
        let _ = known;
        Box::pin(std::future::ready(Ok(None)))
    }

    /// Might the catalog have changed since `since_ms`? (D-025)
    ///
    /// It must be **much cheaper** than a full scan; its purpose is to answer "is
    /// a scan worth it". There are three separate answers, and all three mean
    /// different things (K9):
    ///
    /// - `Some(true)` — it changed, a scan is worth it.
    /// - `Some(false)` — it did not change, the scan can be skipped.
    /// - `None` — **I don't know.** This is the default; a remote provider offers
    ///   no cheap change stamp, and saying "unchanged" would be wrong.
    ///
    /// Again a trait method instead of a downcast: plugins (Phase 2) can supply
    /// their own cheap stamps.
    fn catalog_changed_since(&self, since_ms: i64) -> ProviderFuture<'_, Option<bool>> {
        let _ = since_ms;
        Box::pin(std::future::ready(Ok(None)))
    }

    /// The provider's own cover for a track (D-076). Needs the `ARTWORK` flag.
    ///
    /// `size` is the edge, in pixels, the caller would like. A server that can
    /// resize (Subsonic `size`, Jellyfin `maxWidth`) is asked for it; the rest
    /// ignore it — the core resizes anyway.
    ///
    /// `Ok(None)`: this provider has no cover for the track — "I looked, there
    /// is none". Without the flag the default returns `Unsupported`, because "I
    /// can't" is a different answer (K9). Again a trait method instead of a
    /// downcast: plugins answer it in their own way (api 3).
    fn artwork<'a>(
        &'a self,
        id: &'a ProviderTrackId,
        size: u32,
    ) -> ProviderFuture<'a, Option<ArtworkImage>> {
        let _ = (id, size);
        let info = self.info();
        Box::pin(std::future::ready(Err(crate::Error::new(
            crate::diag::Stage::ArtworkRead,
            crate::error::ErrorKind::Unsupported {
                provider: info.id.as_str().to_owned(),
                what: "cover art".to_owned(),
                capabilities: info.capabilities.describe(),
            },
        ))))
    }

    /// The provider's own lyrics for a track (D-078). Needs the `LYRICS`
    /// flag.
    ///
    /// `Ok(None)`: this provider has no lyrics for the track — "I looked,
    /// there are none". Without the flag the default returns `Unsupported`,
    /// because "I can't" is a different answer (K9).
    fn lyrics<'a>(&'a self, id: &'a ProviderTrackId) -> ProviderFuture<'a, Option<ProviderLyrics>> {
        let _ = id;
        let info = self.info();
        Box::pin(std::future::ready(Err(crate::Error::new(
            crate::diag::Stage::LyricsRead,
            crate::error::ErrorKind::Unsupported {
                provider: info.id.as_str().to_owned(),
                what: "lyrics".to_owned(),
                capabilities: info.capabilities.describe(),
            },
        ))))
    }

    /// Is this a plugin — a provider whose tracks come from an internet
    /// service anyway (D-078)? A plugin track's lyrics are looked up at LRCLIB
    /// the moment it starts; a local file's and a server's only once the user
    /// asks for lyrics. Synchronous, like [`Self::info`].
    fn is_plugin(&self) -> bool {
        false
    }
}

/// The result of a scan: rows + a summary of what happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogScan {
    pub tracks: Vec<ScannedItem>,
    pub summary: ScanSummary,
}

/// A single item seen in a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedItem {
    /// The provider's id for this item.
    pub id: ProviderTrackId,
    /// The change stamp (mtime for a local file). Without it, every scan reads
    /// it again.
    pub mtime_ms: Option<i64>,
    /// The metadata read. If `None` the item has not changed — its state in the
    /// catalog is kept.
    pub track: Option<TrackRef>,
    /// Whether the metadata came from the tags (not from the file name/a guess).
    pub from_tags: bool,
}

pub use local::ScanSummary;

/// The registered providers. `headshell provider list` reads this.
///
/// In Phase 2 plugins join here (K5); we use the same surface today so the
/// registry's interface does not have to change that day.
#[derive(Default, Clone)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn Provider>>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("count", &self.providers.len())
            .finish()
    }
}

impl ProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a provider.
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.push(provider);
    }

    /// The information of the registered providers.
    #[must_use]
    pub fn list(&self) -> Vec<ProviderInfo> {
        self.providers.iter().map(|p| p.info()).collect()
    }

    /// All registered providers.
    #[must_use]
    pub fn all(&self) -> Vec<Arc<dyn Provider>> {
        self.providers.iter().map(Arc::clone).collect()
    }

    /// Finds one by name.
    #[must_use]
    pub fn get(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.providers
            .iter()
            .find(|p| &p.info().id == id)
            .map(Arc::clone)
    }

    /// The providers with a given capability.
    #[must_use]
    pub fn with_capability(&self, wanted: Capabilities) -> Vec<Arc<dyn Provider>> {
        self.providers
            .iter()
            .filter(|p| p.info().capabilities.contains(wanted))
            .map(Arc::clone)
            .collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }
}

/// Sets up the default providers from the configuration.
///
/// The local file provider (D-017) + the remote servers in `servers.json`
/// (D-019), with this build's default HTTP client (D-020). Plugins join here
/// in Phase 2; the setup lives in the core from today so the callers' (CLI,
/// GUI, mobile) signatures do not change.
///
/// # Errors
/// If the server record file cannot be read or is corrupt.
pub fn default_registry(config: &crate::config::Config) -> crate::Result<ProviderRegistry> {
    let servers = remote::load_servers(&config.servers_path())?;
    let http = if servers.is_empty() {
        // If there are no registered servers there is no need to build a
        // client: `provider list` must work in a build with `http-client` off
        // too.
        None
    } else {
        match crate::net::default_http_client() {
            Ok(client) => Some(client),
            Err(err) => {
                // We do not skip silently: the user has registered servers, but
                // this build cannot go online (K9).
                tracing::warn!(
                    error = %err.chain_text().replace('\n', " "),
                    servers = servers.len(),
                    "registered remote servers were skipped"
                );
                None
            }
        }
    };
    registry_with_http(config, http)
}

/// Sets up the providers with the given HTTP transport.
///
/// The GUI and mobile call this: they supply their own HTTP stacks as
/// `Arc<dyn HttpClient>` and do not carry a second TLS tree (D-020). If
/// `http` is `None` only the local provider is set up.
///
/// # Errors
/// If the server record file cannot be read or is corrupt.
pub fn registry_with_http(
    config: &crate::config::Config,
    http: Option<Arc<dyn crate::net::HttpClient>>,
) -> crate::Result<ProviderRegistry> {
    let mut registry = ProviderRegistry::new();
    registry.register(Arc::new(local::LocalProvider::new(config.music_dirs())));

    if let Some(http) = http {
        for server in remote::load_servers(&config.servers_path())? {
            registry.register(remote::provider_for(&server, Arc::clone(&http)));
        }
    }

    // Approved plugins (Phase 2, §2.1). None of them is **started** here: each
    // plugin starts its own engine on its first call, and `provider list` works
    // without starting any.
    let (plugins, summary) = crate::plugin::load(config)?;
    for plugin in plugins {
        registry.register(plugin);
    }
    if summary.discovered > 0 {
        // The ones not loaded must not stay silent: their reasons are in
        // `headshell plugin list`.
        tracing::debug!(
            discovered = summary.discovered,
            ready = summary.ready,
            awaiting_approval = summary.awaiting_approval,
            disabled = summary.disabled,
            incompatible = summary.incompatible,
            broken = summary.broken,
            "plugins scanned"
        );
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_compose_and_describe() {
        let caps = Capabilities::SEARCH | Capabilities::STREAM;
        assert!(caps.contains(Capabilities::SEARCH));
        assert!(caps.contains(Capabilities::STREAM));
        assert!(!caps.contains(Capabilities::CONTROL));
        assert_eq!(caps.describe(), "SEARCH|STREAM");
        assert_eq!(Capabilities::NONE.describe(), "NONE");
    }

    #[test]
    fn contains_requires_all_requested_flags() {
        let caps = Capabilities::SEARCH;
        let wanted = Capabilities::SEARCH | Capabilities::STREAM;
        assert!(
            !caps.contains(wanted),
            "contains must be false when a flag is missing"
        );
    }

    #[test]
    fn capabilities_survive_a_json_round_trip() {
        // `uniffi` will carry this as a u32; the serde form must be a number too.
        let caps = Capabilities::SEARCH | Capabilities::CONTROL;
        let json = serde_json::to_string(&caps).unwrap();
        assert_eq!(json, "9", "1 | 8 = 9");
        let back: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back, caps);
    }

    #[test]
    fn audio_source_is_tagged_in_json() {
        let source = AudioSource::LocalFile {
            path: std::path::PathBuf::from("/music/a.flac"),
        };
        let json = serde_json::to_string(&source).unwrap();
        assert!(json.contains("\"kind\":\"local_file\""), "{json}");
        let back: AudioSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back, source);
    }
}
