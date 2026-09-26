//! The YouTube Music plugin, **against the real YouTube Music** (Phase 2
//! §2.5, D-048).
//!
//! The sibling of `plugin_soundcloud.rs`, and it follows the same procedure:
//! the plugin is installed into the data directory from the live catalog
//! (`headshell/plugins`, D-071) and run against the live service. What is
//! tested is not the protocol (`plugin_script.rs` tests that with a
//! fixed-catalog fixture) but **the plugin itself**: the InnerTube search,
//! the address yt-dlp resolves, and the audio really playing.
//!
//! **These tests are part of the default run** (D-043) and keep two failures
//! apart (K9):
//!
//! - **Not reaching it** is not a failure: without a network the test skips
//!   itself and writes the reason to `stderr`.
//! - **Reaching it and getting the unexpected** fails.
//!
//! Neither `python3` nor `yt-dlp` is a prerequisite: the plugin runs in
//! embedded QuickJS, and the engine downloads yt-dlp's self-contained binary
//! **for this platform** from the version pinned in the manifest (D-069).
//! Failing to download while the network is up **is not a reason to skip but
//! a reason to fail** — if the declared address is dead (orphaned), passing
//! over it silently would show a broken manifest as green.
//!
//! When it turns red, the first question: **should I look at the last commit,
//! or run `yt-dlp -J "https://music.youtube.com/watch?v=..."`?** If the second
//! works and the tests are still red, the fault is ours. If yt-dlp is out of
//! date, the fault is ours too: the manifest pins the version, not the user.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::path::PathBuf;

use headshell_core::config::Config;
use headshell_core::ids::{ProviderId, ProviderTrackId};
use headshell_core::plugin::PluginProvider;
use headshell_core::plugin::artifact::{ArtifactStore, default_artifact_source};
use headshell_core::plugin::manifest::{PluginManifest, Requirement};
use headshell_core::provider::{AudioSource, Capabilities, Provider};
use headshell_core::secrets::{Secrets, plugin_namespace};

/// yt-dlp's binary for this platform: downloaded at most once per run.
///
/// The plugin does not look for yt-dlp itself (D-055); the engine installs it.
/// The test goes **the same way**. The download is ~40 MB, so the result is
/// kept in the project's `target/tmp` (D-070) — but not blindly:
///
/// - The hash of the cached file is verified again on every run.
/// - Whether the download address is **still alive** is asked on every run
///   (without reading the body). If the cache skipped this, when the address
///   died (orphaned, D-055) this machine would stay green and a clean machine
///   would turn red — the result would depend on the machine.
fn shared_ytdlp(requirement: &Requirement) -> Result<PathBuf, String> {
    static SHARED: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| fetch_ytdlp(requirement)).clone()
}

fn fetch_ytdlp(requirement: &Requirement) -> Result<PathBuf, String> {
    let cache = support::root().join("ytdlp-cache");
    let store = ArtifactStore::new(&Config::with_data_dir(&cache));
    let platform = store.platform().to_owned();
    let Some(asset) = requirement.asset_for(&platform) else {
        return Err(format!("no release declared for {platform}"));
    };
    let source = default_artifact_source().map_err(|err| err.chain_text())?;

    // Is the address alive: the status code only, without reading the body.
    match source.open(&asset.url) {
        Ok(response) if response.status == 404 || response.status == 410 => {
            return Err(format!(
                "ORPHANED — {} said {}; the cached copy will not hide this",
                asset.url, response.status
            ));
        }
        Ok(_) => {}
        // If it could not be reached there is no verdict: carry on with the
        // cache if there is one; otherwise the install below will say what
        // happened.
        Err(err) => eprintln!(
            "warning: could not probe {} ({}), carrying on with the cache",
            asset.url,
            err.chain_text().replace('\n', " ")
        ),
    }

    // If the hash matches `install` does not go online at all; if not, it
    // downloads again.
    match store.install(source.as_ref(), requirement) {
        Ok(outcome) if outcome.is_ready() => Ok(store.artifact_path(requirement)),
        Ok(outcome) => Err(outcome.describe()),
        Err(err) => Err(err.chain_text()),
    }
}

