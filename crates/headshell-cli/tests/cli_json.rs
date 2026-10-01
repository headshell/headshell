//! Snapshot tests of the CLI's `--json` output.
//!
//! The goal is twofold: (1) the scripting contract must not break by
//! accident, (2) it stands as proof that the GUI gets the same data.
//!
//! To update the snapshots: `UPDATE_SNAPSHOTS=1 cargo test -p headshell-cli`

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Fields that change from run to run — pinned before comparing.
const VOLATILE_KEYS: &[&str] = &[
    "started_at",
    "finished_at",
    "data_dir",
    "headshell_version",
    "os",
    "arch",
    "command",
    "source",
    // The server record file's path depends on the temporary directory.
    "path",
    // So does the plugin directory.
    "dir",
    // The platform the tool state was measured for: depends on the running
    // machine (D-071).
    "platform",
    // What `artwork` looked at: a file's path depends on the checkout (D-076).
    "subject",
    // How long resolving an address took (D-077).
    "resolve_ms",
];

fn fixtures() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures"))
}

/// A temporary directory that deletes itself — in a failing test too (`Drop`
/// runs during a panic).
///
/// The root is Cargo's `CARGO_TARGET_TMPDIR` (`target/tmp`), not the
/// operating system's shared `/tmp`: tests once left directories there, and
/// 1.2 GB piled up on a development machine (D-070).
struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for TempDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_dir_all(&self.0)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "warning: could not delete the temporary directory ({}): {err}",
                self.0.display()
            );
        }
    }
}

fn temp_dir(label: &str) -> TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&base).expect("the test root must open");
    loop {
        let dir = base.join(format!(
            "cli-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return TempDir(dir),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => panic!(
                "could not open a temporary directory ({}): {err}",
                dir.display()
            ),
        }
    }
}

/// The command for the `headshell` binary under test — **without a console**
/// on Windows (D-070).
///
/// A few tests ask "what happens without a terminal": the password prompt
/// must suggest `HEADSHELL_PASSWORD`, `--tui` must refuse to open. crossterm
/// opens the terminal directly, not through standard input — the console
/// buffer (`CONIN$`) on Windows, `/dev/tty` on Unix — so even with standard
/// input a pipe, the child process reaches its parent's terminal. On GitHub's
/// Windows runner the processes have a console: the prompt waited **forever**
/// for a key that would never come, and CI's first Windows run hung for over
/// an hour because of it. `DETACHED_PROCESS` starts the child without a
/// console; the output pipes are not affected. The Unix counterpart:
/// [`child_is_terminalless`].
fn cli_command() -> Command {
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = Command::new(env!("CARGO_BIN_EXE_headshell"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(DETACHED_PROCESS);
    }
    command
}

/// Can the child process not reach a terminal — the precondition of the
/// terminal-less tests.
///
/// Always yes on Windows: [`cli_command`] starts it detached from the
/// console. On Unix the child inherits the test process's controlling
/// terminal; for a developer running `cargo test` from a terminal the prompt
/// really opens and waits for a key (measured with a pseudo-terminal, D-070
/// addendum). There is no safe std way to detach the child from the
/// terminal: `CommandExt::setsid` is unstable (`process_setsid`), `pre_exec`
/// needs `unsafe` and the workspace forbids it. If the precondition cannot be
/// met the test **is skipped** and says why; CI has no controlling terminal,
/// and the test really runs there.
fn child_is_terminalless(test: &str) -> bool {
    let reachable = cfg!(unix)
        && std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok();
    if reachable {
        eprintln!(
            "{test}: there is a controlling terminal and the child process reaches it — skipped \
             (a terminal-less run, CI for example, tests this)"
        );
    }
    !reachable
}

/// Runs the `headshell` binary; `(stdout, stderr, succeeded)`.
fn run(data_dir: &Path, args: &[&str]) -> (String, String, bool) {
    run_with_music(data_dir, None, args)
}

/// Runs it with the password given through the environment (§1.3: the
/// password is never an argument).
fn run_with_password(data_dir: &Path, password: &str, args: &[&str]) -> (String, String, bool) {
    let mut command = cli_command();
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .env("HEADSHELL_PASSWORD", password)
        .env("HEADSHELL_MUSIC_DIRS", "/nonexistent/dir/headshell-test");
    let output = command.output().expect("the headshell binary must run");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// Runs it with `HEADSHELL_MUSIC_DIRS` set (for the local provider tests).
fn run_with_music(data_dir: &Path, music: Option<&Path>, args: &[&str]) -> (String, String, bool) {
    let mut command = cli_command();
    command.arg("--data-dir").arg(data_dir).args(args);
    match music {
        Some(dir) => {
            command.env("HEADSHELL_MUSIC_DIRS", dir);
        }
        None => {
            // The developer's own music directory must not leak into the tests.
            command.env("HEADSHELL_MUSIC_DIRS", "/nonexistent/dir/headshell-test");
        }
    }
    let output = command.output().expect("the headshell binary must run");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn audio_fixtures() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/audio"))
}

/// Pins the variable fields, so the snapshot only breaks on a meaningful
/// difference.
fn normalize(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if VOLATILE_KEYS.contains(&key.as_str()) {
                    *child = serde_json::Value::String("<variable>".to_owned());
                } else {
                    normalize(child);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(normalize),
        _ => {}
    }
}

fn assert_snapshot(name: &str, stdout: &str) {
    let mut value: serde_json::Value = serde_json::from_str(stdout)
        .unwrap_or_else(|err| panic!("{name}: the output must be valid JSON ({err}):\n{stdout}"));
    normalize(&mut value);
    let actual = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());

    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots"))
        .join(format!("{name}.json"));

    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, &actual).expect("the snapshot must be written");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{}: no snapshot ({err}). Create it with UPDATE_SNAPSHOTS=1.",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "\nthe {name} snapshot changed. If this is intended: UPDATE_SNAPSHOTS=1 cargo test -p headshell-cli\n"
    );
}

