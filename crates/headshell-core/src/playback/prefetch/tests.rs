//! The read-ahead thread against a provider that answers from memory: which
//! track is made ready when, what a start finds, and what it does not do
//! twice (D-077).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::model::TrackRef;
use crate::provider::{ProviderFuture, ProviderHealth, ProviderInfo, ProviderTrack};

/// Resolves every id to a file named after it, after `delay`; `bad` fails,
/// and `shy` gives way the first time it is asked, like a plugin call a
/// user's call stopped.
struct Fake {
    delay: Duration,
    asked: Mutex<Vec<String>>,
    shy_once: AtomicUsize,
    stream_base: Option<String>,
}

impl Fake {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            delay,
            asked: Mutex::new(Vec::new()),
            shy_once: AtomicUsize::new(0),
            stream_base: None,
        })
    }

    #[cfg(feature = "http-client")]
    fn streaming_from(base: String) -> Arc<Self> {
        Arc::new(Self {
            delay: Duration::ZERO,
            asked: Mutex::new(Vec::new()),
            shy_once: AtomicUsize::new(0),
            stream_base: Some(base),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl Provider for Fake {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ProviderId::new("fake"),
            display_name: "Fake".to_owned(),
            capabilities: Capabilities::STREAM,
        }
    }
    fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
        Box::pin(async move {
            Ok(ProviderHealth {
                id: ProviderId::new("fake"),
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
        Box::pin(async move { Ok(Vec::new()) })
    }
    fn resolve_source<'a>(
        &'a self,
        id: &'a ProviderTrackId,
    ) -> ProviderFuture<'a, Option<AudioSource>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(id.id.clone());
            std::thread::sleep(self.delay);
            if id.id == "bad" {
                return Err(Error::new(
                    Stage::ProviderCall,
                    ErrorKind::NotFound {
                        what: "the track the fake does not have".to_owned(),
                    },
                ));
            }
            if id.id == "shy" && self.shy_once.fetch_add(1, Ordering::Relaxed) == 0 {
                return Err(Error::new(
                    Stage::ProviderCall,
                    ErrorKind::PluginPreempted {
                        plugin: "fake".to_owned(),
                        method: "resolve_source".to_owned(),
                    },
                ));
            }
            Ok(Some(match &self.stream_base {
                Some(base) => AudioSource::HttpStream {
                    url: format!("{base}/{}", id.id),
                    headers: Vec::new(),
                },
                None => AudioSource::LocalFile {
                    path: std::path::PathBuf::from(format!("{}.flac", id.id)),
                },
            }))
        })
    }
}

fn registry_of(provider: Arc<Fake>) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.register(provider);
    registry
}

fn planned(ids: &[&str]) -> Vec<PlanItem> {
    ids.iter()
        .enumerate()
        .map(|(step, id)| PlanItem {
            item: item(id),
            index: step + 1,
            reason: if step == 0 {
                PlanReason::Next
            } else {
                PlanReason::AfterNext
            },
            why: None,
        })
        .collect()
}

fn item(id: &str) -> QueueItem {
    QueueItem {
        id: track(id),
        track: TrackRef::new("Artist", id),
    }
}

fn track(id: &str) -> ProviderTrackId {
    ProviderTrackId::new(ProviderId::new("fake"), id)
}

