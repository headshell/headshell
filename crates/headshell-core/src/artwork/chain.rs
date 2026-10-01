//! One track through the chain (D-076). The order and the reasons are in
//! the module documentation ([`super`]).
//!
//! The chain is synchronous: it runs on the background worker's thread, or
//! under the command that waits for it. The providers' and the identity
//! chain's futures are driven with [`crate::plugin::block_on`] — the HTTP
//! client below them blocks anyway.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use super::cache::{ArtworkCache, Entry};
use super::lookup::{CoverArtArchive, Entity, choose_release};
use super::{ArtworkSource, ArtworkStatus, REQUEST_EDGE, embedded, image, key_for};
use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::identity::musicbrainz::{MusicBrainzLookup, Release};
use crate::identity::{MetadataLookup, ResolveMethod, Resolver};
use crate::ids::{ArtworkKey, ProviderTrackId};
use crate::model::TrackRef;
use crate::net::HttpClient;
use crate::plugin::block_on;
use crate::provider::{Capabilities, ProviderRegistry};

/// The links that need the network and a third party: asked only online.
pub(crate) struct Online {
    musicbrainz: Arc<MusicBrainzLookup>,
    resolver: Resolver,
    archive: CoverArtArchive,
}

impl Online {
    /// The public MusicBrainz server and the public archive, over `http`.
    pub(crate) fn new(http: Arc<dyn HttpClient>) -> Self {
        Self::with(
            MusicBrainzLookup::new(Arc::clone(&http)),
            CoverArtArchive::new(http),
        )
    }

    pub(crate) fn with(musicbrainz: MusicBrainzLookup, archive: CoverArtArchive) -> Self {
        let musicbrainz = Arc::new(musicbrainz);
        let lookup: Arc<dyn MetadataLookup> = musicbrainz.clone();
        Self {
            resolver: Resolver::new(lookup),
            musicbrainz,
            archive,
        }
    }

    /// This build's online links.
    ///
    /// # Errors
    /// If the build has no HTTP client: online was asked for and cannot be
    /// had — said, not quietly turned into offline (K9).
    pub(crate) fn from_build() -> Result<Self> {
        Ok(Self::new(crate::net::default_http_client()?))
    }
}

/// What the chain can reach.
pub(crate) struct Links {
    pub(crate) registry: ProviderRegistry,
    /// `None`: offline — the third parties are not asked.
    pub(crate) online: Option<Online>,
}

/// One track's result: the status, and what failed on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) key: ArtworkKey,
    pub(crate) status: ArtworkStatus,
    pub(crate) notes: Vec<String>,
}

/// The chain, with the cache it reads and fills.
pub(crate) struct Chain {
    links: Links,
    cache: Arc<Mutex<ArtworkCache>>,
}

fn one_line(err: &Error) -> String {
    err.chain_text().replace('\n', " ")
}

impl Chain {
    pub(crate) fn new(links: Links, cache: Arc<Mutex<ArtworkCache>>) -> Self {
        Self { links, cache }
    }

    pub(crate) fn online(&self) -> bool {
        self.links.online.is_some()
    }

    /// The providers a new queue plays from.
    pub(crate) fn set_registry(&mut self, registry: ProviderRegistry) {
        self.links.registry = registry;
    }

    /// A queued track: its provider's cover, then the third parties.
    pub(crate) fn resolve(&self, id: &ProviderTrackId, track: &TrackRef) -> Outcome {
        let key = key_for(track);
        if let Some(known) = self.known(&key) {
            return known;
        }
        let mut notes = Vec::new();
        if let Some(settled) = self.provider_cover(id, &key, &mut notes) {
            return settled;
        }
        self.third_parties(key, track, notes)
    }

    /// A track on screen besides the queue — a suggestion (D-080): what the
    /// cache knows, or its provider's own cover. The third parties are not
    /// asked: a suggestion list changes at every pause in typing. So nothing
    /// found is `None`, and nothing is kept for it — the queue still asks
    /// them for the same album later, when online.
    pub(crate) fn resolve_where_it_lives(
        &self,
        id: &ProviderTrackId,
        track: &TrackRef,
    ) -> Option<Outcome> {
        let key = key_for(track);
        if let Some(known) = self.known(&key) {
            return Some(known);
        }
        let mut notes = Vec::new();
        let settled = self.provider_cover(id, &key, &mut notes);
        if settled.is_none() && !notes.is_empty() {
            tracing::info!(
                key = key.as_str(),
                notes = notes.join(" | "),
                "cover of a suggestion: a link failed on the way"
            );
        }
        settled
    }

