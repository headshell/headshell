//! The SoundCloud reference plugin, **against the real SoundCloud** (Phase 2
//! §2.2).
//!
//! `plugin_script.rs` tests the contract with a fixed-catalog fixture; what
//! is tested here is something else: the `soundcloud` plugin in the catalog
//! connects to a real service, and the whole chain — client_id discovery,
//! search, resolving the stream address — runs live. The plugin is not in
//! this repository: it is installed from `headshell/plugins`, from the live
//! catalog (D-071). Since D-069 the plugin runs in embedded QuickJS: it can
//! only go online to the hosts its manifest allows, and these tests are also
//! the proof that this permission is **enough** for the real service.
//!
//! **These tests are part of the default run** (D-043). A deliberate choice,
//! and it has a price: when SoundCloud is down or changes its web surface the
//! suite turns red, although nothing in our code broke. In return, the day
//! the plugin breaks is learned *that day*, not months later.
//!
//! When it turns red the first question should be: **should I look at the
//! last commit, or run `curl https://soundcloud.com/`?** If the second works
//! and the tests are still red, the fault is ours.
//!
//! Two failures are kept apart (K9):
//! - **Not reaching it** is not a test failure: without a network the test
//!   skips itself and writes the reason to `stderr` (the audio device tests'
//!   procedure).
//! - **Reaching it and getting the unexpected** fails. "I could not reach it"
//!   and "it said no" are different diagnoses with different fixes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use headshell_core::config::Config;
use headshell_core::diag::Stage;
use headshell_core::ids::{ProviderId, ProviderTrackId};
use headshell_core::plugin::PluginProvider;
use headshell_core::plugin::manifest::PluginManifest;
use headshell_core::provider::{AudioSource, Capabilities, Provider};
use headshell_core::secrets::Secrets;

/// Can SoundCloud be reached over TCP.
///
/// Only DNS + connect is tested; it never goes into HTTP. The goal is
/// answering "is there a network", not measuring the service's health —
/// measuring health is the tests' own job.
fn soundcloud_reachable() -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = ("soundcloud.com", 443).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| {
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5)).is_ok()
    })
}

/// Says whether the test can run; if it cannot, writes the reason.
fn prerequisites_met(test: &str) -> bool {
    if !soundcloud_reachable() {
        eprintln!("{test}: could not reach soundcloud.com:443 — skipped (taken as no network)");
        return false;
    }
    true
}

fn temp_config(name: &str) -> support::TestConfig {
    support::TestConfig::new(&format!("soundcloud-{name}"))
}

/// Installs the plugin from the live catalog — the same thing the user does
/// (D-071). `None` if the catalog cannot be reached: the test is skipped and
/// the reason is written.
async fn install(config: &Config, test: &str) -> Option<PluginProvider> {
    let dir = match support::install_from_catalog(config, "soundcloud").await {
        Ok(dir) => dir,
        Err(reason) => {
            eprintln!("{test}: {reason} — skipped (taken as no network)");
            return None;
        }
    };

    let manifest = PluginManifest::load(&dir).unwrap();
    let secrets = Secrets::load(&config.secrets_path()).unwrap();
    Some(PluginProvider::from_manifest(config, &manifest, &dir, &secrets).unwrap())
}

/// On an install without secrets the plugin must discover the client_id
/// itself and be able to search.
///
/// This is D-043's "discover if missing" arm, and it is what brings setup
/// friction down to zero: the user can say `headshell play` without giving
/// any key.
#[tokio::test]
async fn a_plugin_with_no_secret_discovers_a_client_id_and_searches() {
    if !prerequisites_met("discovery+search") {
        return;
    }
    let config = temp_config("discovery");
    let Some(provider) = install(&config, "discovery+search").await else {
        return;
    };

    assert!(
        provider
            .info()
            .capabilities
            .contains(Capabilities::SEARCH | Capabilities::STREAM)
    );

    let hits = provider.search("nujabes", 5).await.unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");

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
            "soundcloud",
            "the id is in the wrong namespace: {}",
            hit.id
        );
    }
}

