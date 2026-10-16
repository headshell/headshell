//! Unit tests of the plugin catalog (D-071).
//!
//! The catalog repository is set up inside the test: plugins are written to a
//! temporary directory, the index is produced with [`build_index`] — that is,
//! the same way the maintainer produces it — and a fake HTTP client serves the
//! index and the files. Nothing goes online; the hash, manifest and origin
//! record rules are tested on disk.

use std::sync::Arc;

use super::*;
use crate::net::fake::FakeHttp;
use crate::plugin::consent::ConsentStatus;
use crate::test_support::{TempDir, TestConfig};

const INDEX: &str = "https://catalog.example/refs/heads/main/index.json";
const TEMPLATE: &str = "https://catalog.example/refs/tags/{name}-{version}/{name}/{path}";
const PLATFORM: &str = "linux-x86_64";

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    crate::plugin::block_on(future)
}

fn temp_config(label: &str) -> TestConfig {
    TestConfig::new(&format!("catalog-{label}"))
}

fn manifest_json(name: &str, version: &str, net: &[&str]) -> String {
    format!(
        r#"{{"name":"{name}","display_name":"{name} plugin","version":"{version}","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"main.js","capabilities":["search"],"permissions":{{"net":{}}}}}"#,
        serde_json::to_string(net).unwrap()
    )
}

fn script(tag: &str) -> String {
    format!(
        "export function health() {{ return {{ reachable: true, detail: \"{tag}\" }}; }}\nexport function search() {{ return []; }}\n"
    )
}

/// A catalog repository set up inside the test.
struct Source {
    dir: TempDir,
}

impl Source {
    fn new(label: &str) -> Self {
        Self {
            dir: TempDir::new(&format!("catalog-source-{label}")),
        }
    }

    fn plugin(&self, name: &str, version: &str, net: &[&str], main: &str) -> &Self {
        let dir = self.dir.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.json"), manifest_json(name, version, net)).unwrap();
        std::fs::write(dir.join("main.js"), main).unwrap();
        self
    }

    fn build(&self) -> BuiltIndex {
        build_index(&self.dir, TEMPLATE).unwrap_or_else(|err| panic!("{}", err.chain_text()))
    }

    /// A fake client that serves the index and every file.
    fn http(&self) -> FakeHttp {
        self.http_with_index(&self.build().json)
    }

    fn http_with_index(&self, index: &str) -> FakeHttp {
        let built = self.build();
        let mut http = FakeHttp::new().route("index.json", index);
        for plugin in &built.plugins {
            for file in &plugin.files {
                let body =
                    std::fs::read_to_string(self.dir.join(&plugin.name).join(&file.path)).unwrap();
                let pattern = format!(
                    "{}-{}/{}/{}",
                    plugin.name, plugin.version, plugin.name, file.path
                );
                http = http.route(&pattern, &body);
            }
        }
        http
    }
}

fn catalog_of(source: &Source) -> Catalog {
    Catalog::parse(INDEX, source.build().json.as_bytes()).unwrap()
}

fn install_from(config: &Config, source: &Source, name: &str) -> CatalogFetch {
    let http = source.http();
    run(async {
        let catalog = fetch(&http, INDEX).await.unwrap();
        install(config, &http, &catalog, name).await
    })
    .unwrap_or_else(|err| panic!("{}", err.chain_text()))
}

fn state(config: &Config, source: &Source, name: &str) -> InstallState {
    let catalog = catalog_of(source);
    let survey = catalog.survey(config).unwrap();
    survey
        .plugins
        .into_iter()
        .find(|plugin| plugin.name == name)
        .map(|plugin| plugin.installed)
        .unwrap()
}

// --- address ------------------------------------------------------------

#[test]
fn the_index_url_comes_from_the_environment_or_the_default() {
    assert_eq!(resolve_index_url(&|_| None), DEFAULT_INDEX_URL);
    let mirror =
        |key: &str| (key == INDEX_ENV).then(|| " https://mirror.example/index.json ".to_owned());
    assert_eq!(
        resolve_index_url(&mirror),
        "https://mirror.example/index.json"
    );
    let blank = |key: &str| (key == INDEX_ENV).then(|| "   ".to_owned());
    assert_eq!(resolve_index_url(&blank), DEFAULT_INDEX_URL);
}

