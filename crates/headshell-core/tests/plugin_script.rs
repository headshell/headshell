//! The plugin contract, **with the real engine and the fixture in the
//! repository** (D-069).
//!
//! The unit tests (`src/plugin/tests.rs`) test every gate of the engine with
//! small scripts written inside the tests. What is tested here is something
//! else: the surface seen from outside — discovery, consent, provider
//! registration — and the `fixtures/plugins/echo` files working end to end,
//! written the way a plugin author would write them.
//!
//! Both of Phase 2's "counts as done" criteria are in this file: **a plugin
//! not written in Rust works**, and **the core refuses a version mismatch
//! without crashing**. The difference from api 1: no test skips because
//! "there is no python3", since nothing outside is needed to run a plugin.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use headshell_core::config::Config;
use headshell_core::ids::{ProviderId, ProviderTrackId};
use headshell_core::plugin::consent::ConsentStore;
use headshell_core::plugin::manifest::{Permissions, PluginManifest};
use headshell_core::plugin::{PluginProvider, discover, load};
use headshell_core::provider::{AudioSource, Capabilities, Provider};
use headshell_core::secrets::Secrets;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/plugins/echo")
}

fn temp_config(name: &str) -> support::TestConfig {
    support::TestConfig::new(&format!("plugin-script-{name}"))
}

/// Installs the fixture plugin into the data directory — the way the user
/// would: by copying the directory.
fn install_echo(config: &Config) -> PathBuf {
    let dir = config.plugins_dir().join("echo");
    std::fs::create_dir_all(&dir).unwrap();
    for file in ["plugin.json", "main.js"] {
        std::fs::copy(fixture_dir().join(file), dir.join(file)).unwrap();
    }
    dir
}

fn approve(config: &Config, name: &str) {
    let dir = config.plugins_dir().join(name);
    let manifest = PluginManifest::load(&dir).unwrap();
    let path = config.plugin_consent_path();
    let mut store = ConsentStore::load(&path).unwrap();
    store.approve(name, &manifest.permissions, jiff::Timestamp::now());
    store.save(&path).unwrap();
}

fn provider_for(config: &Config, dir: &Path) -> PluginProvider {
    let manifest = PluginManifest::load(dir).unwrap();
    let secrets = Secrets::load(&config.secrets_path()).unwrap();
    PluginProvider::from_manifest(config, &manifest, dir, &secrets).unwrap()
}