/// Can YouTube Music be reached over TCP.
///
/// Only DNS + connect; it never goes into HTTP. The goal is answering "is
/// there a network", not measuring the service's health.
fn ytmusic_reachable() -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = ("music.youtube.com", 443).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| {
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5)).is_ok()
    })
}

/// Says whether the test can run; if it cannot, writes the reason.
fn prerequisites_met(test: &str) -> bool {
    // The engine downloads the artifact over HTTP; if this build has no
    // client the test cannot run. "I could not run" and "I ran and failed"
    // are separate diagnoses (K9) — and a skipped test does not count as
    // passed (D-043).
    if let Err(err) = default_artifact_source() {
        eprintln!(
            "{test}: {} — skipped (this build cannot download yt-dlp)",
            err.chain_text().replace('\n', " ")
        );
        return false;
    }
    if !ytmusic_reachable() {
        eprintln!("{test}: could not reach music.youtube.com:443 — skipped (taken as no network)");
        return false;
    }
    true
}

fn temp_config(name: &str) -> support::TestConfig {
    support::TestConfig::new(&format!("ytmusic-{name}"))
}

/// Writes the YouTube cookies from the CI environment variable into the
/// secret store.
///
/// YouTube puts up a bot wall for data centre addresses ("Sign in to confirm
/// you're not a bot"), and without cookies no stream can be resolved. The
/// cookies go **through the secret store**, not through a bare environment
/// variable: so the same route the user takes with `headshell secret set
/// plugin:ytmusic cookies` is tested (D-042).
///
/// Returns: whether cookies were given. If not, the bot wall is not something
/// that can be measured and the test skips itself.
fn store_cookies(config: &Config) -> bool {
    let Ok(raw) = std::env::var("HEADSHELL_TEST_YTMUSIC_COOKIES") else {
        return false;
    };
    if raw.trim().is_empty() {
        return false;
    }
    let mut secrets = Secrets::load(&config.secrets_path()).unwrap();
    secrets.set(&plugin_namespace("ytmusic"), "cookies", &raw);
    secrets.save(&config.secrets_path()).unwrap();
    true
}

/// Installs the plugin from the live catalog — the same thing the user does
/// (D-071). `None` if the catalog cannot be reached: the test is skipped and
/// the reason is written.
async fn install(config: &Config, test: &str) -> Option<PluginProvider> {
    let dir = match support::install_from_catalog(config, "ytmusic").await {
        Ok(dir) => dir,
        Err(reason) => {
            eprintln!("{test}: {reason} — skipped (taken as no network)");
            return None;
        }
    };

    let manifest = PluginManifest::load(&dir).unwrap();

    // The engine's job: install the artifacts the plugin declares. The test
    // does this the same way the user does with `headshell plugin install`
    // (from the shared cache), then sets up the provider.
    for requirement in &manifest.requires {
        let shared = match shared_ytdlp(requirement) {
            Ok(path) => path,
            Err(reason) => panic!("could not install yt-dlp: {reason}"),
        };
        let store = ArtifactStore::new(config);
        std::fs::create_dir_all(store.runtime_dir()).unwrap();
        // A hard link, not a copy: so every test does not rewrite 40 MB. If
        // the link cannot be made (another file system) it falls back to a
        // copy; `copy` carries the execute bit too.
        let target = store.artifact_path(requirement);
        if std::fs::hard_link(&shared, &target).is_err() {
            std::fs::copy(&shared, &target).unwrap();
        }
    }

    let secrets = Secrets::load(&config.secrets_path()).unwrap();
    Some(PluginProvider::from_manifest(config, &manifest, &dir, &secrets).unwrap())
}