/// The index is the root of trust: plain HTTP only to this machine itself.
#[test]
fn a_catalog_over_plain_http_is_refused_unless_it_is_this_machine() {
    let http = FakeHttp::new().route("index.json", r#"{"schema":1,"plugins":[]}"#);
    let err = run(fetch(&http, "http://catalog.example/index.json")).unwrap_err();
    assert_eq!(err.stage(), Stage::PluginCatalog);
    assert!(
        err.chain_text().contains("https://"),
        "{}",
        err.chain_text()
    );
    assert!(
        http.requests().is_empty(),
        "a request went to the refused address"
    );

    let local = run(fetch(&http, "http://127.0.0.1:8080/index.json")).unwrap();
    assert!(local.names().is_empty());
}

// --- building the index -------------------------------------------------

#[test]
fn a_built_index_lists_every_plugin_with_version_pinned_urls_and_hashes() {
    let source = Source::new("build");
    source
        .plugin("ytmusic", "0.4.0", &["music.youtube.com"], &script("yt"))
        .plugin("echo", "0.1.0", &["example.invalid"], &script("echo"));
    let built = source.build();

    let names: Vec<&str> = built.plugins.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["echo", "ytmusic"],
        "the order must be deterministic"
    );
    let echo = &built.plugins[0];
    assert_eq!(echo.version, "0.1.0");
    let paths: Vec<&str> = echo.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["plugin.json", "main.js"]);
    assert_eq!(
        echo.files[1].url,
        "https://catalog.example/refs/tags/echo-0.1.0/echo/main.js"
    );
    assert_eq!(echo.files[1].sha256, sha256_hex(script("echo").as_bytes()));

    assert!(built.json.ends_with('\n'));
    assert_eq!(
        source.build().json,
        built.json,
        "the same directory must produce the same text"
    );
    assert_eq!(
        read_url_template(&{
            let path = source.dir.join(INDEX_FILE);
            std::fs::write(&path, &built.json).unwrap();
            path
        })
        .unwrap()
        .as_deref(),
        Some(TEMPLATE)
    );
}

/// All the broken plugins are named at once, each with its own reason.
#[test]
fn build_index_refuses_every_invalid_plugin_at_once() {
    let source = Source::new("broken");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let unversioned = source.dir.join("unversioned");
    std::fs::create_dir_all(&unversioned).unwrap();
    std::fs::write(
        unversioned.join("plugin.json"),
        r#"{"name":"unversioned","display_name":"S","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"main.js"}"#,
    )
    .unwrap();
    std::fs::write(unversioned.join("main.js"), "").unwrap();
    let upper = source.dir.join("Evil");
    std::fs::create_dir_all(&upper).unwrap();
    std::fs::write(
        upper.join("plugin.json"),
        manifest_json("Evil", "1.0.0", &[]),
    )
    .unwrap();
    std::fs::write(upper.join("main.js"), "").unwrap();

    let err = build_index(&source.dir, TEMPLATE).unwrap_err();
    let text = err.chain_text();
    assert!(
        text.contains("2 plugins could not go into the index"),
        "{text}"
    );
    assert!(text.contains("unversioned: no `version`"), "{text}");
    assert!(text.contains("Evil: catalog name"), "{text}");
}

#[test]
fn build_index_skips_hidden_dirs_and_refuses_an_empty_catalog() {
    let source = Source::new("empty");
    let hidden = source.dir.join(".git");
    std::fs::create_dir_all(&hidden).unwrap();
    std::fs::write(hidden.join("plugin.json"), "{}").unwrap();
    let err = build_index(&source.dir, TEMPLATE).unwrap_err();
    assert!(
        err.chain_text().contains("no plugins in the directory"),
        "{}",
        err.chain_text()
    );
}

#[test]
fn a_template_must_pin_the_version_and_use_https() {
    let source = Source::new("template");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    for (template, expected) in [
        ("https://k.example/main/{name}/{path}", "has no `{version}`"),
        ("http://k.example/{name}-{version}/{path}", "https://"),
    ] {
        let err = build_index(&source.dir, template).unwrap_err();
        assert!(
            err.chain_text().contains(expected),
            "{template}: {}",
            err.chain_text()
        );
    }
    assert!(build_index(&source.dir, "http://127.0.0.1:9/{name}/{version}/{path}").is_ok());
}