#[tokio::test]
async fn a_script_plugin_answers_health_search_and_resolve() {
    let config = temp_config("happy");
    let dir = install_echo(&config);

    let mut secrets = Secrets::default();
    secrets.set("plugin:echo", "token", "secret-key");
    secrets.save(&config.secrets_path()).unwrap();

    let provider = provider_for(&config, &dir);
    assert!(
        provider
            .info()
            .capabilities
            .contains(Capabilities::SEARCH | Capabilities::STREAM)
    );

    let health = provider.health().await.unwrap();
    assert!(health.reachable, "{:?}", health.detail);
    assert_eq!(health.track_count, Some(2));
    // The plugin saw the secret but did not report its **value**.
    assert_eq!(health.detail.as_deref(), Some("has secret"));

    let tracks = provider.search("EZHEL", 10).await.unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].id.provider, ProviderId::new("echo"));
    assert_eq!(tracks[0].track.title, "Geceler");
    assert!(tracks[0].track.isrc.is_some(), "a valid ISRC must be kept");

    let tracks = provider.search("sezen", 10).await.unwrap();
    assert!(
        tracks[0].track.isrc.is_none(),
        "a malformed ISRC must not be accepted"
    );

    let id = ProviderTrackId::new(ProviderId::new("echo"), "track-1");
    match provider.resolve_source(&id).await.unwrap() {
        Some(AudioSource::HttpStream { url, headers }) => {
            assert!(url.ends_with("track-1.mp3"), "{url}");
            assert_eq!(headers[0].value, "secret-key");
        }
        other => panic!("unexpected source: {other:?}"),
    }

    let missing = ProviderTrackId::new(ProviderId::new("echo"), "none");
    assert!(
        provider.resolve_source(&missing).await.unwrap().is_none(),
        "`null` is an answer, not an error"
    );

    // api 3 (D-076): the manifest says `"artwork": true`; the cover crosses as
    // base64 and arrives as the PNG the script holds.
    assert!(provider.info().capabilities.contains(Capabilities::ARTWORK));
    let cover = provider.artwork(&id, 500).await.unwrap().unwrap();
    assert!(
        cover.bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "{:?}",
        &cover.bytes[..8]
    );
    assert_eq!(cover.mime.as_deref(), Some("image/png"));
    let other = ProviderTrackId::new(ProviderId::new("echo"), "track-2");
    assert!(
        provider.artwork(&other, 500).await.unwrap().is_none(),
        "no cover is an answer: the chain goes on"
    );

    // api 4 (D-078): the manifest says `"lyrics": true`; `synced` crosses as
    // LRC text and is read with the same parser as a `.lrc` file, the
    // service's credit with it.
    assert!(provider.info().capabilities.contains(Capabilities::LYRICS));
    assert!(provider.is_plugin());
    let lyrics = provider.lyrics(&id).await.unwrap().unwrap();
    assert!(lyrics.lyrics.synced);
    assert_eq!(lyrics.lyrics.lines[1].start_ms, Some(4_500));
    assert_eq!(lyrics.lyrics.lines[1].text, "and a second one");
    assert_eq!(
        lyrics.lyrics.attribution.as_deref(),
        Some("Source: the echo test plugin")
    );
    assert_eq!(
        lyrics.source,
        headshell_core::lyrics::LyricsSource::Provider
    );
    assert!(
        provider.lyrics(&other).await.unwrap().is_none(),
        "no lyrics is an answer: the chain goes on"
    );

    // api 6 (D-087): the manifest says `"related": true`; only the item's
    // id goes in, the item itself is not offered back, and the genres the
    // service names cross with each track.
    assert!(provider.info().capabilities.contains(Capabilities::RELATED));
    let similar = provider.related(&other, 5).await.unwrap();
    assert_eq!(similar.len(), 1);
    assert_eq!(similar[0].id, id);
    assert_eq!(similar[0].genres, vec!["Hip-Hop", "Turkish Rap"]);

    // api 7 (D-095): the manifest's `links` names templates; what stood in
    // `{id}` and the template go in, the song at the link comes out.
    assert!(provider.info().capabilities.contains(Capabilities::LINKS));
    let templates: Vec<String> = provider
        .link_templates()
        .into_iter()
        .map(|template| template.template)
        .collect();
    assert_eq!(
        templates,
        [
            "https://echo.example.invalid/track/{id}",
            "https://echo.example.invalid/list?l={id}"
        ]
    );
    let at = provider
        .open_link("track-1", "https://echo.example.invalid/track/{id}")
        .await
        .unwrap();
    assert_eq!(at.len(), 1);
    assert_eq!(at[0].id, id);
    assert!(
        provider
            .open_link("nowhere", "https://echo.example.invalid/track/{id}")
            .await
            .unwrap()
            .is_empty(),
        "nothing at the link is an answer"
    );
}

