//! Unit tests of the plugin boundary.
//!
//! Two parts: discovery and consent are tested in every build (no engine
//! needed); the engine itself is tested **with real QuickJS** when
//! `plugin-engine` is on. The scripts are written inside the tests and the
//! network goes through a fake client — permission checks, redirects and
//! timeouts can be seen without going online.

use std::path::PathBuf;

use super::*;

fn temp_config(name: &str) -> crate::test_support::TestConfig {
    crate::test_support::TestConfig::new(&format!("plugin-{name}"))
}

fn write_plugin(config: &Config, name: &str, manifest_json: &str) -> PathBuf {
    let dir = config.plugins_dir().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(MANIFEST_FILE), manifest_json).unwrap();
    dir
}

fn approve(config: &Config, name: &str, net: &[&str]) {
    let mut store = ConsentStore::load(&config.plugin_consent_path()).unwrap();
    store.approve(
        name,
        &Permissions {
            net: net.iter().map(|h| (*h).to_owned()).collect(),
        },
        jiff::Timestamp::now(),
    );
    store.save(&config.plugin_consent_path()).unwrap();
}

// --- discovery ------------------------------------------------------------

#[test]
fn a_missing_plugins_dir_is_an_empty_list_not_an_error() {
    let config = temp_config("empty");
    let (entries, summary) = discover(&config).unwrap();
    assert!(entries.is_empty());
    assert_eq!(summary, PluginSummary::default());
}

#[test]
fn discovery_reports_each_plugins_reason_without_stopping() {
    let config = temp_config("mixed");
    write_plugin(
        &config,
        "good",
        r#"{"name":"good","display_name":"Good","api":6,"artwork":false,"lyrics":false,"video":false,"related":false,"main":"main.js",
            "permissions":{"net":["a.example"]}}"#,
    );
    write_plugin(
        &config,
        "old",
        r#"{"name":"old","display_name":"Old","api":1,"exec":["python3","./main.py"]}"#,
    );
    write_plugin(&config, "broken", "{ this is not json");

    let (entries, summary) = discover(&config).unwrap();
    assert_eq!(entries.len(), 3, "a broken plugin must not hide the others");
    assert_eq!(summary.discovered, 3);
    assert_eq!(summary.incompatible, 1);
    assert_eq!(summary.broken, 1);
    assert_eq!(summary.awaiting_approval, 1, "awaiting consent: good");
    assert_eq!(summary.ready, 0);

    let by_name = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap()
            .clone()
    };
    let old = by_name("old");
    assert_eq!(old.api, Some(1));
    let problem = old.problem.unwrap();
    assert!(problem.contains("api 1"), "{problem}");
    assert!(
        problem.contains("QuickJS"),
        "it must say what to do with the old plugin: {problem}"
    );
    assert!(by_name("broken").problem.is_some());
    assert!(
        !by_name("good").is_loadable(),
        "a plugin without consent must not be loaded"
    );
}

#[test]
fn an_approved_plugin_becomes_loadable_and_load_starts_no_engine() {
    let config = temp_config("approved");
    // `main.js` is missing on purpose: if loading started the engine, it would
    // try to read this file and fall over.
    write_plugin(
        &config,
        "good",
        r#"{"name":"good","display_name":"Good","api":6,"artwork":false,"lyrics":false,"video":false,"related":false,"main":"main.js",
            "capabilities":["search"],"permissions":{"net":["a.example"]}}"#,
    );
    approve(&config, "good", &["a.example"]);

    let (entries, summary) = discover(&config).unwrap();
    assert_eq!(summary.ready, 1);
    assert!(entries[0].is_loadable());

    let (providers, summary) = load(&config).unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(summary.ready, 1);
    assert_eq!(providers[0].info().id, ProviderId::new("good"));
    assert!(
        providers[0]
            .info()
            .capabilities
            .contains(Capabilities::SEARCH)
    );
    assert!(config.plugin_state_dir("good").is_dir());
}

#[test]
fn a_plugin_that_grew_its_permissions_is_not_loaded_until_reapproved() {
    let config = temp_config("growing");
    write_plugin(
        &config,
        "good",
        r#"{"name":"good","display_name":"Good","api":6,"artwork":false,"lyrics":false,"video":false,"related":false,"main":"main.js",
            "permissions":{"net":["a.example","new.example"]}}"#,
    );
    approve(&config, "good", &["a.example"]);

    let (entries, summary) = discover(&config).unwrap();
    assert_eq!(summary.awaiting_approval, 1);
    assert!(!entries[0].is_loadable());
    assert!(
        entries[0].status_text().contains("new.example"),
        "{}",
        entries[0].status_text()
    );
    assert!(load(&config).unwrap().0.is_empty());
}

/// An artifact with no release for this platform: the plugin is not loaded
/// and the status line **does not suggest installing** — installing does not
/// fix it.
#[test]
fn a_tool_without_an_asset_for_this_platform_blocks_loading_without_a_false_hint() {
    let config = temp_config("unsupported");
    let other = if artifact::current_platform() == "windows-x86" {
        "linux-x86_64"
    } else {
        "windows-x86"
    };
    write_plugin(
        &config,
        "tool",
        &format!(
            r#"{{"name":"tool","display_name":"Tool","api":6,"artwork":false,"lyrics":false,"video":false,"related":false,"main":"main.js",
                "requires":[{{"name":"yt-dlp","version":"1","assets":{{"{other}":
                {{"url":"https://example.invalid/x","sha256":"{}"}}}}}}]}}"#,
            "a".repeat(64)
        ),
    );
    approve(&config, "tool", &[]);

    let (entries, summary) = discover(&config).unwrap();
    assert_eq!(summary.needs_install, 1);
    assert!(!entries[0].is_loadable());
    let text = entries[0].status_text();
    assert!(text.contains("does not exist for this platform"), "{text}");
    assert!(!text.contains("plugin install"), "wrong advice: {text}");
}