#[test]
fn index_differences_name_what_changed() {
    let source = Source::new("diff");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let before = source.build().json;
    source
        .plugin("echo", "0.2.0", &[], &script("e2"))
        .plugin("new", "1.0.0", &[], &script("y"));
    let after = source.build().json;
    let differences = index_differences(&before, &after);
    assert!(
        differences.contains(&"echo: its entry changed".to_owned()),
        "{differences:?}"
    );
    assert!(
        differences.contains(&"new: not in the index".to_owned()),
        "{differences:?}"
    );
}

// --- reading the index --------------------------------------------------

#[test]
fn a_catalog_reads_back_what_build_index_wrote() {
    let config = temp_config("read");
    let source = Source::new("read");
    source
        .plugin("echo", "0.1.0", &["example.invalid"], &script("e"))
        .plugin("second", "2.0.0", &[], &script("i"));
    let catalog = catalog_of(&source);
    assert_eq!(catalog.names(), vec!["echo", "second"]);

    let survey = catalog.survey(&config).unwrap();
    assert!(survey.delisted.is_empty());
    assert_eq!(
        survey.summary,
        CatalogSummary {
            listed: 2,
            installable: 2,
            installed: 0,
            updates: 0,
            problems: 0,
        }
    );
    let echo = &survey.plugins[0];
    assert_eq!(echo.installed, InstallState::NotInstalled);
    assert_eq!(echo.permissions.net, vec!["example.invalid".to_owned()]);
    assert_eq!(echo.display_name.as_deref(), Some("echo plugin"));
    assert!(echo.problem.is_none(), "{:?}", echo.problem);
}