    /// The provider's own cover: `Some` when it was found — and kept — or
    /// when the plugin gave way to a call for the user (pending); `None`
    /// otherwise, with what failed written into `notes`.
    fn provider_cover(
        &self,
        id: &ProviderTrackId,
        key: &ArtworkKey,
        notes: &mut Vec<String>,
    ) -> Option<Outcome> {
        match self.links.registry.get(&id.provider) {
            Some(provider) if provider.info().capabilities.contains(Capabilities::ARTWORK) => {
                match block_on(provider.artwork(id, REQUEST_EDGE)) {
                    Ok(Some(found)) => match self.keep(key, found.source, &found.bytes, None) {
                        Ok(status) => {
                            return Some(Outcome {
                                key: key.clone(),
                                status,
                                notes: std::mem::take(notes),
                            });
                        }
                        Err(err) => notes.push(format!(
                            "the cover from {} is unusable — {}",
                            id.provider,
                            one_line(&err)
                        )),
                    },
                    Ok(None) => {}
                    // It gave way to a call for the user (D-077): not an
                    // answer. The row stays pending and the worker asks again.
                    Err(err) if matches!(err.kind(), ErrorKind::PluginPreempted { .. }) => {
                        return Some(Outcome {
                            key: key.clone(),
                            status: ArtworkStatus::Pending,
                            notes: std::mem::take(notes),
                        });
                    }
                    Err(err) => notes.push(format!(
                        "{} could not give its cover — {}",
                        id.provider,
                        one_line(&err)
                    )),
                }
            }
            // A provider without covers has nothing to be asked.
            Some(_) => {}
            None => notes.push(format!(
                "the provider {} is no longer registered; its cover was not asked",
                id.provider
            )),
        }
        None
    }

    /// A file on disk (`headshell artwork --file`): its tags and its folder,
    /// then the third parties.
    pub(crate) fn resolve_file(&self, path: &Path, track: &TrackRef) -> Outcome {
        let key = key_for(track);
        if let Some(known) = self.known(&key) {
            return known;
        }
        let mut notes = Vec::new();
        match embedded::file_cover(path) {
            Ok(Some(found)) => match self.keep(&key, found.source, &found.bytes, None) {
                Ok(status) => return Outcome { key, status, notes },
                Err(err) => {
                    notes.push(format!("the file's cover is unusable — {}", one_line(&err)))
                }
            },
            Ok(None) => {}
            Err(err) => notes.push(format!(
                "the file's cover could not be read — {}",
                one_line(&err)
            )),
        }
        self.third_parties(key, track, notes)
    }

