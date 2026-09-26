//! A latency probe run by hand: how long does the user wait — from "play" to
//! sound, from "next" to sound — and how long does one round of `tick` hold
//! the thread that runs it (D-077)?
//!
//! It drives a `LiveSession` the way the desktop's core thread does: a round
//! every 200 ms, commands in between. What it prints is what the user feels:
//! a round that takes seconds is seconds in which pause and next wait in
//! line. `--breakdown` splits one start into its parts instead: the
//! provider's answer, the audio device, the stream and the first sound.
//!
//! Does not run in CI: it needs an audio device, the network and a provider
//! that plays the query — a plugin installed and approved in the data
//! directory. Usage:
//!
//! ```bash
//! HEADSHELL_DATA_DIR=… cargo run --release -p headshell-core \
//!   --features audio,http-client,plugin-engine --example latency_probe -- "let it happen"
//! ```

use std::time::{Duration, Instant};

use headshell_core::config::Config;
use headshell_core::playback::{AudioEngine, LiveSession, PlayState, Player, QueueItem};
use headshell_core::provider::ProviderRegistry;
use headshell_core::session::Session;

/// The desktop's round interval.
const TICK: Duration = Duration::from_millis(200);

/// How long the probe listens between two commands: long enough for the
/// read-ahead to have happened, short enough not to reach a track's end.
const LISTEN: Duration = Duration::from_secs(15);

/// How long a start may take before the probe gives up on it.
const START_LIMIT: Duration = Duration::from_secs(40);

/// The queue position `jump` goes to: far enough not to have been read ahead.
const FAR: usize = 10;

fn main() {
    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("could not set up a runtime: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = runtime.block_on(run()) {
        eprintln!("{}", err.chain_text());
        std::process::exit(1);
    }
}

/// The longest round seen, and how many rounds there were.
#[derive(Default)]
struct Rounds {
    count: usize,
    longest: Duration,
}

impl Rounds {
    async fn tick(&mut self, live: &mut LiveSession) {
        let started = Instant::now();
        if let Err(err) = live.tick().await {
            println!("  tick error: {}", err.chain_text().replace('\n', " "));
        }
        let took = started.elapsed();
        self.count += 1;
        self.longest = self.longest.max(took);
        std::thread::sleep(TICK.saturating_sub(took));
    }

    async fn listen(&mut self, live: &mut LiveSession, how_long: Duration) {
        let started = Instant::now();
        while started.elapsed() < how_long {
            self.tick(live).await;
        }
    }
}