#[test]
fn an_unknown_schema_is_refused_with_what_to_do() {
    let err = Catalog::parse(INDEX, br#"{"schema":2,"plugins":[]}"#).unwrap_err();
    assert!(
        err.chain_text().contains("update headshell"),
        "{}",
        err.chain_text()
    );
    let err = Catalog::parse(INDEX, br#"{"plugins":[]}"#).unwrap_err();
    assert!(
        err.chain_text().contains("`schema`"),
        "{}",
        err.chain_text()
    );
    let err = Catalog::parse(INDEX, b"<html>404</html>").unwrap_err();
    assert!(err.chain_text().contains("JSON"), "{}", err.chain_text());
}

/// A broken entry does not hide the others; each carries its own reason.
#[test]
fn a_bad_entry_is_reported_without_hiding_the_others() {
    let source = Source::new("entry");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let mut index: serde_json::Value = serde_json::from_str(&source.build().json).unwrap();
    let good = index["plugins"][0].clone();

    let mut newer = good.clone();
    newer["manifest"]["name"] = "new".into();
    newer["manifest"]["api"] = 8.into();

    let mut escaping = good.clone();
    escaping["manifest"]["name"] = "escaping".into();
    escaping["files"][1]["path"] = "../../main.js".into();

    let mut plain = good.clone();
    plain["manifest"]["name"] = "plain".into();
    plain["files"][1]["url"] = "http://evil.example/main.js".into();

    let mut extra = good.clone();
    extra["manifest"]["name"] = "extra".into();
    extra["files"].as_array_mut().unwrap().push(
        serde_json::json!({"path":"state/x.js","url":"https://k.example/x","sha256":"a".repeat(64)}),
    );

    let twin = good.clone();
    let mut other_twin = good.clone();
    other_twin["manifest"]["display_name"] = "twin".into();

    index["plugins"] = serde_json::json!([newer, escaping, plain, extra, twin, other_twin]);
    let catalog = Catalog::parse(INDEX, index.to_string().as_bytes()).unwrap();
    let config = temp_config("entry");
    let survey = catalog.survey(&config).unwrap();
    assert_eq!(survey.summary.listed, 6);
    assert_eq!(survey.summary.problems, 6);

    let problem = |name: &str| {
        survey
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .and_then(|plugin| plugin.problem.clone())
            .unwrap_or_default()
    };
    assert!(
        problem("new").contains("plugin api 8"),
        "{}",
        problem("new")
    );
    assert!(
        problem("escaping").contains("is not a valid plugin file path"),
        "{}",
        problem("escaping")
    );
    assert!(
        problem("plain").contains("https://"),
        "{}",
        problem("plain")
    );
    assert!(
        problem("extra").contains("the engine uses in the plugin directory"),
        "{}",
        problem("extra")
    );
    assert!(
        problem("echo").contains("more than once"),
        "{}",
        problem("echo")
    );

    let err = run(install(&config, &FakeHttp::new(), &catalog, "echo")).unwrap_err();
    assert!(
        err.chain_text().contains("cannot be installed"),
        "{}",
        err.chain_text()
    );
}

#[test]
fn a_name_not_in_the_catalog_lists_what_is_there_and_suggests_the_right_case() {
    let source = Source::new("name");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let catalog = catalog_of(&source);
    let err = catalog.find("Echo").unwrap_err();
    let text = err.chain_text();
    assert!(text.contains("did you mean `echo`"), "{text}");
    assert!(text.contains("in the catalog: echo"), "{text}");
}

// --- installing ---------------------------------------------------------

#[test]
fn install_puts_the_plugin_in_place_with_its_origin_and_it_awaits_approval() {
    let config = temp_config("install");
    let source = Source::new("install");
    source.plugin("echo", "0.1.0", &["example.invalid"], &script("e"));

    let fetched = install_from(&config, &source, "echo");
    assert_eq!(fetched.version, "0.1.0");
    assert_eq!(fetched.index, INDEX);

    let dir = config.plugins_dir().join("echo");
    assert_eq!(
        std::fs::read_to_string(dir.join("main.js")).unwrap(),
        script("e")
    );
    let record = read_record(&dir).unwrap().unwrap();
    assert_eq!(record.version, "0.1.0");
    assert_eq!(record.index, INDEX);
    assert_eq!(record.files.len(), 2);

    // Coming from the catalog is not consent (D-040).
    let (entries, _) = crate::plugin::discover(&config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].consent, Some(ConsentStatus::NotAsked));
    assert!(entries[0].problem.is_none(), "{:?}", entries[0].problem);

    assert_eq!(
        state(&config, &source, "echo"),
        InstallState::Current {
            version: "0.1.0".to_owned()
        }
    );
    assert_no_staging_left(&config);
}

fn assert_no_staging_left(config: &Config) {
    let leftovers: Vec<String> = std::fs::read_dir(config.data_dir())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".installing"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a temporary install directory was left behind: {leftovers:?}"
    );
}

/// If one file's hash does not match, **nothing** is written to disk.
#[test]
fn a_hash_mismatch_writes_nothing() {
    let config = temp_config("hash");
    let source = Source::new("hash");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let http = FakeHttp::new()
        .route("index.json", &source.build().json)
        .route("echo/plugin.json", &manifest_json("echo", "0.1.0", &[]))
        .route(
            "echo/main.js",
            "export function health() { /* modified */ }",
        );

    let err = run(async {
        let catalog = fetch(&http, INDEX).await.unwrap();
        install(&config, &http, &catalog, "echo").await
    })
    .unwrap_err();
    assert_eq!(err.stage(), Stage::PluginCatalog);
    assert!(
        err.chain_text().contains("hash mismatch"),
        "{}",
        err.chain_text()
    );
    assert!(
        err.chain_text().contains("nothing was written"),
        "{}",
        err.chain_text()
    );
    assert!(!config.plugins_dir().join("echo").exists());
}

/// The permissions shown in the catalog must be the installed plugin's.
#[test]
fn a_manifest_that_differs_from_the_index_is_refused() {
    let config = temp_config("manifest");
    let source = Source::new("manifest");
    source.plugin("echo", "0.1.0", &["example.invalid"], &script("e"));
    let mut index: serde_json::Value = serde_json::from_str(&source.build().json).unwrap();
    // The index shows fewer permissions; the downloaded file (hash correct) asks for more.
    index["plugins"][0]["manifest"]["permissions"]["net"] = serde_json::json!([]);
    let http = source.http_with_index(&index.to_string());

    let err = run(async {
        let catalog = fetch(&http, INDEX).await.unwrap();
        install(&config, &http, &catalog, "echo").await
    })
    .unwrap_err();
    assert!(
        err.chain_text()
            .contains("differs from what the index showed"),
        "{}",
        err.chain_text()
    );
    assert!(!config.plugins_dir().join("echo").exists());
}

