//! Reading the queue ahead (D-077): the tracks likely to play next are made
//! ready before they are asked for — their address resolved and the first
//! bytes of their stream fetched — so that "next" does not wait.
//!
//! ## What was measured before it
//!
//! With YouTube Music, in a release build: pressing "next" waited 5.6–7 s,
//! nearly all of it yt-dlp resolving the address (4.7 s). The audio itself is
//! cheap — the first 256 KB of a stream arrive in 0.26 s. D-024 already read
//! the next track ahead, but inside `tick`, on the thread the shells run
//! commands on: one round held that thread 4.7 s, and pause waited that long.
//!
//! ## What is kept, where, and for how long
//!
//! **Memory only**: no audio is written to disk (the user's decision). An
//! address is trusted for [`ADDRESS_LIFETIME`] — YouTube's live six hours, but
//! SoundCloud's eleven minutes (both measured), and the core does not know
//! which provider signs for how long. An address that went stale anyway is
//! resolved again: a start that cannot open it resolves it itself, and a
//! stream that began from its head asks for a new one while the head plays.
//! A head ([`HEAD_BYTES`]) is kept only while its track is in the plan.
//!
//! ## The plan
//!
//! At most [`PLAN_DEPTH`] tracks, the likeliest first: the next one and the
//! one after it — "next" is by far the likeliest, the user's rule — and a
//! third that statistics pick. They are made ready in that order, one at a
//! time: the address, then the head, then the next track.
//!
//! ## The thread
//!
//! One worker, marked as working ahead of time
//! ([`crate::provider::ahead_of_time`]): a plugin puts every call the user
//! makes before this thread's calls, and stops a tool it runs for them. A
//! start the user asks for never waits behind this thread — except for the
//! very track the thread is resolving at that moment: that one it waits for,
//! rather than resolving it a second time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::diag::{DiagReport, Stage};
use crate::error::{Error, ErrorKind, Result};
use crate::ids::{ProviderId, ProviderTrackId};
use crate::net::StreamHead;
use crate::playback::queue::QueueItem;
use crate::provider::{AudioSource, Capabilities, Provider, ProviderRegistry};

/// How many tracks are kept ready at most (the user's decision): the next,
/// the one after it, and one that statistics pick.
pub const PLAN_DEPTH: usize = 3;

/// How much of a stream is fetched ahead of time. An m4a's header and first
/// fragment are ~163 KB (measured on YouTube's): this is ~30 s of AAC and a
/// few seconds of FLAC — time enough for the rest to be asked for.
pub const HEAD_BYTES: u64 = 512 * 1024;

/// How long a resolved address is trusted. Below SoundCloud's eleven
/// minutes, the shortest measured.
pub const ADDRESS_LIFETIME: Duration = Duration::from_secs(8 * 60);

/// Why a track is in the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReason {
    /// The track a play command starts with (`headshell prefetch`).
    First,
    /// The track "next" plays.
    Next,
    /// The one after it.
    AfterNext,
    /// The playing track, again: repeat one plays it once more when it ends.
    Again,
    /// The third track, picked by how the user listens: the queue's
    /// continuation for someone skipping through it, a track they often play
    /// for someone who picks from it. [`PrefetchItem::why`] says which.
    Likely,
}

/// How the user moves through a queue in this session (D-077): what the
/// third track of the plan is guessed from, with the history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Habits {
    /// Tracks left with "next" before the play rule counted them.
    pub skipped: u32,
    /// Tracks that played to their end, or were left after they counted.
    pub finished: u32,
    /// Tracks picked from the queue.
    pub jumped: u32,
    /// Times the user went back.
    pub went_back: u32,
}

/// A track in the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanItem {
    pub(crate) item: QueueItem,
    /// Its position in play order.
    pub(crate) index: usize,
    pub(crate) reason: PlanReason,
    /// Why a [`PlanReason::Likely`] track was picked.
    pub(crate) why: Option<String>,
}