/// Waits until `done` holds for the report, or 5 s pass.
fn until(prefetcher: &Prefetcher, done: impl Fn(&PrefetchReport) -> bool) -> PrefetchReport {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let report = prefetcher.report().unwrap();
        if done(&report) || Instant::now() > deadline {
            return report;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn all_settled(report: &PrefetchReport) -> bool {
    report.items.iter().all(|item| {
        matches!(
            item.status,
            PrefetchStatus::Ready { .. } | PrefetchStatus::Failed { .. }
        )
    })
}

#[test]
fn the_plan_is_made_ready_in_its_order() {
    let fake = Fake::new(Duration::ZERO);
    let prefetcher = Prefetcher::new();
    prefetcher.plan(planned(&["a", "b"]), registry_of(Arc::clone(&fake)));

    let report = until(&prefetcher, all_settled);
    assert_eq!(fake.asked(), ["a", "b"], "the likeliest first");
    assert_eq!(report.summary.ready, 2);
    let PrefetchStatus::Ready { no_head, .. } = &report.items[0].status else {
        panic!("{:?}", report.items[0].status)
    };
    assert!(
        no_head
            .as_deref()
            .is_some_and(|why| why.contains("local file")),
        "why there is no head is said: {no_head:?}"
    );
    assert_eq!(report.counters.resolved, 2);
}

#[test]
fn a_start_takes_a_ready_track_without_resolving_it_again() {
    let fake = Fake::new(Duration::ZERO);
    let prefetcher = Prefetcher::new();
    prefetcher.plan(planned(&["a"]), registry_of(Arc::clone(&fake)));
    until(&prefetcher, all_settled);

    let prepared = prefetcher.for_start(&track("a")).expect("ready");
    assert_eq!(
        prepared.source,
        AudioSource::LocalFile {
            path: "a.flac".into()
        }
    );
    assert_eq!(fake.asked(), ["a"], "resolved once");
    assert_eq!(prefetcher.report().unwrap().counters.hits, 1);
}

/// The user presses "next" while the thread resolves that very track: the
/// start waits for it instead of asking the provider a second time.
#[test]
fn a_start_waits_for_the_track_in_flight_instead_of_resolving_it_twice() {
    let fake = Fake::new(Duration::from_millis(300));
    let prefetcher = Prefetcher::new();
    prefetcher.plan(planned(&["a"]), registry_of(Arc::clone(&fake)));
    until(&prefetcher, |report| {
        matches!(report.items[0].status, PrefetchStatus::Resolving)
    });

    let started = Instant::now();
    let prepared = prefetcher.for_start(&track("a"));
    assert!(prepared.is_some(), "the start got the track in flight");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(fake.asked(), ["a"], "resolved once");
    let counters = prefetcher.report().unwrap().counters;
    assert_eq!((counters.waited, counters.hits, counters.misses), (1, 0, 0));
}

#[test]
fn a_start_for_a_track_nobody_planned_is_a_miss_at_once() {
    let prefetcher = Prefetcher::new();
    prefetcher.plan(planned(&["a"]), registry_of(Fake::new(Duration::ZERO)));
    let started = Instant::now();
    assert!(prefetcher.for_start(&track("z")).is_none());
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "no wait for a track not coming"
    );
    assert_eq!(prefetcher.report().unwrap().counters.misses, 1);
}

/// A track that cannot be resolved is not asked again and again while it
/// stays planned; the one after it is not held up; the report says why.
#[test]
fn a_failing_track_is_asked_once_says_why_and_holds_up_nothing() {
    let fake = Fake::new(Duration::ZERO);
    let prefetcher = Prefetcher::new();
    prefetcher.plan(planned(&["bad", "b"]), registry_of(Arc::clone(&fake)));

    let report = until(&prefetcher, all_settled);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(fake.asked(), ["bad", "b"]);
    let PrefetchStatus::Failed { chain } = &report.items[0].status else {
        panic!("{:?}", report.items[0].status)
    };
    assert!(chain.starts_with("STEP: PROVIDER_CALL"), "{chain}");
    assert!(matches!(
        report.items[1].status,
        PrefetchStatus::Ready { .. }
    ));
    assert_eq!(report.summary.failed, 1);
}

/// A resolution that gave way to the user is not a failure: it is asked
/// again, and the track becomes ready.
#[test]
fn a_resolution_that_gave_way_is_asked_again() {
    let fake = Fake::new(Duration::ZERO);
    let prefetcher = Prefetcher::new();
    prefetcher.plan(planned(&["shy"]), registry_of(Arc::clone(&fake)));

    let report = until(&prefetcher, all_settled);
    assert!(
        matches!(report.items[0].status, PrefetchStatus::Ready { .. }),
        "{report:?}"
    );
    assert_eq!(fake.asked(), ["shy", "shy"]);
    assert_eq!(report.counters.gave_way, 1);
    assert_eq!(report.counters.failed, 0);
}

/// An address past its lifetime is not handed out: the thread resolves it
/// again while the track is planned.
#[test]
fn an_address_past_its_lifetime_is_resolved_again() {
    let fake = Fake::new(Duration::ZERO);
    let prefetcher = Prefetcher::with_lifetime(Duration::from_millis(100));
    let registry = registry_of(Arc::clone(&fake));
    prefetcher.plan(planned(&["a"]), registry.clone());
    until(&prefetcher, all_settled);
    std::thread::sleep(Duration::from_millis(150));

    assert!(
        prefetcher.ready(&track("a")).is_none(),
        "a stale address is not handed out"
    );
    // The next plan (the thread is woken) resolves it again.
    prefetcher.plan(planned(&["a"]), registry);
    until(&prefetcher, |_| fake.asked().len() >= 2);
    assert_eq!(fake.asked(), ["a", "a"]);
}

/// A playing stream whose address went stale asks for a new one: the thread
/// resolves that track before anything else, and says since when.
#[test]
fn a_stale_stream_is_resolved_again_first() {
    let fake = Fake::new(Duration::ZERO);
    let prefetcher = Prefetcher::new();
    prefetcher.plan(planned(&["a", "b"]), registry_of(Arc::clone(&fake)));
    until(&prefetcher, all_settled);

    let since = Instant::now();
    prefetcher.refresh(&item("a"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while prefetcher.resolved_since(&track("a"), since).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(prefetcher.resolved_since(&track("a"), since).is_some());
    assert_eq!(fake.asked(), ["a", "b", "a"]);
    assert_eq!(prefetcher.report().unwrap().counters.stale, 1);
}

/// What the player resolved itself is kept: "previous" finds it.
#[test]
fn an_address_the_player_resolved_is_kept_for_previous() {
    let prefetcher = Prefetcher::new();
    let source = AudioSource::LocalFile {
        path: "played.flac".into(),
    };
    prefetcher.remember(&track("played"), &source);
    assert_eq!(
        prefetcher.for_start(&track("played")).map(|p| p.source),
        Some(source)
    );
}

/// An HTTP stream gets its head; the head goes when its track leaves the
/// plan, and the address stays.
#[cfg(feature = "http-client")]
#[test]
fn a_stream_gets_its_head_while_planned_and_keeps_its_address_after() {
    use crate::test_support::{Answer, LocalServer};

    let file: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    let served = file.clone();
    let server = LocalServer::start(move |req| Answer::file(&served, req.range.as_deref()));
    let fake = Fake::streaming_from(server.url(""));
    let registry = registry_of(Arc::clone(&fake));
    let prefetcher = Prefetcher::new();
    prefetcher.plan(planned(&["a"]), registry.clone());

    let report = until(&prefetcher, |report| {
        matches!(
            report.items[0].status,
            PrefetchStatus::Ready {
                head_bytes: Some(_),
                ..
            }
        )
    });
    let PrefetchStatus::Ready {
        head_bytes,
        total_bytes,
        ..
    } = report.items[0].status.clone()
    else {
        panic!("{:?}", report.items[0].status)
    };
    // A file smaller than a head is its own head.
    assert_eq!((head_bytes, total_bytes), (Some(20_000), Some(20_000)));
    let ready = prefetcher.ready(&track("a")).expect("ready with its head");
    assert_eq!(ready.head.map(|head| head.bytes.clone()), Some(file));

    prefetcher.plan(planned(&["b"]), registry);
    until(&prefetcher, all_settled);
    let kept = prefetcher
        .for_start(&track("a"))
        .expect("the address stays");
    assert!(kept.head.is_none(), "the head went with the plan");
}

/// The command form: each planned track is made ready in turn, and each
/// says how it went.
#[test]
fn preparing_now_reports_each_track() {
    let fake = Fake::new(Duration::ZERO);
    let items = prepare_now(&registry_of(Arc::clone(&fake)), &planned(&["a", "bad"]));
    assert!(
        matches!(items[0].status, PrefetchStatus::Ready { .. }),
        "{items:?}"
    );
    assert!(
        matches!(items[1].status, PrefetchStatus::Failed { .. }),
        "{items:?}"
    );
    assert_eq!(PrefetchSummary::of(&items).failed, 1);
}