/// Search must give the fields K6's fuzzy matching link wants.
///
/// This test exists because of a measurement: yt-dlp's own search only gave
/// `title` + `id`, no artist and no duration (D-048). That is the only reason
/// for moving to InnerTube, and its regression is caught here.
#[tokio::test]
async fn a_search_carries_artist_and_duration_not_just_a_title() {
    if !prerequisites_met("search") {
        return;
    }
    let config = temp_config("search");
    let Some(provider) = install(&config, "search").await else {
        return;
    };

    assert!(
        provider
            .info()
            .capabilities
            .contains(Capabilities::SEARCH | Capabilities::STREAM)
    );

    let hits = provider.search("nujabes aruarian dance", 5).await.unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");

    let mut with_duration = 0;
    for hit in &hits {
        assert!(
            !hit.track.title.trim().is_empty(),
            "a track without a title: {hit:?}"
        );
        assert!(
            !hit.track.artist.trim().is_empty(),
            "a track without an artist: {hit:?}"
        );
        // The core adds the provider name; the plugin sends a bare id.
        assert_eq!(
            hit.id.provider.as_str(),
            "ytmusic",
            "the id is in the wrong namespace: {}",
            hit.id
        );
        if hit.track.duration_ms.unwrap_or(0) > 0 {
            with_duration += 1;
        }
    }
    assert_eq!(
        with_duration,
        hits.len(),
        "there is a track without a duration; the fuzzy matching link cannot use it: {hits:?}"
    );
}

/// The results must stay in the **relevance order** YouTube gives: for an
/// exactly matching query the first result must be the track searched for.
///
/// The first live run found this broken: the walk that collected the rows in
/// the InnerTube tree was stack-based and reversed the order. `limit=5` gave
/// five random rows rather than the best matches — the track searched for was
/// not in the list at all. A unit test could not have seen this: all five
/// rows that came back were valid tracks, just the wrong five.
///
/// **Why we do not compare the order of two calls:** it was measured,
/// YouTube does not give the same order to the same query. Across five runs
/// the first result came out the same 5/5, while places 2 and 3 moved. The
/// test only claims what is stable — the lesson D-045 learned at MusicBrainz
/// (forcing stability out of a service that is unstable across runs tests
/// the service, not your own code).
#[tokio::test]
async fn the_best_match_comes_first_not_somewhere_in_the_list() {
    if !prerequisites_met("order") {
        return;
    }
    let config = temp_config("order");
    let Some(provider) = install(&config, "order").await else {
        return;
    };

    let hits = provider.search("nujabes aruarian dance", 5).await.unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");

    let first = &hits[0].track;
    let artist = first.artist.to_lowercase();
    let title = first.title.to_lowercase();
    assert!(
        artist.contains("nujabes") && title.contains("aruarian"),
        "the first result is not the track searched for — the order may be broken. \
         The list that came back: {:?}",
        hits.iter()
            .map(|hit| format!("{} - {}", hit.track.artist, hit.track.title))
            .collect::<Vec<_>>()
    );
}

/// A song's cover is its album's square art, and it crosses `host.http` in
/// binary mode intact (api 3, D-076). Measured when it was written: InnerTube's
/// `next` gives it up to 544 px on `yt3.googleusercontent.com`.
#[tokio::test]
async fn a_song_gives_its_album_art_as_a_real_image() {
    if !prerequisites_met("cover") {
        return;
    }
    let config = temp_config("artwork");
    let Some(provider) = install(&config, "cover").await else {
        return;
    };
    assert!(provider.info().capabilities.contains(Capabilities::ARTWORK));

    let hits = provider.search("portishead sour times", 3).await.unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");
    let image = match provider.artwork(&hits[0].id, 500).await {
        Ok(Some(image)) => image,
        Ok(None) => panic!(
            "no cover for {} ({} - {}) — the `next` answer may have changed",
            hits[0].id, hits[0].track.artist, hits[0].track.title
        ),
        Err(err) => panic!("the cover failed:\n{}", err.chain_text()),
    };
    assert!(
        image.bytes.starts_with(&[0xFF, 0xD8, 0xFF]) || image.bytes.starts_with(b"\x89PNG"),
        "not a JPEG or a PNG: {:02x?}",
        &image.bytes[..image.bytes.len().min(8)]
    );
    assert!(
        image.bytes.len() > 10_000,
        "a 544 px cover in {} bytes — the small one came, or it was cut",
        image.bytes.len()
    );
}