/// All the commands run in order on a single data directory; the order
/// matters (import first, then statistics).
#[test]
fn json_output_is_stable_across_subcommands() {
    let dir = temp_dir("json");
    let zip = fixtures().join("spotify_extended_mini.zip");

    let (stdout, stderr, ok) = run(&dir, &["--json", "import", zip.to_str().unwrap()]);
    assert!(ok, "import failed: {stderr}");
    assert_snapshot("import", &stdout);

    let (stdout, stderr, ok) = run(&dir, &["--json", "stats", "--top", "3"]);
    assert!(ok, "stats failed: {stderr}");
    assert_snapshot("stats", &stdout);

    let (stdout, stderr, ok) = run(&dir, &["--json", "stats", "--year", "2024", "--top", "2"]);
    assert!(ok, "stats --year failed: {stderr}");
    assert_snapshot("stats_2024", &stdout);

    let (stdout, stderr, ok) = run(&dir, &["--json", "resolve", "Radiohead - Creep"]);
    assert!(ok, "resolve failed: {stderr}");
    assert_snapshot("resolve", &stdout);

    let (stdout, stderr, ok) = run(&dir, &["--json", "library", "search", "radio"]);
    assert!(ok, "search failed: {stderr}");
    assert_snapshot("search", &stdout);

    let (stdout, stderr, ok) = run(&dir, &["--json", "sleeve", "--year", "2024"]);
    assert!(ok, "sleeve failed: {stderr}");
    assert_snapshot("sleeve_2024", &stdout);

    let (stdout, stderr, ok) = run(&dir, &["--json", "diag"]);
    assert!(ok, "diag failed: {stderr}");
    assert_snapshot("diag", &stdout);
}

/// Phase 0.5's done criterion: `headshell sleeve --out card.png` must
/// produce a real PNG.
/// D-076: the covers of the tracks a query finds. Offline, only where the
/// tracks live is asked — here, the pictures in their tags — and a track
/// with no cover there is "not looked up", not "not found" (K9).
#[test]
fn artwork_reads_embedded_covers_and_says_what_it_did_not_look_up() {
    let dir = temp_dir("artwork");
    let music = fixtures().join("artwork");
    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan"]);
    assert!(ok, "scan failed: {stderr}");

    let (stdout, stderr, ok) = run_with_music(
        &dir,
        Some(&music),
        &["--json", "artwork", "cover artist", "--all"],
    );
    assert!(ok, "artwork failed: {stderr}");
    assert_snapshot("artwork", &stdout);
    // `source` is pinned by the snapshot's normaliser (it is the import
    // report's path there); the sources are checked here instead.
    let report = json(&stdout);
    let sources: Vec<&str> = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["source"].as_str().unwrap_or("-"))
        .collect();
    assert_eq!(sources, ["embedded", "embedded"], "{report}");
    assert_eq!(report["online"], false);

    // `--out`: one image per album, named after it.
    let out = dir.join("covers");
    let (_, stderr, ok) = run_with_music(
        &dir,
        Some(&music),
        &[
            "artwork",
            "cover artist",
            "--all",
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert!(ok, "artwork --out failed: {stderr}");
    let mut written: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    written.sort();
    assert_eq!(written.len(), 2, "{written:?}");
    assert!(
        written
            .iter()
            .any(|name| name.ends_with("Cover Artist - Covered.png")),
        "{written:?}"
    );
    assert!(
        written
            .iter()
            .any(|name| name.ends_with("Cover Artist - Covered Too.jpg")),
        "{written:?}"
    );

    // A file without a cover anywhere, offline.
    let bare = audio_fixtures().join("Dir Artist/Untitled.flac");
    let (stdout, stderr, ok) = run(
        &dir,
        &["--json", "artwork", "--file", bare.to_str().unwrap()],
    );
    assert!(ok, "{stderr}");
    let report = json(&stdout);
    assert_eq!(
        report["items"][0]["status"], "not_checked_offline",
        "{report}"
    );
    assert_eq!(report["summary"]["not_checked_offline"], 1);
    assert_eq!(
        report["summary"]["not_found"], 0,
        "not asked is not \"not found\""
    );
}

/// D-078: the lyrics of the tracks a query finds. The fixtures carry every
/// kind a file can carry itself — a `.lrc` beside it, LRC in a tag, `USLT`,
/// `SYLT` — so the chain ends at the file and nothing goes online.
#[test]
fn lyrics_reads_what_the_files_carry_prints_them_and_keeps_a_shift() {
    let dir = temp_dir("lyrics");
    let music = fixtures().join("lyrics");
    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan"]);
    assert!(ok, "scan failed: {stderr}");

    let (stdout, stderr, ok) = run_with_music(
        &dir,
        Some(&music),
        &["--json", "lyrics", "lyric artist", "--all"],
    );
    assert!(ok, "lyrics failed: {stderr}");
    // A track's key names its file by its absolute path, which depends on
    // the checkout: pinned here, checked for its shape instead.
    let mut report = json(&stdout);
    for item in report["items"].as_array_mut().unwrap() {
        assert!(
            item["key"].as_str().unwrap().starts_with("lyrics:"),
            "{item}"
        );
        item["key"] = "<variable>".into();
    }
    assert_snapshot("lyrics", &report.to_string());
    // `source` is pinned by the snapshot's normaliser; checked here.
    let by_title = |title: &str| {
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["title"] == title)
            .cloned()
            .unwrap_or_else(|| panic!("no {title} in {report}"))
    };
    for (title, source, synced) in [
        ("Sidecar", "sidecar", true),
        ("Flac Lrc", "embedded", true),
        ("Mp3 Sylt", "embedded", true),
        ("Mp3 Uslt", "embedded", false),
    ] {
        let item = by_title(title);
        assert_eq!(item["status"], "found", "{item}");
        assert_eq!(item["source"], source, "{item}");
        assert_eq!(item["synced"], synced, "{item}");
        assert_eq!(item["fit"], "own", "{item}");
        assert!(item.get("lyrics").is_none(), "without --print: {item}");
    }

    // `--print`: the lines, with their times.
    let (stdout, stderr, ok) = run_with_music(
        &dir,
        Some(&music),
        &["--json", "lyrics", "flac lrc", "--print"],
    );
    assert!(ok, "{stderr}");
    let lines = json(&stdout)["items"][0]["lyrics"]["lines"].clone();
    assert_eq!(lines[0]["start_ms"], 500, "{lines}");
    assert_eq!(lines[1]["text"], "a second one with çğıİöşü", "{lines}");
    let (stdout, _, ok) = run_with_music(&dir, Some(&music), &["lyrics", "flac lrc", "--print"]);
    assert!(ok);
    assert!(
        stdout.contains("[00:00.50] A made-up first line"),
        "{stdout}"
    );
    assert!(stdout.contains("from the file's tags"), "{stdout}");

    // `--shift`: kept per track, and taken back with 0.
    for (shift, expected) in [("500", 500), ("-1500", -1500), ("0", 0)] {
        let (_, stderr, ok) = run_with_music(
            &dir,
            Some(&music),
            &["lyrics", "mp3 sylt", "--shift", shift],
        );
        assert!(ok, "{stderr}");
        let (stdout, _, _) = run_with_music(&dir, Some(&music), &["--json", "lyrics", "mp3 sylt"]);
        assert_eq!(json(&stdout)["items"][0]["offset_ms"], expected, "{stdout}");
    }
}

