# CLAUDE.md

The **operational summary** read in every session. The normative text is not
here: the invariant rules, the phase plan and the working protocol live in
[`PLAN.md`](PLAN.md). If they contradict each other, **PLAN.md wins**.

This file is the sole owner of: the workspace tree, the commands, the CLI test
surface, the code conventions, the diagnostics practice, the test layout.
PLAN.md does not repeat these; it points here.

> The project is named `headshell` (D-058). A turntable's headshell doesn't
> pick the record; it reads whatever you put on.

---

## What the project is

A provider-independent music listening layer. The product is not audio — it is
the **listening identity**: history, statistics, playlists and social ties
belong to the user, not to the provider. Wherever the audio comes from (a local
file, Subsonic/Jellyfin, SoundCloud, YouTube Music, torrent, FTP), the layer on
top stays the same.

The core is a Rust library; a CLI, a Tauri desktop shell and (later) mobile
bindings through `uniffi` consume it.

---

## THE GOLDEN RULE (K1)

**The CLI is a thin shell. All logic lives in `headshell-core`.**

The test: when a feature is deleted from the CLI, the core must still be able
to offer it. The CLI only does this — argument parsing, calling the core,
formatting the output, the exit code.

**Never** in the CLI: business logic, data transformation, network calls, SQL,
matching algorithms. If you want to write something in the CLI, first ask
"will the GUI want this too?" If the answer is yes, put it in the core.

The same goes for the Tauri shell (`crates/headshell`): it is a shell too.

> Full text and reasoning: PLAN.md §2, K1. The only reason it is repeated here
> is that it is the rule most often broken while writing code.

---

## Invariant rules — index

The full text and reasoning are in **[`PLAN.md` §2](PLAN.md)**. What follows is
only a reminder index; read the text there before applying a rule.

| # | Rule |
|---|---|
| **K1** | The Golden Rule: the CLI is a thin shell |
| **K2** | Importing is done from export files, not from APIs |
| **K3** | Audio is never relayed; only the position is synced |
| **K4** | Spotify does not enter the core |
| **K5** | Plugins run in an embedded JS engine (QuickJS); they reach the outside only through `host` |
| **K6** | The order of the canonical identity chain: ISRC → MBID → fuzzy → AcoustID |
| **K7** | The core API must be expressible with `uniffi` |
| **K8** | No `unwrap()` / `expect()` / `panic!()` in `headshell-core` |
| **K9** | Every failure says which stage it happened in |
| **K10** | Phase boundaries are not crossed |
| **K11** | Everything in the repositories is written in English |

If you need to break a rule, **stop and ask** — PLAN.md §0.1.

> A common mistake about K7: the rule forbids *lifetimes, generic parameters
> and closure parameters*. `Arc<dyn Trait>` and `async fn` are **allowed**
> (the D-006 correction). The rule's first wording, which said "no trait
> objects", is void.

> A common mistake about K11: following the conversation's language. A
> session held in Turkish still writes English code, comments, messages and
> commit messages. Real names (artists, the `Müzik` folder) stay as they are;
> the full list of what stays is in PLAN.md §2.
>
> Since D-089 a word the app **shows** is not written in the code but in
> `crates/headshell-core/locales/en.json`, by key; a translation is a file
> of its own. New interface or CLI text means a new key there — a literal
> sentence in a surface is a gap the language files can't reach.

---

## Workspace

