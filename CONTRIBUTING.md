# Contributing

The project is at an early stage and architectural decisions are still being
made. Before writing code, read [`PLAN.md`](PLAN.md) and
[`DECISIONS.md`](DECISIONS.md) — the answer to most "why wasn't this done like
that?" questions is there, with its reasoning.

## Read first

- [`PLAN.md`](PLAN.md) — the roadmap, the invariant rules (§2), phase
  boundaries, NEVER DO, the glossary. **For rules, this file is authoritative.**
- [`CLAUDE.md`](CLAUDE.md) — the workspace tree, commands, code conventions,
  the CLI test surface, diagnostics practice. **For operational knowledge, this
  file is authoritative.**
- [`DECISIONS.md`](DECISIONS.md) — the decisions that have been made and their
  reasoning. The same question is not argued twice.

The documents are written in English. Turkish snapshots of them sit next to
them as `*.tr.md`; they are not kept up to date (D-073).

## The three gates

A change is not done until these pass clean:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

All three at once: `make gates` (Unix only; it needs `make` and `sh` — on
Windows the three `cargo` commands above are run directly).

The full "done" criterion is in PLAN.md §0.4. The three also run in CI
([`.github/workflows/ci.yml`](.github/workflows/ci.yml), D-053): all three on
Linux, clippy and the tests on Windows and macOS (D-070). But they must pass on
your own machine first — CI is a reminder, not the first line of defence.

### Runtime requirements

None. The plugin engine (QuickJS) is embedded in the core (D-069); the tests
and commands that run plugins look for no interpreter on the machine. The
tools plugins need (yt-dlp) are downloaded by the engine as the platform's
self-contained binary. Building needs a C compiler — but SQLite (`rusqlite`
`bundled`) already wanted one.

The tests don't need anything from outside either: the test that checks the
webview's anchor formula used to run `node`; it now evaluates the same file in
the embedded QuickJS (D-070).

### Tests leave no trace on the machine

Every test **deletes** the temporary directory it opened — a failing test too
(`Drop` runs during a panic). Unit tests use the operating system's temporary
directory, integration tests Cargo's `target/tmp`; even the leftovers of a
killed run land in the project, not in the shared `/tmp`, and go away with
`cargo clean`. When writing a new test, don't open a directory directly under
`std::env::temp_dir()`: the core has `crate::test_support::TempDir`, the
integration tests have `tests/support/mod.rs`. The rule came from a
measurement: the tests had left 1,100 directories, 1.2 GB, in the `/tmp` of a
development machine, which was a tmpfs.

The yt-dlp that the YouTube Music tests download (~40 MB) is cached in
`target/tmp`, but the cache is not used blindly: on every run its hash is
checked, and so is whether the download address is still alive — so the cache
doesn't show a dead address as green on this machine.

### Developing on Windows and macOS

Building and the tests run with the same `cargo` commands. The differences:

- The data directory is `%LOCALAPPDATA%\headshell` on Windows and
  `~/Library/Application Support/headshell` on macOS. For experiments, give a
  separate directory with `HEADSHELL_DATA_DIR` or the CLI's `--data-dir`.
- The `HEADSHELL_MUSIC_DIRS` list is written like `PATH`: with `;` on Windows,
  with `:` elsewhere. It decides the folders for a run that sets it, over the
  ones chosen in the app (sources → music folders, or `headshell music-dirs`;
  D-085).
- The desktop has no command line: its `--online` is a choice kept in the data
  directory (settings, or `headshell online on|off`), and `HEADSHELL_ONLINE`
  decides one launch over it (D-082). The variable takes `1` or `0`; anything
  else stops startup with the reason, so a `true` that went unrecognised doesn't
  silently leave you offline.
- The desktop hides the developer's details — diagnostics, raw reports, ids,
  zero counts — unless the debug mode is on: `DEBUG=1` (exactly `1`) for one
  launch, or the switch at the bottom of settings (D-083).
- The repository checks out with LF line endings on every platform
  (`.gitattributes`). Even with `core.autocrlf` on in Windows, the snapshots
  match byte for byte.
- The tool-running tests install, as an "artifact", an `sh` script on Unix and
  a copy of the system's `cmd.exe` on Windows.

### Tests that skip themselves

Network tests (D-043) **don't fail when they can't reach the service — they
skip themselves and write the reason to `stderr`.** A skipped test does not
count as passed — say "skipped" when you report. Today 466 tests run on Linux
and 3 of them skip themselves (no AcoustID key); torrent's 56 tests were parked
together with the plugin (D-069). If you run the tests from a terminal, two
more CLI tests skip: the question "what happens with no terminal" cannot be
tested while the child process can reach the terminal (D-070 addendum). The
audio test in `playback_local` can fail intermittently on a machine that has an
audio device but is under load (D-059, D-070).