/// An index pointing at a version that does not exist is the maintainer's fault, not the network's.
#[test]
fn a_missing_file_is_the_catalogs_fault_not_the_network() {
    let config = temp_config("missing");
    let source = Source::new("missing");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let http = FakeHttp::new()
        .route("index.json", &source.build().json)
        .route("echo/plugin.json", &manifest_json("echo", "0.1.0", &[]))
        .route_status("echo/main.js", 404, "Not Found");

    let err = run(async {
        let catalog = fetch(&http, INDEX).await.unwrap();
        install(&config, &http, &catalog, "echo").await
    })
    .unwrap_err();
    assert_eq!(err.stage(), Stage::PluginCatalog);
    assert!(
        err.chain_text().contains("file not found (HTTP 404"),
        "{}",
        err.chain_text()
    );
}

#[test]
fn installing_over_an_existing_directory_is_refused() {
    let config = temp_config("present");
    let source = Source::new("present");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let dir = config.plugins_dir().join("echo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.js"), "// the developer's copy").unwrap();

    let http = source.http();
    let err = run(async {
        let catalog = fetch(&http, INDEX).await.unwrap();
        install(&config, &http, &catalog, "echo").await
    })
    .unwrap_err();
    assert!(
        err.chain_text().contains("already installed"),
        "{}",
        err.chain_text()
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("main.js")).unwrap(),
        "// the developer's copy"
    );
}

// --- updating -----------------------------------------------------------

fn update_from(config: &Config, source: &Source, name: &str) -> Result<UpdateOutcome> {
    let http = source.http();
    run(async {
        let catalog = fetch(&http, INDEX).await?;
        update(config, &http, &catalog, name, PLATFORM).await
    })
}

#[test]
fn update_brings_a_catalog_plugin_to_the_new_version_and_keeps_its_state() {
    let config = temp_config("update");
    let source = Source::new("update");
    source.plugin("echo", "0.1.0", &["a.example"], &script("v1"));
    install_from(&config, &source, "echo");
    let state_file = config.plugin_state_dir("echo").join("storage.json");
    std::fs::create_dir_all(state_file.parent().unwrap()).unwrap();
    std::fs::write(&state_file, r#"{"client_id":"keep"}"#).unwrap();

    source.plugin("echo", "0.2.0", &["a.example", "b.example"], &script("v2"));
    assert_eq!(
        state(&config, &source, "echo"),
        InstallState::UpdateAvailable {
            installed: "0.1.0".to_owned(),
            available: "0.2.0".to_owned()
        }
    );

    match update_from(&config, &source, "echo").unwrap() {
        UpdateOutcome::Updated {
            from,
            to,
            permissions_added,
            tools_changed,
        } => {
            assert_eq!((from.as_str(), to.as_str()), ("0.1.0", "0.2.0"));
            assert_eq!(permissions_added.net, vec!["b.example".to_owned()]);
            assert!(tools_changed.is_empty(), "{tools_changed:?}");
        }
        other => panic!("not updated: {other:?}"),
    }
    let dir = config.plugins_dir().join("echo");
    assert_eq!(
        std::fs::read_to_string(dir.join("main.js")).unwrap(),
        script("v2")
    );
    assert_eq!(
        std::fs::read_to_string(&state_file).unwrap(),
        r#"{"client_id":"keep"}"#,
        "the plugin's store must not be deleted by an update"
    );
    assert_eq!(read_record(&dir).unwrap().unwrap().version, "0.2.0");
    assert_eq!(
        update_from(&config, &source, "echo").unwrap(),
        UpdateOutcome::Current {
            version: "0.2.0".to_owned()
        }
    );
    let leftovers: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".downloading"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a temporary file was left behind: {leftovers:?}"
    );
}