#[test]
fn sleeve_writes_a_real_png_and_svg() {
    let dir = temp_dir("sleeve");
    let zip = fixtures().join("spotify_extended_mini.zip");
    let (_, stderr, ok) = run(&dir, &["import", zip.to_str().unwrap()]);
    assert!(ok, "{stderr}");

    let png = dir.join("card.png");
    let (stdout, stderr, ok) = run(&dir, &["sleeve", "--out", png.to_str().unwrap()]);
    assert!(ok, "sleeve --out png failed: {stderr}");
    assert!(stdout.contains("written"), "{stdout}");
    let bytes = std::fs::read(&png).expect("the png file must be written");
    assert_eq!(&bytes[0..8], b"\x89PNG\r\n\x1a\n", "it must be a real PNG");

    let svg = dir.join("card.svg");
    let (_, stderr, ok) = run(
        &dir,
        &[
            "sleeve",
            "--format",
            "story",
            "--out",
            svg.to_str().unwrap(),
        ],
    );
    assert!(ok, "sleeve --out svg failed: {stderr}");
    let text = std::fs::read_to_string(&svg).expect("the svg file must be written");
    assert!(text.contains("height=\"1920\""), "the story size: {text}");
}

/// An unrecognised extension must not silently write the wrong format; it
/// must fail, saying the stage.
#[test]
fn sleeve_rejects_an_unknown_extension() {
    let dir = temp_dir("sleeveext");
    let zip = fixtures().join("spotify_account_mini.zip");
    let (_, stderr, ok) = run(&dir, &["import", zip.to_str().unwrap()]);
    assert!(ok, "{stderr}");

    let bad = dir.join("card.gif");
    let (_, stderr, ok) = run(&dir, &["sleeve", "--out", bad.to_str().unwrap()]);
    assert!(!ok, "an unrecognised extension must fail");
    assert!(stderr.contains("STEP: SLEEVE_RENDER"), "{stderr}");
    assert!(!bad.exists(), "no file must be written in the wrong format");
}

/// Phase 1's done criterion: a local file plays and produces a `listen`
/// record.
///
/// Without an audio device the test skips itself — not silencing it, but
/// saying the condition is not met and moving on.
#[test]
fn playing_a_local_file_records_a_listen_in_the_same_table_as_imports() {
    let dir = temp_dir("play");
    let music = audio_fixtures();

    // The index first: the scan must count what it found.
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan"]);
    assert!(ok, "the scan failed: {stderr}");
    assert!(stdout.contains("indexed"), "{stdout}");

    // Then play.
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["play", "sine"]);
    if !ok {
        // Without an audio device playback cannot be set up; tell that apart.
        if stderr.contains("PLAYBACK_OUTPUT") {
            eprintln!("no audio output — skipping the playback test:\n{stderr}");
            return;
        }
        panic!("playback failed: {stderr}");
    }
    assert!(stdout.contains("queued"), "{stdout}");
    assert!(stdout.contains("listens recorded: 1"), "{stdout}");

    // §1.6's real claim: the scrobble is in the same table as the imported
    // data.
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["stats"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("Test Artist"),
        "the played track must show up in the statistics:\n{stdout}"
    );
    assert!(stdout.contains("1 play"), "{stdout}");
}

/// Every track in the queue plays, each one produces a listen, and `stats`
/// shows **the same number** (D-024 gapless + §1.6).
///
/// The real trap here is consistency: the duration of the untagged fixtures
/// was unknown in the catalog, so `PlayRule` could not use its "half the
/// track" arm and fell back to the 30 s threshold. The result was the CLI
/// saying "4 listens recorded" while the statistics showed 2. The duration is
/// now read from the container.
#[test]
fn every_queued_track_produces_a_listen_that_stats_also_counts() {
    let dir = temp_dir("gapless");
    let music = audio_fixtures();

    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan"]);
    assert!(ok, "{stderr}");

    // "Artist" matches all four fixtures: two have tags (`Test Artist`),
    // two are untagged and derived from their names (`Other Artist`, and
    // `Dir Artist` from the parent directory).
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["play", "Artist", "--all"]);
    if !ok {
        if stderr.contains("PLAYBACK_OUTPUT") {
            eprintln!("no audio output — skipping the gapless test:\n{stderr}");
            return;
        }
        panic!("playback failed: {stderr}");
    }
    assert!(stdout.contains("4 tracks queued"), "{stdout}");
    assert!(
        stdout.contains("listens recorded: 4"),
        "every track in the queue must produce a listen:\n{stdout}"
    );

    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["stats", "--json"]);
    assert!(ok, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    let report = &value["report"];
    assert_eq!(
        report["plays"],
        serde_json::json!(4),
        "what the CLI counted and what the statistics counted must be the same: {report}"
    );
    assert_eq!(
        report["skipped_short"],
        serde_json::json!(0),
        "a track played from start to end must not count as 'short': {report}"
    );
}

