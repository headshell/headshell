//! Playback (Phase 1).
//!
//! ## Layers
//!
//! - [`anchor`] — the single representation of state: [`PlaybackAnchor`]
//!   (D-015). The consumer computes the position from the anchor itself; the
//!   core does not shower it with notifications.
//! - [`queue`] — the queue, repeat, shuffle. A pure data structure.
//! - `engine` — symphonia (decoding) + cpal (output). **Behind the `audio`
//!   feature** (D-016): with it off the queue and the anchor still compile,
//!   only the real audio output drops out. Server and mobile builds should
//!   not be tied to ALSA.
//!
//! ## Why an anchor
//!
//! Phase 4's room sync primitive is exactly [`PlaybackAnchor`]. Today it is
//! written for local playback, tomorrow it will be broadcast over the network
//! — it is the same type from the start so we do not keep two separate state
//! models.

pub mod anchor;
#[cfg(feature = "audio")]
pub mod engine;
/// A progressive reader connecting a remote stream to symphonia (§1.3).
/// Compiled when both the audio pipeline and the HTTP client are on.
#[cfg(all(feature = "audio", feature = "http-client"))]
pub mod http_source;
pub mod live;
pub mod player;
/// Reading the queue ahead (D-077).
pub mod prefetch;
pub mod queue;

pub use anchor::{PlayState, PlaybackAnchor};
#[cfg(feature = "audio")]
pub use engine::AudioEngine;
pub use live::{LiveSession, TickReport};
pub use player::Player;
pub use prefetch::{
    Habits, PlanReason, PrefetchCounters, PrefetchItem, PrefetchReport, PrefetchStatus,
    PrefetchSummary,
};
pub use queue::{Queue, QueueItem, QueueView, RepeatMode};