    fn third_parties(&self, key: ArtworkKey, track: &TrackRef, mut notes: Vec<String>) -> Outcome {
        let Some(online) = &self.links.online else {
            // Offline. When a link we could ask failed, that failure is the
            // answer; when none failed, the answer is "not asked".
            if notes.is_empty() {
                return Outcome {
                    key,
                    status: ArtworkStatus::NotCheckedOffline,
                    notes,
                };
            }
            // The failures are the status now; kept twice they would be
            // printed twice.
            return Outcome {
                key,
                status: ArtworkStatus::Failed {
                    chain: notes.join("\n"),
                },
                notes: Vec::new(),
            };
        };

        // What was looked at on the way: the answer when nothing is found.
        let mut looked = Vec::new();
        let album = track
            .album
            .as_deref()
            .filter(|album| !album.trim().is_empty());
        let mut chosen: Option<Release> = None;

        // 1. The recording the identity chain matches, and its releases.
        let resolution = match block_on(online.resolver.resolve(track)) {
            Ok(resolution) => resolution,
            Err(err) => return failed(key, &err, notes),
        };
        match &resolution.matched {
            Some(candidate) if !matches!(resolution.method, ResolveMethod::LocalKey) => {
                let recording = &candidate.mbid;
                match block_on(online.musicbrainz.releases_of(recording)) {
                    Ok(Some(releases)) => match choose_release(&releases, album) {
                        Some(release) => chosen = Some(release.clone()),
                        None => looked.push(match album {
                            Some(album) => format!(
                                "the recording MusicBrainz matched ({}) is on {} release(s), none titled \"{album}\"",
                                recording.as_str(),
                                releases.len()
                            ),
                            None => format!("recording {} is on no release", recording.as_str()),
                        }),
                    },
                    Ok(None) => looked.push(format!(
                        "MusicBrainz does not know recording {}",
                        recording.as_str()
                    )),
                    Err(err) => return failed(key, &err, notes),
                }
            }
            _ => {
                looked.push("MusicBrainz has no recording matching the artist and title".to_owned())
            }
        }

        // 2. The album itself, when the recording did not lead to it.
        if chosen.is_none()
            && let Some(album) = album
        {
            match block_on(online.musicbrainz.search_releases(&track.artist, album)) {
                Ok(releases) => match choose_release(&releases, Some(album)) {
                    Some(release) => chosen = Some(release.clone()),
                    None => looked.push(format!(
                        "MusicBrainz has no release titled \"{album}\" by {}",
                        track.artist
                    )),
                },
                Err(err) => return failed(key, &err, notes),
            }
        }

        let Some(release) = chosen else {
            return self.not_found(key, looked.join("; "), notes);
        };

        let asks = [
            Some((Entity::Release, release.id.clone())),
            release
                .group
                .clone()
                .map(|group| (Entity::ReleaseGroup, group)),
        ];
        for (entity, mbid) in asks.into_iter().flatten() {
            match block_on(online.archive.front(entity, &mbid)) {
                Ok(Some(bytes)) => {
                    let detail = format!(
                        "{} {} (\"{}\")",
                        match entity {
                            Entity::Release => "release",
                            Entity::ReleaseGroup => "release group",
                        },
                        mbid.as_str(),
                        release.title
                    );
                    match self.keep(&key, ArtworkSource::CoverArtArchive, &bytes, Some(detail)) {
                        Ok(status) => return Outcome { key, status, notes },
                        Err(err) => notes.push(format!(
                            "the Cover Art Archive's image is unusable — {}",
                            one_line(&err)
                        )),
                    }
                }
                Ok(None) => {}
                Err(err) => return failed(key, &err, notes),
            }
        }
        self.not_found(
            key,
            format!(
                "the Cover Art Archive has no front cover for release {} (\"{}\") or its release group",
                release.id.as_str(),
                release.title
            ),
            notes,
        )
    }

    /// Checks, resizes and keeps an image.
    fn keep(
        &self,
        key: &ArtworkKey,
        source: ArtworkSource,
        bytes: &[u8],
        detail: Option<String>,
    ) -> Result<ArtworkStatus> {
        let images = image::normalize(bytes)?;
        self.lock()?.put_found(key, source, &images, detail)?;
        Ok(ArtworkStatus::Found { source })
    }

    /// "None" — kept for a while, so the album is not asked again. Only a
    /// clean "none": when something failed on the way, the answer may be
    /// different next time.
    fn not_found(&self, key: ArtworkKey, detail: String, mut notes: Vec<String>) -> Outcome {
        if notes.is_empty() {
            let kept = self
                .lock()
                .and_then(|mut cache| cache.put_not_found(&key, detail.clone()));
            if let Err(err) = kept {
                notes.push(format!("could not remember it — {}", one_line(&err)));
            }
        }
        Outcome {
            key,
            status: ArtworkStatus::NotFound { detail },
            notes,
        }
    }