/// A track's cover crosses `host.http` in binary mode intact (api 3, D-076):
/// what arrives is an image, not text read into U+FFFD — the failure the
/// binary mode exists for, and a unit test with a fake server cannot see
/// what the real one sends.
#[tokio::test]
async fn a_search_hit_gives_its_cover_as_a_real_image() {
    if !prerequisites_met("cover") {
        return;
    }
    let config = temp_config("artwork");
    let Some(provider) = install(&config, "cover").await else {
        return;
    };
    assert!(provider.info().capabilities.contains(Capabilities::ARTWORK));

    let hits = provider.search("nujabes", 10).await.unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");

    // Not every upload has artwork, and "none" is an answer: the first one
    // that has a cover is enough. An error is not an answer — it fails.
    let mut found = None;
    let mut none = Vec::new();
    for hit in &hits {
        match provider.artwork(&hit.id, 500).await {
            Ok(Some(image)) => {
                found = Some(image);
                break;
            }
            Ok(None) => none.push(hit.id.to_string()),
            Err(err) => panic!("the cover of {} failed:\n{}", hit.id, err.chain_text()),
        }
    }
    let image = found.unwrap_or_else(|| panic!("none of them has a cover: {none:?}"));
    assert!(
        image.bytes.starts_with(&[0xFF, 0xD8, 0xFF]) || image.bytes.starts_with(b"\x89PNG"),
        "not a JPEG or a PNG: {:02x?}",
        &image.bytes[..image.bytes.len().min(8)]
    );
    assert!(
        image.bytes.len() > 5_000,
        "a 500 px cover in {} bytes — the small one came, or it was cut",
        image.bytes.len()
    );
}

/// api 6 (D-087): SoundCloud's related tracks — other tracks, the seed left
/// out, at most `limit`.
#[tokio::test]
async fn a_track_names_tracks_like_it() {
    if !prerequisites_met("related") {
        return;
    }
    let config = temp_config("related");
    let Some(provider) = install(&config, "related").await else {
        return;
    };
    assert!(provider.info().capabilities.contains(Capabilities::RELATED));

    let hits = provider.search("nujabes aruarian dance", 5).await.unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");
    let similar = match provider.related(&hits[0].id, 10).await {
        Ok(similar) => similar,
        Err(err) => panic!("the related tracks failed:\n{}", err.chain_text()),
    };
    assert!(
        !similar.is_empty(),
        "no related tracks for {} — the endpoint may have changed",
        hits[0].id
    );
    assert!(similar.len() <= 10);
    assert!(similar.iter().all(|track| track.id != hits[0].id));
}

/// api 7 (D-095): a soundcloud.com link opens the track at it; a set's link
/// is refused saying so, and an address with nothing at it is "nothing there".
#[tokio::test]
async fn a_soundcloud_link_opens_its_track() {
    if !prerequisites_met("link") {
        return;
    }
    let config = temp_config("link");
    let Some(provider) = install(&config, "link").await else {
        return;
    };
    assert!(provider.info().capabilities.contains(Capabilities::LINKS));
    let templates = provider.link_templates();

    let link =
        headshell_core::link::Link::parse("https://soundcloud.com/forss/flickermood?utm_source=x")
            .unwrap();
    let fit = headshell_core::link::fit_any(&templates, &link).expect("a track's link fits");
    assert_eq!(fit.id, "forss/flickermood");
    let at = match provider.open_link(&fit.id, &fit.template).await {
        Ok(at) => at,
        Err(err) => panic!("the link did not open:\n{}", err.chain_text()),
    };
    assert_eq!(at.len(), 1, "{at:?}");
    assert!(
        at[0].track.title.to_lowercase().contains("flickermood"),
        "{at:?}"
    );

    let err = provider
        .open_link("forss/sets/soulhack", "https://soundcloud.com/{id}")
        .await
        .unwrap_err();
    assert!(
        err.chain_text().contains("set's link"),
        "{}",
        err.chain_text()
    );

    let none = provider
        .open_link(
            "forss/no-such-track-headshell-test",
            "https://soundcloud.com/{id}",
        )
        .await
        .unwrap();
    assert!(none.is_empty(), "{none:?}");
}

/// The health answer must say which source the client_id came from.
///
/// D-043 allows three sources (secret → cache → discovery); if which one was
/// used is invisible, an install running with the wrong key silently looks
/// right.
#[tokio::test]
async fn health_reports_which_client_id_source_was_used() {
    if !prerequisites_met("health") {
        return;
    }
    let config = temp_config("health");
    let Some(provider) = install(&config, "health").await else {
        return;
    };

    let health = provider.health().await.unwrap();
    assert!(
        health.reachable,
        "SoundCloud was reached but the health answer is negative: {:?}",
        health.detail
    );
    let detail = health.detail.unwrap_or_default();
    assert!(
        detail.contains("client_id source"),
        "the health answer does not say the client_id source: {detail}"
    );
    // The catalog size is unknown; it must say **unknown**, not zero.
    assert_eq!(health.track_count, None);
}