/// Where a planned track stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PrefetchStatus {
    /// Not reached yet.
    Waiting,
    /// Its address is being resolved.
    Resolving,
    /// Ready: the address, and its head if one could be fetched.
    Ready {
        /// How long resolving the address took (ms).
        resolve_ms: u64,
        /// The head's size, when one was fetched.
        head_bytes: Option<u64>,
        /// The stream's whole length, as the head's server stated it.
        total_bytes: Option<u64>,
        /// Why there is no head, when there is none: "not fetched" says why
        /// (K9).
        no_head: Option<String>,
    },
    /// Resolving failed; the chain says at which stage.
    Failed { chain: String },
}

/// One planned track and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefetchItem {
    /// Its position in play order.
    pub index: usize,
    pub provider: ProviderId,
    pub artist: String,
    pub title: String,
    pub reason: PlanReason,
    /// Why this track was picked, for the one statistics picked.
    #[serde(default)]
    pub why: Option<String>,
    pub status: PrefetchStatus,
}

/// What reading ahead did in a playing session, counted (K9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefetchCounters {
    /// Addresses resolved ahead of time.
    pub resolved: usize,
    /// Heads fetched.
    pub heads: usize,
    /// Starts that found their track ready.
    pub hits: usize,
    /// Starts that found their track being resolved, and waited for it.
    pub waited: usize,
    /// Starts that resolved their track themselves.
    pub misses: usize,
    /// Resolutions that gave way to a call the user made; asked again later.
    pub gave_way: usize,
    /// Addresses that no longer answered and were resolved again.
    pub stale: usize,
    /// Resolutions that failed.
    pub failed: usize,
}

/// The plan, counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefetchSummary {
    pub planned: usize,
    pub ready: usize,
    pub with_head: usize,
    pub failed: usize,
}

impl PrefetchSummary {
    #[must_use]
    pub fn of(items: &[PrefetchItem]) -> Self {
        let mut summary = Self {
            planned: items.len(),
            ..Self::default()
        };
        for item in items {
            match &item.status {
                PrefetchStatus::Ready { head_bytes, .. } => {
                    summary.ready += 1;
                    if head_bytes.is_some() {
                        summary.with_head += 1;
                    }
                }
                PrefetchStatus::Failed { .. } => summary.failed += 1,
                PrefetchStatus::Waiting | PrefetchStatus::Resolving => {}
            }
        }
        summary
    }
}

/// The plan and where each track stands: the playing session's, or what
/// `headshell prefetch` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefetchReport {
    /// `queue` for the playing session; the query for the command.
    pub subject: String,
    pub items: Vec<PrefetchItem>,
    pub summary: PrefetchSummary,
    /// The playing session's counts since it started; zero for the command.
    pub counters: PrefetchCounters,
    /// How the user moved through the queue this session; zero for the
    /// command.
    #[serde(default)]
    pub habits: Habits,
    pub diag: Option<DiagReport>,
}

// ————————————————————————————————————————————————— one track

/// The provider that plays `id`, if it can stream.
///
/// # Errors
/// If no provider of that name is registered, or it cannot stream — "I
/// can't" is not "no results" (K9).
pub(crate) fn streaming_provider(
    registry: &ProviderRegistry,
    id: &ProviderTrackId,
) -> Result<Arc<dyn Provider>> {
    let provider = registry.get(&id.provider).ok_or_else(|| {
        Error::new(
            Stage::PlaybackResolve,
            ErrorKind::NotFound {
                what: format!("provider: {}", id.provider),
            },
        )
    })?;
    let info = provider.info();
    if !info.capabilities.contains(Capabilities::STREAM) {
        return Err(Error::new(
            Stage::PlaybackResolve,
            ErrorKind::Unsupported {
                provider: info.id.to_string(),
                what: "audio stream".to_owned(),
                capabilities: info.capabilities.describe(),
            },
        ));
    }
    Ok(provider)
}