    /// What the cache already knows.
    fn known(&self, key: &ArtworkKey) -> Option<Outcome> {
        let cache = self.lock().ok()?;
        let status = match cache.get(key)? {
            Entry::Found { source, .. } => ArtworkStatus::Found { source: *source },
            Entry::NotFound { detail, .. } => ArtworkStatus::NotFound {
                detail: detail.clone(),
            },
        };
        Some(Outcome {
            key: key.clone(),
            status,
            notes: Vec::new(),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, ArtworkCache>> {
        self.cache.lock().map_err(|_| {
            Error::new(
                Stage::ArtworkStore,
                ErrorKind::Artwork {
                    detail: "the cover cache lock is poisoned (an earlier call panicked)"
                        .to_owned(),
                },
            )
        })
    }
}

/// A link that failed ended the chain: its error is the status. The notes
/// keep what failed before it.
fn failed(key: ArtworkKey, err: &Error, notes: Vec<String>) -> Outcome {
    Outcome {
        key,
        status: ArtworkStatus::Failed {
            chain: err.chain_text(),
        },
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artwork::cache::ArtworkCache;
    use crate::ids::ProviderId;
    use crate::net::fake::FakeHttp;
    use crate::test_support::TempDir;

    fn chain(dir: &Path, online: Option<Online>) -> Chain {
        let cache = Arc::new(Mutex::new(ArtworkCache::open(dir).unwrap()));
        Chain::new(
            Links {
                registry: ProviderRegistry::new(),
                online,
            },
            cache,
        )
    }

    #[cfg(feature = "audio")]
    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/artwork")
            .join(name)
    }

    #[cfg(feature = "audio")]
    #[test]
    fn offline_a_files_own_cover_is_found_and_kept() {
        let dir = TempDir::new("artwork-chain");
        let chain = chain(dir.path(), None);
        let track =
            TrackRef::new("Cover Artist", "Flac Front").with_album(Some("Covered".to_owned()));
        let outcome = chain.resolve_file(&fixture("Cover Artist - Flac Front.flac"), &track);
        assert_eq!(
            outcome.status,
            ArtworkStatus::Found {
                source: ArtworkSource::Embedded
            }
        );
        // Asked again, the cache answers — the file is not read a second time.
        let again = chain.resolve_file(Path::new("/nonexistent/file.flac"), &track);
        assert_eq!(again.status, outcome.status);
    }

    /// Offline, a track whose provider is not registered and that has no
    /// cover anywhere local: the third parties were not asked, and that is
    /// said — with the reason the local link was skipped.
    #[test]
    fn offline_nothing_is_looked_up_and_that_is_not_called_not_found() {
        let dir = TempDir::new("artwork-chain");
        let chain = chain(dir.path(), None);
        let id = ProviderTrackId::new(ProviderId::new("gone"), "1");
        let outcome = chain.resolve(&id, &TrackRef::new("A", "B"));
        assert!(
            matches!(&outcome.status, ArtworkStatus::Failed { chain } if chain.contains("no longer registered")),
            "{outcome:?}"
        );
        assert!(
            outcome.notes.is_empty(),
            "the reason is said once: {outcome:?}"
        );
    }

    /// Online: the recording → its releases → the album's release at the
    /// archive. The canned MusicBrainz and archive answers stand in for the
    /// network (D-020).
    #[test]
    fn online_the_albums_release_cover_comes_from_the_archive() {
        let recording = "b1a9c0e9-d987-4042-ae91-78d6a3267d69";
        let album = "6c8c3d3a-5e2b-4b8e-8f39-f0c7f41d4a01";
        let png = crate::artwork::image::tests_support::png_2x2();
        let http = Arc::new(
            FakeHttp::new()
                .route(
                    "/recording?query=",
                    &format!(
                        r#"{{"recordings":[{{"id":"{recording}","title":"Creep","length":238000,
                            "artist-credit":[{{"name":"Radiohead","joinphrase":""}}]}}]}}"#
                    ),
                )
                .route(
                    &format!("/recording/{recording}?inc=releases"),
                    &format!(
                        r#"{{"releases":[
                            {{"id":"00000000-0000-4000-8000-000000000009","title":"Creep","status":"Official","date":"1992"}},
                            {{"id":"{album}","title":"Pablo Honey","status":"Official","date":"1993"}}]}}"#
                    ),
                )
                .route_bytes(&format!("/release/{album}/front-500"), "image/png", &png),
        );
        let online = Online::with(
            MusicBrainzLookup::new(http.clone())
                .with_base_url("http://mb.test/ws/2")
                .with_min_interval(std::time::Duration::ZERO),
            CoverArtArchive::new(http.clone()).with_base_url("http://caa.test"),
        );
        let dir = TempDir::new("artwork-chain");
        let chain = chain(dir.path(), Some(online));
        let track = TrackRef::new("Radiohead", "Creep").with_album(Some("Pablo Honey".to_owned()));
        let id = ProviderTrackId::new(ProviderId::new("local"), "/x/creep.flac");
        let outcome = chain.resolve(&id, &track);
        assert_eq!(
            outcome.status,
            ArtworkStatus::Found {
                source: ArtworkSource::CoverArtArchive
            },
            "{outcome:?}"
        );
        assert!(
            http.requests().iter().any(|r| r.url.contains(album)),
            "the album's release, not the single's, was asked"
        );
    }

    /// The recording MusicBrainz matched is a namesake on a compilation — a
    /// well-known song has dozens (verification found 77 for "Sour Times").
    /// Its compilation's cover is not the album's: the album is searched by
    /// its title, and the compilation's release is never asked for.
    #[test]
    fn online_a_namesake_recording_does_not_lend_its_compilation_cover() {
        let recording = "b1a9c0e9-d987-4042-ae91-78d6a3267d69";
        let compilation = "00000000-0000-4000-8000-000000000009";
        let album = "6c8c3d3a-5e2b-4b8e-8f39-f0c7f41d4a01";
        let png = crate::artwork::image::tests_support::png_2x2();
        let search = format!(
            r#"{{"recordings":[{{"id":"{recording}","title":"Sour Times","length":251000,
                "artist-credit":[{{"name":"Portishead","joinphrase":""}}]}}]}}"#
        );
        let on_compilation = format!(
            r#"{{"releases":[{{"id":"{compilation}","title":"Rencontres Trans Musicales","status":"Official",
                "date":"1994","release-group":{{"id":"{compilation}","primary-type":"Album",
                "secondary-types":["Compilation"]}}}}]}}"#
        );
        let online = |releases: &str| {
            let http = Arc::new(
                FakeHttp::new()
                    .route("/recording?query=", &search)
                    .route(
                        &format!("/recording/{recording}?inc=releases"),
                        &on_compilation,
                    )
                    .route("/release?query=", releases)
                    .route_bytes(&format!("/release/{album}/front-500"), "image/png", &png)
                    .route_bytes(
                        &format!("/release/{compilation}/front-500"),
                        "image/png",
                        &png,
                    ),
            );
            let online = Online::with(
                MusicBrainzLookup::new(http.clone())
                    .with_base_url("http://mb.test/ws/2")
                    .with_min_interval(std::time::Duration::ZERO),
                CoverArtArchive::new(http.clone()).with_base_url("http://caa.test"),
            );
            (http, online)
        };
        let track = TrackRef::new("Portishead", "Sour Times").with_album(Some("Dummy".to_owned()));
        let id = ProviderTrackId::new(ProviderId::new("local"), "/x/sour-times.flac");

        let (http, found) = online(&format!(
            r#"{{"releases":[{{"id":"{album}","title":"Dummy","status":"Official","date":"1994-08-22",
                "release-group":{{"id":"{album}","primary-type":"Album"}}}}]}}"#
        ));
        let dir = TempDir::new("artwork-chain-namesake");
        let outcome = chain(dir.path(), Some(found)).resolve(&id, &track);
        assert_eq!(
            outcome.status,
            ArtworkStatus::Found {
                source: ArtworkSource::CoverArtArchive
            },
            "{outcome:?}"
        );
        let asked: Vec<String> = http.requests().iter().map(|r| r.url.clone()).collect();
        assert!(
            asked
                .iter()
                .any(|url| url.contains(&format!("/release/{album}/front"))),
            "{asked:#?}"
        );
        assert!(
            !asked
                .iter()
                .any(|url| url.contains(&format!("/release/{compilation}/front"))),
            "the compilation's cover must not be asked for: {asked:#?}"
        );

        // No release of that title either: not found, saying what was looked at.
        let (_, nothing) = online(r#"{"releases":[]}"#);
        let dir = TempDir::new("artwork-chain-namesake-none");
        let outcome = chain(dir.path(), Some(nothing)).resolve(&id, &track);
        let ArtworkStatus::NotFound { detail } = &outcome.status else {
            panic!("{outcome:?}");
        };
        assert!(detail.contains("none titled \"Dummy\""), "{detail}");
        assert!(
            detail.contains("no release titled \"Dummy\" by Portishead"),
            "{detail}"
        );
    }
}
