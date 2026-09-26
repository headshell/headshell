//! One track through the chain (D-078). The order and the reasons are in the
//! module documentation ([`super`]).
//!
//! Two halves: [`Chain::gather`] asks the links that were not asked yet —
//! the network only where it is allowed — and [`decide`] weighs what they
//! answered, without asking anything. The playing session keeps what it
//! gathered for the playing track, so opening the lyrics view later asks
//! only LRCLIB, not the plugin again.
//!
//! The chain is synchronous: it runs on the background worker's thread, or
//! under the command that waits for it. The providers' futures are driven
//! with [`crate::plugin::block_on`] — the HTTP client below them blocks
//! anyway.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use super::cache::LyricsCache;
use super::lrclib::{Answer, Lrclib};
use super::{Lyrics, LyricsFit, LyricsSource, LyricsStatus, local};
use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::ids::{LyricsKey, ProviderTrackId};
use crate::model::TrackRef;
use crate::plugin::block_on;
use crate::provider::{Capabilities, ProviderLyrics, ProviderRegistry};

/// What the chain can reach.
pub(crate) struct Links {
    pub(crate) registry: ProviderRegistry,
    /// `Err`: this build cannot go online, and why — said when LRCLIB would
    /// be asked, not turned into "none" (K9).
    pub(crate) lrclib: std::result::Result<Lrclib, String>,
}

impl Links {
    /// This build's links.
    pub(crate) fn from_build(registry: ProviderRegistry) -> Self {
        Self {
            registry,
            lrclib: Lrclib::from_build().map_err(|err| err.chain_text().replace('\n', " ")),
        }
    }
}

/// The track to look up.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ask<'a> {
    pub(crate) id: &'a ProviderTrackId,
    pub(crate) track: &'a TrackRef,
    /// The recording's length: the decoder's when known, else the metadata's.
    pub(crate) duration_ms: Option<u64>,
    /// A file on disk outside the registry (`headshell lyrics --file`).
    pub(crate) file: Option<&'a Path>,
    /// Has the user asked for lyrics — the view, the command (D-078)?
    pub(crate) requested: bool,
}

/// Where the track lives: the file's own or the provider's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Own {
    Found(ProviderLyrics),
    /// Asked; there are none.
    None {
        detail: String,
    },
    /// The provider gives no lyrics at all: nothing to ask.
    Unsupported,
    /// A plugin gave way to a call the user made (D-077): asked again later.
    GaveWay,
    Failed {
        chain: String,
    },
}

/// LRCLIB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Remote {
    Answer(Answer),
    Failed { chain: String },
}

/// What the links answered for one track, so far. `None`: not asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Gathered {
    pub(crate) own: Option<Own>,
    pub(crate) lrclib: Option<Remote>,
}

impl Gathered {
    /// Does the track's own answer end the chain — so LRCLIB is not needed?
    fn settled_by_own(&self) -> bool {
        match &self.own {
            Some(Own::Found(found)) => {
                found.source.is_the_files() || found.lyrics.synced || found.instrumental
            }
            // Not answered yet: LRCLIB waits for it.
            Some(Own::GaveWay) | None => true,
            Some(_) => false,
        }
    }
}

/// One track's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) status: LyricsStatus,
    pub(crate) lyrics: Option<Lyrics>,
    pub(crate) notes: Vec<String>,
}

/// The chain, with the cache it reads and fills.
pub(crate) struct Chain {
    links: Links,
    cache: Arc<Mutex<LyricsCache>>,
}

fn one_line(err: &Error) -> String {
    err.chain_text().replace('\n', " ")
}

impl Chain {
    pub(crate) fn new(links: Links, cache: Arc<Mutex<LyricsCache>>) -> Self {
        Self { links, cache }
    }

    /// The providers a new queue plays from.
    pub(crate) fn set_registry(&mut self, registry: ProviderRegistry) {
        self.links.registry = registry;
    }

    /// May LRCLIB be asked for this track? When the user asked, or when the
    /// track plays from a plugin — from the internet anyway (D-078).
    pub(crate) fn lrclib_allowed(&self, ask: &Ask<'_>) -> bool {
        ask.requested
            || (ask.file.is_none()
                && self
                    .links
                    .registry
                    .get(&ask.id.provider)
                    .is_some_and(|provider| provider.is_plugin()))
    }

