//! The read-ahead thread and what it keeps (D-077); see the parent module.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::{
    ADDRESS_LIFETIME, Head, PlanItem, PrefetchCounters, PrefetchItem, PrefetchReport,
    PrefetchStatus, PrefetchSummary, fetch_head, item_of, millis, no_source, resolve,
};
use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::ids::ProviderTrackId;
use crate::net::StreamHead;
use crate::playback::queue::QueueItem;
use crate::provider::{AudioSource, ProviderRegistry};

/// How long a start waits for the address being resolved at that moment,
/// before resolving it itself: a plugin call's own budget, and a margin.
const IN_FLIGHT_WAIT: Duration = Duration::from_secs(25);

/// How many addresses are remembered; beyond it the oldest outside the plan
/// go.
const MAX_ADDRESSES: usize = 256;

/// A track made ready: what a start begins from.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    pub(crate) source: AudioSource,
    pub(crate) head: Option<Arc<StreamHead>>,
}

/// Reads the queue ahead on a thread of its own (D-077). Cheap to clone: a
/// playing session and its player share one, and the thread stops when the
/// last clone goes.
#[derive(Clone)]
pub(crate) struct Prefetcher {
    owner: Arc<Owner>,
}

impl std::fmt::Debug for Prefetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prefetcher").finish_non_exhaustive()
    }
}

/// Stops the thread when the last handle goes.
struct Owner {
    shared: Arc<Shared>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.stop = true;
        }
        self.shared.signal.notify_all();
    }
}

struct Shared {
    state: Mutex<State>,
    /// Wakes the thread for work, and a start waiting for a track in flight.
    signal: Condvar,
}

struct State {
    plan: Vec<PlanItem>,
    registry: Option<ProviderRegistry>,
    entries: HashMap<ProviderTrackId, Entry>,
    /// The track whose address is being resolved right now.
    in_flight: Option<ProviderTrackId>,
    /// Tracks whose address could not be resolved — not tried again while
    /// they stay in the plan: the start says why (K9).
    failed: HashMap<ProviderTrackId, String>,
    /// Asked to be resolved again first: a playing stream's address went
    /// stale.
    urgent: VecDeque<QueueItem>,
    counters: PrefetchCounters,
    /// [`ADDRESS_LIFETIME`]; a test shortens it.
    lifetime: Duration,
    started: bool,
    stop: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            plan: Vec::new(),
            registry: None,
            entries: HashMap::new(),
            in_flight: None,
            failed: HashMap::new(),
            urgent: VecDeque::new(),
            counters: PrefetchCounters::default(),
            lifetime: ADDRESS_LIFETIME,
            started: false,
            stop: false,
        }
    }
}

struct Entry {
    source: AudioSource,
    resolved_at: Instant,
    resolve_ms: u64,
    head: HeadState,
}

enum HeadState {
    NotTried,
    Fetched(Arc<StreamHead>),
    /// No head, and why.
    None(String),
}

impl Entry {
    fn fresh(&self, lifetime: Duration) -> bool {
        self.resolved_at.elapsed() < lifetime
    }

    fn prepared(&self) -> Prepared {
        Prepared {
            source: self.source.clone(),
            head: match &self.head {
                HeadState::Fetched(head) => Some(Arc::clone(head)),
                HeadState::NotTried | HeadState::None(_) => None,
            },
        }
    }
}

/// What the thread does next.
enum Work {
    Resolve(QueueItem),
    Head(ProviderTrackId, AudioSource),
}

fn lock(shared: &Shared) -> Option<MutexGuard<'_, State>> {
    shared.state.lock().ok()
}

impl Prefetcher {
    /// A prefetcher with nothing planned. The thread starts with the first
    /// plan.
    pub(crate) fn new() -> Self {
        Self {
            owner: Arc::new(Owner {
                shared: Arc::new(Shared {
                    state: Mutex::new(State::default()),
                    signal: Condvar::new(),
                }),
            }),
        }
    }