/// A provider answered "no source" for `id`.
pub(crate) fn no_source(id: &ProviderTrackId) -> Error {
    Error::new(
        Stage::PlaybackResolve,
        ErrorKind::NotFound {
            what: format!("a playable source for {id}"),
        },
    )
}

/// Resolves `id`'s address on the calling thread.
pub(crate) fn resolve(registry: &ProviderRegistry, id: &ProviderTrackId) -> Result<AudioSource> {
    let provider = streaming_provider(registry, id)?;
    crate::plugin::block_on(provider.resolve_source(id))?.ok_or_else(|| no_source(id))
}

/// What fetching a head came to.
#[cfg_attr(not(feature = "http-client"), allow(dead_code))]
pub(crate) enum Head {
    Fetched(StreamHead),
    /// There is no head to fetch, and why.
    None(String),
    Failed(Error),
}

pub(crate) fn fetch_head(source: &AudioSource) -> Head {
    match source {
        AudioSource::LocalFile { .. } => {
            Head::None("a local file: there is nothing to fetch ahead".to_owned())
        }
        #[cfg(feature = "http-client")]
        AudioSource::HttpStream { url, headers } => {
            match crate::net::UreqClient::for_streams().fetch_head(url, headers, HEAD_BYTES) {
                Ok(Some(head)) => Head::Fetched(head),
                Ok(None) => Head::None(
                    "the server does not cut ranges: the stream starts from its address".to_owned(),
                ),
                Err(err) => Head::Failed(err),
            }
        }
        #[cfg(not(feature = "http-client"))]
        AudioSource::HttpStream { .. } => {
            Head::None("this build has no HTTP client (`http-client` is off)".to_owned())
        }
    }
}

pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Makes the plan's tracks ready one after another on the calling thread,
/// and says how each went: what the playing session does ahead of time, as
/// a command (`headshell prefetch`, as `artwork` is for covers).
pub(crate) fn prepare_now(registry: &ProviderRegistry, plan: &[PlanItem]) -> Vec<PrefetchItem> {
    plan.iter()
        .map(|planned| {
            let started = Instant::now();
            let status = match resolve(registry, &planned.item.id) {
                Ok(source) => {
                    let resolve_ms = millis(started.elapsed());
                    let (head_bytes, total_bytes, no_head) = match fetch_head(&source) {
                        Head::Fetched(head) => {
                            (Some(head.bytes.len() as u64), Some(head.total), None)
                        }
                        Head::None(why) => (None, None, Some(why)),
                        Head::Failed(err) => (
                            None,
                            None,
                            Some(format!(
                                "the head could not be fetched — {}",
                                err.chain_text().replace('\n', " ")
                            )),
                        ),
                    };
                    PrefetchStatus::Ready {
                        resolve_ms,
                        head_bytes,
                        total_bytes,
                        no_head,
                    }
                }
                Err(err) => PrefetchStatus::Failed {
                    chain: err.chain_text(),
                },
            };
            item_of(planned, status)
        })
        .collect()
}

pub(crate) fn item_of(planned: &PlanItem, status: PrefetchStatus) -> PrefetchItem {
    PrefetchItem {
        index: planned.index,
        provider: planned.item.id.provider.clone(),
        artist: planned.item.track.artist.clone(),
        title: planned.item.track.title.clone(),
        reason: planned.reason,
        why: planned.why.clone(),
        status,
    }
}

/// The third track's guess.
#[cfg(feature = "audio")]
pub(crate) mod likely;
/// The thread that reads the queue ahead: it needs a player, so the audio
/// pipeline.
#[cfg(feature = "audio")]
mod worker;
#[cfg(feature = "audio")]
pub(crate) use worker::{Prefetcher, Prepared};

#[cfg(all(test, feature = "audio"))]
mod tests;
