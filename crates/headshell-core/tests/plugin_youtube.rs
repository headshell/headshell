//! The YouTube plugin, **against the real YouTube** (D-086).
//!
//! The sibling of `plugin_ytmusic.rs`, and the same procedure: the plugin is
//! installed from the live catalog (`headshell/plugins`, D-071) and asked the
//! live service. What is tested is the plugin itself: its video search gives
//! the fields the app's matching needs, its videos are ids YouTube's player
//! takes, its covers are real images. Nothing here plays: the video plays in
//! the desktop's embedded YouTube player, which a test has no window for.
//!
//! **Part of the default run** (D-043), keeping two failures apart (K9): no
//! network is a skip with its reason; reaching YouTube and getting the
//! unexpected is a failure.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use headshell_core::ids::{ProviderId, ProviderTrackId};
use headshell_core::plugin::PluginProvider;
use headshell_core::plugin::manifest::PluginManifest;
use headshell_core::provider::{Capabilities, Provider};
use headshell_core::secrets::Secrets;
use headshell_core::video::{VideoSource, is_youtube_id};

fn reachable() -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = ("music.youtube.com", 443).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| {
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(5)).is_ok()
    })
}

/// Installs the plugin the way the user does; `None` (with the reason
/// written) when there is no network.
async fn install(test: &str) -> Option<(support::TestConfig, PluginProvider)> {
    if headshell_core::net::default_http_client().is_err() {
        eprintln!("{test}: this build has no HTTP client — skipped");
        return None;
    }
    if !reachable() {
        eprintln!("{test}: could not reach music.youtube.com:443 — skipped (taken as no network)");
        return None;
    }
    let config = support::TestConfig::new(&format!("youtube-{test}"));
    let dir = match support::install_from_catalog(&config, "youtube").await {
        Ok(dir) => dir,
        Err(reason) => {
            eprintln!("{test}: {reason} — skipped (taken as no network)");
            return None;
        }
    };
    let manifest = PluginManifest::load(&dir).unwrap();
    let secrets = Secrets::load(&config.secrets_path()).unwrap();
    let provider = PluginProvider::from_manifest(&config, &manifest, &dir, &secrets).unwrap();
    Some((config, provider))
}

/// A video service: it searches and shows videos, and plays no sound of its
/// own — its tracks are videos only.
#[tokio::test]
async fn it_is_a_video_service_and_its_search_names_the_song() {
    let test = "search";
    let Some((_config, provider)) = install(test).await else {
        return;
    };
    let caps = provider.info().capabilities;
    assert!(caps.contains(Capabilities::VIDEO | Capabilities::SEARCH | Capabilities::ARTWORK));
    assert!(!caps.contains(Capabilities::STREAM));

    let rows = provider.search("Daft Punk Get Lucky", 10).await.unwrap();
    assert!(!rows.is_empty(), "no rows for a song with a billion views");
    let song = rows
        .iter()
        .find(|row| row.track.artist.eq_ignore_ascii_case("Daft Punk"))
        .unwrap_or_else(|| panic!("no row by the artist: {rows:#?}"));
    // The labels are off the title, so the app compares a song with a song.
    assert!(
        song.track.title.starts_with("Get Lucky") && !song.track.title.contains("Official"),
        "{:?}",
        song.track.title
    );
    assert!(song.track.duration_ms.unwrap_or(0) > 60_000, "{song:?}");
    assert!(is_youtube_id(&song.id.id), "{:?}", song.id.id);
}

/// D-096, measured 2026-10-15: YouTube Music files the artist's audio upload
/// (a still picture of the cover) as "official" too, titled the song's name
/// alone. The official video comes first, an upload whose title says it is
/// the sound not at all, and the video YouTube Music titled "Live At Wembley
/// Stadium" is Queen's song.
#[tokio::test]
async fn the_official_video_comes_first_and_an_audio_upload_not_at_all() {
    let test = "official";
    let Some((_config, provider)) = install(test).await else {
        return;
    };
    // (query, the official video when there is one, an audio upload whose
    // title says so)
    for (query, video, audio) in [
        (
            "Tame Impala The Less I Know The Better",
            Some("sBzrzS1Ag_g"),
            Some("2SUwOgmvzK4"),
        ),
        // "Ezhel - Geceler": nothing in its title says it is the sound — it
        // stays, after the video.
        ("Ezhel Geceler", Some("iurfbuXpqXM"), None),
        // Get Lucky has no official video, only its "(Official Audio)".
        ("Daft Punk Get Lucky", None, Some("5NV6Rdv1a3I")),
    ] {
        let rows = provider.search(query, 10).await.unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.id.id.as_str()).collect();
        if let Some(video) = video {
            assert_eq!(ids.first(), Some(&video), "{query}: {rows:#?}");
        }
        if let Some(audio) = audio {
            assert!(
                !ids.contains(&audio),
                "{query}: the audio upload is offered: {rows:#?}"
            );
        }
    }

    let rows = provider
        .search("Queen Bohemian Rhapsody", 10)
        .await
        .unwrap();
    let queen = rows
        .iter()
        .find(|row| row.id.id == "fJ9rUzIMcZQ")
        .unwrap_or_else(|| panic!("Queen's video is not offered: {rows:#?}"));
    assert_eq!(queen.track.title, "Bohemian Rhapsody", "{queen:?}");
}

/// An item's video is its id, in the shape YouTube's player takes; a cover is
/// a real image.
#[tokio::test]
async fn an_items_video_is_its_id_and_its_cover_a_real_image() {
    let test = "video";
    let Some((_config, provider)) = install(test).await else {
        return;
    };
    let rows = provider
        .search("Tame Impala Let It Happen", 5)
        .await
        .unwrap();
    let row = rows.first().expect("a row");
    assert_eq!(
        provider.resolve_video(&row.id).await.unwrap(),
        Some(VideoSource::Youtube {
            video_id: row.id.id.clone()
        })
    );

    let cover = provider
        .artwork(&row.id, 500)
        .await
        .unwrap()
        .expect("a cover");
    assert!(cover.bytes.starts_with(&[0xFF, 0xD8]), "not a JPEG");

    // Not an id YouTube gives: refused with the reason, before any player
    // sees it.
    let odd = ProviderTrackId::new(ProviderId::new("youtube"), "../../etc");
    let err = provider.resolve_video(&odd).await.unwrap_err();
    assert!(
        err.chain_text().contains("not a YouTube video id"),
        "{}",
        err.chain_text()
    );
}