#[test]
fn a_plugin_only_sees_its_own_secrets() {
    let config = temp_config("secrets");
    let dir = write_plugin(
        &config,
        "good",
        r#"{"name":"good","display_name":"Good","api":6,"artwork":false,"lyrics":false,"video":false,"related":false,"main":"main.js","capabilities":["search"]}"#,
    );
    let mut secrets = Secrets::default();
    secrets.set("plugin:good", "client_id", "mine");
    secrets.set("plugin:other", "client_id", "theirs");

    let manifest = PluginManifest::load(&dir).unwrap();
    let provider = PluginProvider::from_manifest(&config, &manifest, &dir, &secrets).unwrap();
    assert_eq!(provider.spec.secrets.len(), 1);
    assert_eq!(
        provider.spec.secrets.get("client_id"),
        Some(&"mine".to_owned())
    );
}

fn offline_provider(config: &Config, name: &str, capabilities: &str) -> PluginProvider {
    let dir = write_plugin(
        config,
        name,
        &format!(
            r#"{{"name":"{name}","display_name":"X","api":6,"artwork":false,"lyrics":false,"video":false,"related":false,"main":"main.js",
                "capabilities":{capabilities}}}"#
        ),
    );
    let manifest = PluginManifest::load(&dir).unwrap();
    PluginProvider::with_http(
        config,
        &manifest,
        &dir,
        &Secrets::default(),
        Err("test".to_owned()),
    )
    .unwrap()
}

#[tokio::test]
async fn a_capability_the_plugin_lacks_is_refused_before_the_engine_starts() {
    let config = temp_config("no-capabilities");
    // No `main.js`: had the engine started it would say "could not be read", not
    // "not supported".
    let provider = offline_provider(&config, "demo", r#"["search"]"#);
    let id = ProviderTrackId::new(ProviderId::new("demo"), "1");
    let err = provider.resolve_source(&id).await.unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::Unsupported { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_track_id_from_another_provider_is_refused() {
    let config = temp_config("foreign");
    let provider = offline_provider(&config, "demo", r#"["stream"]"#);
    let id = ProviderTrackId::new(ProviderId::new("other"), "1");
    let err = provider.resolve_source(&id).await.unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::InvalidInput { .. }),
        "{err:?}"
    );
}

#[test]
fn block_on_waits_for_a_future_that_is_not_ready_on_the_first_poll() {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// Not ready on the first poll; it calls the waker from another thread — a
    /// busy-waiting `block_on` would pass here too, but a `block_on` that ignores
    /// the waker would sleep forever.
    struct Later(bool);
    impl Future for Later {
        type Output = u8;
        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<u8> {
            if self.0 {
                return Poll::Ready(7);
            }
            self.0 = true;
            let waker = context.waker().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                waker.wake();
            });
            Poll::Pending
        }
    }
    assert_eq!(block_on(Later(false)), 7);
}

// --- the order of calls (D-077) ---------------------------------------------

/// A call for the user arriving while one made ahead of time holds the
/// engine raises that call's stop flag, and gets the engine when it ends.
#[test]
fn a_call_for_the_user_asks_the_running_call_made_ahead_of_time_to_give_way() {
    let gate = Arc::new(Gate::default());
    let ahead = gate.enter(true).expect("a turn");
    let flag = Arc::clone(
        ahead
            .cancel
            .as_ref()
            .expect("a call made ahead of time can be stopped"),
    );

    let user = {
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || {
            let turn = gate.enter(false).expect("a turn");
            assert!(turn.cancel.is_none(), "nothing stops a call for the user");
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !flag.load(std::sync::atomic::Ordering::Acquire) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        flag.load(std::sync::atomic::Ordering::Acquire),
        "the running call must be asked to give way"
    );
    drop(ahead);
    user.join().unwrap();
}

/// A call made ahead of time does not take the engine while a call for the
/// user is waiting for it, even if it asked first.
#[test]
fn a_call_made_ahead_of_time_waits_behind_every_call_for_the_user() {
    let gate = Arc::new(Gate::default());
    let first = gate.enter(false).expect("a turn");
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));

    let ahead = {
        let (gate, order) = (Arc::clone(&gate), Arc::clone(&order));
        std::thread::spawn(move || {
            let _turn = gate.enter(true).expect("a turn");
            order.lock().unwrap().push("ahead");
        })
    };
    std::thread::sleep(Duration::from_millis(50));
    let user = {
        let (gate, order) = (Arc::clone(&gate), Arc::clone(&order));
        std::thread::spawn(move || {
            let _turn = gate.enter(false).expect("a turn");
            order.lock().unwrap().push("user");
        })
    };
    std::thread::sleep(Duration::from_millis(50));
    drop(first);
    user.join().unwrap();
    ahead.join().unwrap();
    assert_eq!(*order.lock().unwrap(), vec!["user", "ahead"]);
}

// --- engine ---------------------------------------------------------------

#[cfg(feature = "plugin-engine")]
mod engine {
    use super::*;
    use crate::net::fake::FakeHttp;

    const CALL: Duration = Duration::from_secs(5);

    /// The query that makes the give-way scripts run their slow tool.
    #[cfg(any(unix, windows))]
    const SLOW_QUERY: &str = "slow";

    /// Writes the script and the manifest, sets up the provider with a fake
    /// network.
    struct Rig {
        config: crate::test_support::TestConfig,
        dir: PathBuf,
    }

    impl Rig {
        fn new(name: &str, manifest: &str, script: &str) -> Self {
            let config = temp_config(name);
            let dir = write_plugin(&config, "demo", manifest);
            std::fs::write(dir.join("main.js"), script).unwrap();
            Self { config, dir }
        }

        fn simple(name: &str, script: &str) -> Self {
            Self::new(
                name,
                r#"{"name":"demo","display_name":"Demo","api":6,"artwork":false,"lyrics":false,"video":false,"related":false,"main":"main.js",
                    "capabilities":["search","stream"],
                    "permissions":{"net":["allowed.example","*.cdn.example"]}}"#,
                script,
            )
        }

        fn provider_with(&self, http: Arc<FakeHttp>, secrets: &Secrets) -> PluginProvider {
            let manifest = PluginManifest::load(&self.dir).unwrap();
            let http: Arc<dyn HttpClient> = http;
            PluginProvider::with_http(&self.config, &manifest, &self.dir, secrets, Ok(http))
                .unwrap()
                .with_timeouts(Duration::from_secs(2), CALL)
        }