    fn shared(&self) -> &Arc<Shared> {
        &self.owner.shared
    }

    /// A prefetcher that trusts an address only for `lifetime`.
    #[cfg(test)]
    pub(crate) fn with_lifetime(lifetime: Duration) -> Self {
        let prefetcher = Self::new();
        if let Some(mut state) = lock(prefetcher.shared()) {
            state.lifetime = lifetime;
        }
        prefetcher
    }

    /// Hands over the plan, the likeliest track first.
    pub(crate) fn plan(&self, plan: Vec<PlanItem>, registry: ProviderRegistry) {
        let Some(mut state) = lock(self.shared()) else {
            tracing::warn!("the read-ahead lock is poisoned; the plan was not handed over");
            return;
        };
        let planned: HashSet<ProviderTrackId> =
            plan.iter().map(|planned| planned.item.id.clone()).collect();
        // A head is kept only while its track is planned; a track planned again
        // gets its head fetched again.
        for (id, entry) in &mut state.entries {
            if !planned.contains(id) && matches!(entry.head, HeadState::Fetched(_)) {
                entry.head = HeadState::NotTried;
            }
        }
        state.failed.retain(|id, _| planned.contains(id));
        state.plan = plan;
        state.registry = Some(registry);
        if !state.started {
            state.started = true;
            let shared = Arc::clone(self.shared());
            if let Err(err) = std::thread::Builder::new()
                .name("headshell-prefetch".to_owned())
                .spawn(move || run(&shared))
            {
                // Without the thread nothing is read ahead: every start resolves
                // its own track, as before D-077. Said, not hidden (K9).
                tracing::warn!(error = %err, "the read-ahead thread could not start");
            }
        }
        drop(state);
        self.shared().signal.notify_all();
    }

    /// A track that is ready — its address, and its head when it has one —
    /// without waiting: what the next track is handed to the engine from.
    pub(crate) fn ready(&self, id: &ProviderTrackId) -> Option<Prepared> {
        let mut state = lock(self.shared())?;
        let entry = live_entry(&mut state, id)?;
        match entry.head {
            HeadState::NotTried => None,
            HeadState::Fetched(_) | HeadState::None(_) => Some(entry.prepared()),
        }
    }

    /// A track the user starts: ready, or being resolved at this moment —
    /// then it waits for it — or not at all, and the caller resolves it.
    pub(crate) fn for_start(&self, id: &ProviderTrackId) -> Option<Prepared> {
        let deadline = Instant::now() + IN_FLIGHT_WAIT;
        let mut state = lock(self.shared())?;
        let mut waited = false;
        loop {
            if let Some(entry) = live_entry(&mut state, id) {
                let prepared = entry.prepared();
                if waited {
                    state.counters.waited += 1;
                } else {
                    state.counters.hits += 1;
                }
                return Some(prepared);
            }
            let coming = state.in_flight.as_ref() == Some(id)
                || state.urgent.iter().any(|item| &item.id == id);
            let left = deadline.checked_duration_since(Instant::now());
            match left {
                Some(left) if coming => {
                    waited = true;
                    state = self.shared().signal.wait_timeout(state, left).ok()?.0;
                }
                _ => {
                    state.counters.misses += 1;
                    return None;
                }
            }
        }
    }

    /// An address the player resolved itself: kept for "previous" and a
    /// replay.
    pub(crate) fn remember(&self, id: &ProviderTrackId, source: &AudioSource) {
        let Some(mut state) = lock(self.shared()) else {
            return;
        };
        insert(
            &mut state,
            id.clone(),
            Entry {
                source: source.clone(),
                resolved_at: Instant::now(),
                resolve_ms: 0,
                head: initial_head(source),
            },
        );
    }