/// A song's own lyrics come timed, line by line, with the service's credit
/// (api 4, D-078). Measured when it was written: asked as the mobile app,
/// YouTube Music's lyrics page carried timed lines for every song tried, from
/// LyricFind or Musixmatch; the plugin hands them over as LRC.
#[tokio::test]
async fn a_song_gives_its_lyrics_timed_line_by_line() {
    if !prerequisites_met("lyrics") {
        return;
    }
    let config = temp_config("lyrics");
    let Some(provider) = install(&config, "lyrics").await else {
        return;
    };
    assert!(provider.info().capabilities.contains(Capabilities::LYRICS));

    let hits = provider.search("radiohead creep", 3).await.unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");
    let found = match provider.lyrics(&hits[0].id).await {
        Ok(Some(found)) => found,
        Ok(None) => panic!(
            "no lyrics for {} ({} - {}) — the lyrics page may have moved",
            hits[0].id, hits[0].track.artist, hits[0].track.title
        ),
        Err(err) => panic!("the lyrics failed:\n{}", err.chain_text()),
    };
    assert!(
        found.lyrics.synced,
        "the words came without their timing — the mobile answer may have changed"
    );
    assert!(
        found.lyrics.lines.len() > 10,
        "{} lines",
        found.lyrics.lines.len()
    );
    let starts: Vec<u64> = found
        .lyrics
        .lines
        .iter()
        .filter_map(|line| line.start_ms)
        .collect();
    assert!(
        starts.windows(2).all(|pair| pair[0] <= pair[1]),
        "the lines are not in time order"
    );
    assert!(
        found
            .lyrics
            .attribution
            .as_deref()
            .is_some_and(|credit| credit.starts_with("Source:")),
        "{:?}",
        found.lyrics.attribution
    );
}

/// The resolved source must carry the `Range` header that **lifts the
/// throttling**.
///
/// Measured (D-048): the same address gives 32 KB/s on a plain GET and 8 MB/s
/// with `Range: bytes=0-` — 250 times as much. If the header drops, nothing
/// "breaks"; the audio just downloads at 2× real time and cuts out at the
/// first fluctuation. This assert is the only guard against a flaw that
/// degrades silently.
#[tokio::test]
async fn a_search_hit_resolves_to_a_stream_with_the_unthrottling_range_header() {
    if !prerequisites_met("resolving the source") {
        return;
    }
    let config = temp_config("resolve");
    let cookies_given = store_cookies(&config);
    let Some(provider) = install(&config, "resolving the source").await else {
        return;
    };

    let hits = provider.search("nujabes aruarian dance", 5).await.unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");

    let mut resolved = None;
    let mut refusals = Vec::new();
    for hit in hits.iter().take(3) {
        match provider.resolve_source(&hit.id).await {
            Ok(Some(source)) => {
                resolved = Some(source);
                break;
            }
            Ok(None) => refusals.push(format!("{}: cannot be played", hit.id)),
            // `chain_text`, not a plain `{err}`: `Error`'s `Display` only prints
            // `STEP: X`, and this test failed in CI exactly like that — three
            // candidates, three stage names, zero reasons. An error message that
            // swallows the diagnosis is what K9 forbids.
            Err(err) => refusals.push(format!("{}: {}", hit.id, err.chain_text())),
        }
    }

    let Some(source) = resolved else {
        let report = refusals.join("\n  ");

        // YouTube puts up a bot wall for data centre addresses. If cookies
        // were given, getting past the wall is **our job** and failing to is
        // a reason to fail. If no cookies were given there is nothing to
        // measure: the service did not let us look and said nothing about
        // the product. This is D-043's "could not reach it" side (D-061).
        //
        // The match is **narrow**: only the bot wall's own signature. Every
        // other refusal still fails, otherwise a real regression would hide
        // here.
        if !cookies_given && report.contains("not a bot") {
            eprintln!(
                "resolving the source: YouTube bot wall and no cookies given — \
                 skipped (this is not a failure).\n  \
                 To give cookies: HEADSHELL_TEST_YTMUSIC_COOKIES\n  {report}"
            );
            return;
        }
        panic!("none of the three candidates resolved:\n  {report}");
    };

    match source {
        AudioSource::HttpStream { url, headers } => {
            assert!(
                url.starts_with("https://"),
                "the stream address is not https: {url}"
            );
            let range = headers
                .iter()
                .find(|header| header.name.eq_ignore_ascii_case("range"))
                .unwrap_or_else(|| {
                    panic!("no Range header; the stream will be throttled: {headers:?}")
                });
            assert_eq!(range.value, "bytes=0-");
        }
        other => panic!("an http stream was expected, got: {other:?}"),
    }
}