        fn provider(&self) -> PluginProvider {
            self.provider_with(Arc::new(FakeHttp::new()), &Secrets::default())
        }
    }

    fn track_id(id: &str) -> ProviderTrackId {
        ProviderTrackId::new(ProviderId::new("demo"), id)
    }

    /// Starts `search("slow")` ahead of time on a background thread, then
    /// searches for the user: the user's search must answer at once, and the
    /// one made ahead of time must end as "gave way" (D-077).
    #[cfg(any(unix, windows))]
    async fn gives_way(provider: PluginProvider) {
        let provider = Arc::new(provider);
        let background = {
            let provider = Arc::clone(&provider);
            std::thread::spawn(move || {
                crate::provider::ahead_of_time::mark_this_thread();
                let started = std::time::Instant::now();
                let outcome = crate::plugin::block_on(provider.search(SLOW_QUERY, 1));
                (outcome, started.elapsed())
            })
        };
        // Long enough for the tool to be running.
        std::thread::sleep(Duration::from_millis(700));

        let started = std::time::Instant::now();
        let tracks = provider.search("for the user", 1).await.unwrap();
        let user_waited = started.elapsed();
        assert_eq!(tracks[0].track.artist, "user");
        assert!(
            user_waited < Duration::from_secs(2),
            "the user waited for the tool run ahead of time: {user_waited:?}"
        );

        let (outcome, took) = background.join().unwrap();
        let err = outcome.expect_err("the call made ahead of time was stopped");
        assert!(
            matches!(err.kind(), ErrorKind::PluginPreempted { .. }),
            "{}",
            err.chain_text()
        );
        assert!(took < Duration::from_secs(3), "{took:?}");
    }

    const BASIC: &str = r#"
        let calls = 0;
        export function health() {
            calls += 1;
            return { reachable: true, detail: `call ${calls}` };
        }
        export function search(query, limit) {
            return [
                { id: "42", artist: "Ezhel", title: query, duration_ms: 1000, isrc: "TRA111700001" },
                { id: "43", artist: "B", title: "C", isrc: "bogus" },
            ].slice(0, limit);
        }
        export function resolve_source(id) {
            if (id === "none") return null;
            return { kind: "http_stream", url: `https://a.cdn.example/${id}.mp3`, headers: [] };
        }
    "#;

    /// api 3 (D-076): the plugin fetches its cover itself, through
    /// `host.http` in binary mode, and hands it over as base64. Read as text
    /// the image would be destroyed — every byte that is not UTF-8 becomes
    /// U+FFFD — so the bytes the core gets must be the bytes the server sent.
    #[tokio::test]
    async fn a_plugins_cover_crosses_host_http_in_binary_mode_intact() {
        let rig = Rig::new(
            "plugin-artwork",
            r#"{"name":"demo","display_name":"Demo","api":6,"artwork":true,"lyrics":false,"video":false,"related":false,"main":"main.js",
                "capabilities":["search"],
                "permissions":{"net":["*.cdn.example"]}}"#,
            r#"
            export function health() { return { reachable: true }; }
            export function search() { return []; }
            export function artwork(id, size) {
                if (id === "none") return null;
                const response = host.http.request({
                    url: `https://img.cdn.example/${id}.png?w=${size}`,
                    binary: true,
                });
                if (!response.ok || response.encoding !== "base64") return null;
                return { mime: response.headers["content-type"], data: response.body };
            }
            "#,
        );
        let png = crate::artwork::image::tests_support::png_2x2();
        let http =
            Arc::new(FakeHttp::new().route_bytes("img.cdn.example/42.png", "image/png", &png));
        let provider = rig.provider_with(http.clone(), &Secrets::default());
        assert!(provider.info().capabilities.contains(Capabilities::ARTWORK));

        let image = provider
            .artwork(&track_id("42"), 500)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(image.bytes, png, "the image arrived damaged");
        assert_eq!(image.mime.as_deref(), Some("image/png"));
        assert_eq!(image.source, crate::artwork::ArtworkSource::Provider);
        assert!(
            http.last_url().ends_with("42.png?w=500"),
            "{}",
            http.last_url()
        );
        assert_eq!(
            provider.artwork(&track_id("none"), 500).await.unwrap(),
            None
        );
    }

    /// `"artwork": true` without the export does not start — the same rule as a
    /// declared capability without its function.
    #[tokio::test]
    async fn declaring_covers_without_the_export_does_not_start() {
        let rig = Rig::new(
            "plugin-artwork-missing",
            r#"{"name":"demo","display_name":"Demo","api":6,"artwork":true,"lyrics":false,"video":false,"related":false,"main":"main.js"}"#,
            "export function health() { return { reachable: true }; }",
        );
        let err = rig
            .provider()
            .artwork(&track_id("1"), 500)
            .await
            .unwrap_err();
        assert!(
            err.chain_text().contains("`artwork`"),
            "{}",
            err.chain_text()
        );
    }

    /// With `"artwork": false` the plugin is not asked: the chain goes on to
    /// MusicBrainz and the archive.
    #[tokio::test]
    async fn a_plugin_that_gives_no_covers_says_it_cannot() {
        let rig = Rig::simple("plugin-no-artwork", BASIC);
        let provider = rig.provider();
        assert!(!provider.info().capabilities.contains(Capabilities::ARTWORK));
        let err = provider.artwork(&track_id("42"), 500).await.unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::Unsupported { .. }),
            "{err:?}"
        );
    }

    /// api 4 (D-078): `synced` is LRC text and is read with the core's own
    /// parser; `plain` is the words alone; `synced` without a single stamp is
    /// a contract error with the reason, not words shown as "synced".
    #[tokio::test]
    async fn a_plugins_lyrics_are_read_as_lrc_and_a_stampless_synced_is_refused() {
        let rig = Rig::new(
            "plugin-lyrics",
            r#"{"name":"demo","display_name":"Demo","api":6,"artwork":false,"lyrics":true,"video":false,"related":false,"main":"main.js"}"#,
            r#"
            export function health() { return { reachable: true }; }
            export function lyrics(id) {
                if (id === "timed") {
                    return { synced: "[00:01.50]one\n[00:03.250]two", attribution: "Source: test" };
                }
                if (id === "words") return { plain: "one\ntwo" };
                if (id === "quiet") return { instrumental: true };
                if (id === "broken") return { synced: "one\ntwo" };
                return null;
            }
            "#,
        );
        let provider = rig.provider();
        assert!(provider.info().capabilities.contains(Capabilities::LYRICS));
        assert!(provider.is_plugin());

        let timed = provider.lyrics(&track_id("timed")).await.unwrap().unwrap();
        assert!(timed.lyrics.synced);
        assert_eq!(timed.lyrics.lines[1].start_ms, Some(3_250));
        assert_eq!(timed.lyrics.attribution.as_deref(), Some("Source: test"));
        assert_eq!(timed.source, crate::lyrics::LyricsSource::Provider);

        let words = provider.lyrics(&track_id("words")).await.unwrap().unwrap();
        assert!(!words.lyrics.synced);
        assert_eq!(words.lyrics.lines.len(), 2);

        let quiet = provider.lyrics(&track_id("quiet")).await.unwrap().unwrap();
        assert!(quiet.instrumental);

        assert_eq!(provider.lyrics(&track_id("none")).await.unwrap(), None);

        let err = provider.lyrics(&track_id("broken")).await.unwrap_err();
        assert!(
            err.chain_text().contains("no `[mm:ss.xx]` stamp"),
            "{}",
            err.chain_text()
        );
    }

    /// api 5 (D-086): a video service — `search` and `video`, no `stream` —
    /// hands over YouTube ids; one of the wrong shape, and a file, are
    /// contract errors with their reason.
    #[tokio::test]
    async fn a_plugins_videos_are_checked_before_a_shell_gets_them() {
        let rig = Rig::new(
            "plugin-video",
            r#"{"name":"demo","display_name":"Demo","api":6,"artwork":false,"lyrics":false,"video":true,"related":false,"main":"main.js","capabilities":["search"]}"#,
            r#"
            export function health() { return { reachable: true }; }
            export function search() { return []; }
            export function resolve_video(id) {
                if (id === "good") return { kind: "youtube", video_id: "CCHdMIEGaaM" };
                if (id === "odd") return { kind: "youtube", video_id: "x\" onload=\"y" };
                if (id === "file") return { kind: "local_file", path: "/etc/passwd" };
                if (id === "other") return { kind: "vimeo", id: "1" };
                return null;
            }
            "#,
        );
        let provider = rig.provider();
        let caps = provider.info().capabilities;
        assert!(caps.contains(Capabilities::VIDEO));
        assert!(
            !caps.contains(Capabilities::STREAM),
            "a video service plays no sound of its own"
        );

        assert_eq!(
            provider.resolve_video(&track_id("good")).await.unwrap(),
            Some(crate::video::VideoSource::Youtube {
                video_id: "CCHdMIEGaaM".to_owned()
            })
        );
        assert_eq!(
            provider.resolve_video(&track_id("none")).await.unwrap(),
            None
        );
        for (id, why) in [
            ("odd", "is not a YouTube video id"),
            ("file", "cannot hand over a file"),
            ("other", "expected `{ kind: \"youtube\", video_id }`"),
        ] {
            let err = provider.resolve_video(&track_id(id)).await.unwrap_err();
            assert!(err.chain_text().contains(why), "{id}: {}", err.chain_text());
        }

        let rig = Rig::simple("plugin-no-video", BASIC);
        let err = rig
            .provider()
            .resolve_video(&track_id("42"))
            .await
            .unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::Unsupported { .. }),
            "{err:?}"
        );
    }

    /// `"lyrics": true` without the export does not start; with `false` the
    /// plugin says it cannot, and LRCLIB is asked instead.
    #[tokio::test]
    async fn declaring_lyrics_without_the_export_does_not_start() {
        let rig = Rig::new(
            "plugin-lyrics-missing",
            r#"{"name":"demo","display_name":"Demo","api":6,"artwork":false,"lyrics":true,"video":false,"related":false,"main":"main.js"}"#,
            "export function health() { return { reachable: true }; }",
        );
        let err = rig.provider().lyrics(&track_id("1")).await.unwrap_err();
        assert!(
            err.chain_text().contains("`lyrics`"),
            "{}",
            err.chain_text()
        );

        let rig = Rig::simple("plugin-no-lyrics", BASIC);
        let provider = rig.provider();
        assert!(!provider.info().capabilities.contains(Capabilities::LYRICS));
        let err = provider.lyrics(&track_id("42")).await.unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::Unsupported { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_script_answers_health_search_and_resolve() {
        let rig = Rig::simple("happy", BASIC);
        let provider = rig.provider();

        let health = provider.health().await.unwrap();
        assert!(health.reachable, "{:?}", health.detail);
        assert_eq!(health.detail.as_deref(), Some("call 1"));

        let tracks = provider.search("Geceler", 10).await.unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].id.provider, ProviderId::new("demo"));
        assert_eq!(tracks[0].track.title, "Geceler");
        assert!(tracks[0].track.isrc.is_some());
        assert!(
            tracks[1].track.isrc.is_none(),
            "a malformed ISRC must be dropped"
        );

        match provider.resolve_source(&track_id("7")).await.unwrap() {
            Some(AudioSource::HttpStream { url, .. }) => {
                assert_eq!(url, "https://a.cdn.example/7.mp3");
            }
            other => panic!("unexpected source: {other:?}"),
        }
        assert!(
            provider
                .resolve_source(&track_id("none"))
                .await
                .unwrap()
                .is_none(),
            "`null` is an answer"
        );
    }

    #[tokio::test]
    async fn a_throw_is_a_rejection_with_a_location_and_does_not_restart_the_engine() {
        let rig = Rig::simple(
            "thrower",
            r#"
            let calls = 0;
            export function health() { calls += 1; return { reachable: true, detail: String(calls) }; }
            export function search() {
                throw new Error("quota exceeded");
            }
            export function resolve_source() { return null; }
            "#,
        );
        let provider = rig.provider();
        assert_eq!(
            provider.health().await.unwrap().detail.as_deref(),
            Some("1")
        );

        let err = provider.search("x", 1).await.unwrap_err();
        match err.kind() {
            ErrorKind::PluginThrew {
                message, location, ..
            } => {
                assert_eq!(message, "quota exceeded");
                assert!(location.contains("main.js:"), "no location: {location}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(err.stage(), Stage::ProviderCall);

        // The same engine: the counter carries on where it left off.
        assert_eq!(
            provider.health().await.unwrap().detail.as_deref(),
            Some("2")
        );
    }

    #[tokio::test]
    async fn an_async_export_is_awaited() {
        let rig = Rig::simple(
            "async",
            r#"
            export async function health() { await null; return { reachable: true }; }
            export async function search(q) { const x = await Promise.resolve(q); return [{ id: "1", artist: "a", title: x }]; }
            export async function resolve_source() { throw new Error("async ret"); }
            "#,
        );
        let provider = rig.provider();
        assert!(provider.health().await.unwrap().reachable);
        assert_eq!(
            provider.search("word", 5).await.unwrap()[0].track.title,
            "word"
        );
        let err = provider.resolve_source(&track_id("1")).await.unwrap_err();
        assert!(
            err.chain_text().contains("async ret"),
            "{}",
            err.chain_text()
        );
    }

    #[tokio::test]
    async fn a_hanging_call_times_out_and_the_next_call_gets_a_fresh_engine() {
        let rig = Rig::simple(
            "hung",
            r#"
            let calls = 0;
            export function health() { calls += 1; return { reachable: true, detail: String(calls) }; }
            export function search() { for (;;) {} }
            export function resolve_source() { try { for (;;) {} } catch (_) { return null; } }
            "#,
        );
        let provider = rig
            .provider()
            .with_timeouts(Duration::from_secs(2), Duration::from_millis(300));
        assert_eq!(
            provider.health().await.unwrap().detail.as_deref(),
            Some("1")
        );

        let started = std::time::Instant::now();
        let err = provider.search("x", 1).await.unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::PluginTimeout { method, .. } if method == "search"),
            "{err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );

        // `try/catch` cannot catch the interrupt.
        let err = provider.resolve_source(&track_id("1")).await.unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::PluginTimeout { .. }),
            "{err:?}"
        );

        // A new engine: the counter starts from zero.
        assert_eq!(
            provider.health().await.unwrap().detail.as_deref(),
            Some("1")
        );
    }

    #[tokio::test]
    async fn a_hang_while_loading_times_out_at_the_start_stage() {
        let rig = Rig::simple("hung-on-load", "for (;;) {}\nexport function health() {}");
        let provider = rig
            .provider()
            .with_timeouts(Duration::from_millis(300), CALL);
        let started = std::time::Instant::now();
        let err = provider.search("x", 1).await.unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::PluginTimeout { method, .. } if method == "load"),
            "{err:?}"
        );
        assert_eq!(err.stage(), Stage::PluginStart);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn a_missing_export_is_a_contract_breach_and_is_not_retried() {
        let rig = Rig::simple(
            "missing",
            "export function health() { return { reachable: true }; }\n\
             export function search() { return []; }",
        );
        let provider = rig.provider();
        let err = provider.search("x", 1).await.unwrap_err();
        match err.kind() {
            ErrorKind::PluginContract { detail, .. } => {
                assert!(detail.contains("resolve_source"), "{detail}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(err.stage(), Stage::PluginStart);

        // Not retried: a contract violation is not fixed by a restart.
        let again = provider.search("x", 1).await.unwrap_err();
        assert!(matches!(again.kind(), ErrorKind::PluginCrashed { .. }));
        assert!(provider.state.lock().unwrap().starts == 1);
    }

    #[tokio::test]
    async fn a_script_that_keeps_failing_to_load_is_given_up_after_max_starts() {
        let rig = Rig::simple("always-failing", "throw new Error('blew up on start');");
        let provider = rig.provider();
        for _ in 0..MAX_STARTS {
            let err = provider.search("x", 1).await.unwrap_err();
            assert!(
                matches!(err.kind(), ErrorKind::PluginThrew { method, .. } if method == "load"),
                "{err:?}"
            );
        }
        let err = provider.search("x", 1).await.unwrap_err();
        assert!(
            err.chain_text().contains("giving up"),
            "{}",
            err.chain_text()
        );
    }

    #[tokio::test]
    async fn a_wrong_return_shape_is_a_contract_breach_not_an_empty_answer() {
        // api 1's `{ tracks: [...] }` shape: api 2 expects an array.
        let rig = Rig::simple(
            "shape",
            r#"
            export function health() { return { reachable: true }; }
            export function search() { return { tracks: [] }; }
            export function resolve_source() { return { kind: "teleport" }; }
            "#,
        );
        let provider = rig.provider();
        let err = provider.search("x", 1).await.unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::PluginContract { .. }),
            "{err:?}"
        );
        let err = provider.resolve_source(&track_id("1")).await.unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::PluginContract { .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn running_out_of_memory_is_an_error_not_a_crash() {
        let rig = Rig::simple(
            "memory",
            r#"
            export function health() { return { reachable: true }; }
            export function search() { const all = []; for (;;) all.push(new Array(100000).fill(1)); }
            export function resolve_source() { return null; }
            "#,
        );
        let provider = rig.provider();
        let err = provider.search("x", 1).await.unwrap_err();
        assert!(
            err.chain_text().contains("out of memory"),
            "{}",
            err.chain_text()
        );
        assert!(
            provider.health().await.unwrap().reachable,
            "the engine must stay up"
        );
    }

    #[tokio::test]
    async fn the_network_is_refused_outside_the_declared_hosts() {
        let rig = Rig::simple(
            "unpermitted",
            r#"
            export function health() { return { reachable: true }; }
            export function search() { host.http.get("https://evil.example/leak"); return []; }
            export function resolve_source() { return null; }
            "#,
        );
        let http = Arc::new(FakeHttp::new().route("evil.example", "{}"));
        let provider = rig.provider_with(http.clone(), &Secrets::default());
        let err = provider.search("x", 1).await.unwrap_err();
        assert!(
            err.chain_text().contains("not allowed"),
            "{}",
            err.chain_text()
        );
        assert!(
            err.chain_text().contains("evil.example"),
            "{}",
            err.chain_text()
        );
        assert!(
            http.requests().is_empty(),
            "an unpermitted request went out to the network"
        );
    }

    #[tokio::test]
    async fn a_declared_host_is_reached_and_the_response_comes_back_whole() {
        let rig = Rig::simple(
            "allowed",
            r#"
            export function health() { return { reachable: true }; }
            export function search(q) {
                const res = host.http.post("https://allowed.example/search", JSON.stringify({ q }),
                    { "Content-Type": "application/json" });
                const body = JSON.parse(res.body);
                return [{ id: String(res.status), artist: body.artist, title: res.url }];
            }
            export function resolve_source() { return null; }
            "#,
        );
        let http =
            Arc::new(FakeHttp::new().route("allowed.example/search", r#"{"artist":"Ezhel"}"#));
        let provider = rig.provider_with(http.clone(), &Secrets::default());
        let tracks = provider.search("geceler", 1).await.unwrap();
        assert_eq!(tracks[0].id.id, "200");
        assert_eq!(tracks[0].track.artist, "Ezhel");
        let seen = http.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, crate::net::HttpMethod::Post);
        assert_eq!(
            seen[0].body.as_deref(),
            Some(br#"{"q":"geceler"}"#.as_slice())
        );
    }

    /// If an allowed address redirects to a forbidden one, the engine stops **at
    /// the second step**: had the client followed the redirect itself, this would
    /// be invisible.
    #[tokio::test]
    async fn a_redirect_to_an_undeclared_host_is_refused_at_the_hop() {
        let rig = Rig::simple(
            "redirect",
            r#"
            export function health() { return { reachable: true }; }
            export function search(q) {
                const res = host.http.get(`https://allowed.example/${q}`);
                return [{ id: "1", artist: "a", title: res.body }];
            }
            export function resolve_source() { return null; }
            "#,
        );
        let http = Arc::new(
            FakeHttp::new()
                .route_redirect("allowed.example/escape", 302, "https://evil.example/x")
                .route_redirect("allowed.example/inner", 301, "/end")
                .route("allowed.example/end", "arrived")
                .route("evil.example", "leaked"),
        );
        let provider = rig.provider_with(http.clone(), &Secrets::default());

        let err = provider.search("escape", 1).await.unwrap_err();
        assert!(
            err.chain_text().contains("evil.example"),
            "{}",
            err.chain_text()
        );
        assert_eq!(
            http.requests().len(),
            1,
            "a request went to a forbidden address"
        );

        let tracks = provider.search("inner", 1).await.unwrap();
        assert_eq!(tracks[0].track.title, "arrived");
    }

    #[tokio::test]
    async fn the_network_is_refused_while_the_module_loads() {
        let rig = Rig::simple(
            "net-while-loading",
            r#"
            host.http.get("https://allowed.example/");
            export function health() { return { reachable: true }; }
            "#,
        );
        let http = Arc::new(FakeHttp::new().route("allowed.example", ""));
        let provider = rig.provider_with(http.clone(), &Secrets::default());
        let err = provider.health().await.unwrap();
        let detail = err.detail.unwrap_or_default();
        assert!(!err.reachable);
        assert!(detail.contains("while the module is loading"), "{detail}");
        assert!(http.requests().is_empty());
    }

    #[tokio::test]
    async fn a_stream_the_plugin_may_not_reach_is_refused() {
        let rig = Rig::simple(
            "stream",
            r#"
            export function health() { return { reachable: true }; }
            export function search() { return []; }
            export function resolve_source(id) {
                if (id === "local") return { kind: "local_file", path: "/etc/passwd" };
                return { kind: "http_stream", url: "http://192.168.1.1/admin", headers: [] };
            }
            "#,
        );
        let provider = rig.provider();
        let err = provider
            .resolve_source(&track_id("remote"))
            .await
            .unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::PluginContract { .. }),
            "{err:?}"
        );
        assert!(
            err.chain_text().contains("192.168.1.1"),
            "{}",
            err.chain_text()
        );

        let err = provider
            .resolve_source(&track_id("local"))
            .await
            .unwrap_err();
        assert!(
            err.chain_text().contains("local file"),
            "{}",
            err.chain_text()
        );
    }

    #[tokio::test]
    async fn secrets_are_scoped_and_a_secret_file_is_private_and_removed() {
        let rig = Rig::simple(
            "scoped-secrets",
            r#"
            export function health() {
                return { reachable: true, detail: `${host.secrets.get("token")}|${host.secrets.get("missing")}` };
            }
            export function search() { return []; }
            export function resolve_source() {
                return { kind: "http_stream", url: "https://a.cdn.example/x",
                         headers: [{ name: "X-Path", value: host.secrets.file("cookies") }] };
            }
            "#,
        );
        let mut secrets = Secrets::default();
        secrets.set("plugin:demo", "token", "mine");
        secrets.set("plugin:demo", "cookies", "# Netscape HTTP Cookie File\n");
        secrets.set("plugin:other", "token", "theirs");
        let provider = rig.provider_with(Arc::new(FakeHttp::new()), &secrets);

        assert_eq!(
            provider.health().await.unwrap().detail.as_deref(),
            Some("mine|null")
        );

        let path = match provider.resolve_source(&track_id("1")).await.unwrap() {
            Some(AudioSource::HttpStream { headers, .. }) => PathBuf::from(&headers[0].value),
            other => panic!("unexpected source: {other:?}"),
        };
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Netscape HTTP Cookie File\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the secret file is open to others");
        }

        drop(provider);
        assert!(
            !path.exists(),
            "the engine shut down but the secret file remained"
        );
    }

    #[tokio::test]
    async fn storage_survives_a_restart_and_a_corrupt_store_is_reported_not_reset() {
        let script = r#"
            export function health() {
                const seen = host.storage.get("counter");
                host.storage.set("counter", String(Number(seen ?? "0") + 1));
                return { reachable: true, detail: seen ?? "first" };
            }
            export function search() { host.storage.remove("counter"); return []; }
            export function resolve_source() { return null; }
        "#;
        let rig = Rig::simple("storage", script);
        assert_eq!(
            rig.provider().health().await.unwrap().detail.as_deref(),
            Some("first")
        );
        assert_eq!(
            rig.provider().health().await.unwrap().detail.as_deref(),
            Some("1")
        );

        let provider = rig.provider();
        provider.search("x", 1).await.unwrap();
        assert_eq!(
            provider.health().await.unwrap().detail.as_deref(),
            Some("first")
        );

        std::fs::write(
            rig.config.plugin_state_dir("demo").join("storage.json"),
            "{broken",
        )
        .unwrap();
        let health = rig.provider().health().await.unwrap();
        assert!(!health.reachable);
        assert!(
            health
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("store is corrupt"),
            "{:?}",
            health.detail
        );
    }

    #[tokio::test]
    async fn console_goes_to_the_log_and_does_not_break_the_call() {
        let rig = Rig::simple(
            "console",
            r#"
            console.log("loading", { a: 1 }, undefined);
            export function health() { console.warn(new Error("warning")); return { reachable: true }; }
            export function search() { return []; }
            export function resolve_source() { return null; }
            "#,
        );
        assert!(rig.provider().health().await.unwrap().reachable);
    }

    #[tokio::test]
    async fn the_host_object_cannot_be_replaced_by_the_plugin() {
        let rig = Rig::simple(
            "frozen",
            r#"
            "use strict";
            export function health() {
                try { host.http = null; return { reachable: false, detail: "changed" }; }
                catch (e) { return { reachable: true, detail: host.platform }; }
            }
            export function search() { return []; }
            export function resolve_source() { return null; }
            "#,
        );
        let health = rig.provider().health().await.unwrap();
        assert!(health.reachable, "{:?}", health.detail);
        assert_eq!(health.detail, Some(artifact::current_platform()));
    }

    #[cfg(unix)]
    mod tools {
        use super::*;
        use crate::plugin::artifact::sha256_hex;

        const TOOL: &str = "#!/bin/sh\necho \"arg:$1\"\necho \"error\" >&2\nexit 3\n";
        const SLOW: &str = "#!/bin/sh\nsleep 5\n";

        fn manifest(tool_sha: &str, slow_sha: &str) -> String {
            let platform = artifact::current_platform();
            format!(
                r#"{{"name":"demo","display_name":"Demo","api":6,"artwork":false,"lyrics":false,"video":false,"related":false,"main":"main.js",
                    "capabilities":["search"],
                    "requires":[
                      {{"name":"tool","version":"1","assets":{{"{platform}":
                        {{"url":"https://example.invalid/tool","sha256":"{tool_sha}"}}}}}},
                      {{"name":"slow","version":"1","assets":{{"{platform}":
                        {{"url":"https://example.invalid/slow","sha256":"{slow_sha}"}}}}}},
                      {{"name":"uninstalled","version":"1","assets":{{"{platform}":
                        {{"url":"https://example.invalid/u","sha256":"{}"}}}}}}
                    ]}}"#,
                "c".repeat(64)
            )
        }

        fn install(rig: &Rig, name: &str, body: &str) {
            use std::os::unix::fs::PermissionsExt as _;
            let manifest = PluginManifest::load(&rig.dir).unwrap();
            let requirement = manifest
                .requires
                .iter()
                .find(|r| r.name == name)
                .unwrap()
                .clone();
            let store = ArtifactStore::new(&rig.config);
            std::fs::create_dir_all(store.runtime_dir()).unwrap();
            let path = store.artifact_path(&requirement);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        const SCRIPT: &str = r#"
            export function health() { return { reachable: true }; }
            export function search(q) {
                const r = host.tools.run(q, ["one", "two"], { timeoutMs: 400 });
                return [{ id: String(r.code), artist: r.stdout.trim(), title: r.stderr.trim() }];
            }
        "#;

        /// Runs the slow tool with a budget longer than it takes, so only
        /// giving way can stop it; any other query answers at once.
        const PATIENT: &str = r#"
            export function health() { return { reachable: true }; }
            export function search(q) {
                if (q === "slow") {
                    const r = host.tools.run("slow", [], { timeoutMs: 8000 });
                    return [{ id: String(r.code), artist: "slow", title: "done" }];
                }
                return [{ id: "1", artist: "user", title: q }];
            }
        "#;

        #[tokio::test]
        async fn a_declared_installed_tool_runs_and_its_output_comes_back() {
            let rig = Rig::new(
                "tool",
                &manifest(&sha256_hex(TOOL.as_bytes()), &sha256_hex(SLOW.as_bytes())),
                SCRIPT,
            );
            install(&rig, "tool", TOOL);
            let tracks = rig.provider().search("tool", 1).await.unwrap();
            assert_eq!(tracks[0].id.id, "3", "exit code");
            assert_eq!(tracks[0].track.artist, "arg:one");
            assert_eq!(tracks[0].track.title, "error");
        }

        #[tokio::test]
        async fn only_declared_and_installed_tools_run_and_the_reason_is_said() {
            let rig = Rig::new(
                "no-tool",
                &manifest(&sha256_hex(TOOL.as_bytes()), &sha256_hex(SLOW.as_bytes())),
                SCRIPT,
            );
            let provider = rig.provider();

            let err = provider.search("/bin/sh", 1).await.unwrap_err();
            assert!(
                err.chain_text().contains("declared"),
                "{}",
                err.chain_text()
            );

            let err = provider.search("uninstalled", 1).await.unwrap_err();
            assert!(
                err.chain_text().contains("headshell plugin install demo"),
                "{}",
                err.chain_text()
            );

            // A file whose hash does not match is not run.
            install(&rig, "tool", "#!/bin/sh\necho tampered\n");
            let err = provider.search("tool", 1).await.unwrap_err();
            assert!(
                err.chain_text().contains("hash mismatch"),
                "{}",
                err.chain_text()
            );
        }

        /// D-077: a tool run by a call made ahead of time is stopped when a
        /// call for the user needs the engine — the user does not wait for
        /// the rest of it — and the call made ahead of time says it gave way,
        /// not that the track failed.
        #[tokio::test]
        async fn a_tool_run_ahead_of_time_is_stopped_for_a_call_for_the_user() {
            let rig = Rig::new(
                "gives-way",
                &manifest(&sha256_hex(TOOL.as_bytes()), &sha256_hex(SLOW.as_bytes())),
                PATIENT,
            );
            install(&rig, "slow", SLOW);
            gives_way(rig.provider()).await;
        }

        #[tokio::test]
        async fn a_tool_that_overruns_its_budget_is_stopped() {
            let rig = Rig::new(
                "slow",
                &manifest(&sha256_hex(TOOL.as_bytes()), &sha256_hex(SLOW.as_bytes())),
                SCRIPT,
            );
            install(&rig, "slow", SLOW);
            let started = std::time::Instant::now();
            let err = rig.provider().search("slow", 1).await.unwrap_err();
            assert!(
                err.chain_text().contains("did not finish"),
                "{}",
                err.chain_text()
            );
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{:?}",
                started.elapsed()
            );
        }
    }

    /// The Windows counterpart of the Unix tool tests (D-070).
    ///
    /// Windows has no shell scripts; a copy of the system's own `cmd.exe` is
    /// installed as the "tool", and its hash is computed at run time. What the
    /// engine is tested for is the same: only a declared, installed file whose
    /// hash matches runs; the output, the exit code and the time limit come back.
    #[cfg(windows)]
    mod tools_windows {
        use super::*;
        use crate::plugin::artifact::sha256_hex;

        fn system_cmd() -> Vec<u8> {
            let root = std::env::var_os("SystemRoot").expect("SystemRoot must be defined");
            std::fs::read(PathBuf::from(root).join("System32").join("cmd.exe"))
                .expect("cmd.exe must be readable")
        }

        /// `/D`: so the AutoRun commands in the registry do not run — the test must
        /// not depend on the machine's settings.
        const SCRIPT: &str = r#"
            export function health() { return { reachable: true }; }
            export function search(q) {
                const [tool, command] = q.split("|");
                const r = host.tools.run(tool, ["/D", "/C", command], { timeoutMs: 1500 });
                return [{ id: String(r.code), artist: r.stdout.trim(), title: r.stderr.trim() }];
            }
        "#;

        fn rig(name: &str) -> Rig {
            let body = system_cmd();
            let platform = artifact::current_platform();
            let rig = Rig::new(
                name,
                &format!(
                    r#"{{"name":"demo","display_name":"Demo","api":6,"artwork":false,"lyrics":false,"video":false,"related":false,"main":"main.js",
                        "capabilities":["search"],
                        "requires":[
                          {{"name":"tool","version":"1","assets":{{"{platform}":
                            {{"url":"https://example.invalid/tool","sha256":"{}"}}}}}},
                          {{"name":"uninstalled","version":"1","assets":{{"{platform}":
                            {{"url":"https://example.invalid/u","sha256":"{}"}}}}}}
                        ]}}"#,
                    sha256_hex(&body),
                    "c".repeat(64)
                ),
                SCRIPT,
            );
            install(&rig, "tool", &body);
            rig
        }

        fn install(rig: &Rig, name: &str, body: &[u8]) {
            let manifest = PluginManifest::load(&rig.dir).unwrap();
            let requirement = manifest
                .requires
                .iter()
                .find(|r| r.name == name)
                .unwrap()
                .clone();
            let store = ArtifactStore::new(&rig.config);
            std::fs::create_dir_all(store.runtime_dir()).unwrap();
            std::fs::write(store.artifact_path(&requirement), body).unwrap();
        }

        #[tokio::test]
        async fn a_declared_installed_tool_runs_and_its_output_comes_back() {
            let rig = rig("tool-win");
            let tracks = rig
                .provider()
                .search("tool|echo arg:one& echo error 1>&2& exit /b 3", 1)
                .await
                .unwrap();
            assert_eq!(tracks[0].id.id, "3", "exit code");
            assert_eq!(tracks[0].track.artist, "arg:one");
            assert_eq!(tracks[0].track.title, "error");
        }

        #[tokio::test]
        async fn only_declared_and_installed_tools_run_and_the_reason_is_said() {
            let rig = rig("no-tool-win");
            let provider = rig.provider();

            let err = provider
                .search(r"C:\Windows\System32\cmd.exe|echo x", 1)
                .await
                .unwrap_err();
            assert!(
                err.chain_text().contains("declared"),
                "{}",
                err.chain_text()
            );

            let err = provider.search("uninstalled|echo x", 1).await.unwrap_err();
            assert!(
                err.chain_text().contains("headshell plugin install demo"),
                "{}",
                err.chain_text()
            );

            // A file whose hash does not match is not run.
            install(&rig, "tool", b"MZ tampered");
            let fresh = rig.provider();
            let err = fresh.search("tool|echo x", 1).await.unwrap_err();
            assert!(
                err.chain_text().contains("hash mismatch"),
                "{}",
                err.chain_text()
            );
        }

        /// D-077, as on Unix: a call for the user stops a tool run ahead of
        /// time.
        #[tokio::test]
        async fn a_tool_run_ahead_of_time_is_stopped_for_a_call_for_the_user() {
            let rig = rig("gives-way-win");
            std::fs::write(
                rig.dir.join("main.js"),
                r#"
                export function health() { return { reachable: true }; }
                export function search(q) {
                    if (q === "slow") {
                        const r = host.tools.run("tool", ["/D", "/C", "ping -n 6 127.0.0.1 >nul"], { timeoutMs: 8000 });
                        return [{ id: String(r.code), artist: "slow", title: "done" }];
                    }
                    return [{ id: "1", artist: "user", title: q }];
                }
            "#,
            )
            .unwrap();
            gives_way(rig.provider()).await;
        }

        #[tokio::test]
        async fn a_tool_that_overruns_its_budget_is_stopped() {
            let rig = rig("slow-win");
            let started = std::time::Instant::now();
            // A command that waits ~5 s; the time limit is 1.5 s.
            let err = rig
                .provider()
                .search("tool|ping -n 6 127.0.0.1 >nul", 1)
                .await
                .unwrap_err();
            assert!(
                err.chain_text().contains("did not finish"),
                "{}",
                err.chain_text()
            );
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "{:?}",
                started.elapsed()
            );
        }
    }
}