    /// The address kept for `id` no longer opens.
    pub(crate) fn forget_stale(&self, id: &ProviderTrackId) {
        let Some(mut state) = lock(self.shared()) else {
            return;
        };
        if state.entries.remove(id).is_some() {
            state.counters.stale += 1;
        }
    }

    /// A playing stream asks for a new address: resolve it again, before
    /// anything else.
    pub(crate) fn refresh(&self, item: &QueueItem) {
        let Some(mut state) = lock(self.shared()) else {
            return;
        };
        if state.entries.remove(&item.id).is_some() {
            state.counters.stale += 1;
        }
        state.failed.remove(&item.id);
        if !state.urgent.iter().any(|queued| queued.id == item.id) {
            state.urgent.push_back(item.clone());
        }
        drop(state);
        self.shared().signal.notify_all();
    }

    /// The address resolved for `id` after `since`, if one is.
    pub(crate) fn resolved_since(
        &self,
        id: &ProviderTrackId,
        since: Instant,
    ) -> Option<AudioSource> {
        let state = lock(self.shared())?;
        let entry = state.entries.get(id)?;
        (entry.resolved_at >= since).then(|| entry.source.clone())
    }

    /// The plan as it stands, with the session's counts.
    ///
    /// # Errors
    /// If the lock is poisoned: an empty plan would say "nothing planned".
    pub(crate) fn report(&self) -> Result<PrefetchReport> {
        let state = lock(self.shared()).ok_or_else(|| {
            Error::new(
                Stage::PlaybackResolve,
                ErrorKind::Audio {
                    detail: "the read-ahead lock is poisoned (an earlier call panicked)".to_owned(),
                },
            )
        })?;
        let items: Vec<PrefetchItem> = state
            .plan
            .iter()
            .map(|planned| item_of(planned, status_of(&state, &planned.item.id)))
            .collect();
        Ok(PrefetchReport {
            subject: "queue".to_owned(),
            summary: PrefetchSummary::of(&items),
            items,
            counters: state.counters,
            habits: super::Habits::default(),
            diag: None,
        })
    }
}

/// The entry for `id` if it is still to be trusted; a stale one is dropped.
fn live_entry<'a>(state: &'a mut State, id: &ProviderTrackId) -> Option<&'a Entry> {
    let lifetime = state.lifetime;
    if state
        .entries
        .get(id)
        .is_some_and(|entry| !entry.fresh(lifetime))
    {
        state.entries.remove(id);
    }
    state.entries.get(id)
}

fn initial_head(source: &AudioSource) -> HeadState {
    match source {
        AudioSource::HttpStream { .. } => HeadState::NotTried,
        AudioSource::LocalFile { .. } => {
            HeadState::None("a local file: there is nothing to fetch ahead".to_owned())
        }
    }
}

fn insert(state: &mut State, id: ProviderTrackId, entry: Entry) {
    state.entries.insert(id, entry);
    if state.entries.len() <= MAX_ADDRESSES {
        return;
    }
    let planned: HashSet<&ProviderTrackId> = state.plan.iter().map(|p| &p.item.id).collect();
    let oldest = state
        .entries
        .iter()
        .filter(|(id, _)| !planned.contains(id))
        .min_by_key(|(_, entry)| entry.resolved_at)
        .map(|(id, _)| id.clone());
    if let Some(oldest) = oldest {
        state.entries.remove(&oldest);
    }
}

fn status_of(state: &State, id: &ProviderTrackId) -> PrefetchStatus {
    if state.in_flight.as_ref() == Some(id) {
        return PrefetchStatus::Resolving;
    }
    if let Some(chain) = state.failed.get(id) {
        return PrefetchStatus::Failed {
            chain: chain.clone(),
        };
    }
    match state.entries.get(id) {
        Some(entry) if entry.fresh(state.lifetime) => {
            let (head_bytes, total_bytes, no_head) = match &entry.head {
                HeadState::Fetched(head) => (Some(head.bytes.len() as u64), Some(head.total), None),
                HeadState::NotTried => (None, None, None),
                HeadState::None(why) => (None, None, Some(why.clone())),
            };
            PrefetchStatus::Ready {
                resolve_ms: entry.resolve_ms,
                head_bytes,
                total_bytes,
                no_head,
            }
        }
        _ => PrefetchStatus::Waiting,
    }
}