    /// Asks the links `gathered` has no answer from: where the track lives,
    /// then — when allowed and still needed — LRCLIB, through the cache.
    pub(crate) fn gather(&self, ask: &Ask<'_>, gathered: &mut Gathered) {
        if matches!(gathered.own, None | Some(Own::GaveWay)) {
            gathered.own = Some(self.own(ask));
        }
        if gathered.lrclib.is_none() && self.lrclib_allowed(ask) && !gathered.settled_by_own() {
            gathered.lrclib = Some(self.lrclib(ask));
        }
    }

    fn own(&self, ask: &Ask<'_>) -> Own {
        // A track that is a file on disk had its own lyrics read from it —
        // "none" is said about the file; a server's or a plugin's by its name.
        let is_a_file = ask.file.is_some() || Path::new(&ask.id.id).is_file();
        let (outcome, holder) = match ask.file {
            Some(path) => (local::file_lyrics(path), String::new()),
            None => match self.links.registry.get(&ask.id.provider) {
                Some(provider) if provider.info().capabilities.contains(Capabilities::LYRICS) => (
                    block_on(provider.lyrics(ask.id)),
                    provider.info().display_name,
                ),
                Some(_) => return Own::Unsupported,
                None => {
                    return Own::Failed {
                        chain: format!(
                            "the provider {} is no longer registered; its lyrics were not asked",
                            ask.id.provider
                        ),
                    };
                }
            },
        };
        match outcome {
            Ok(Some(found)) if found.instrumental || !found.lyrics.is_empty() => Own::Found(found),
            Ok(_) => Own::None {
                detail: if is_a_file {
                    "the file has no .lrc beside it and no lyrics in its tags".to_owned()
                } else {
                    format!("{holder} has none for it")
                },
            },
            Err(err) if matches!(err.kind(), ErrorKind::PluginPreempted { .. }) => Own::GaveWay,
            // A server without lyrics support: nothing to ask, not a failure.
            Err(err)
                if ask.file.is_none() && matches!(err.kind(), ErrorKind::Unsupported { .. }) =>
            {
                Own::Unsupported
            }
            Err(err) => Own::Failed {
                chain: err.chain_text(),
            },
        }
    }

    fn lrclib(&self, ask: &Ask<'_>) -> Remote {
        let key = LyricsKey::of(ask.id);
        match self.lock().and_then(|cache| cache.lrclib(&key)) {
            Ok(Some(answer)) => return Remote::Answer(answer),
            Ok(None) => {}
            // A broken cache entry is asked again; the reason is logged.
            Err(err) => {
                tracing::warn!(error = %one_line(&err), "the lyrics cache could not be read")
            }
        }
        let client = match &self.links.lrclib {
            Ok(client) => client,
            Err(reason) => {
                return Remote::Failed {
                    chain: format!("STEP: LYRICS_LOOKUP\n  → LRCLIB was not asked: {reason}"),
                };
            }
        };
        match block_on(client.find(ask.track, ask.duration_ms)) {
            Ok(answer) => {
                if let Err(err) = self
                    .lock()
                    .and_then(|mut cache| cache.put_lrclib(&key, &answer))
                {
                    tracing::warn!(error = %one_line(&err), "LRCLIB's answer could not be kept");
                }
                Remote::Answer(answer)
            }
            Err(err) => Remote::Failed {
                chain: err.chain_text(),
            },
        }
    }

    /// The user's shift for a track.
    pub(crate) fn offset(&self, key: &LyricsKey) -> i64 {
        self.lock().map_or(0, |cache| cache.offset(key))
    }

    fn lock(&self) -> Result<MutexGuard<'_, LyricsCache>> {
        self.cache.lock().map_err(|_| {
            Error::new(
                Stage::LyricsStore,
                ErrorKind::Lyrics {
                    detail: "the lyrics cache lock is poisoned (an earlier call panicked)"
                        .to_owned(),
                },
            )
        })
    }
}