/// A track from a search must really resolve to a playable address.
///
/// The last link of the chain: for the user this is the answer to "does it
/// play".
#[tokio::test]
async fn a_search_hit_resolves_to_a_playable_http_stream() {
    if !prerequisites_met("resolving the source") {
        return;
    }
    let config = temp_config("resolve");
    let Some(provider) = install(&config, "resolving the source").await else {
        return;
    };

    let hits = provider.search("lofi", 10).await.unwrap();
    assert!(!hits.is_empty(), "the live search came back empty");

    // Some tracks are offered only encrypted, and the plugin refuses them on
    // purpose (D-026). The test's job is not chasing those; the first one that
    // resolves is enough.
    let mut resolved = None;
    let mut refusals = Vec::new();
    for hit in hits.iter().take(5) {
        match provider.resolve_source(&hit.id).await {
            Ok(Some(source)) => {
                resolved = Some(source);
                break;
            }
            Ok(None) => refusals.push(format!("{}: cannot be played", hit.id)),
            // `chain_text`: `Display` only prints `STEP: X` (D-061's lesson).
            Err(err) => refusals.push(format!("{}: {}", hit.id, err.chain_text())),
        }
    }

    let source = resolved.unwrap_or_else(|| {
        panic!(
            "none of the five candidates resolved:\n  {}",
            refusals.join("\n  ")
        )
    });

    match source {
        AudioSource::HttpStream { url, headers } => {
            assert!(
                url.starts_with("https://"),
                "the stream address is not https: {url}"
            );
            // K3: the address is a source the client fetches itself. SoundCloud
            // carries the signature in the query string; we send no headers.
            assert!(headers.is_empty(), "unexpected header: {headers:?}");
        }
        other => panic!("an http stream was expected, got: {other:?}"),
    }
}

/// A track that does not exist must give a "none" answer, **not an error**.
///
/// This distinction broke on the plugin's first live run: `api_get` turned a
/// 404 into a generic error and the "none" arm of `resolve_source` never ran.
/// A fake server could not have shown this — we would have been producing
/// the 404 ourselves (K9).
#[tokio::test]
async fn a_missing_track_is_an_answer_not_an_error() {
    if !prerequisites_met("a missing track") {
        return;
    }
    let config = temp_config("missing");
    let Some(provider) = install(&config, "a missing track").await else {
        return;
    };

    let id = ProviderTrackId::new(ProviderId::new("soundcloud"), "999999999999");
    let source = provider
        .resolve_source(&id)
        .await
        .expect("a missing track must be an answer, not an error");
    assert_eq!(source, None);
}

/// §2.2's real proof: the audio coming from SoundCloud **really plays**.
///
/// Search and address resolution can be right on paper; for the user the
/// question is "does sound come out". In an environment without an audio
/// device the test skips itself.
#[tokio::test]
async fn a_soundcloud_track_actually_plays() {
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
        .get(&ProviderId::new("soundcloud"))
        .unwrap()
        .search("nujabes aruarian dance", 1)
        .await
        .unwrap();
    let item = headshell_core::playback::QueueItem {
        id: hits[0].id.clone(),
        track: hits[0].track.clone(),
    };

    let mut player = headshell_core::playback::Player::new(registry);
    if let Err(err) = player.play_items(vec![item]).await {
        // No sound device here, or no network: the condition isn't met. Any
        // other failure is SoundCloud reached and the track not playing — the
        // skip once hid exactly that, when SoundCloud dropped its plain
        // streams (D-091).
        let stage = err.stage();
        assert!(
            matches!(stage, Stage::PlaybackOutput | Stage::NetworkRequest),
            "the track did not play:\n{}",
            err.chain_text()
        );
        eprintln!("playback: {stage:?} unavailable ({err}) — skipped");
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

    // D-091: the stream is HLS, and moving in it opens the fragment that
    // holds the second — the position is there and goes on from there.
    player.seek(30_000).await.unwrap();
    let landed = player.anchor().position_ms;
    assert!(
        (30_000..30_100).contains(&landed),
        "the seek should land on 30 s, it is at {landed} ms"
    );
    let mut after = landed;
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        player.tick().await.unwrap();
        after = player.anchor().position_ms;
        if after > landed + 1000 {
            break;
        }
    }
    assert!(
        after > landed + 1000,
        "the audio did not go on after the seek: {after} ms"
    );
    player.stop();
}