/// The next thing to do: a stale address first, then the plan in order —
/// each track's address, then its head.
fn next_work(state: &mut State) -> Option<Work> {
    if let Some(item) = state.urgent.pop_front() {
        state.entries.remove(&item.id);
        state.in_flight = Some(item.id.clone());
        return Some(Work::Resolve(item));
    }
    let mut todo = None;
    for planned in &state.plan {
        let id = &planned.item.id;
        if state.failed.contains_key(id) {
            continue;
        }
        match state.entries.get(id) {
            Some(entry) if entry.fresh(state.lifetime) => {
                if matches!(entry.head, HeadState::NotTried) {
                    todo = Some(Work::Head(id.clone(), entry.source.clone()));
                    break;
                }
            }
            _ => {
                todo = Some(Work::Resolve(planned.item.clone()));
                break;
            }
        }
    }
    if let Some(Work::Resolve(item)) = &todo {
        state.in_flight = Some(item.id.clone());
    }
    todo
}

fn run(shared: &Shared) {
    crate::provider::ahead_of_time::mark_this_thread();
    loop {
        let (work, registry) = {
            let Some(mut state) = lock(shared) else {
                tracing::warn!("the read-ahead lock is poisoned; the thread stops");
                return;
            };
            loop {
                if state.stop {
                    return;
                }
                if let Some(work) = next_work(&mut state) {
                    break (work, state.registry.clone());
                }
                state = match shared.signal.wait(state) {
                    Ok(state) => state,
                    Err(_) => return,
                };
            }
        };

        match work {
            Work::Resolve(item) => {
                let started = Instant::now();
                let outcome = match &registry {
                    Some(registry) => resolve(registry, &item.id),
                    None => Err(no_source(&item.id)),
                };
                let resolve_ms = millis(started.elapsed());
                let Some(mut state) = lock(shared) else {
                    return;
                };
                state.in_flight = None;
                match outcome {
                    Ok(source) => {
                        state.counters.resolved += 1;
                        let head = initial_head(&source);
                        insert(
                            &mut state,
                            item.id,
                            Entry {
                                source,
                                resolved_at: Instant::now(),
                                resolve_ms,
                                head,
                            },
                        );
                    }
                    // It gave way to the user: asked again on the next turn.
                    Err(err) if matches!(err.kind(), ErrorKind::PluginPreempted { .. }) => {
                        state.counters.gave_way += 1;
                    }
                    Err(err) => {
                        let chain = err.chain_text();
                        tracing::info!(
                            track = item.track.display_name(),
                            error = %chain.replace('\n', " "),
                            "reading ahead: the address could not be resolved"
                        );
                        state.counters.failed += 1;
                        state.failed.insert(item.id, chain);
                    }
                }
            }
            Work::Head(id, source) => {
                let head = fetch_head(&source);
                let Some(mut state) = lock(shared) else {
                    return;
                };
                let counted = matches!(head, Head::Fetched(_));
                if let Some(entry) = state.entries.get_mut(&id)
                    && entry.source == source
                {
                    entry.head = match head {
                        Head::Fetched(head) => HeadState::Fetched(Arc::new(head)),
                        Head::None(why) => HeadState::None(why),
                        // The address stays: a start can still open it the
                        // usual way, and says so if it cannot.
                        Head::Failed(err) => HeadState::None(format!(
                            "the head could not be fetched — {}",
                            err.chain_text().replace('\n', " ")
                        )),
                    };
                    if counted {
                        state.counters.heads += 1;
                    }
                }
            }
        }
        shared.signal.notify_all();
    }
}