/// `--if-stale` does not scan an unchanged directory, and scans a changed
/// one (D-025).
///
/// The dependency-free route: directory stamps are looked at, no `notify`.
#[test]
fn scanning_if_stale_skips_an_unchanged_library_and_notices_a_new_file() {
    let dir = temp_dir("stale");
    let music = temp_dir("stale-music");
    std::fs::copy(
        audio_fixtures().join("tagged.flac"),
        music.join("tagged.flac"),
    )
    .expect("the fixture must be copied");

    // Never scanned counts as stale: "I don't know" is not enough to skip.
    let (stdout, stderr, ok) =
        run_with_music(&dir, Some(&music), &["provider", "scan", "--if-stale"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("scanned"),
        "it must be scanned the first time:\n{stdout}"
    );

    // Right after: the directory did not change, the scan must be skipped —
    // and it must say so.
    let (stdout, stderr, ok) = run_with_music(
        &dir,
        Some(&music),
        &["provider", "scan", "--if-stale", "--json"],
    );
    assert!(ok, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(
        value["scanned"],
        serde_json::json!(false),
        "an unchanged library must not be rescanned: {value}"
    );
    assert!(
        value["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("unchanged")),
        "the reason for skipping must be given: {value}"
    );

    // A new file: the directory stamp changes, the scan must run.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::copy(
        audio_fixtures().join("Test Artist - Mp3 Track.mp3"),
        music.join("new.mp3"),
    )
    .expect("new file");

    let (stdout, stderr, ok) = run_with_music(
        &dir,
        Some(&music),
        &["provider", "scan", "--if-stale", "--json"],
    );
    assert!(ok, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(
        value["scanned"],
        serde_json::json!(true),
        "a new file must trigger a scan: {value}"
    );
    assert_eq!(
        value["write"]["inserted"],
        serde_json::json!(1),
        "the new file must go into the catalog: {}",
        value["write"]
    );
}

/// The index is persistent: `play` does not scan, it reads from the catalog
/// scanned once.
#[test]
fn the_catalog_persists_so_play_does_not_rescan() {
    let dir = temp_dir("catalog");
    let music = audio_fixtures();

    // Scan once.
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan", "--json"]);
    assert!(ok, "{stderr}");
    let first: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert!(
        first["write"]["inserted"].as_u64().unwrap_or(0) >= 3,
        "the first scan must write catalog rows: {}",
        first["write"]
    );

    // The second scan: the stamps did not change, no file must be read again.
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan", "--json"]);
    assert!(ok, "{stderr}");
    let second: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(
        second["write"]["inserted"],
        serde_json::json!(0),
        "unchanged files must not be rewritten"
    );
    assert!(
        second["summary"]["unchanged"].as_u64().unwrap_or(0) >= 3,
        "stamp matches must be counted: {}",
        second["summary"]
    );

    // The real test: search must work **without** a music directory being
    // given. Since the catalog is on disk, `play` does not need a scan.
    let (stdout, stderr, ok) = run(&dir, &["play", "sine", "--dry-run", "--json"]);
    assert!(ok, "the catalog should have been persistent: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(
        value["queued"].as_array().map(Vec::len),
        Some(1),
        "it must be found in the scanned catalog"
    );
}

/// A file deleted from disk drops out of the catalog, but its **history** is
/// not deleted.
#[test]
fn a_deleted_file_leaves_the_catalog_but_keeps_its_history() {
    let dir = temp_dir("deleted");
    let music = temp_dir("deleted-music");
    std::fs::copy(
        audio_fixtures().join("tagged.flac"),
        music.join("tagged.flac"),
    )
    .expect("the fixture must be copied");

    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan"]);
    assert!(ok, "{stderr}");

    // Play it so it has a history.
    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["play", "sine"]);
    if !ok && stderr.contains("PLAYBACK_OUTPUT") {
        eprintln!("no audio output — skipping the test");
        return;
    }
    assert!(ok, "{stderr}");

    // Delete the file and scan again.
    std::fs::remove_file(music.join("tagged.flac")).expect("must be deleted");
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan", "--json"]);
    assert!(ok, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(
        value["write"]["removed"],
        serde_json::json!(1),
        "the deleted file must drop out of the catalog"
    );

    // It is not in the catalog...
    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["play", "sine", "--dry-run"]);
    assert!(!ok, "a deleted file must not look playable");
    assert!(stderr.contains("PLAYBACK_RESOLVE"), "{stderr}");

    // ...but the history remains. A file you delete from disk does not delete
    // its history.
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["stats"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("Test Artist"),
        "the listening history must be kept:\n{stdout}"
    );
}

/// `prefetch` (D-077): the tracks `play` would start with, made ready one
/// after another, each saying how — a local file has no head to fetch, and
/// says so rather than leaving it blank (K9).
#[test]
fn prefetch_makes_the_first_tracks_ready_and_says_why_a_file_has_no_head() {
    let dir = temp_dir("prefetch");
    let music = fixtures().join("artwork");
    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan"]);
    assert!(ok, "scan failed: {stderr}");

    let (stdout, stderr, ok) = run_with_music(
        &dir,
        Some(&music),
        &["--json", "prefetch", "cover artist", "--all"],
    );
    assert!(ok, "prefetch failed: {stderr}");
    assert_snapshot("prefetch", &stdout);
    let report = json(&stdout);
    let reasons: Vec<&str> = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["reason"].as_str().unwrap_or("-"))
        .collect();
    assert_eq!(reasons, ["first", "next"], "{report}");
}

#[test]
fn dry_run_queues_without_playing() {
    let dir = temp_dir("dryrun");
    let music = audio_fixtures();

    // The catalog is persistent; `play` does not scan, it must be scanned once
    // first.
    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan"]);
    assert!(ok, "{stderr}");

    let (stdout, stderr, ok) =
        run_with_music(&dir, Some(&music), &["play", "sine", "--dry-run", "--json"]);
    assert!(ok, "{stderr}");

    let value: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(value["played"], serde_json::json!(false));
    assert_eq!(value["listens_recorded"], serde_json::json!(0));
    assert_eq!(
        value["queued"].as_array().map(Vec::len),
        Some(1),
        "a single track must be queued"
    );
}

#[test]
fn provider_commands_report_capabilities_and_scan_counts() {
    let dir = temp_dir("provider");
    let music = audio_fixtures();

    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "list"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("local"), "{stdout}");
    assert!(
        stdout.contains("STREAM"),
        "the capabilities must be visible: {stdout}"
    );
    assert!(
        !stdout.contains("CONTROL"),
        "the local provider cannot be remote-controlled: {stdout}"
    );

    // The scan must give a K9-style report: the corrupt fixture must be counted,
    // not swallowed.
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan", "--json"]);
    assert!(ok, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    let summary = &value["summary"];
    assert!(summary["audio_files"].as_u64().unwrap_or(0) >= 4);
    assert_eq!(
        summary["failed"],
        serde_json::json!(1),
        "corrupt.flac must be counted: {summary}"
    );
    assert!(summary["indexed"].as_u64().unwrap_or(0) >= 3, "{summary}");
}

/// §1.3's CLI surface: a remote server is registered, listed, deleted —
/// and at no stage do the credentials end up in plain text in the output or
/// on disk (D-021).
///
/// It does not go online (`--no-verify`); the transport layer is tested with
/// a real socket in `headshell-core`'s `remote_http.rs` integration test
/// (D-022).
#[test]
fn remote_servers_are_registered_listed_and_removed_without_leaking_credentials() {
    let dir = temp_dir("server");

    // When empty it must tell the user what to do.
    let (stdout, stderr, ok) = run(&dir, &["provider", "servers"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("no registered remote servers"), "{stdout}");
    assert!(stdout.contains("provider add"), "{stdout}");

    let add_args = [
        "provider",
        "add",
        "subsonic",
        "--url",
        "https://music.home",
        "--user",
        "enai",
        "--name",
        "home",
        "--no-verify",
    ];
    let (stdout, stderr, ok) = run_with_password(&dir, "sesame", &add_args);
    assert!(ok, "registration failed: {stderr}");
    assert!(stdout.contains("registered: home"), "{stdout}");
    assert!(
        !stdout.contains("sesame"),
        "the password must not end up in the output:\n{stdout}"
    );

    // What was written to disk? Not the password, but the derived token
    // (D-021).
    let servers = dir.join("servers.json");
    let text = std::fs::read_to_string(&servers).expect("servers.json must be written");
    assert!(
        !text.contains("sesame"),
        "the password must not be written to disk:\n{text}"
    );
    assert!(text.contains("subsonic_token"), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&servers)
            .expect("the permissions must be readable")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the credentials file must not be open to everyone"
        );
    }

    // The `--json` contract: the summary carries no token. `--json` output
    // can end up in a pipeline, a log or an error report.
    let stored: serde_json::Value =
        serde_json::from_str(&text).expect("servers.json must be valid JSON");
    let token = stored["servers"][0]["auth"]["token"]
        .as_str()
        .expect("the token must be stored")
        .to_owned();

    let (stdout, stderr, ok) = run(&dir, &["--json", "provider", "servers"]);
    assert!(ok, "{stderr}");
    assert!(
        !stdout.contains(&token),
        "the JSON output must not carry the token:\n{stdout}"
    );
    assert_snapshot("provider_servers", &stdout);

    // A registered server goes into the provider list.
    let (stdout, stderr, ok) = run(&dir, &["provider", "list"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("home"),
        "the remote provider must be listed:\n{stdout}"
    );
    assert!(
        stdout.contains("local"),
        "the local provider must stay:\n{stdout}"
    );

    // A second registration with the same name must not silently overwrite.
    let (_, stderr, ok) = run_with_password(&dir, "sesame", &add_args);
    assert!(!ok, "the same name must not be accepted twice");
    assert!(stderr.contains("already registered"), "{stderr}");

    let (stdout, stderr, ok) = run(&dir, &["provider", "remove", "home"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("removed: home"), "{stdout}");

    // Deleting something that does not exist must not say "removed": it would
    // hide a typo.
    let (_, stderr, ok) = run(&dir, &["provider", "remove", "home"]);
    assert!(!ok, "an unregistered name must be an error");
    assert!(stderr.contains("STEP: CONFIG_LOAD"), "{stderr}");
}

/// An unknown server type must not silently be taken as Subsonic.
#[test]
fn an_unknown_server_kind_is_rejected_not_guessed() {
    let dir = temp_dir("kind-error");
    let (_, stderr, ok) = run_with_password(
        &dir,
        "sesame",
        &[
            "provider",
            "add",
            "plex",
            "--url",
            "https://music.home",
            "--user",
            "enai",
        ],
    );
    assert!(!ok, "an unrecognised type must fail");
    assert!(
        stderr.contains("subsonic"),
        "the options must be given:\n{stderr}"
    );
}

/// Without a tty the password must not be silently echoed to the screen;
/// what to do must be said.
#[test]
fn without_a_terminal_the_password_prompt_points_at_the_env_var() {
    if !child_is_terminalless("without_a_terminal_the_password_prompt_points_at_the_env_var") {
        return;
    }
    let dir = temp_dir("password-prompt");
    // `run` does not set HEADSHELL_PASSWORD.
    let (_, stderr, ok) = run(
        &dir,
        &[
            "provider",
            "add",
            "subsonic",
            "--url",
            "https://music.home",
            "--user",
            "enai",
        ],
    );
    assert!(!ok, "no registration without reading the password");
    assert!(
        stderr.contains("HEADSHELL_PASSWORD"),
        "the user must be shown a way out:\n{stderr}"
    );
    assert!(
        !dir.join("servers.json").exists(),
        "no half record must be written"
    );
}

/// D-080: what the play box offers, from the CLI. The track whose title
/// begins with the text comes first, then its artist; the provider is named.
/// A track's id is its file's absolute path — that depends on the checkout,
/// so it is reduced to the file name before the snapshot.
#[test]
fn suggest_offers_the_track_then_the_artist() {
    let dir = temp_dir("suggest");
    let music = audio_fixtures();
    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan"]);
    assert!(ok, "{stderr}");

    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["suggest", "sine", "--json"]);
    assert!(ok, "{stderr}");
    let mut value = json(&stdout);
    for section in value["sections"].as_array_mut().expect("sections") {
        for list in ["tracks", "more"] {
            for item in section[list].as_array_mut().expect("a list") {
                let path = item["id"]["id"].as_str().expect("a track id").to_owned();
                let name = Path::new(&path).file_name().expect("a file name");
                item["id"]["id"] = serde_json::json!(name.to_string_lossy());
            }
        }
    }
    assert_snapshot("suggest", &value.to_string());

    // The human form says the same, in the same order.
    let (stdout, stderr, ok) = run_with_music(&dir, Some(&music), &["suggest", "sine"]);
    assert!(ok, "{stderr}");
    let track = stdout.find("track   Sine 440").expect(&stdout);
    let artist = stdout.find("artist  Test Artist").expect(&stdout);
    assert!(track < artist, "{stdout}");

    // Nothing anywhere is said, not left blank.
    let (stdout, _, ok) = run_with_music(&dir, Some(&music), &["suggest", "zzz"]);
    assert!(ok);
    assert!(
        stdout.contains("nothing found for \"zzz\" anywhere"),
        "{stdout}"
    );
}

/// D-080: the provider the play box searches first is a choice kept in the
/// data directory; only a registered one can be chosen, and the list marks it.
#[test]
fn the_provider_to_play_from_is_chosen_kept_and_listed() {
    let dir = temp_dir("provideruse");

    let (stdout, stderr, ok) = run(&dir, &["provider", "list", "--json"]);
    assert!(ok, "{stderr}");
    let list = json(&stdout);
    assert_eq!(list["play_from"]["provider"], "local");
    assert_eq!(
        list["play_from"]["chosen"], false,
        "nothing chosen yet: the first that can play"
    );

    let (_, stderr, ok) = run(&dir, &["provider", "use", "ytmusic"]);
    assert!(!ok, "an unregistered provider must not be chosen");
    assert!(
        stderr.contains("PROVIDER_CALL") && stderr.contains("registered: local"),
        "{stderr}"
    );

    let (stdout, stderr, ok) = run(&dir, &["provider", "use", "local", "--json"]);
    assert!(ok, "{stderr}");
    assert_eq!(json(&stdout)["play_from"]["chosen"], true);
    assert!(dir.join("preferences.json").exists());

    let (stdout, stderr, ok) = run(&dir, &["provider", "list"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("* local"),
        "the chosen one is marked:\n{stdout}"
    );
    assert!(stdout.contains("play from: local (chosen)"), "{stdout}");
}

#[test]
fn testing_an_unknown_provider_lists_the_known_ones() {
    let dir = temp_dir("providertest");
    let (_, stderr, ok) = run(&dir, &["provider", "test", "spotify"]);
    assert!(!ok, "a missing provider must fail");
    assert!(stderr.contains("PROVIDER_CALL"), "{stderr}");
    assert!(
        stderr.contains("local"),
        "the user must be told the registered providers:\n{stderr}"
    );
}

#[test]
fn playing_with_no_match_says_what_to_do() {
    let dir = temp_dir("no-match");
    let music = audio_fixtures();
    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["play", "nosuchtrackexists"]);

    assert!(!ok, "it must fail when there is no match");
    assert!(stderr.contains("PLAYBACK_RESOLVE"), "{stderr}");
    assert!(
        stderr.contains("provider scan"),
        "the user must be told what to do:\n{stderr}"
    );
}

/// The TUI needs a real terminal; where there is none it must fail **saying
/// the stage**.
///
/// Silently falling back to text mode would be worse: the user knows they
/// typed `--tui`, and should know why the interface did not open.
#[test]
fn the_tui_refuses_to_start_without_a_terminal() {
    if !child_is_terminalless("the_tui_refuses_to_start_without_a_terminal") {
        return;
    }
    let dir = temp_dir("tui");
    let music = audio_fixtures();
    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["provider", "scan"]);
    assert!(ok, "{stderr}");

    let (_, stderr, ok) = run_with_music(&dir, Some(&music), &["play", "sine", "--tui"]);
    assert!(!ok, "a TUI without a terminal must fail");
    assert!(stderr.contains("PLAYBACK_OUTPUT"), "{stderr}");
    assert!(
        stderr.contains("terminal interface"),
        "the error must say what failed:\n{stderr}"
    );
}