/// Waits until the audio really plays; returns the time since `since`.
///
/// It looks at the engine without running rounds: the audio plays on its own
/// threads, and a round that holds the thread would hide the moment the
/// sound started.
fn until_playing(state: impl Fn() -> PlayState, since: Instant) -> Option<Duration> {
    while since.elapsed() < START_LIMIT {
        if state() == PlayState::Playing {
            return Some(since.elapsed());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

fn now_playing(live: &LiveSession) -> String {
    live.player()
        .current_track()
        .map_or_else(|| "—".to_owned(), |track| track.display_name())
}

fn secs(duration: Duration) -> String {
    format!("{:6.2} s", duration.as_secs_f64())
}

fn show_start(label: &str, call: Duration, sound: Option<Duration>, live: &LiveSession) {
    let sound = sound.map_or_else(|| "no sound".to_owned(), secs);
    println!(
        "{label:<14} call {} · sound {sound} · {}",
        secs(call),
        now_playing(live)
    );
}

async fn run() -> headshell_core::Result<()> {
    let query = std::env::args()
        .skip(1)
        .find(|arg| !arg.starts_with("--"))
        .unwrap_or_else(|| "let it happen".to_owned());
    let session = Session::open(Config::discover()?)?;
    let registry = headshell_core::provider::default_registry(session.config())?;

    // — Enter: the search, then the first start.
    let pressed = Instant::now();
    let items = session
        .queue_from_search(&registry, &query, true, 100)
        .await?;
    let searched = pressed.elapsed();
    println!("query          {query:?} · {} tracks", items.len());
    println!("search         {}", secs(searched));

    if std::env::args().any(|arg| arg == "--breakdown") {
        return breakdown(&registry, &items).await;
    }

    let mut player = Player::new(registry);
    let started = Instant::now();
    player.play_items(items).await?;
    let call = pressed.elapsed();
    let mut live = LiveSession::new(session, player);
    let sound = until_playing(|| live.player().state(), pressed);
    println!("first start    resolve+open {}", secs(started.elapsed()));
    show_start("enter", call, sound, &live);

    let mut rounds = Rounds::default();
    for label in ["next", "next again", "previous", "jump"] {
        rounds.listen(&mut live, LISTEN).await;
        let pressed = Instant::now();
        let outcome = match label {
            "previous" => live.player_mut().previous().await,
            "jump" => live.player_mut().jump_to(FAR).await,
            _ => live.player_mut().next().await,
        };
        let call = pressed.elapsed();
        if let Err(err) = outcome {
            println!(
                "{label:<14} failed: {}",
                err.chain_text().replace('\n', " ")
            );
            continue;
        }
        let sound = until_playing(|| live.player().state(), pressed);
        show_start(label, call, sound, &live);
    }

    // Skipping through the queue: three "next"s half a second apart. With
    // three tracks read ahead, none of them waits for an address.
    rounds.listen(&mut live, LISTEN).await;
    for step in 1..=3 {
        let pressed = Instant::now();
        let outcome = live.player_mut().next().await;
        let call = pressed.elapsed();
        if let Err(err) = outcome {
            println!(
                "skip {step:<9} failed: {}",
                err.chain_text().replace('\n', " ")
            );
            break;
        }
        let sound = until_playing(|| live.player().state(), pressed);
        show_start(&format!("skip {step}"), call, sound, &live);
        rounds.listen(&mut live, Duration::from_millis(500)).await;
    }

    rounds.listen(&mut live, Duration::from_secs(3)).await;
    println!(
        "rounds         {} · longest {}",
        rounds.count,
        secs(rounds.longest)
    );
    if let Ok(report) = live.prefetch_report() {
        let c = report.counters;
        println!(
            "read ahead     resolved {} · heads {} · hits {} · waited {} · misses {} · gave way {} · stale {} · failed {}",
            c.resolved, c.heads, c.hits, c.waited, c.misses, c.gave_way, c.stale, c.failed
        );
        for item in &report.items {
            println!(
                "  plan         {:?} {} — {}{}",
                item.reason,
                item.artist,
                item.title,
                item.why
                    .as_deref()
                    .map_or_else(String::new, |why| format!(" ({why})"))
            );
        }
    }
    live.player_mut().stop();
    Ok(())
}

/// One start taken apart: the provider's answer (cold, then warm), the audio
/// device, opening the stream, the first sound.
async fn breakdown(registry: &ProviderRegistry, items: &[QueueItem]) -> headshell_core::Result<()> {
    let mut sources = Vec::new();
    for (label, item) in ["resolve cold", "resolve warm"].iter().zip(items) {
        let Some(provider) = registry.get(&item.id.provider) else {
            println!("no provider for {}", item.id);
            return Ok(());
        };
        let started = Instant::now();
        let source = provider.resolve_source(&item.id).await?;
        println!(
            "{label:<14} {} · {}",
            secs(started.elapsed()),
            item.track.display_name()
        );
        sources.extend(source);
    }
    let Some(source) = sources.last() else {
        println!("the provider gave no source");
        return Ok(());
    };

    let started = Instant::now();
    let engine = AudioEngine::open()?;
    println!("device open    {}", secs(started.elapsed()));

    let started = Instant::now();
    engine.play_source(source)?;
    println!("stream open    {}", secs(started.elapsed()));
    let sound = until_playing(|| engine.state(), started);
    println!(
        "first sound    {}",
        sound.map_or_else(|| "no sound".to_owned(), secs)
    );
    Ok(())
}