/// A track that does not exist must fail **in yt-dlp's own words**.
///
/// In the SoundCloud plugin the counterpart was "none is an answer, not an
/// error"; here it is deliberately different: yt-dlp knows the difference
/// between "This video is unavailable", "Private video" and "Sign in to
/// confirm you're not a bot", and that distinction must reach the user.
/// Flattening it into `Ok(None)` would turn three diagnoses into one (K9,
/// D-048).
#[tokio::test]
async fn a_missing_track_fails_with_the_tools_own_words() {
    if !prerequisites_met("a missing track") {
        return;
    }
    let config = temp_config("missing");
    let Some(provider) = install(&config, "a missing track").await else {
        return;
    };

    let id = ProviderTrackId::new(ProviderId::new("ytmusic"), "zzzzzzzzzzz");
    let err = provider
        .resolve_source(&id)
        .await
        .expect_err("an error was expected for a missing track");

    // `Display` only writes the stage (`STEP: PROVIDER_CALL`); the chain of
    // causes is in `chain_text()`, and that is also what the CLI and the GUI
    // show the user.
    let message = err.chain_text();
    assert!(
        message.contains("yt-dlp"),
        "the error does not carry yt-dlp's message: {message}"
    );
    assert!(
        message.contains("STEP: PROVIDER_CALL"),
        "the error does not say which stage it is in: {message}"
    );
}

/// §2.5's real proof: the audio coming from YouTube Music **really plays**.
///
/// The trap here had been measured: yt-dlp's `bestaudio` choice gives
/// opus/webm, and the core's symphonia has neither that codec nor that
/// container. The plugin is pinned to m4a; this test is the only place that
/// says that pin is right.
#[tokio::test]
async fn a_ytmusic_track_actually_plays() {
    if !prerequisites_met("playback") {
        return;
    }
    let config = temp_config("playback");
    let Some(provider) = install(&config, "playback").await else {
        return;
    };

    let mut registry = headshell_core::provider::ProviderRegistry::new();
    registry.register(std::sync::Arc::new(provider));

    let hits = registry
        .get(&ProviderId::new("ytmusic"))
        .unwrap()
        .search("nujabes aruarian dance", 1)
        .await
        .unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");
    let item = headshell_core::playback::QueueItem {
        id: hits[0].id.clone(),
        track: hits[0].track.clone(),
    };

    let mut player = headshell_core::playback::Player::new(registry);
    if let Err(err) = player.play_items(vec![item]).await {
        eprintln!("playback: could not open the audio pipeline ({err}) — skipped");
        return;
    }

    // Measure that the audio really advances: the position must grow.
    let mut positions = Vec::new();
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        player.tick().await.unwrap();
        positions.push((player.state(), player.anchor().position_ms));
        if player.anchor().position_ms > 1500 {
            break;
        }
    }

    let best = positions.iter().map(|(_, ms)| *ms).max().unwrap_or(0);
    assert!(
        best > 1000,
        "the audio did not advance; the measured state/position sequence: {positions:?}"
    );
}