#[test]
fn human_output_names_the_stage_on_failure() {
    let dir = temp_dir("error");
    let missing = dir.join("missing.zip");
    let (_, stderr, ok) = run(&dir, &["import", missing.to_str().unwrap()]);

    assert!(!ok, "a missing file must fail");
    assert!(
        stderr.contains("STEP: IMPORT_READ"),
        "the stage must be printed:\n{stderr}"
    );
    assert!(
        stderr.contains("headshell diag"),
        "the user must be pointed to diag:\n{stderr}"
    );
}

#[test]
fn diag_without_any_run_is_not_an_error() {
    let dir = temp_dir("empty-diag");
    let (stdout, _, ok) = run(&dir, &["diag"]);
    assert!(ok);
    assert!(stdout.contains("yet"), "{stdout}");
}

#[test]
fn human_stats_output_is_readable() {
    let dir = temp_dir("human");
    let zip = fixtures().join("spotify_account_mini.zip");
    let (_, stderr, ok) = run(&dir, &["import", zip.to_str().unwrap()]);
    assert!(ok, "{stderr}");

    let (stdout, _, ok) = run(&dir, &["stats"]);
    assert!(ok);
    assert!(stdout.contains("top artists"), "{stdout}");
    assert!(stdout.contains("Portishead"), "{stdout}");
}