```
headshell/
├── Cargo.toml                  # workspace
├── CLAUDE.md                   # this file — the operational summary
├── PLAN.md                     # rules, phase plan, protocol (normative)
├── DECISIONS.md                # the decision log (D-001…)
├── CONTRIBUTING.md             # the contributor process
├── crates/
│   ├── headshell-core/              # ALL the logic is here
│   │   ├── src/
│   │   │   ├── artwork/        # covers: the chain, the cache, the background worker
│   │   │   ├── import/         # export zip parsers
│   │   │   ├── identity/       # canonical resolution (+ musicbrainz, acoustid, fuzzy)
│   │   │   ├── stats/          # listening statistics
│   │   │   ├── sleeve/         # shareable card (svg + png)
│   │   │   ├── library/        # SQLite + FTS
│   │   │   ├── lyrics/         # lyrics: the file's .lrc and tags, the provider, LRCLIB; the cache
│   │   │   ├── provider/       # provider traits + local + remote/{subsonic,jellyfin}
│   │   │   ├── recommend/      # for you: the taste, sittings, scoring (D-087)
│   │   │   ├── plugin/         # QuickJS engine (script) + host gates + artifacts + catalog
│   │   │   ├── playback/       # symphonia + cpal, reading the queue ahead
│   │   │   ├── net/            # HTTP trait + ureq client + fake
│   │   │   ├── diag/           # diagnostics, see below
│   │   │   ├── i18n/           # the language engine: language files, plurals, two-voice diagnostics (D-089)
│   │   │   ├── video.rs        # music videos: sources, matching (D-086)
│   │   │   └── session.rs      # the outward command surface (Session)
│   │   ├── locales/en.json     # the source language, built in; every word the app shows (D-089)
│   │   ├── examples/           # probes run by hand (fingerprint, mb, playback)
│   │   └── tests/              # integration tests over fixtures/
│   ├── headshell-cli/               # thin shell (binary name: `headshell`)
│   │   └── tests/snapshots/    # snapshots of the --json output
│   └── headshell/                   # Tauri desktop shell (binary name: `headshell-desktop`)
│       ├── src/                # main + env + state + core_thread + commands + localhost + video_server
│       ├── ui/                 # plain static webview — no bundler, no npm
│       ├── icons/              # icon.svg is the source, the others are generated (icons/README.md)
│       └── themes/             # two reference themes (contrast, daylight)
├── parked/                     # code that is NOT BUILT but not deleted — torrent (D-069)
├── packaging/                  # distribution: copyright, .desktop entry, aur/ (D-066)
├── docs/                       # the plugin writing guide, the landing page
├── spike/                      # THROWAWAY prototypes — OUTSIDE the workspace, OUTSIDE CI
└── fixtures/                   # test data: trimmed exports, the accuracy set
```

`spike/` is not built, not tested and not in CI. Try matching heuristics there
first; port them to `identity/` once the accuracy is satisfying.