/// api 1's Python plugin is refused **without crashing** and the user is told
/// what to do — the second half of the Phase 2 criterion.
#[tokio::test]
async fn an_api1_plugin_is_refused_with_a_reason_and_the_core_keeps_going() {
    let config = temp_config("old");
    install_echo(&config);
    let old = config.plugins_dir().join("old");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(
        old.join("plugin.json"),
        r#"{"name":"old","display_name":"Old","api":1,"exec":["python3","./main.py"]}"#,
    )
    .unwrap();
    approve(&config, "echo");

    let (entries, summary) = discover(&config).unwrap();
    assert_eq!(summary.incompatible, 1);
    let old_entry = entries.iter().find(|entry| entry.name == "old").unwrap();
    assert!(
        old_entry.status_text().contains("api 1"),
        "{}",
        old_entry.status_text()
    );

    let (providers, _) = load(&config).unwrap();
    assert_eq!(
        providers.len(),
        1,
        "the old plugin must not load, echo must load"
    );
    assert!(providers[0].health().await.unwrap().reachable);
}

#[tokio::test]
async fn an_unapproved_plugin_is_not_loaded_but_is_visible() {
    let config = temp_config("consent");
    install_echo(&config);

    let (providers, summary) = load(&config).unwrap();
    assert!(
        providers.is_empty(),
        "a plugin without consent must not be loaded"
    );
    assert_eq!(summary.discovered, 1);
    assert_eq!(summary.awaiting_approval, 1);

    approve(&config, "echo");
    let (providers, summary) = load(&config).unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(summary.ready, 1);

    let registry = headshell_core::provider::registry_with_http(&config, None).unwrap();
    assert!(registry.get(&ProviderId::new("echo")).is_some());
}

#[tokio::test]
async fn a_plugin_asking_for_more_permissions_stops_loading_until_reapproved() {
    let config = temp_config("permissions");
    let dir = install_echo(&config);
    approve(&config, "echo");
    assert_eq!(load(&config).unwrap().0.len(), 1);

    // The plugin was updated and wants a new host.
    let mut manifest = PluginManifest::load(&dir).unwrap();
    manifest.permissions = Permissions {
        net: vec!["example.invalid".to_owned(), "*.new.invalid".to_owned()],
    };
    std::fs::write(
        dir.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let (providers, summary) = load(&config).unwrap();
    assert!(providers.is_empty());
    assert_eq!(summary.awaiting_approval, 1);

    approve(&config, "echo");
    assert_eq!(load(&config).unwrap().0.len(), 1);
}

#[tokio::test]
async fn two_plugins_do_not_see_each_others_secrets() {
    let config = temp_config("two-secrets");
    let dir = install_echo(&config);

    let mut secrets = Secrets::default();
    secrets.set("plugin:other", "token", "neighbours-key");
    secrets.save(&config.secrets_path()).unwrap();

    let provider = provider_for(&config, &dir);
    assert_eq!(
        provider.health().await.unwrap().detail.as_deref(),
        Some("no secret")
    );
    let id = ProviderTrackId::new(ProviderId::new("echo"), "track-1");
    match provider.resolve_source(&id).await.unwrap() {
        Some(AudioSource::HttpStream { headers, .. }) => assert_eq!(headers[0].value, ""),
        other => panic!("unexpected source: {other:?}"),
    }
}

/// A hung plugin does not hang the core; when its time is up it returns an
/// error.
#[tokio::test]
async fn a_hanging_plugin_times_out_instead_of_freezing_the_core() {
    let config = temp_config("hung");
    let dir = install_echo(&config);
    std::fs::write(
        dir.join("main.js"),
        "export function health() { for (;;) {} }\n\
         export function search() { return []; }\n\
         export function resolve_source() { return null; }\n\
         export function artwork() { return null; }\n\
         export function lyrics() { return null; }\n\
         export function resolve_video() { return null; }\n\
         export function related() { return []; }\n\
         export function link() { return []; }\n",
    )
    .unwrap();

    let provider = provider_for(&config, &dir)
        .with_timeouts(Duration::from_secs(2), Duration::from_millis(500));
    let started = std::time::Instant::now();
    let health = provider.health().await.unwrap();
    assert!(!health.reachable);
    let detail = health.detail.unwrap_or_default();
    assert!(detail.contains("did not answer"), "{detail}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the timeout did not work: {:?}",
        started.elapsed()
    );
}