/// A plugin put there or changed by hand is not overwritten.
#[test]
fn update_leaves_a_manual_or_modified_plugin_alone() {
    let config = temp_config("hands-off");
    let source = Source::new("hands-off");
    source
        .plugin("manual", "0.1.0", &[], &script("manual"))
        .plugin("echo", "0.1.0", &[], &script("v1"));

    let manual = config.plugins_dir().join("manual");
    std::fs::create_dir_all(&manual).unwrap();
    std::fs::write(manual.join("main.js"), "// working copy").unwrap();
    assert_eq!(state(&config, &source, "manual"), InstallState::Manual);

    install_from(&config, &source, "echo");
    let main = config.plugins_dir().join("echo").join("main.js");
    std::fs::write(&main, "// fixed by hand").unwrap();
    source
        .plugin("manual", "0.2.0", &[], &script("manual2"))
        .plugin("echo", "0.2.0", &[], &script("v2"));
    assert_eq!(
        state(&config, &source, "echo"),
        InstallState::Modified {
            version: "0.1.0".to_owned(),
            files: vec!["main.js".to_owned()]
        }
    );

    for name in ["manual", "echo"] {
        match update_from(&config, &source, name).unwrap() {
            UpdateOutcome::Skipped { reason } => {
                assert!(reason.contains("overwrit"), "{name}: {reason}");
            }
            other => panic!("{name} should not have been updated: {other:?}"),
        }
    }
    assert_eq!(std::fs::read_to_string(&main).unwrap(), "// fixed by hand");
    assert_eq!(
        std::fs::read_to_string(manual.join("main.js")).unwrap(),
        "// working copy"
    );
}

/// The new file left behind by an update that stopped halfway does not count
/// as a local change — otherwise the user could only fix it by removing and
/// reinstalling.
#[test]
fn a_half_finished_update_is_not_mistaken_for_a_local_edit() {
    let config = temp_config("half");
    let source = Source::new("half");
    source.plugin("echo", "0.1.0", &[], &script("v1"));
    install_from(&config, &source, "echo");
    source.plugin("echo", "0.2.0", &[], &script("v2"));
    std::fs::write(
        config.plugins_dir().join("echo").join("main.js"),
        script("v2"),
    )
    .unwrap();

    assert!(matches!(
        state(&config, &source, "echo"),
        InstallState::UpdateAvailable { .. }
    ));
    assert!(matches!(
        update_from(&config, &source, "echo").unwrap(),
        UpdateOutcome::Updated { .. }
    ));
}