**The plugins are not in this repository** (D-071): they live as directories in
the [`headshell/plugins`](https://github.com/headshell/plugins) repository, and
the app installs them from that repository's `index.json`. The index is
generated by `headshell plugin index <catalog-repo>`; it is not written by
hand. The live tests for SoundCloud/YouTube Music stay in this repository and
install the plugin from the live catalog — a broken release in the catalog
turns red here.

`parked/` is in the workspace's `exclude`: not built, not tested, not in CI.
The difference from `spike/` is that the code there is not throwaway but
**expected to come back**. Today its only resident is the torrent provider: it
was written for api 1's subprocess protocol, and when the plugin system moved
to QuickJS (D-069) it was not ported, by the user's decision. The open
questions about its return are in `parked/README.md`; it is not taken back
into the workspace before they are decided.

---

## Commands

```bash
cargo run -p headshell-cli -- <subcommand>
cargo run -p headshell            # the desktop interface
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Shortcuts for the same are in the `Makefile` (Unix only; on Windows the
commands are run directly); `make` or `make help` lists the targets.
`make gates` runs the three gates in CI order (formatting is the cheapest, so
it fails first), `make core-features` checks the core without features being
merged (D-054), `make cli ARGS="stats --year 2024"` calls the CLI, and
`make aur-test PKG=…` builds an AUR package in an Arch container. The Makefile
sets no rule; it only makes the commands here runnable from one place.

All three must pass clean before a change counts as done: `test`, `clippy`,
`fmt`. The full "done" criterion: PLAN.md §0.4.

> When `cargo test -p headshell-core` is run on its own, the features are not
> merged, so `dead_code` warnings appear that are invisible in the workspace
> run. The three gates run over the **workspace**; a single-crate run is a
> diagnostic tool, not a gate.

---

## The CLI test surface

The CLI's purpose is to exercise the core by hand. Every core capability must
have a subcommand.

```
headshell import <zip|dir>                       # import an export
headshell stats [--year N] [--top N] [--min-ms MS]
headshell resolve "<artist> - <title>" | --file <audio>
headshell artwork "<query>" [--all] | --file <audio> [--out <dir>]
headshell lyrics "<query>" [--all] | --file <audio> [--print] [--shift MS]
headshell prefetch "<query>" [--all]
headshell library search <query> [--limit N] [--min-ms MS]
headshell sleeve [--year N] [--out <file>] [--format square|story]
headshell provider list | test <name> | scan [--if-stale] | use <name> [--for main|music|video]
headshell provider follow <main|music|video>     # music/video follow the main provider again (D-086)
headshell provider add <kind> --url U --user K [--name NAME] [--api-key A] [--verify]
headshell provider remove <name> | servers
headshell plugin list | approve <name> | install <name> | disable <name> | enable <name> | forget <name>
headshell plugin catalog | update [<name>] | remove <name>
headshell plugin index <catalog-repo> [--url-template T] [--check]
headshell secret list | set <namespace> <key> | remove <namespace> <key>
headshell suggest "<query>"                       # what the play box offers while typed in
headshell recommend [--from <provider>] | dismiss "<artist> - <title>" | reset   # for you (D-087)
headshell video "<artist> - <title>" | --file <path>   # the song's video, from the video provider (D-086)
headshell online [on|off]                        # the desktop's online choice (not the CLI's)
headshell music-dirs [add <dir> | remove <dir>]  # the local music folders (HEADSHELL_MUSIC_DIRS overrides)
headshell language [use <code> | system]         # the language the app speaks; the list is the locales/ folder (D-089)
headshell play <track> [--all] [--shuffle] [--dry-run] [--tui]   # --tui: `/` searches, Enter plays, Tab queues
headshell diag                                   # the last run's diagnostics report
```

Global flags: `--json`, `--data-dir <DIR>`, `--online`, `-v/-vv`.

Every command must support `--json` — both for scriptability and as proof that
the GUI gets the same data. The desktop shell's IPC commands mirror this list
one to one (D-033); adding a capability to the interface first requires a
subcommand here. The human-readable output is a separate formatting layer
(`headshell-cli/src/output.rs`).

`--online` is **off** by default: importing an export does not silently
connect anyone to the network. Without the flag the identity chain runs only
its local links, and covers come only from where the tracks live (their tags,
their folder, their provider). The desktop has no flags: its `--online` is a
saved choice — its settings, or `headshell online on|off` — and
`HEADSHELL_ONLINE=1|0` decides one launch over it (D-082); the CLI itself is not
bound by that choice. Where downloading **is the command itself** (`plugin
catalog`, `install`, `update`), the flag is not required; the catalog address
changes with `HEADSHELL_PLUGIN_INDEX` (D-071). Lyrics follow the user's own rule
instead of the flag (D-078): LRCLIB is asked when the user asks for lyrics —
`headshell lyrics`, or the lyrics view being on screen — and for a plugin's track
the moment it starts; a file's and a server's own lyrics are always read.

---

## Diagnostics culture

This project was born from a bash prototype, and the only thing that worked
there was **every failure saying where it was.** Keep that (K9):

- Every failure must say **which stage** it happened in (`STEP: IDENTITY_RESOLVE`).
- `headshell diag` must print the last run's environment, stage, error chain
  and the relevant counts in one block that can be copied and pasted.
- Logging goes through `tracing`; `println!` only in the CLI's user-facing
  output.
- A silent `unwrap_or_default()` is forbidden — it swallows data loss. Either
  return an error or count it and report it.
- **"I didn't look" and "I couldn't find it" are different diagnoses.** An
  unconfigured provider returns not an empty set but an error that says what
  to write.

Operations that produce partial success, such as matching, **always** return a
summary: how many records came in, how many by ISRC, how many fuzzy, how many
did not match.

---

## Code conventions

- **Language: everything is English — invariant rule K11** (PLAN.md §2;
  D-073, D-074). Identifiers and text alike; the full scope and what stays as
  it is (real names, the `*.tr.md` snapshots) are there, not here.
- `headshell-core` errors are typed with `thiserror`; `headshell-cli` may use
  `anyhow`.
- `async` in the public API — let the caller choose the runtime; the core does
  not set up `#[tokio::main]`.
- Ask before adding a new dependency. The tree must stay small (mobile binary
  size). If it isn't required, put it behind an optional cargo feature
  (`render-png`, `audio`, `fingerprint`, `http-client`, `plugin-engine`).
- Everything that touches the network and the file system sits behind a trait
  so that tests can use a fake.
- IDs are type-safe: `CanonicalId`, `ProviderTrackId`, `ListenId` are separate
  newtypes, not `String`.

---

## Tests

- `headshell-core`: unit tests + integration tests over `fixtures/`.
- Trim real export zips and make them fixtures.
- **Network tests may be written (D-043)**, but "can't reach it" is not a
  failure: without a network the test skips itself and writes the reason to
  `stderr`; if it reaches the service and gets the unexpected, it fails. The
  line is not "going to the network" but telling the two failures apart (K9).
  A skipped test **does not count as passed** — say "skipped" when you report.
- Keep a **labelled accuracy set** for identity resolution
  (`fixtures/identity/cases.json`). Measure the accuracy rate with every
  change — this number is the project's most important metric:
  `cargo test -p headshell-core --test identity_accuracy`
- For the CLI: verify the subcommands' `--json` output with snapshot tests.
- **Tests leave no trace on the machine (D-070).** A temporary directory is
  opened with a helper that deletes itself: `crate::test_support::TempDir` /
  `TestConfig` in the core, `tests/support/mod.rs` in the integration tests
  (root `target/tmp`). Don't open a directory directly under
  `std::env::temp_dir()` — the tests once left 1.2 GB there.
- **Tests don't need a program from outside.** A test that leans on a runtime
  like `node`, `python3` or `sh` either fails or silently skips on another
  machine. If JS is needed, the embedded QuickJS is used (D-070); if a program
  specific to one operating system is needed, the test is gated to that system
  (`cfg(unix)` / `cfg(windows)`) and the other system's counterpart is written.
- Compare paths with `PathBuf`, not strings: both `/` and `\` are valid
  separators on Windows, and a string comparison gets it wrong there.

---

## Where things are

| If you're looking for | Look at |
|---|---|
| The full text and reasoning of a rule | PLAN.md §2 |
| When to stop and ask | PLAN.md §0.1 |
| Which phase we're in, what's next | PLAN.md — the phase headings and section statuses |
| The reasoning behind a decision (D-001…) | DECISIONS.md |
| The "never do" list | PLAN.md — NEVER DO |
| Terms (canonical id, anchor, listen…) | PLAN.md — GLOSSARY |
| Which platform is on which side legally | PLAN.md — APPENDIX: Streaming platforms |
| How to write a plugin (JS, the `host` API) | docs/writing-plugins.md |
| The plugin catalog, publishing a new version | the README of the `headshell/plugins` repository |
| Why parked code is there | parked/README.md |
| How to write a theme | crates/headshell/themes/README.md |
| How the desktop packages are produced | .github/workflows/release.yml, crates/headshell/icons/README.md |
| How the AUR package is published | packaging/aur/README.md |

**Don't write the phase status into this file.** Keep it in one place so it
doesn't go stale: PLAN.md's phase headings and their `DONE` / `TODO` marks.