/// Is the plugin lifecycle visible from the CLI (Phase 2 §2.1)?
///
/// The shell's job is only to show: the consent decision, the permission
/// comparison and the version check are in the core. The claim here is "the
/// CLI gets the same data" — that is, the GUI will too (the Golden Rule).
#[test]
fn plugin_lifecycle_is_visible_from_the_cli() {
    let dir = temp_dir("plugin");
    install_echo_plugin(&dir);

    // Installed but not approved: visible, not loaded.
    let (stdout, stderr, ok) = run(&dir, &["--json", "plugin", "list"]);
    assert!(ok, "plugin list failed: {stderr}");
    assert_snapshot("plugin_list", &stdout);

    let (stdout, stderr, ok) = run(&dir, &["plugin", "approve", "echo"]);
    assert!(ok, "approve failed: {stderr}");
    assert!(stdout.contains("approved"), "{stdout}");
    assert!(
        stdout.contains("permissions are enforced") && stdout.contains("outside this boundary"),
        "the consent output must say what is guaranteed **and what is not** \
         (D-040, D-069):\n{stdout}"
    );

    let (stdout, stderr, ok) = run(&dir, &["--json", "plugin", "list"]);
    assert!(ok, "{stderr}");
    assert_snapshot("plugin_list_approved", &stdout);

    // An approved plugin goes into the provider list — without starting a
    // process.
    let (stdout, stderr, ok) = run(&dir, &["provider", "list"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("echo"), "{stdout}");

    // Disabling and re-enabling do not ask for consent.
    let (stdout, _, ok) = run(&dir, &["plugin", "disable", "echo"]);
    assert!(ok);
    assert!(stdout.contains("disabled"), "{stdout}");
    let (stdout, _, ok) = run(&dir, &["provider", "list"]);
    assert!(ok);
    assert!(
        !stdout.contains("echo"),
        "a disabled plugin must not be loaded:\n{stdout}"
    );
    let (stdout, _, ok) = run(&dir, &["plugin", "enable", "echo"]);
    assert!(ok);
    assert!(stdout.contains("approved"), "{stdout}");
}

/// The secret store: the value appears in no output (D-042).
#[test]
fn secrets_are_listed_by_name_and_never_by_value() {
    let dir = temp_dir("secret");

    let mut command = cli_command();
    let output = command
        .arg("--data-dir")
        .arg(&dir)
        .args(["secret", "set", "plugin:echo", "token"])
        .env("HEADSHELL_SECRET", "very-secret-value")
        .env("HEADSHELL_MUSIC_DIRS", "/nonexistent/dir/headshell-test")
        .output()
        .expect("the headshell binary must run");
    assert!(
        output.status.success(),
        "secret set failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let (stdout, stderr, ok) = run(&dir, &["--json", "secret", "list"]);
    assert!(ok, "{stderr}");
    assert!(
        !stdout.contains("very-secret-value"),
        "the secret value must not leak into the output:\n{stdout}"
    );
    assert_snapshot("secret_list", &stdout);

    // The diagnostics report must not carry the value either: it is the text
    // that gets copied and shared.
    let (stdout, _, ok) = run(&dir, &["diag"]);
    assert!(ok);
    assert!(!stdout.contains("very-secret-value"), "{stdout}");

    let (stdout, _, ok) = run(&dir, &["secret", "remove", "plugin:echo", "token"]);
    assert!(ok);
    assert!(stdout.contains("removed"), "{stdout}");
}

/// Does a plugin work **with an empty `PATH`** (D-069)?
///
/// This was api 1's whole trouble: the plugin looked for `python3` on `PATH`,
/// and "whoever I sent it to had a problem". In api 2 the engine is inside
/// the binary; the plugin must answer even when the environment is emptied.
/// The test runs the binary with its environment wiped completely — no
/// interpreter, no tool.
#[test]
fn a_plugin_runs_with_nothing_on_the_path() {
    let dir = temp_dir("empty-path");
    install_echo_plugin(&dir);
    let (_, stderr, ok) = run(&dir, &["plugin", "approve", "echo"]);
    assert!(ok, "approve failed: {stderr}");

    let mut command = cli_command();
    command
        .arg("--data-dir")
        .arg(&*dir)
        .args(["--json", "provider", "test", "echo"])
        .env_clear()
        .env("HEADSHELL_MUSIC_DIRS", "/nonexistent/dir/headshell-test");
    // On Windows a completely empty environment is too empty: some system DLLs
    // (like the socket stack) do not start without `SystemRoot`. That is not an
    // interpreter or a tool but the operating system itself — it is put back,
    // `PATH` is not.
    #[cfg(windows)]
    for key in ["SystemRoot", "windir"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let output = command.output().expect("the headshell binary must run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "provider test failed in an empty environment:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(
        report["health"]["reachable"],
        serde_json::Value::Bool(true),
        "{stdout}"
    );
    assert_eq!(report["health"]["track_count"], 2, "{stdout}");
}

/// The catalog: building the index, installing, consent, updating and
/// removing — with the real binary, against a server opened on this machine
/// (D-071).
///
/// What is tested is that the shell gets **the same data**: the maintainer's
/// command produces the index, the catalog points at that server through
/// `HEADSHELL_PLUGIN_INDEX`, and the engine runs the installed plugin. The
/// rules themselves are in the core's unit tests (`plugin::catalog`).
#[test]
fn a_plugin_goes_from_the_catalog_through_approval_and_update_to_removal() {
    let dir = temp_dir("catalog");
    let repo = temp_dir("catalog-repo");
    let source = fixtures().join("plugins/echo");
    std::fs::create_dir_all(repo.join("echo")).unwrap();
    for file in ["plugin.json", "main.js"] {
        std::fs::copy(source.join(file), repo.join("echo").join(file)).unwrap();
    }
    let repo_arg = repo.to_str().unwrap();
    let server = FileServer::serve(repo.to_path_buf());
    let index = server.url("index.json");
    // The server serves the working tree; the version still goes into the
    // address because `{version}` is mandatory in the template.
    let template = format!(
        "http://{}/{{name}}/{{path}}?version={{version}}",
        server.addr
    );

    // 1. The maintainer's route: the core's validation produces the index.
    let (stdout, stderr, ok) = run(
        &dir,
        &[
            "--json",
            "plugin",
            "index",
            repo_arg,
            "--url-template",
            &template,
        ],
    );
    assert!(ok, "plugin index failed: {stderr}");
    let report = json(&stdout);
    assert_eq!(report["written"], true, "{stdout}");
    assert_eq!(report["plugins"][0]["name"], "echo", "{stdout}");
    let (_, stderr, ok) = run(&dir, &["plugin", "index", repo_arg, "--check"]);
    assert!(
        ok,
        "a freshly built index must count as up to date: {stderr}"
    );

    // 2. The catalog: listed, not installed.
    let (stdout, stderr, ok) = run_with_index(&dir, &index, &["--json", "plugin", "catalog"]);
    assert!(ok, "plugin catalog failed: {stderr}");
    assert_snapshot(
        "plugin_catalog",
        &stdout.replace(&server.addr.to_string(), "127.0.0.1:<port>"),
    );

    // 3. Installing: it comes down from the catalog and awaits consent (D-040).
    let (stdout, stderr, ok) = run_with_index(&dir, &index, &["plugin", "install", "echo"]);
    assert!(ok, "plugin install failed: {stderr}");
    assert!(stdout.contains("catalog    : 0.2.0 downloaded"), "{stdout}");
    assert!(
        stdout.contains("awaiting consent"),
        "coming from the catalog is not consent:\n{stdout}"
    );
    assert!(dir.join("plugins/echo/origin.json").exists());

    // 4. Once approved, the engine runs it.
    let (_, stderr, ok) = run(&dir, &["plugin", "approve", "echo"]);
    assert!(ok, "{stderr}");
    let (stdout, stderr, ok) = run(&dir, &["--json", "provider", "test", "echo"]);
    assert!(ok, "{stderr}");
    assert_eq!(json(&stdout)["health"]["reachable"], true, "{stdout}");

    // 5. A new version: the outdated index does not pass `--check` and says
    // what is out of date; it is rebuilt without giving the template.
    let manifest = std::fs::read_to_string(repo.join("echo/plugin.json"))
        .unwrap()
        .replace("\"0.2.0\"", "\"0.3.0\"");
    std::fs::write(repo.join("echo/plugin.json"), manifest).unwrap();
    let (_, stderr, ok) = run(&dir, &["plugin", "index", repo_arg, "--check"]);
    assert!(!ok, "an outdated index must not pass --check");
    assert!(stderr.contains("echo: its entry changed"), "{stderr}");
    let (_, stderr, ok) = run(&dir, &["plugin", "index", repo_arg]);
    assert!(ok, "the template must be read from index.json: {stderr}");

    let (stdout, _, ok) = run_with_index(&dir, &index, &["--json", "plugin", "catalog"]);
    assert!(ok);
    assert_eq!(
        json(&stdout)["plugins"][0]["installed"]["state"],
        "update_available",
        "{stdout}"
    );

    // 6. Updating; the permissions stayed the same, so the consent stays in
    // place.
    let (stdout, stderr, ok) = run_with_index(&dir, &index, &["plugin", "update"]);
    assert!(ok, "plugin update failed: {stderr}");
    assert!(stdout.contains("updated: 0.2.0 → 0.3.0"), "{stdout}");
    let (stdout, _, ok) = run(&dir, &["plugin", "list"]);
    assert!(ok);
    assert!(stdout.contains("approved"), "{stdout}");

    // 7. Removing: the directory goes, the consent is forgotten.
    let (stdout, stderr, ok) = run(&dir, &["plugin", "remove", "echo"]);
    assert!(ok, "plugin remove failed: {stderr}");
    assert!(stdout.contains("forgotten"), "{stdout}");
    assert!(!dir.join("plugins/echo").exists());
    let (stdout, _, ok) = run(&dir, &["plugin", "list"]);
    assert!(ok);
    assert!(stdout.contains("no plugins installed"), "{stdout}");
}

/// A name that is not in the catalog: what it is and what is in the catalog
/// are said.
#[test]
fn installing_a_name_the_catalog_does_not_have_lists_what_it_has() {
    let dir = temp_dir("catalog-missing");
    let repo = temp_dir("catalog-missing-repo");
    let source = fixtures().join("plugins/echo");
    std::fs::create_dir_all(repo.join("echo")).unwrap();
    for file in ["plugin.json", "main.js"] {
        std::fs::copy(source.join(file), repo.join("echo").join(file)).unwrap();
    }
    let server = FileServer::serve(repo.to_path_buf());
    let template = format!(
        "http://{}/{{name}}/{{path}}?version={{version}}",
        server.addr
    );
    let (_, stderr, ok) = run(
        &dir,
        &[
            "plugin",
            "index",
            repo.to_str().unwrap(),
            "--url-template",
            &template,
        ],
    );
    assert!(ok, "{stderr}");

    let (_, stderr, ok) = run_with_index(
        &dir,
        &server.url("index.json"),
        &["plugin", "install", "Echo"],
    );
    assert!(!ok, "a missing name must not be installed");
    assert!(stderr.contains("STEP: PLUGIN_CATALOG"), "{stderr}");
    assert!(stderr.contains("did you mean `echo`"), "{stderr}");
}

fn json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout).unwrap_or_else(|err| panic!("not JSON ({err}):\n{stdout}"))
}

/// Runs it reading the catalog from the `index` address (D-071).
fn run_with_index(data_dir: &Path, index: &str, args: &[&str]) -> (String, String, bool) {
    let mut command = cli_command();
    command
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .env("HEADSHELL_PLUGIN_INDEX", index)
        .env("HEADSHELL_MUSIC_DIRS", "/nonexistent/dir/headshell-test");
    let output = command.output().expect("the headshell binary must run");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// A small server that serves a directory over plain HTTP on `127.0.0.1` —
/// for testing the catalog commands without going online. The core only
/// accepts plain HTTP to this machine itself (D-071); the server is exactly
/// there.
///
/// It stops when dropped: the flag goes down and the waiting `accept` is
/// woken up with a connection.
struct FileServer {
    addr: std::net::SocketAddr,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FileServer {
    fn serve(root: PathBuf) -> Self {
        use std::io::{BufRead as _, Write as _};
        use std::sync::atomic::Ordering;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("the listener must open");
        let addr = listener.local_addr().expect("address");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let Ok(clone) = stream.try_clone() else {
                    continue;
                };
                let mut reader = std::io::BufReader::new(clone);
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                // The headers are read up to the empty line and thrown away.
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line.trim().is_empty() => break,
                        Ok(_) => {}
                    }
                }
                let target = request.split_whitespace().nth(1).unwrap_or("/");
                let target = target.split('?').next().unwrap_or(target);
                let file = target
                    .trim_start_matches('/')
                    .split('/')
                    .fold(root.clone(), |path, part| path.join(part));
                let (status, body) = match std::fs::read(&file) {
                    Ok(body) => ("200 OK", body),
                    Err(_) => ("404 Not Found", b"none".to_vec()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        Self {
            addr,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}/{path}", self.addr)
    }
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Installs the fixture plugin into the data directory — the way the user
/// would, by copying the directory.
fn install_echo_plugin(data_dir: &Path) {
    let source = fixtures().join("plugins/echo");
    let target = data_dir.join("plugins/echo");
    std::fs::create_dir_all(&target).expect("plugin directory");
    for file in ["plugin.json", "main.js"] {
        std::fs::copy(source.join(file), target.join(file)).expect("plugin file");
    }
}