/// A plugin pulled from the catalog must not silently look "up to date".
#[test]
fn a_plugin_delisted_from_its_catalog_is_reported() {
    let config = temp_config("pulled");
    let source = Source::new("pulled");
    source.plugin("echo", "0.1.0", &[], &script("e")).plugin(
        "remaining",
        "1.0.0",
        &[],
        &script("k"),
    );
    install_from(&config, &source, "echo");
    std::fs::remove_dir_all(source.dir.join("echo")).unwrap();

    let catalog = catalog_of(&source);
    let survey = catalog.survey(&config).unwrap();
    assert_eq!(survey.delisted, vec!["echo".to_owned()]);
    assert_eq!(
        catalog.update_candidates(&config).unwrap(),
        vec!["echo".to_owned()]
    );
    match update_from(&config, &source, "echo").unwrap() {
        UpdateOutcome::Skipped { reason } => {
            assert!(reason.contains("not in the catalog"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn updating_a_plugin_that_is_not_installed_says_how_to_install_it() {
    let config = temp_config("not-installed");
    let source = Source::new("not-installed");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let err = update_from(&config, &source, "echo").unwrap_err();
    assert!(
        err.chain_text().contains("headshell plugin install echo"),
        "{}",
        err.chain_text()
    );
}

/// A tool change does not ask for consent (the user's decision, D-071), but
/// it is said.
#[test]
fn tool_changes_are_named_for_this_platform() {
    use crate::plugin::manifest::Asset;
    let tool = |version: &str, sha: char| Requirement {
        name: "yt-dlp".to_owned(),
        version: version.to_owned(),
        assets: [(
            PLATFORM.to_owned(),
            Asset {
                url: format!("https://example/{version}"),
                sha256: sha.to_string().repeat(64),
            },
        )]
        .into_iter()
        .collect(),
    };

    let bumped = tool_changes(&[tool("1", 'a')], &[tool("2", 'b')], PLATFORM);
    assert_eq!(bumped.len(), 1);
    assert_eq!(bumped[0].describe(), "yt-dlp 1 → 2");

    let rebuilt = tool_changes(&[tool("1", 'a')], &[tool("1", 'b')], PLATFORM);
    assert!(rebuilt[0].binary_changed);
    assert!(
        rebuilt[0].describe().contains("binary changed"),
        "{}",
        rebuilt[0].describe()
    );

    assert_eq!(
        tool_changes(&[], &[tool("1", 'a')], PLATFORM)[0].describe(),
        "yt-dlp 1 added"
    );
    assert_eq!(
        tool_changes(&[tool("1", 'a')], &[], PLATFORM)[0].describe(),
        "yt-dlp 1 no longer requested"
    );
    assert!(tool_changes(&[tool("1", 'a')], &[tool("1", 'a')], PLATFORM).is_empty());
    // Another platform's binary changed: no change for this machine.
    assert!(tool_changes(&[tool("1", 'a')], &[tool("1", 'b')], "windows-x86_64").is_empty());
}

// --- removing -----------------------------------------------------------

#[test]
fn remove_deletes_the_directory_with_its_state() {
    let config = temp_config("remove");
    let source = Source::new("remove");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    install_from(&config, &source, "echo");
    std::fs::create_dir_all(config.plugin_state_dir("echo")).unwrap();

    let removed = remove(&config, "echo").unwrap();
    assert_eq!(removed.path, config.plugins_dir().join("echo"));
    assert!(removed.link_target.is_none());
    assert!(!removed.path.exists());
}

/// Removing appends the name to a path: a name that reaches outside the data
/// directory is refused.
#[test]
fn remove_refuses_a_name_that_leaves_the_plugins_dir() {
    let config = temp_config("escape");
    let outside = config.data_dir().join("victim");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(config.plugins_dir()).unwrap();

    let err = remove(&config, "../victim").unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::InvalidInput { .. }),
        "{err:?}"
    );
    assert!(
        outside.exists(),
        "something outside the plugin directory was deleted"
    );

    let err = remove(&config, "missing").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::NotFound { .. }), "{err:?}");
}

/// A link to the developer's working copy: the link goes, the copy stays.
#[cfg(unix)]
#[test]
fn remove_takes_away_only_the_link_of_a_symlinked_plugin() {
    let config = temp_config("link");
    let work = TempDir::new("catalog-working-copy");
    std::fs::write(work.join("main.js"), "// source").unwrap();
    std::fs::create_dir_all(config.plugins_dir()).unwrap();
    let link = config.plugins_dir().join("dev");
    std::os::unix::fs::symlink(work.path(), &link).unwrap();

    let removed = remove(&config, "dev").unwrap();
    assert_eq!(removed.link_target.as_deref(), Some(work.path()));
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the link is still there"
    );
    assert_eq!(
        std::fs::read_to_string(work.join("main.js")).unwrap(),
        "// source"
    );
}

#[test]
fn install_state_says_manual_for_a_plugin_placed_by_hand() {
    let config = temp_config("manual");
    let source = Source::new("manual");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let dir = config.plugins_dir().join("echo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(ORIGIN_FILE), "{ broken").unwrap();
    match state(&config, &source, "echo") {
        InstallState::Unreadable { detail } => assert!(detail.contains("origin.json"), "{detail}"),
        other => panic!("{other:?}"),
    }
    std::fs::remove_file(dir.join(ORIGIN_FILE)).unwrap();
    assert_eq!(state(&config, &source, "echo"), InstallState::Manual);
}

#[test]
fn the_http_client_is_only_used_through_the_trait() {
    // `Arc<dyn HttpClient>` — the form Session receives from its callers (K7).
    let source = Source::new("dyn");
    source.plugin("echo", "0.1.0", &[], &script("e"));
    let http: Arc<dyn HttpClient> = Arc::new(source.http());
    let catalog = run(fetch(http.as_ref(), INDEX)).unwrap();
    assert_eq!(catalog.names(), vec!["echo"]);
}