/// Weighs what the links answered (D-078):
///
/// 1. the file's own, synced or not — made for this file;
/// 2. the provider's synced lyrics, or its word that there are none;
/// 3. LRCLIB's synced version — of the length, else the nearest — or its
///    word that the version of this length has none;
/// 4. unsynced words: the provider's, then LRCLIB's.
///
/// Without any: a plugin that gave way is pending; LRCLIB not asked is "not
/// requested" — or the own link's failure, when it failed; LRCLIB's failure
/// or its "none" otherwise. A failure the answer did not come from is a note.
pub(crate) fn decide(gathered: &Gathered) -> Outcome {
    let found = |lyrics: &Lyrics, source, fit, delta_ms| LyricsStatus::Found {
        source,
        synced: lyrics.synced,
        fit,
        delta_ms,
    };
    let own = gathered.own.as_ref();
    let own_found = match own {
        Some(Own::Found(found)) => Some(found),
        _ => None,
    };
    let lrclib_found = match &gathered.lrclib {
        Some(Remote::Answer(Answer::Found {
            lyrics,
            fit,
            delta_ms,
            ..
        })) => Some((lyrics, *fit, *delta_ms)),
        _ => None,
    };
    let mut notes = Vec::new();
    if let Some(Own::Failed { chain }) = own {
        notes.push(format!(
            "where the track lives could not give its lyrics — {}",
            chain.replace('\n', " ")
        ));
    }
    let with = |status, lyrics: Option<&Lyrics>, notes: Vec<String>| Outcome {
        status,
        lyrics: lyrics.cloned(),
        notes,
    };

    // 1–2: where the track lives.
    if let Some(own) = own_found {
        if own.instrumental {
            return with(
                LyricsStatus::Instrumental { source: own.source },
                None,
                notes,
            );
        }
        if own.source.is_the_files() || own.lyrics.synced {
            return with(
                found(&own.lyrics, own.source, LyricsFit::Own, None),
                Some(&own.lyrics),
                notes,
            );
        }
    }
    // 3: LRCLIB's synced version, or its "instrumental".
    if let Some((lyrics, fit, delta_ms)) = lrclib_found
        && lyrics.synced
    {
        return with(
            found(lyrics, LyricsSource::Lrclib, fit, delta_ms),
            Some(lyrics),
            notes,
        );
    }
    if let Some(Remote::Answer(Answer::Instrumental { .. })) = &gathered.lrclib {
        return with(
            LyricsStatus::Instrumental {
                source: LyricsSource::Lrclib,
            },
            None,
            notes,
        );
    }
    // 4: unsynced words.
    if let Some(own) = own_found {
        return with(
            found(&own.lyrics, own.source, LyricsFit::Own, None),
            Some(&own.lyrics),
            notes,
        );
    }
    if let Some((lyrics, fit, delta_ms)) = lrclib_found {
        return with(
            found(lyrics, LyricsSource::Lrclib, fit, delta_ms),
            Some(lyrics),
            notes,
        );
    }

    // None anywhere.
    if matches!(own, None | Some(Own::GaveWay)) {
        return with(LyricsStatus::Pending, None, notes);
    }
    let status = match &gathered.lrclib {
        None => match own {
            // The failure is the status now; kept twice it would print twice.
            Some(Own::Failed { chain }) => {
                notes.clear();
                LyricsStatus::Failed {
                    chain: chain.clone(),
                }
            }
            _ => LyricsStatus::NotRequested,
        },
        Some(Remote::Failed { chain }) => LyricsStatus::Failed {
            chain: chain.clone(),
        },
        Some(Remote::Answer(Answer::None { detail })) => LyricsStatus::NotFound {
            detail: match own {
                Some(Own::None { detail: own }) => format!("{own}; {detail}"),
                _ => detail.clone(),
            },
        },
        // Found and instrumental returned above.
        Some(Remote::Answer(_)) => LyricsStatus::Pending,
    };
    with(status, None, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProviderId;
    use crate::net::fake::FakeHttp;
    use crate::provider::{
        AudioSource, Provider, ProviderFuture, ProviderHealth, ProviderInfo, ProviderTrack,
    };
    use crate::test_support::TempDir;

    fn lyrics(text: &str) -> Lyrics {
        super::super::lrc::parse(text)
    }

    fn own(text: &str, source: LyricsSource) -> Own {
        Own::Found(ProviderLyrics {
            lyrics: lyrics(text),
            instrumental: false,
            source,
        })
    }

    fn remote(text: &str, fit: LyricsFit) -> Remote {
        Remote::Answer(Answer::Found {
            lyrics: lyrics(text),
            fit,
            delta_ms: Some(-7_000),
            record: 1,
        })
    }

    const SYNCED: &str = "[00:01.00]timed";
    const PLAIN: &str = "words";

    #[test]
    fn the_files_own_lyrics_win_even_unsynced() {
        let gathered = Gathered {
            own: Some(own(PLAIN, LyricsSource::Embedded)),
            lrclib: Some(remote(SYNCED, LyricsFit::Length)),
        };
        let outcome = decide(&gathered);
        assert!(
            matches!(
                outcome.status,
                LyricsStatus::Found {
                    source: LyricsSource::Embedded,
                    synced: false,
                    fit: LyricsFit::Own,
                    ..
                }
            ),
            "{outcome:?}"
        );
        assert!(
            gathered.settled_by_own(),
            "with the file's own lyrics LRCLIB is not asked"
        );
    }

    #[test]
    fn a_providers_unsynced_words_give_way_to_a_synced_version_even_the_nearest() {
        let gathered = Gathered {
            own: Some(own(PLAIN, LyricsSource::Provider)),
            lrclib: Some(remote(SYNCED, LyricsFit::Closest)),
        };
        let outcome = decide(&gathered);
        assert_eq!(
            outcome.status,
            LyricsStatus::Found {
                source: LyricsSource::Lrclib,
                synced: true,
                fit: LyricsFit::Closest,
                delta_ms: Some(-7_000)
            }
        );
        assert!(outcome.lyrics.is_some_and(|l| l.synced));

        // LRCLIB with unsynced words only: the provider's own are taken.
        let gathered = Gathered {
            own: Some(own(PLAIN, LyricsSource::Provider)),
            lrclib: Some(remote(PLAIN, LyricsFit::Length)),
        };
        assert!(matches!(
            decide(&gathered).status,
            LyricsStatus::Found {
                source: LyricsSource::Provider,
                ..
            }
        ));
    }

    #[test]
    fn not_asked_not_found_and_failed_are_three_answers() {
        let none = || Own::None {
            detail: "the file has no .lrc beside it and no lyrics in its tags".to_owned(),
        };
        let not_asked = decide(&Gathered {
            own: Some(none()),
            lrclib: None,
        });
        assert_eq!(not_asked.status, LyricsStatus::NotRequested);

        let not_found = decide(&Gathered {
            own: Some(none()),
            lrclib: Some(Remote::Answer(Answer::None {
                detail: "LRCLIB has no version".to_owned(),
            })),
        });
        let LyricsStatus::NotFound { detail } = not_found.status else {
            panic!("{not_found:?}");
        };
        assert!(
            detail.contains("no .lrc") && detail.contains("LRCLIB has no version"),
            "{detail}"
        );

        let failed = decide(&Gathered {
            own: Some(Own::Failed {
                chain: "STEP: LYRICS_READ\n  → broken".to_owned(),
            }),
            lrclib: None,
        });
        assert!(matches!(failed.status, LyricsStatus::Failed { .. }));
        assert!(failed.notes.is_empty(), "the reason is said once");

        let lrclib_down = decide(&Gathered {
            own: Some(Own::Unsupported),
            lrclib: Some(Remote::Failed {
                chain: "STEP: LYRICS_LOOKUP\n  → busy".to_owned(),
            }),
        });
        assert!(matches!(lrclib_down.status, LyricsStatus::Failed { .. }));

        assert_eq!(
            decide(&Gathered {
                own: Some(Own::GaveWay),
                lrclib: None
            })
            .status,
            LyricsStatus::Pending
        );
    }

    #[test]
    fn a_failure_the_answer_did_not_come_from_is_a_note() {
        let outcome = decide(&Gathered {
            own: Some(Own::Failed {
                chain: "STEP: PROVIDER_CALL\n  → 500".to_owned(),
            }),
            lrclib: Some(remote(SYNCED, LyricsFit::Length)),
        });
        assert!(matches!(outcome.status, LyricsStatus::Found { .. }));
        assert_eq!(outcome.notes.len(), 1, "{outcome:?}");
    }

    /// A provider answering from memory.
    struct Canned {
        id: ProviderId,
        plugin: bool,
        answer: Option<ProviderLyrics>,
    }

    impl Provider for Canned {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: self.id.clone(),
                display_name: "canned".to_owned(),
                capabilities: Capabilities::STREAM | Capabilities::LYRICS,
            }
        }
        fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
            unimplemented!()
        }
        fn search<'a>(&'a self, _: &'a str, _: usize) -> ProviderFuture<'a, Vec<ProviderTrack>> {
            unimplemented!()
        }
        fn resolve_source<'a>(
            &'a self,
            _: &'a ProviderTrackId,
        ) -> ProviderFuture<'a, Option<AudioSource>> {
            unimplemented!()
        }
        fn lyrics<'a>(
            &'a self,
            _: &'a ProviderTrackId,
        ) -> ProviderFuture<'a, Option<ProviderLyrics>> {
            Box::pin(std::future::ready(Ok(self.answer.clone())))
        }
        fn is_plugin(&self) -> bool {
            self.plugin
        }
    }

    const SEARCH: &str = r#"[{"id":5,"trackName":"Song","artistName":"Band","duration":200.0,
        "instrumental":false,"plainLyrics":"x","syncedLyrics":"[00:02.00]from lrclib"}]"#;

    fn chain(dir: &Path, provider: Canned, http: Arc<FakeHttp>) -> Chain {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(provider));
        let cache = Arc::new(Mutex::new(LyricsCache::open(dir).unwrap()));
        Chain::new(
            Links {
                registry,
                lrclib: Ok(Lrclib::for_tests(http, "http://lrclib.test")),
            },
            cache,
        )
    }

    #[test]
    fn a_servers_track_waits_for_the_user_and_a_plugins_does_not() {
        let track = TrackRef::new("Band", "Song");
        for plugin in [false, true] {
            let dir = TempDir::new("lyrics-chain-ask");
            let http = Arc::new(FakeHttp::new().route("/api/search?", SEARCH));
            let id = ProviderTrackId::new(ProviderId::new("remote"), "7");
            let chain = chain(
                dir.path(),
                Canned {
                    id: id.provider.clone(),
                    plugin,
                    answer: None,
                },
                http.clone(),
            );
            let ask = Ask {
                id: &id,
                track: &track,
                duration_ms: Some(200_500),
                file: None,
                requested: false,
            };
            let mut gathered = Gathered::default();
            chain.gather(&ask, &mut gathered);
            let outcome = decide(&gathered);
            if plugin {
                assert!(
                    matches!(
                        outcome.status,
                        LyricsStatus::Found {
                            source: LyricsSource::Lrclib,
                            fit: LyricsFit::Length,
                            ..
                        }
                    ),
                    "a plugin's track goes online at once: {outcome:?}"
                );
            } else {
                assert_eq!(outcome.status, LyricsStatus::NotRequested);
                assert!(
                    http.requests().is_empty(),
                    "LRCLIB must not be asked unasked"
                );
                // The user opens the lyrics: now LRCLIB is asked, and the
                // provider is not asked twice.
                chain.gather(
                    &Ask {
                        requested: true,
                        ..ask
                    },
                    &mut gathered,
                );
                assert!(matches!(
                    decide(&gathered).status,
                    LyricsStatus::Found {
                        source: LyricsSource::Lrclib,
                        ..
                    }
                ));
                assert_eq!(http.requests().len(), 1);
            }
        }
    }

    #[test]
    fn lrclibs_answer_is_kept_and_its_own_synced_lyrics_spare_the_network() {
        let dir = TempDir::new("lyrics-chain-cache");
        let track = TrackRef::new("Band", "Song");
        let id = ProviderTrackId::new(ProviderId::new("remote"), "7");
        let http = Arc::new(FakeHttp::new().route("/api/search?", SEARCH));
        let ask = Ask {
            id: &id,
            track: &track,
            duration_ms: Some(200_000),
            file: None,
            requested: true,
        };
        let first = chain(
            dir.path(),
            Canned {
                id: id.provider.clone(),
                plugin: false,
                answer: None,
            },
            http.clone(),
        );
        let mut gathered = Gathered::default();
        first.gather(&ask, &mut gathered);
        assert_eq!(http.requests().len(), 1);
        // A new chain on the same cache: LRCLIB is not asked again.
        let second = chain(
            dir.path(),
            Canned {
                id: id.provider.clone(),
                plugin: false,
                answer: None,
            },
            http.clone(),
        );
        let mut again = Gathered::default();
        second.gather(&ask, &mut again);
        assert_eq!(http.requests().len(), 1, "the kept answer is read");
        assert_eq!(decide(&again), decide(&gathered));

        // A provider with synced lyrics of its own: LRCLIB is not needed.
        let dir = TempDir::new("lyrics-chain-own");
        let http = Arc::new(FakeHttp::new().route("/api/search?", SEARCH));
        let own_synced = chain(
            dir.path(),
            Canned {
                id: id.provider.clone(),
                plugin: true,
                answer: Some(ProviderLyrics {
                    lyrics: lyrics(SYNCED),
                    instrumental: false,
                    source: LyricsSource::Provider,
                }),
            },
            http.clone(),
        );
        let mut gathered = Gathered::default();
        own_synced.gather(&ask, &mut gathered);
        assert!(http.requests().is_empty());
        assert!(matches!(
            decide(&gathered).status,
            LyricsStatus::Found {
                source: LyricsSource::Provider,
                synced: true,
                ..
            }
        ));
    }
}