The environment variables needed to really run the skipped ones:

| Variable | What it turns on |
|---|---|
| `HEADSHELL_ACOUSTID_KEY` | The live AcoustID checks (3 tests). In a build without a key they skip, because `EMBEDDED_API_KEY` is empty. |
| `HEADSHELL_TEST_YTMUSIC_COOKIES` | Cookies to get past YouTube's bot wall (D-061). Only needed on data centre addresses; on a home connection the tests run without cookies too. |
| `HEADSHELL_PLUGIN_INDEX` | The catalog the SoundCloud and YouTube Music live tests install the plugin from (D-071). The default is the published index of `headshell/plugins`; to test an unpublished plugin change it can point at a local mirror made with `headshell plugin index` (plain `http` only to `127.0.0.1`). |

`HEADSHELL_PYTHON` and `HEADSHELL_YTDLP` **were removed** (D-055, D-069): the
engine doesn't look for Python, and yt-dlp is installed by the engine, not the
plugin. The `ytmusic` tests download it from the pinned version in the
manifest, as this platform's binary. The plugins themselves are not in this
repository (D-071): the live tests skip themselves if they can't reach the
catalog, and fail if they reach it but can't install. The Torznab variables
were parked together with the torrent plugin (`parked/`, D-069).

The `.env` at the repository root **is read by no code** — there is no
`dotenv`-like dependency. A value you write there does not enter the
environment on its own; you have to load it into your shell yourself
(`set -a; . ./.env; set +a`). The file is in `.gitignore`.

If you added a new capability, also:

- It has a CLI subcommand, and that subcommand supports `--json`.
- If you touched identity or importing, the accuracy set was run and the rate
  is written in the PR description.

## The Golden Rule (K1)

**The CLI is a thin shell. All logic lives in `headshell-core`.**

The test: when a feature is deleted from the CLI, the core must still be able
to offer it. The CLI only parses arguments, calls the core, formats the output
and sets the exit code. The CLI has **no** business logic, data
transformation, network calls, SQL or matching algorithms. The same goes for
the Tauri shell.

If you want to write something in the CLI, first ask: "will the GUI want this
too?" If the answer is yes, put it in the core. This rule exists so that the
GUI and the mobile bindings through `uniffi` work with zero code duplication.

## Invariant rules

Not open for debate. If you think you need to break one, don't write code —
open an issue and explain why it's needed.

The full text and reasoning are in **[`PLAN.md` §2](PLAN.md)**, numbered
K1–K11. What follows is only an index; they are not repeated here because they
were once written in three files at once, and the copies drifted far enough to
contradict each other.

| # | Rule |
|---|---|
| **K1** | The Golden Rule: the CLI is a thin shell |
| **K2** | Importing is done from export files, not from APIs |
| **K3** | Audio is never relayed; only the position is synced |
| **K4** | Spotify does not enter the core |
| **K5** | Plugins run in an embedded JS engine; they reach the outside only through the engine's gates |
| **K6** | The order of the canonical identity chain: ISRC → MBID → fuzzy → AcoustID |
| **K7** | The core API must be expressible with `uniffi` |
| **K8** | No `unwrap()` / `expect()` / `panic!()` in `headshell-core` |
| **K9** | Every failure says which stage it happened in |
| **K10** | Phase boundaries are not crossed |
| **K11** | Everything in the repositories is written in English |

The **NEVER DO** list (DRM, audio relaying, deleting raw `listen`
records…) is at the end of PLAN.md.

## Code conventions

Owned solely by [`CLAUDE.md`](CLAUDE.md), under "Code conventions" — error
types, the `async` contract, newtype IDs and the dependency policy. The
language is not a convention but a rule: K11, everything in the repositories
is English — identifiers, comments, messages, documents, commit messages
(D-073, D-074).

The workspace tree and the commands are there too.

## Diagnostics culture and dependencies

Owned solely by [`CLAUDE.md`](CLAUDE.md). The gist (K9): every failure says
**which stage** it happened in, every operation that produces partial success
returns a summary, and a skipped record is counted and reported — never
silently dropped.

**Ask before** adding a new dependency; the tree must stay small (mobile
binary size).

## Identity accuracy

`fixtures/identity/cases.json` is a hand-labelled accuracy set, and the rate
there is **the project's most important metric**. If you touch the matching
code:

```bash
cargo test -p headshell-core --test identity_accuracy
```

The test prints a per-class breakdown — the overall rate can hide a collapse in
a single class. If you found a new class of error, **add the case first and see
the test fail**, then fix it.

## License

Your contribution agrees to be published under MIT or Apache-2.0 (dual
license).
