# PLAN.md — Phase-by-Phase Execution Plan

This file is the project's roadmap and **normative rule set**: the working
protocol, the invariant rules (§2), the phase plan, NEVER DO, the GLOSSARY.
`CLAUDE.md` is the operational summary read in every session and owns: the
workspace tree, the commands, the CLI test surface, the code conventions, the
diagnostics practice, the test layout.

**Every fact lives in one file.** If a topic is written in two places, one of
them drifts — this has already happened once. If they contradict each other,
**this file wins on rules, CLAUDE.md wins on operational knowledge**.

> The project is named **`headshell`** — settled by D-058, not a placeholder.

---

# 0. READ FIRST: The Working Protocol

## 0.1 When to stop and ask

When one of the following happens, **don't write code; stop and ask.** Don't
guess, don't assume, don't say "they probably want this".

| Situation | What to do |
|---|---|
| You need to break an **invariant rule** | STOP. Explain why it's needed, ask. |
| You're going **outside the current phase's scope** | STOP. Say which phase it belongs to, ask. |
| You need to add a **new dependency** | Ask. The crate name, its size, why it's needed, what the alternative is. |
| The **database schema** will change | Ask. Ask especially if a migration is needed. |
| There are two reasonable designs and the choice is **irreversible** | Ask. Present the options and the trade-offs. |
| A change that will **lower the identity matching accuracy rate** | Ask. With before/after numbers. |
| You **don't know** how a service behaves (API shape, file format) | Ask or verify. **Don't make it up.** |
| A public **API signature** will change | Ask. It may break the mobile/GUI line. |
| A test **broke** and fixing it changes behaviour | Ask. Don't silence the test. |

## 0.2 How to ask

Ask the question in this form:

```
DECISION NEEDED: <the topic in one sentence>
Context:   <why we got to this point>
Option A:  <what> — pro: <...> con: <...>
Option B:  <what> — pro: <...> con: <...>
My advice: <which one and why>
```

Then **wait.** Don't move on before the answer comes, and don't carry on with a
workaround.

## 0.3 The decision log

Every answered question is added to `DECISIONS.md`:

```
## D-NNN — <the decision's one-line title>
**Date:** YYYY-MM-DD · **Status:** APPLIED (YYYY-MM-DD)
**Question:** <the question that was answered>
**Decision:** <the decision that was made>
**Reasoning:** <why this, why not the other>
```

The number is **not made up**: it is one more than the last number at the end
of `DECISIONS.md`. This block is a format example; the `D-NNN` in it is a
deliberately fake number. Once the example here said `D-007`, and the
repository had a real D-007 on a completely different topic — a reader looking
up the example found the wrong decision.

The same question is not asked twice. Read `DECISIONS.md` before deciding
anything.

## 0.4 Before counting a job as "done"

1. `cargo test --workspace` is clean
2. `cargo clippy --workspace --all-targets -- -D warnings` is clean
3. `cargo fmt --all` has been applied
4. If there is a new capability, **it has a CLI subcommand** and it supports `--json`
5. If you touched identity/import, the accuracy set was run and the number reported
6. If the change invalidates a decision in `DECISIONS.md`, that decision was updated

---

# 1. FOUNDATIONAL DECISIONS (answered — details in `DECISIONS.md`)

| Topic | Decision | Consequence |
|---|---|---|
| **Mobile** | Yes, in later phases — but the API is compatible **from today** (D-001) | K7 is binding, no compromise |
| **Distribution** | It will be published; a community is the aim (D-002) | Phase 4 is in scope; public repo hygiene is needed |
| **Local archive** | None; test fixtures can be produced (D-003) | Phase 1 cannot be dogfooded; the order follows from that |
| **Sleeve** | An explicit goal; the December season sets the calendar (D-004) | **Phase 0.5 was added** |

**Nothing is left open.** The project name was closed with **D-058:
`headshell`**; the license is **D-005: MIT OR Apache-2.0**.

---

# 2. INVARIANT RULES

Not open for debate. If one must be broken, ask as in §0.1.

### K1 — The Golden Rule: the CLI is a thin shell
All logic lives in `headshell-core`. The CLI only does argument parsing,
calling the core, formatting the output, the exit code.

**Test:** when a feature is deleted from the CLI, the core must still be able
to offer it.

Never in the CLI: business logic, data transformation, network calls, SQL,
matching algorithms.

### K2 — Importing is done from export files, not from APIs
Spotify/Apple/Google export zips. The GDPR's right to data portability
guarantees this; a provider's developer terms cannot restrict it. **This is the
one part of the project that cannot be broken.** Don't go to a provider's API
to pull history or a library.

### K3 — Audio is never relayed; only the position is synced
In rooms every client plays from its own source; only a time anchor crosses
the network. Don't propose a design that streams audio from a server. (Cost +
law + multiple providers, all three at once.)

### K4 — Spotify does not enter the core
A separate, optional plugin. Nothing that belongs to Spotify is in
`headshell-core`'s dependency tree.

### K5 — Plugins run in an embedded engine; they reach the outside only through the engine's gates
Plugins are written in JavaScript and run in the QuickJS embedded in the core
(D-069). The user is **never asked to install a runtime.** A plugin reaches the
network, secrets, persistent storage and tools only through `host`, and the
permissions it declares are **enforced**. An error a plugin throws, a loop it
gets stuck in or a memory overflow does not bring the core down. No dynamic
libraries (`dlopen`).

> Its first wording was "subprocess + JSON-RPC; plugins are written in any
> language" (api 1). It changed in D-069, because "any language" in practice
> meant "a runtime the user has to install". The trade-off was made on
> purpose: in api 1 the plugin was a separate process, and even a C-level
> crash could not reach the core; in api 2 only a flaw in QuickJS itself can
> bring the core down.

### K6 — The order of the canonical identity chain
`ISRC → MusicBrainz ID → fuzzy match (artist+title+duration) → AcoustID fingerprint`
The order is not broken. Every step returns a **confidence score**.

### K7 — The core API must be expressible with `uniffi`
It is **binding** because of D-001 and cannot be loosened.

**Forbidden** in public signatures: generic parameters (`<L: Trait>`),
lifetimes, closure parameters. **Allowed**: `Arc<dyn Trait>` — `uniffi`
models it as a callback interface (`#[uniffi::export(with_foreign)]`), so it
can be implemented on the Kotlin/Swift side. `async fn` is allowed too;
`uniffi` supports it.

**What "public" means (D-052):** the surface `uniffi` will export — `Session`
methods, the types that appear in those signatures, and the traits modelled as
callback interfaces. If a type crosses this surface, its fields **cannot**
carry lifetimes; that is why `PlayOptions` carries a `String` instead of
`&'a str`.

Constructors written for ergonomics, in the style of
`pub fn new(x: impl Into<String>)`, are **outside** the rule: `uniffi` only
looks at the marked item; in Phase 6 a second constructor taking a `String` is
added next to them with `#[uniffi::constructor]`, and existing Rust callers
don't break. The reasoning is in D-052.

> This rule's first wording said "no trait objects"; it was too strict and was
> corrected in D-006.

> **A known gap (D-052):** `ProviderFuture<'a, T>`, `HttpFuture<'a>` and
> `LookupFuture<'a, T>` carry lifetimes on the public surface. Not an accident
> — it is the only macro-free way to a dyn-compatible async trait. But `uniffi`
> **cannot** express `Pin<Box<dyn Future + Send + 'a>>` in the return of a
> trait method: `Provider`, `HttpClient`, `MetadataLookup` and
> `FingerprintLookup` will be rewritten for `uniffi`'s own async machinery in
> Phase 6. It is not something that can be fixed today; it is a debt to keep
> an eye on.

### K8 — No `unwrap()` / `expect()` / `panic!()` in `headshell-core`
Tests excepted. A silent `unwrap_or_default()` is forbidden too — it swallows
data loss.

### K9 — Every failure says which stage it happened in
Every operation that produces partial success returns a summary: how many came
in, how many succeeded, how many by which route, how many failed.

### K10 — Phase boundaries are not crossed
Don't write the next phase's code "so that it's ready". No server code before
Phase 4 arrives.

### K11 — Everything in the repositories is written in English
Identifiers and text alike: code, comments and doc comments, CLI help and
output, interface text, error and diagnostic messages (`STEP:`), test names
and test data, fixtures, workflows, packaging, documents, commit messages and
release notes. Both repositories: `headshell/headshell` and
`headshell/plugins`.

**The language of the conversation doesn't change it.** A session held in
Turkish still writes English into the repository — code, comments, messages,
commit messages.

What stays as it is — it is data, not text we write:
- real names: artists, tracks and albums (Şebnem Ferah, Ezhel's
  `Müptezhel`), and the `Müzik` folder the music directory search looks for;
- Turkish characters where they are a test's input: the normaliser, URL
  encoding, a multi-byte boundary;
- the entries a platform format localises itself (`Comment[tr]=` in the
  `.desktop` file);
- the documents' `*.tr.*` snapshots and the `Türkçe` link that leads to them.
  They stand as of 2026-09-25 and are not kept up to date; the English text is
  canonical, and a snapshot is translated again, not patched;
- **the app's translations** (D-089): a language file other than English
  (`locales/<code>.json`). The words the app shows live in language files,
  English the source (`crates/headshell-core/locales/en.json`, built in);
  a translation is data the user's language gives, not text we write.
  Diagnostics are written in both: the user's language, ` ‖ `, English;
- the commit messages from before D-073: rewriting history would change every
  hash the release tags point at.

> Until D-074 this was a code convention in CLAUDE.md (D-073, which had itself
> replaced D-036's "text in Turkish" half). It became a rule so that breaking
> it means stopping and asking (§0.1), not a note in review.

---

# 3. CONVENTIONS, WORKSPACE, COMMANDS — in `CLAUDE.md`

The sole owner of these three topics is [`CLAUDE.md`](CLAUDE.md); they are not
repeated here. The reason is drift: while the same tree stood in two files, one
went stale and listed a nonexistent `sync/` directory for months, while never
showing the `net/` and `sleeve/` that did exist.

- **Code conventions** (error types, `async`, newtype IDs, dependencies) →
  CLAUDE.md, "Code conventions". The language is not among them: it is K11,
  above (D-074)
- **The workspace tree** → CLAUDE.md, "Workspace"
- **Commands** → CLAUDE.md, "Commands"
- **Diagnostics practice and test layout** → CLAUDE.md, "Diagnostics culture" /
  "Tests"

What is normative stays in this file: the working protocol (§0), the invariant
rules (§2), the phase plan, NEVER DO, the GLOSSARY.

---

# PHASE 0 — The Identity Layer

**Goal:** the user hands over the export zip and sees their lifetime
statistics. No playback, no server, no account. This phase is a valuable
product on its own.

**Counts as done:** `headshell import x.zip && headshell stats --year 2024`
works, and the identity accuracy rate is measured and reported.

### 0.1 Workspace skeleton
A Cargo workspace, two crates, CI (test + clippy + fmt), `DECISIONS.md` as an
empty file.

### 0.2 Export parser
- Spotify **extended streaming history**: `Streaming_History_Audio_*.json`
  Fields: `ts`, `master_metadata_track_name`, `master_metadata_album_artist_name`,
  `master_metadata_album_album_name`, `ms_played`, `spotify_track_uri`, `skipped`, `platform`.
- Spotify **account data**: `Playlist1.json`, the library, follows.
- Read the zip as a stream — an 8-year archive can be large; don't load all of
  it into memory.
- **The format can change.** If you see an unexpected field, don't drop it;
  count it and report it (K9).

> DECISION POINT: Is Last.fm / ListenBrainz importing in this phase or in
> Phase 2? **NEITHER — the question went stale.** Phase 0 and Phase 2 closed,
> and it was written in neither: today `import/` carries only `spotify.rs` +
> `archive.rs`. The question is no longer "which phase"; it should be asked
> **whether it will be done**, and tied to a new phase. Because of K2 the
> source would again be an export file (the ListenBrainz export is JSON;
> Last.fm has no export of its own — a third-party tool is needed, and that is
> a decision on its own). Open, ownerless, and not tied to a phase.

### 0.3 Storage
SQLite. Raw `listen` events are **never deleted** — the statistics are derived
from them. The schema is versioned; migrations are planned from the start.

> DECISION POINT: Present the schema draft before writing it and get approval.
> Changing it later is expensive. **CLOSED — the schema was written and
> applied.** `library/schema.rs`: an ordered, irreversible series of
> migrations, the version in SQLite's `user_version` pragma, and the rule "add
> a new migration, don't change an existing one" written in the module header.
> There is no migration that deletes a raw `listen` row (NEVER DO).

### 0.4 Canonical identity resolution
**Prototype it in Python inside `spike/` first.** The algorithm is not settled;
the thresholds, the duration tolerance, the remaster/live/deluxe distinction and
the "feat." cleanup will change ten times.

Keep a **labelled accuracy set** in `fixtures/identity/cases.json`. The accuracy
rate is this project's most important metric; measure it with every change.

Once it reaches a satisfying rate, port it to `identity/`. The chain follows the
order in K6.

> DECISION POINT: The minimum acceptable accuracy rate and the confidence
> threshold. **CLOSED — the code and D-009.** The floor is
> `ACCURACY_FLOOR = 0.97` in `tests/identity_accuracy.rs`, and it is never
> lowered "so that the test passes"; the set is a contract too (at least 60
> cases, at least 15 negatives). On the fuzzy side, if the artist similarity is
> below `ARTIST_MIN_SIMILARITY = 0.7` the candidate is not considered at all
> (`identity/fuzzy.rs`). Today's measurement: **72/72 = 100%**, 22 negative
> cases.

### 0.5 Statistics engine
Total time, the most listened, the per-period breakdown, the skip rate,
first/last listen, the discovery timeline. All computed from the raw events;
don't keep precomputed tables (for now — ask if performance becomes a problem).

### 0.6 CLI commands
```
headshell import <zip> [--dry-run]
headshell stats [--year N] [--top N] [--artist X]
headshell resolve "<artist> - <title>"
headshell diag
```
All of them support `--json`.

### 0.7 Diagnostics
`headshell diag` prints the last run's environment, stage, error chain and
counts in one block, in a copyable form.

---

### 0.8 Phase 0 closing work — STATUS: DONE

The Phase 0 code was written and all three gates are clean (**69 tests**,
clippy, fmt).

- [x] **D-006** — the generics of `import_archive` / `resolve_track` are gone;
      there is no generic parameter on the core's public surface.
      `Arc<dyn MetadataLookup>`; the trait returns a boxed future so that it is
      `dyn`-compatible. No new dependency.
- [x] **D-007** — the `diag` error chain starts from `err.source()`; the
      `STEP:` heading is printed exactly once, locked with a regression test.
- [x] **D-008** — `model::PlayRule` is the single definition. `stats` and
      `library search` call the same rule; the SQL counterpart is generated
      from the rule and the parity is verified with a test on a real SQLite.
      `SearchHit.plays` → `play_count`; the raw count (`listen_events`) only in
      the diagnostic counters.
- [x] **D-009** — The set went from 15 → **69 cases** (21 negative),
      class-labelled, with a per-class breakdown printed. If the set shrinks,
      the test fails.

**Identity accuracy — the project's most important metric:**

| Stage | Rate |
|---|---|
| The old set (15 cases) | 15/15 = 100% — *not a measurement* |
| The new set, the old algorithm | 62/69 = **89.9%** |
| The new set, the corrected algorithm | 68/69 = **98.6%** |
| After D-010 was decided | 69/69 = **100%** (22 negative) |

The growing set found two real flaws: live recordings were linked to the studio
recording with 100% confidence (the `live` tag was counted as a droppable
version suffix), and a foreign artist could pass the threshold on title
similarity alone. Both were fixed; the details and numbers are in
`DECISIONS.md` D-009. The test threshold went 90% → 95% → **97%**.

The one remaining case (a radio edit) was not a flaw but an unanswered
question; with **D-010** it was decided that "a radio edit is a separate
recording". The code didn't change; the case was relabelled.

**Phase 0 closed.** The next job is Phase 0.5 (Sleeve). The rasterization
decision was made (**D-011**: resvg, an optional `render-png` feature), the
card design decision was made (**D-012**: a fixed design), and **D-005
(license)** closed: MIT OR Apache-2.0.

---

# PHASE 0.5 — The Shareable Sleeve

**Goal:** Phase 0's output is terminal text; terminal text doesn't spread. The
community goal (D-002, D-004) needs a shareable visual artifact. The December
year-end card season (Spotify Wrapped®) opens once a year and is a free window
of attention.

**Counts as done:** `headshell sleeve --year 2026 --out card.png` works, and the
image it produces is good enough to share on social media without explanation.

### 0.5.1 The card generator lives in the core
Generate SVG, rasterize to PNG. **In the core as the `sleeve/` module, on top of
`stats/`.** The CLI only drives it. The GUI and mobile will call the same
generator — code written here is not written three times.

### 0.5.2 Formats
Square (feed) and vertical 9:16 (story) at least. The dimensions are
parameters, not embedded constants.

### 0.5.3 Content
Total time, the most listened artist/track/album, the discovery timeline, the
first listen date. **The distinguishing point:** "your 8-year history" —
something the provider's year-end card (Spotify Wrapped®), limited to 12
months, can't do. The card must make this visible.

> DECISION POINT: How will rasterization be done? **CLOSED — D-011:** resvg
> behind an optional `render-png` feature. The core always produces SVG; PNG
> is compiled only with the feature on. The CLI turns it on, mobile doesn't.

> DECISION POINT: Should the card design be a preview of the theme system
> (Phase 3), or fixed? **CLOSED — D-012:** a fixed design, with named internal
> constants. The dimensions are a parameter (`CardSize`), the colours are
> module-internal constants; the token contract is designed in Phase 3.

### 0.5.4 Release hygiene (D-002)
README, LICENSE (**D-005 closed: MIT OR Apache-2.0**), CONTRIBUTING, release
tagging. Phase 0.5 will probably be the first public release — the repo must be
ready that day.

---

### 0.5.5 Phase 0.5 status — DONE (`v0.0.1-beta`)

The card generator is in the core (`sleeve/`); the CLI only drives it. All
three gates are clean (**87 tests**, clippy, fmt).

- [x] **0.5.1** — the `sleeve/` module has three layers: `data` (deriving the
      data), `svg` (drawing), `png` (rasterization, behind a feature). The CLI
      only has argument parsing + the `Session::sleeve` call + output
      formatting. `CardFormat` (a clap `ValueEnum`) stays in the CLI so that
      `clap` doesn't leak into the core.
- [x] **0.5.2** — `CardSize` is a parameter; `square()` is 1080×1080,
      `story()` 1080×1920. The dimensions are not embedded constants; any size
      can be given with `CardSize::new`.
- [x] **0.5.3** — Total time, the most listened artist/track/album, the
      discovery timeline, the first listen date, a bar chart by year. The
      footer prints the archive's age ("since 1 January 2023 (2 years)") — what
      the 12-month window of the provider's Spotify Wrapped® can't do is
      visible here.
- [x] **0.5.4** — Release hygiene is done. README (what/why/how + a sample
      card), `LICENSE-MIT` + `LICENSE-APACHE` (D-005), CONTRIBUTING (the three
      gates, the Golden Rule, the invariant rules, the accuracy set procedure).
      The repo was `git init`-ed and the first commit made; the version went
      `0.0.0` → **`0.0.1-beta`**, and the `v0.0.1-beta` tag was put on the
      Phase 0.5 closing.
      `target/`, `spike/`, `tmp/` are outside the commit; the fixtures are
      synthetic.

**D-011 was measured:** with `render-png` off the dependency tree is **56
crates**, with it on **114**. So the decision's reasoning (mobile binary size,
K7) is real: the mobile bindings don't carry the 58-crate rasterization tree;
the CLI does.

**The discovery logic has two modes**, and that is deliberate: on a year card,
"what you listened to for the first time this year" (first listen year = the
query year); on the all-time card, the first listen years of your most
listened, chronologically. Both look at the artist's first year **in the whole
archive** — the year filter doesn't define discovery, it only narrows the
scope. Otherwise every artist would look like a "new discovery" every year.

**Fitting the layout is locked with a test.** In the first working version the
bar section overflowed the card's bottom edge and collided with the footer; the
cause was that the comparison `tall = h >= w` counted the square card as "tall"
too and gave it 5 discovery rows + a 260px bar area. Now it is `h > w`, and if
the content doesn't fit, the number of discovery rows is cut first, then the
bar height. The tests `content_never_spills_past_the_card` and
`a_long_archive_still_fits_the_square_card` measure the lowest `y` coordinate
drawn and compare it with the card height — an overflow cannot come back
silently.

**Phase 0.5 closed.** The next job is a decision: will Phase 1 (playback) come
first, or Phase 3 (GUI + theme) — the Phase 1 decision point below. It decides
what will make it to the December year-end card window, and **no code is
written before it is answered.**

---

# PHASE 1 — Playback

**Goal:** to be able to play from local files and Subsonic/Jellyfin. **From
this moment on, you produce the scrobble yourself** — history no longer forms
at any provider.

**Counts as done:** `headshell play` plays without interruption from local and
remote sources, and every play produces a `listen` record.

**Beware (D-003):** The developer has no local archive. You **can't dogfood**
this phase — code you can't test through your own use is verified only by
tests, not by intuition.

Consequences:
1. Before Phase 1 starts, produce royalty-free test fixtures (Creative Commons
   / public domain, short recordings, different formats: FLAC, MP3, OGG,
   including samples with broken tags).
2. Whether the local provider or the Subsonic provider comes first depends on
   which environment can really be tested.
3. **The phase order can be questioned.** Considering Sleeve + the community
   goal (D-004), Phase 3 (GUI + theme) could come before Phase 1 — because the
   community's engine is themes, and playback can't be tried by the developer.
   The counter-argument: a music app that doesn't play music has a weak
   identity.

> DECISION POINT: The order after Phase 0.5 — Phase 1 (playback) first, or
> Phase 3 (GUI + theme) first? **CLOSED — D-014: Phase 1 (playback) first.**
> Playback is a foundation; the GUI and the theme are built on top of it.
> D-003's dogfood constraint is carried as an accepted risk — it will be
> verified with royalty-free fixtures and tests.

### 1.1 Provider trait design — DONE
Capability flags are a must: `SEARCH | BROWSE | STREAM | CONTROL`.
Not all of them can do the same thing — Spotify will later be `CONTROL` only.
If you assume the trait is uniform, the abstraction collapses at the first
remote player.

> DECISION POINT: Present the trait signatures before writing them.
> **CLOSED — D-015:** state is exposed through an anchor (`PlaybackAnchor`);
> no observer/callback.

**Applied.** The `provider::Provider` trait is `Arc<dyn>`-compatible (K7): it
returns boxed futures and carries no generics/lifetimes/closures. The
`Capabilities` bit mask is a `u32` for `uniffi`. A call without the capability
returns an **explicit error** (`ErrorKind::Unsupported`), not a silent empty
list — "I can't" and "no results" are different things (K9). `rescan` was put
on the trait as a method with a default implementation: instead of a downcast,
so that plugins (Phase 2) can provide their own scanning too.

### 1.2 Local file provider — DONE
Directory scanning, tag reading, watching, library indexing, search with
SQLite FTS.

**Done:** recursive directory scanning, tag reading with symphonia
(artist/title/album/ISRC/duration), deriving from the file name when there are
no tags. The scan returns a summary in line with K9: `files_seen / audio_files
/ indexed / tag_fallback / failed / unreadable_dirs / unchanged`. A broken file
doesn't bring the scan down, but it **is counted**; so is an unreadable
subdirectory.

**The index is now persistent** (schema v2, `provider_tracks` + FTS5).
`headshell play` doesn't scan; the user says `headshell provider scan` once,
and later plays read from the catalog. Search, too, is FTS rather than linear.

Three design decisions:
- **The catalog is a table separate from `tracks`/`listens`.** `tracks` is
  "what you listened to" (derived from raw events, never deleted);
  `provider_tracks` is "what you can play" (a mirror of the source; when a file
  is deleted its row goes too). Merging them would mean also deleting the
  **history** of a file you deleted from disk — locked with a test
  (`dropping_a_file_from_the_catalog_never_touches_its_history`).
- **Incremental scanning with an mtime stamp.** The tags of a file whose stamp
  hasn't changed are not read again; that was the expensive part of the scan.
  In the fixture directory, a second scan counts 4/4 files as `unchanged`.
- **`resolve_source`'s root check uses `canonicalize`.** It used to look at an
  in-memory index; since the index is in the catalog, the path is now checked
  for being under the scanned roots. `<root>/../../etc/passwd` is rejected.

**A staleness probe instead of directory watching (D-025).** The `notify`
dependency was not added: `headshell provider scan --if-stale` asks the
provider a cheap question (`catalog_changed_since`) and scans only if needed.
The local provider answers it by walking **only directories** — files are not
`stat`-ed.

The three answers are three different things: changed / unchanged / **I don't
know**. "I don't know" is a reason to scan, not a reason to skip.

What it doesn't see is written down explicitly: **retagging a file in place**
(the file changes, the directory stamp doesn't). In that case a plain
`provider scan` is needed, and the command's help says so. If real-time
watching is needed, it will be looked at again in Phase 3 together with the
GUI's event loop.

### 1.3 Subsonic / Jellyfin client — DONE (verified on a real server)
The Subsonic API is a widespread standard. It also keeps open the possibility
of our own server speaking Subsonic-compatibly later.

> DECISION POINT: Starting 1.3 presses all three triggers in §0.1 at once —
> **a new dependency**, **service behaviour you don't know**, **an
> irreversible design**. **CLOSED — D-019…D-022.** The four sub-questions below
> were answered too; the original question text stays at the end of this
> section as history.

**Applied.** All four decisions turned into code:

- **D-019 (scope):** Subsonic **and** Jellyfin. Shared plumbing under
  `provider/remote/` (the server registry, credential storage, the stream
  source) + `subsonic.rs` + `jellyfin.rs`. Only the endpoints and the JSON
  shapes diverge.
- **D-020 (transport):** the `net::HttpClient` trait is **always** compiled;
  the concrete client (`ureq` + rustls) is behind the `http-client` feature.
  The provider logic compiles and is tested with the feature off too — and if
  the caller gives its own `Arc<dyn HttpClient>`, it even works. The tree
  measurement is in D-020.
- **D-021 (credentials):** `servers.json`, in the data directory, `0600` on
  Unix. The schema didn't change. For Subsonic the password is not written to
  disk in plain text (salt + `md5(password+salt)`); for Jellyfin it is turned
  into an access key once. No dependency was added for md5; RFC 1321 is in the
  core (`provider/remote/md5.rs`).
- **D-022 (test path):** `tests/remote_http.rs` — a fake server written by
  hand with `std::net`, talking to the **real `UreqClient`**. No new
  dependency.

`AudioSource::HttpStream` now plays. `playback/http_source.rs` downloads the
stream in the background while the decoder starts reading the first bytes; the
`Player`'s old "coming in Phase 1.3" error gave way to an error that **states
the build decision** (with `http-client` off: "not in this build", K9).

Three details are worth recording:

- **Subsonic sends an error with HTTP 200.** If the envelope's
  `status: "failed"` weren't read, "all is well" would be assumed. The
  envelope is checked in every response and the error becomes
  `ErrorKind::RemoteApi` — separate from a transport error (`Network`,
  `HttpStatus`), because "I couldn't reach the server" and "the server said no"
  are different diagnoses with different fixes (K9). A new stage:
  `NETWORK_REQUEST`.
- **Where the credential is carried depends on the provider.** For Subsonic in
  the query string (the path the protocol imposes), for Jellyfin in the
  `Authorization` header. The second is deliberate: if the stream address ends
  up in a log or on the screen, the key doesn't leak. The tests lock both.
- **Being unreachable is a health answer**, not the command's error.
  `provider test` shows the reason; `health()` doesn't return `Err`.

**Automatic verification.** The 10 tests of `remote_http.rs` lock the
following over a real socket: that registering never puts the password on the
wire, that Jellyfin turns the password into a key, the 200+`failed` trap, that
a closed port is a health answer with a reason, that the stream downloads fully
and seeks backwards, and that **the fixture FLAC really plays over HTTP** (if
there is no audio device the test skips itself, writing the reason to
`stderr`).

**Real server verification — 2026-08-31, DONE.** Navidrome 0.63.2 and Jellyfin
10.11.11 were set up in Docker; on both, the chain register → verify → search →
**stream** → scrobble worked end to end. The details, the procedure and what is
still untested (TLS, a reverse proxy, transcoding, a large library, Subsonic
implementations other than Navidrome) are **in D-022's verification section**.

The one flaw reality showed: when verification failed, the outer sentence said
"could not be reached", while the server was reachable and had rejected the
password. Now it says "could not be verified"; `detail` carries the reason (K9).
The fake server couldn't have shown this, because the flaw was not in the
protocol but in **merging two different failures into one sentence**.

**The CLI surface** (the Golden Rule test passed — no decisions in the shell):

```
headshell provider add subsonic --url https://music.home --user yourname [--name home] [--no-verify]
headshell provider add jellyfin --url https://jf.home --user yourname [--api-key KEY]
headshell provider servers      # credentials are not shown
headshell provider remove <name>
```

The password is **not an argument**: it is read from `HEADSHELL_PASSWORD` or
from the terminal without echo — a password typed on the command line would
land in the shell history and the `ps` output. Without a tty it does not
silently fall back to echoed reading; `HEADSHELL_PASSWORD` is pointed out. No
new dependency for reading without echo (`crossterm` was already there for the
TUI). The name suggestion (`https://music.home:4533` → `music`) is in the core,
not the CLI: the GUI will show the same suggestion.

<details>
<summary>The original text of the decision point (kept as history)</summary>

```
DECISION NEEDED: Phase 1.3 remote provider — scope, transport layer, credentials, test path

Context: Today there is no HTTP/TLS in headshell-core's dependency tree; even
         `tokio` is only a dev-dependency (the public API is runtime-independent,
         C: "the core does not set up #[tokio::main]"). The first network call
         changes this balance. Also, D-017 had recorded the assumption "the user
         has NO running Subsonic/Jellyfin server"; and D-003 says "whichever
         environment can really be tested should come first".

Q1 — Scope
  A: Subsonic (OpenSubsonic) only. Jellyfin can also talk through a Subsonic
     plugin. — pro: one API, one test surface, 1.3 closes quickly.
     con: Jellyfin installs without the plugin are left out.
  B: Subsonic + the native Jellyfin API. — pro: wide coverage.
     con: two separate clients, two auth models; 1.3 doubles.
  My advice: A. The PLAN's own reasoning ("Subsonic is a widespread
  standard") supports it; making Jellyfin a separate provider in Phase 2, as
  the plugin boundary's first real customer, is cleaner.

Q2 — Transport layer / dependency
  A: `reqwest` in the core (rustls, default-features off). — pro: sits
     directly on the async API. con: the biggest tree growth
     (hyper+tokio+rustls), makes tokio a **permanent** dependency of the core;
     mobile binary size.
  B: `ureq` in the core (rustls). — pro: a small tree, no tokio.
     con: a blocking API; it blocks the executor inside an async trait method,
     it has to be wrapped.
  C: An `HttpClient` trait + the Subsonic logic in the core; the concrete
     client behind an `http-client` feature (just like `audio`), turned on by
     the CLI. — pro: the exact counterpart of the convention "everything that
     touches the network sits behind a trait so that tests use a fake";
     testable without a network; mobile can provide its own transport.
     con: one more layer of indirection.
  My advice: C — the trait in the core, the concrete client behind a feature.
  Which crate goes inside the feature (ureq or reqwest) becomes a secondary,
  reversible choice; the real decision, "the core doesn't connect to the
  network directly", is kept.

Q3 — Authentication and storage
  Subsonic auth: `t=md5(password+salt)&s=salt` (md5 needs a crate) or a plain
  `p=` over HTTPS. Where will the server address + the credential be written?
  A: A plain-text config file next to `config.rs` (0600).
  B: A new table in SQLite (schema v3 → a migration).
  C: The OS keyring (the `keyring` crate) — a new dependency, fragile on
     headless Linux.
  My advice: A + an md5 token (the salt/token route, so the password isn't kept
  in plain text on disk), with no schema change. The keyring in Phase 2
  together with the plugin permission model.

Q4 — Test path (1.3 can't count as "done" before this is answered)
  **Do you have** a running Subsonic/Jellyfin server?
  If not, verification relies on a fake HTTP server brought up in the test
  (it can be written by hand with std::net, no new dependency needed) — it
  would be a client that has never run against a real server. D-003's
  constraint comes up again here.
  My advice: if there is no server, Q2/C plus a hand-written fake server; and
  1.3 should be marked explicitly as "code done, not verified on a real
  server".
```

</details>

### 1.4 The playback pipeline — DONE
`symphonia` (decoding) + `cpal` (output). Pure Rust, no external dependency.
Video is out of scope — if needed, it is discussed as a separate provider.
**Discussed in D-086 (§3.20):** a video is not a separate track but another
surface of the same one — the engine pauses, the shell's player makes the
sound, and the listen is the same listen.

**Applied (D-016).** Decoding runs on a background thread, output in the cpal
callback; there is a ring buffer between them, and **the audio callback never
blocks** (if the lock can't be taken, silence is written — a gap instead of a
crackle).

Two design decisions are worth recording:
1. **The position is computed from the frames handed to the output**, not from
   the decoded frames. The difference between them is a buffer's worth of
   time; looking at the decoded ones would put the progress bar ahead of the
   audio.
2. **`Buffering` is a separate state.** `Paused` is the user's decision,
   `Buffering` is the pipeline waiting; merging the two tells the user the
   wrong thing.

Resampling is nearest-neighbour (including mono→stereo copying). If
high-quality resampling is needed, it is measured separately — Phase 1's goal
was to produce correct audio.

### 1.5 Queue and playback state — DONE
The queue, repeat, shuffle, gapless. The state is kept in the core; the CLI
only shows it.

**Done:** the queue, `RepeatMode::{Off,All,One}`, shuffle. The state is in the
core; the CLI only shows it.

Two subtle points are locked with tests:
- **Shuffling doesn't disturb the order**; it keeps a separate play order.
  When it is turned off, the user doesn't lose their list; when it is turned
  on, the playing track **is not pulled out from under them** — it stays
  first.
- **`RepeatMode::One` only repeats on a natural end.** If the user says
  "next", it moves on in repeat mode too — otherwise the key would look
  broken. The distinction is between `Queue::next` and `advance_after_finish`.

**Gapless arrived (D-024).** The cpal stream and the ring buffer **stay open**
between tracks; when a track ends, the decoding thread takes the next one and
keeps writing into the same buffer. It used to set up a new device + a new
decoder for every track; that was the gap.

The position is now read from **slices**: since the buffer carries samples of
several tracks side by side, a single counter is not enough. Every track knows
where it started in output frames; the playing track is the slice
`frames_played` falls into. We move on when the transition **is heard** — a
track that has been queued but not yet played does not count as "playing",
otherwise the interface would show a track that can't be heard.

A transition requested by the user (`next`, `enter`) is deliberately **not**
gapless: the engine is set up again. Playing audio that was read ahead would
mean playing a track the user didn't choose.

Measurement: 4 fixture tracks (5 s of audio) played end to end in 5.47 s.

**An inconsistency was closed along the way too.** Since the duration of
untagged files was unknown in the catalog, `PlayRule`'s "half of the track"
branch didn't work and the rule fell back to the 30 s threshold: a 1 s track
listened to from start to finish produced no scrobble. The duration is now read
from the container and goes into both the rule and the record — otherwise the
CLI said "4 listens recorded" while `stats` showed 2. Locked with a test.

### 1.6 Scrobbling — DONE
Every play is a `listen` record. It is written to the same table as the
import data from Phase 0 — the past and today become a single timeline.

**Applied and verified end to end.** A track played with `headshell play`
shows up in the output of `stats` and `library search`; a CLI test locks this
(`playing_a_local_file_records_a_listen_in_the_same_table_as_imports`).

What is written as the duration is **the audio handed to the output**, not
the wall clock time that passed — paused time does not count as listened. The
threshold is the single `PlayRule` from D-008; there is no second
interpretation here.

### 1.7 The CLI player interface — DONE
A TUI with `ratatui`. This is not the GUI's prototype; it is the proof that the
core is fully usable.

**Applied:** `headshell play <query> --tui`. The playing track + state, a
progress bar, the queue (the playing one marked with `▸`), a repeat/shuffle
indicator, key help. Keys: space pause, `n`/`b` next/previous, `↑↓`/`jk`
select, `enter` play the selected one, `s` shuffle, `r` repeat mode,
`q`/`Esc`/`Ctrl+C` quit.

**It passed the Golden Rule's test.** Not a single line in the TUI makes a
decision: the key → action mapping (`action_for`) and the action → core call
(`apply`) are separate, and both only forward. Even the decision "pause or
resume" is in the core (`Player::toggle_pause`) — so the TUI and the GUI don't
make the same decision twice.

The position **is not polled**: the TUI calls `anchor.position_now()` at its
own drawing rate; the formula is in the core (D-015). This is exactly the
behaviour PLAN 3.2 asks of the GUI — so the TUI became the proof that that
design works.

Two details:
- `TerminalGuard` gives the raw mode back with `Drop`: even on a panic, the
  user's terminal isn't left broken.
- An error from the core **doesn't bring the TUI down**; it is shown on the
  bottom line and the loop goes on. If the terminal can't be opened at all it
  fails with `STEP: PLAYBACK_OUTPUT` (locked with a test) — it doesn't
  silently escape to text mode.

---

### 1.8 Phase 1 status — CLOSED

**203 tests**, clippy and fmt clean. `headshell play` and
`headshell play --tui` work end to end; from a local file as well as from
Navidrome/Jellyfin.

| Section | Status |
|---|---|
| 1.1 Provider trait | DONE |
| 1.2 Local provider | DONE — persistent index; `--if-stale` (D-025) |
| 1.3 Subsonic/Jellyfin | DONE — verified on Navidrome + Jellyfin (D-022) |
| 1.4 Audio pipeline | DONE |
| 1.5 Queue | DONE — gapless included (D-024) |
| 1.6 Scrobbling | DONE |
| 1.7 TUI | DONE |

**The test fixtures were produced (the PLAN's precondition for Phase 1):**
sine tones produced with `ffmpeg` under `fixtures/audio/` — royalty-free, 84 KB
in total. FLAC (tagged), MP3 (tagged), OGG (untagged), an untagged FLAC in a
subdirectory and a deliberately broken file. D-003's "cannot be dogfooded"
constraint was thus partly overcome: the audio pipeline is tested with real
files, on a real device.

**In an environment without an audio device the tests skip themselves** (for
CI). The skip is not silent: it writes the reason to `stderr`.

**Phase 1's done criterion is met:** "it plays from local **and remote**
sources, and every play produces a `listen` record" — the remote half was
verified on 2026-08-31 on a real Navidrome and a real Jellyfin (D-022). All
seven sections are DONE; nothing blocks the move to Phase 2.

**Handed over to Phase 2** (none of these leaves Phase 1 incomplete):
- The `keyring` decision (D-021) — today the credential sits in `servers.json`
  with `0600`; it will be looked at again together with the plugin permission
  model.
- Real-time directory watching (D-025) — today there is a probe that looks
  when triggered; it will be reconsidered in Phase 3 when the GUI's event loop
  arrives.
- Remote provider verification under TLS/a reverse proxy/transcoding (D-022) —
  the code was written to support them but they weren't tried.

**A debt handed over to Phase 2:** the `keyring` decision (D-021; the
credential sits in plain text in `servers.json` — a token/key, not a password)
will be looked at again together with the plugin permission model.

---

# PHASE 2 — The Plugin Boundary and Provider Expansion

**Goal:** Providers move out of the core. Third parties become able to write
plugins.

**Counts as done:** A non-Rust reference plugin works, and the core can reject
it on a version mismatch without crashing.

### 2.1 The JSON-RPC plugin protocol — DONE; with api 2 it gave way to QuickJS (§2.9)
Lifecycle, handshake, **versioning**, timeouts, crash isolation. The protocol
is versioned; an incompatible plugin is not loaded, and an error message is
given.

> **D-069:** What follows is the record of api 1. The wire protocol (JSON-RPC,
> a subprocess, a handshake) is gone; `transport.rs` and `client.rs` were
> deleted. Kept: the manifest + the consent ledger + the secret namespace +
> version rejection + timeouts + counted restarts. The current state is in
> §2.9.

**Done** (`crates/headshell-core/src/plugin/`). Five files, five jobs:
`protocol` (the wire format), `manifest` (`plugin.json` + the permission
declaration), `consent` (the consent ledger), `transport` (the subprocess +
line framing), `client` (id matching + timeouts). On top of them,
`PluginProvider` — it implements the existing `Provider` trait, so the queue,
search and the playback pipeline don't even know about the plugin.

The decisions and their reasons:

1. **api 1's methods are narrow:** `handshake`, `health`, `search`,
   `resolve_source`, `shutdown`. `scan_catalog` and `catalog_changed_since`
   are **left out on purpose** — both are optional methods with a default on
   the trait, and the first reference plugin (SoundCloud) has no local catalog
   to scan. A wire format without a user is a guess. The versioning rule is
   the same as D-039's: **adding doesn't raise `api`**; old plugins return
   `-32601`, and the core reads that as "not supported".
2. **Starting is lazy.** The process is opened on the first call;
   `headshell stats` doesn't open six processes alongside, and
   `provider list` opens none. What makes this possible is that the
   capabilities are written in the manifest too — if they contradict, the
   handshake wins and the difference is reported as a warning.
3. **The timeout moved reading onto a thread.** In `std` there is no timeout
   on a pipe; there is `mpsc::recv_timeout`. A hung plugin doesn't hang the
   core.
4. **What the boundary holds:** the ID space (the plugin sends a bare `id`, the
   core adds the provider name — it can't write into another provider's
   namespace), the secret space (only its own namespace), time, life. **What
   it doesn't hold:** files and the network (D-040).
5. **Restarts are counted** (`MAX_STARTS = 3`). Endless restarts would make a
   crash loop silent. On a version mismatch it isn't even tried: repetition
   won't fix it.

Stages: `PLUGIN_LOAD` (manifest/consent), `PLUGIN_HANDSHAKE` (start/version),
`PROVIDER_CALL` (the call). The error types are separate too: `PluginCrashed`
/ `PluginTimeout` / `PluginRpc` / `PluginIncompatible` — "it died", "it didn't
answer", "it said no" and "we can't talk" are four different diagnoses (K9).

CLI: `headshell plugin list|approve|disable|enable|forget`,
`headshell secret list|set|remove`. All `--json`.
The plugin author's document: `docs/writing-plugins.md`.

**Testing.** The unit tests check the protocol logic with a scripted
transport; `crates/headshell-core/tests/plugin_process.rs` runs **a real
Python plugin** (`fixtures/plugins/echo`): handshake, search, source
resolution, version rejection, crash + restart, timeout, permission growth,
secret isolation. The fixture imitates a bad plugin with command-line flags
(`--api 99`, `--crash-on`, `--hang-on`, `--noise`) — not environment
variables, because `set_var` is `unsafe` in Rust 2024 and the workspace says
`unsafe_code = "forbid"` (the same wall as D-031).

> DECISION POINT: The plugin permission model (will network/file access be
> restricted?). **CLOSED — D-040: declaration + consent, enforcement later.**
> A machine-readable permission declaration in the manifest (`net` hosts, `fs`
> path prefixes), user consent on first load, the consent stored with its
> digest in `<data_dir>/plugins.json`. There is **no** operating-system jail,
> and the user is told so — the declaration is a contract, not a firewall.
> Landlock/bwrap can be fitted later without breaking `api`.
>
> Secrets were tied to a single concept in D-042: `<data_dir>/secrets.json`,
> `0600`, namespaced; a plugin sees only its own namespace. Still no
> `keyring`.

### 2.2 The reference plugin — SoundCloud (D-027)
A provider written in a language other than Rust — proof that the contract can
be written outside the core. Its first version was Python; in D-069 it was
moved to JS (`plugins/soundcloud/main.js`) and passed its live tests again.
Since D-071 it isn't in this repository: it is in the `headshell/plugins`
catalog, `soundcloud/` (§2.10).

The platform was chosen not by catalog quality but by **testability**:
SoundCloud is the only candidate that needs no subscription, so the only one
that can run in CI and on someone else's machine. The reference plugin's job
is to prove the protocol, not to offer a catalog.

**DONE.** `plugins/soundcloud/` — Python, standard library only (no
`pip install` needed), ~330 lines. `echo` stayed as a fixture (the protocol
regression test); SoundCloud didn't replace it.

This was **the first half** of Phase 2's "counts as done" criterion, and it
was met: a non-Rust plugin connects to a real service, searches and **plays**.
`headshell play "nujabes aruarian dance"` played a 250-second track from
SoundCloud from start to finish and produced a `listen` record — into the same
table as the import data (§1.6).

The decisions are in D-043. In short:

1. **The client_id from three sources**, in this order: the user's secret →
   the disk cache → discovery from SoundCloud's web client. `health()` says
   which one was used. If there is a secret, discovery is never attempted; if
   the user's key is rejected, it isn't worked around — the user is told.
2. **`progressive` only (plain HTTP MP3).** Measured: 99% coverage in a sample
   of 200 tracks. The remaining 1% offers only HLS and gets an explicit error —
   not a silent empty result (K9). An HLS decoder is not §2.2's job.
3. **The stream address is signed and time-limited**; it is not cached, and
   it is resolved again on every play.
4. Tracks with **`policy: SNIP`** are 30 s previews; `[preview]` is appended
   to the title because api 1 has no field to carry it.

**The live tests are in the default run** (D-043, the user's decision; my
advice was behind `--ignored`). The price is clear: if SoundCloud goes down,
the package turns red. In return, the day the plugin breaks is learned *that
day*. Without a network the tests skip themselves, writing the reason — "I
couldn't reach it" and "it said no" are separate (K9).

**The live run found a flaw left over from Phase 1 (D-044).** `headshell play`
queued the SoundCloud track, said `listens recorded: 0` and quit instantly; no
error, a clean `diag`, no sound. The cause was that the engine showed a newly
submitted job as `Stopped` until the next audio callback — the window is
milliseconds for a local file, seconds over HTTP. A fake server couldn't have
shown this; the flaw was not in the protocol but **in the timing**.

### 2.3 Identity resolution matures — DONE
AcoustID / Chromaprint fingerprinting. For local files with broken tags.

**The AcoustID half is DONE (D-046).** `identity/fingerprint.rs` produces a
Chromaprint fingerprint from symphonia's PCM, `identity/acoustid.rs` asks
AcoustID about it, and `Resolver::resolve_file` ties the two into the chain.
`headshell resolve --file <path>` runs an audio file through all four.

The order is kept: first the three text links from the file's **own tags**,
and audio only if the result falls to `LocalKey`. The fingerprint is the most
expensive link.

Two failures are separate (K9): **failing to produce** a fingerprint doesn't
bring the chain down (the reason is logged, and it ends with the local key);
**failing to ask** AcoustID is propagated. The key comes from the secret store
or the embedded default, and if there is neither, the call is refused *saying
what to do*.

**Tested live with a real key (D-046 addendum 3).** AcoustID accepts the
fingerprint we produce and rejects a broken string (so "it accepted it" is not
an empty claim), and the match path now **really** works: the `duration` field
arrives as a decimal (`309.0`), it had been written as `Option<u32>`, and the
first real match would have fallen over with a JSON error. What hid the flaw
was a hand-written fixture; the fixture is now taken from the live service,
and a separate live test guards against its frozen state turning into a lie.

> **Open debt — the embedded key is empty.** `EMBEDDED_API_KEY` was left empty
> on purpose; until a key is obtained in the project's name from
> `acoustid.org/new-application`, the 4th link works only with the user's own
> key. The user's personal key sits in `.env` and is used in the tests as
> `HEADSHELL_ACOUSTID_KEY` — embedding it in the source would make it public,
> so that will be asked separately.

**The MusicBrainz half is DONE.** `identity/musicbrainz.rs` — the first real
implementation of `MetadataLookup`; the chain's 2nd and 3rd links now work.
`headshell --online resolve "Şebnem Ferah - Sil Baştan"` connects to the real
MusicBrainz and returns an MBID. The rate limit (1 request/s), the
`User-Agent`, Lucene escaping and the `503` retry are inside; `--online` is off
by default.

The live run found three flaws, and all three were fixed (details in the D-045
addendum): live recordings are marked **not in the title but in
`disambiguation`**, on a tie the choice was left to the server's order (the
same query gave two different MBIDs), and a tie was reported as "100%
confidence". The accuracy set went 97.2% → 100% (72 cases); a new class,
`mb_disambiguation`.

> DECISION POINT: Where does the AcoustID key come from, and where is the 4th
> link tied into the chain? **CLOSED — D-046.** The key: an embedded default +
> a user override from the secret store. The connection: no field was added to
> `TrackRef`; a separate entry point was opened (`Resolver::resolve_file`) —
> `resolve()` and `TrackRef` didn't change at all.

> DECISION POINT: MusicBrainz search is unstable between runs. **CLOSED —
> D-046 (Q3).** `mb_stability_probe` measured it: the same `Radiohead — Creep`
> search, twice in a row, returns 25 candidates, and in some runs **the number
> of shared candidates is zero** — MusicBrainz serves search from several index
> replicas. D-045's deterministic ordering can't solve this, because the set to
> be ordered is different each time. **Decision: without distinguishing
> evidence, no authority is claimed** — the chain ends with the local key, and
> `tied_candidates` keeps the weakness of the evidence on record. Paging and
> work-level identity were rejected (reasons in D-046; the work level was
> postponed, not cancelled).
>
> The second flaw the rule exposed was fixed too: **the duration evidence was
> being thrown away.** For `Şebnem Ferah — Sil Baştan`, three candidates of
> 309, 313 and 315 s all got exactly 1.0000, although the query was 309 s
> (`clamp` + a banded duration comparison). The duration difference is now the
> third tie-breaker in the ordering. The measured effect: the accuracy set
> stayed at 100%, and the chain now finds **the same** recording the ISRC link
> gives — before the fix it picked the wrong recording.

> **D-081 — the set is read whole, or nothing is claimed.** Q3 judged a tie
> within the 25 a replica returned; one replica's 25 held a single plain
> `Creep`, and the chain claimed it — CI went red. Now a winner counts only
> when the source's top tier was read whole: the first 25, and if the tier
> goes on, the first 100 **in one response** (a next page comes from another
> replica's ranking — measured). A duration narrows the search to ±15 s
> first (`dur:[…]`), which reads `Creep`'s 33 exact recordings whole and gives
> the studio take every time. A tier wider than one response claims nothing;
> `Resolution::unread_equals` says so. At most one more request — D-046's
> objection to paging was every match, ~9 s a query.

> DECISION POINT: this round's scope and order. **CLOSED — D-045.** The round
> was opened and covers **all three** (§2.3 + §2.4 + §2.5); the order is §2.3 →
> §2.4 → §2.5. Inside §2.3 the order is the reverse of what the PLAN wrote:
> **MusicBrainz first, then AcoustID.** The reason was measured before the
> round — `default_lookup()` always returned `OfflineLookup`, so K6's 2nd and
> 3rd links didn't work at all, and AcoustID can't touch import records that
> have no file. The fingerprint path: **`rusty-chromaprint`** (pure Rust; the
> PCM from symphonia), not an `fpcalc` subprocess.

### 2.4 The torrent provider — PARKED (D-069)
`librqbit`. Sequential streaming.

> **D-069:** When the plugin system moved to QuickJS, torrent "didn't come
> with us" (the user's decision). The code is in `parked/`, outside the
> workspace, not deleted; the open questions about its return are in
> `parked/README.md`. What follows is the record of the days it worked. The
> heir of the bash prototype — the lessons from there still hold: every
> torrent into its own directory, a real readiness check instead of a fixed
> `sleep` for preparation, the peer count and download speed reported.

> DECISION POINT: is torrent in the core or in a plugin? This section implied
> the core by saying "`librqbit`", while K5 said "providers are subprocess
> plugins". **CLOSED — D-047: a plugin.** The tree cost was measured before
> the decision: `librqbit` added **+179 crates** to `headshell-core` (77 → 256),
> and that tree would have gone to mobile too, through `uniffi`. The provider
> became `crates/headshell-plugin-torrent`, a separate Rust binary;
> `headshell-core`'s tree stayed at 77.
>
> Two more sub-questions closed in the same decision. **Audio delivery:** the
> plugin offers a sequential HTTP stream on `127.0.0.1` and `resolve_source`
> returns `HttpStream` — not a single line of the protocol changed, and the
> download doesn't have to finish. **Search:** Torznab (Prowlarr/Jackett) —
> one standard API, one parser; there is no site-specific scraper in the
> repository. If Torznab isn't configured, search returns not a silent empty
> set but an error that says what to write (K9).
>
> Torznab returns a *release*, not a track. It was solved in two steps without
> growing api 1: `search "<query>"` → releases (`<infohash>`),
> `search "<infohash>"` → the audio files inside it (`<infohash>/<index>`).
> When there are several files, `resolve_source` doesn't guess.
>
> **This decision was reversed once and then restored.** D-050 Q3
> (2026-09-09) had decided, on grounds of consistency, to move torrent into
> the core as a provider behind a feature; **D-056 (2026-09-19) cancelled that
> decision**, and D-047 stayed in force. A measurement was the reason for the
> cancellation: what would have been moved was 2,335 lines of source + 647
> lines of tests, 56 tests — not "not yet written", but a working plugin.
> Tearing out an architecture whose price had been paid, before the first
> release, would have bought nothing.

### 2.5 Streaming platform plugins
Which platforms audio can be played from depends not on whether an API exists
but on **DRM**. The full list, the reasons and the legal line: **APPENDIX —
Streaming platforms**.

In short: SoundCloud / Qobuz / YouTube Music can be streamed; Tidal / Apple
Music / Deezer only provide metadata; Spotify only `CONTROL` (K4); Amazon Music
/ Pandora / Idagio / Tencent are closed.

All of them are plugins — none enters the core. The reason is not only K5:
these APIs break without warning, and when they break **the music that is
playing must not stop**; only that plugin should fail.

> DECISION POINT: Which platform will §2.2's reference plugin be, and how many
> will be written in this phase? **CLOSED — D-027 (platform: SoundCloud) +
> D-041 (count: one).** This round is only §2.1 + §2.2. §2.3 (AcoustID), §2.4
> (torrent) and §2.5 (streaming platforms) aren't cancelled; they were moved
> to after the protocol settles: a second provider proves nothing new about
> the protocol, and it has the first provider's mistakes written twice.

> DECISION POINT: Which platform will be written in §2.5? **CLOSED — D-048:
> YouTube Music, a single plugin.** Qobuz needs a subscription, so it couldn't
> be run live — and the lesson of every round from D-044 to D-047 was "only a
> real run showed the flaw". It was accepted from the start that this round
> is not a *protocol* round but a *product value* round.
>
> The APPENDIX's "the way is yt-dlp" line **was tested before writing, and half
> of it turned out wrong.** yt-dlp's search only gives `title` + `id` (no
> artist, no duration), so K6's fuzzy match link can't work with it. The work
> was split in two: **search from InnerTube** (`WEB_REMIX`, the songs filter;
> gives artist + album + duration + `videoId`), **the stream from yt-dlp** (a
> subprocess, not a library).
>
> Two more traps were measured. **`bestaudio` can't be played:** it picks
> opus/webm, and the core's symphonia has neither that codec nor that
> container — the format was pinned to m4a (AAC-LC). **Plain GET is
> throttled:** the same address gives 32 KB/s on a plain request, 8 MB/s with a
> `Range: bytes=0-` header — 250 times. The header travels through the
> protocol's existing `source.headers` field; not a single line changed in the
> core.

### 2.6 Phase 2 status — D-041's scope CLOSED

| Section | Status |
|---|---|
| 2.1 JSON-RPC protocol | DONE — permission model (D-040), secrets (D-042), tested with a real process |
| 2.2 Reference plugin (SoundCloud) | DONE — searches and plays on the live SoundCloud (D-043) |
| 2.3 AcoustID | DONE — fingerprint + AcoustID + `resolve --file` (D-046), tested live with a real key; the one debt: `EMBEDDED_API_KEY` is still empty |
| 2.4 Torrent provider | PARKED (D-069) — it was a working subprocess plugin (D-047); not ported to api 2, in `parked/` |
| 2.5 Streaming platform plugins | DONE — `plugins/ytmusic` (D-048; in the `headshell/plugins` catalog since D-071): InnerTube search, an m4a stream through yt-dlp, the `Range` header that lifts the throttling; played start to finish on the live service |

**Debts carried over from Phase 1:**
- `keyring` (D-021) — **closed**: D-042 defined a single secret concept, the
  `keyring` dependency still wasn't added, and the reasoning was written down.
- Real-time directory watching (D-025) — still open; the `--if-stale` probe
  stays in place.

**The round D-041 drew (§2.1 + §2.2) is finished; all three gates are clean.**

Today's run: **433 tests pass**, 7 tests skip themselves and write the reason
(AcoustID without a key ×3, Torznab without configuration ×3, the audio device
couldn't be opened ×1). A skipped test does not count as passed — D-043's
rule. Both parts of Phase 2's "counts as done" criterion are met: a non-Rust
reference plugin works on a real service (§2.2), and the core rejects it on a
version mismatch without crashing (§2.1, `plugin_process.rs`).

The round D-045 opened ended with §2.3 (D-046), §2.4 (D-047) and §2.5 (D-048).
**All of Phase 2's sections are DONE.**

Of the five debts left open on the way out of Phase 2, one closed (the plugin
engine, D-055), four remain, and one got smaller:

- **~~The plugin engine~~ — CLOSED (D-055).** The engine was written; `ytmusic`
  and `soundcloud` go through it.
- ~~**`torrent` makes the user run `cargo build --release`**~~ — torrent was
  parked (D-069); the distribution question is one of the questions of its
  return (`parked/README.md`).
- **No embedded AcoustID key** (`EMBEDDED_API_KEY` is empty, D-046).
- ~~**The permission vocabulary doesn't accept wildcards** (D-040)~~ —
  **CLOSED (D-069)**: `*.googlevideo.com` can be declared, and the permissions
  are now enforced. "A random peer" still can't be expressed, and shouldn't
  be — the question of torrent's return.
- **Real-time directory watching** (D-025) — the `--if-stale` probe is in
  place.
- **`play --dry-run` doesn't write the provider track ID in the human output**
  (measured in D-054).

### 2.7 The plugin dependency contract — RULE CLOSED (D-049), IMPLEMENTATION DONE in §2.8

**The rule: no plugin may ask for root privileges or a system-wide install.**
A plugin either brings its dependencies itself, or it offers a procedure that
installs them without root. "Install it with your package manager" is not an
installation procedure: it opens a separate support surface for every
distribution and every operating system, and that surface is carried not by the
plugin author but by us.

When the rule was written, **all three plugins** broke it. After D-055 one was
left:

| plugin | what it asks for | status |
|---|---|---|
| `soundcloud` | nothing | the embedded engine — **complies** (D-069) |
| `ytmusic` | yt-dlp | the platform binary is installed by the engine, no Python — **complies** (D-069) |
| `torrent` | `cargo build --release` | **parked** (D-069) — to be solved on its return |

Where the rule must not slip: D-048 had deliberately leaned on "the user
updates yt-dlp with their own package manager", because YouTube breaks it
regularly and yt-dlp does the repair. If the rule turns into "freeze a copy
into the repository", we take over that maintenance burden. Three conditions at
once: **no root + every operating system + able to stay current.**

The diagnostics side was already up: a missing dependency didn't silently turn
into "no results"; `headshell provider test` said UNAVAILABLE and wrote the
reason (K9). What was broken was **installability**, not visibility — and D-055
repaired it in two plugins. Visibility went further too: the gap now shows
without a process being opened, in the output of `headshell plugin list`.

> DECISION POINT: how will the rule be applied? **CLOSED — D-050: a plugin
> engine.** The runtime is the **host's** job, not the plugin's. `headshell`
> has a single engine, the runtime is Python, and it is declared once as
> `headshell`'s own requirement (the interpreter isn't embedded — "it's enough
> to say it's required"). Plugins are scripts running on top of that engine.
>
> The knot was in the packages: saying "there's Python" doesn't bring yt-dlp,
> because yt-dlp is not an interpreter but a **package**. The solution wasn't
> left to the plugin — the plugin **declares** what it wants with `requires` in
> `plugin.json`, and **the engine does** the installing, into its own private
> environment in the data directory. The plugin installs nothing, downloads
> nothing, doesn't call `pip`.
>
> Torrent stops being a plugin and becomes a **built-in provider behind a
> feature gate**: the user builds nothing, but the +179 crates D-047 measured go
> only into the build that opens the gate, and `cargo tree -p headshell-core`
> still says 77 on mobile.
>
> **K5 doesn't change:** the subprocess + JSON-RPC boundary stays open to
> everyone; the engine becomes not the only way but *the supported way that
> needs no installation*. Every plugin distributed in the repository goes
> through it. — **D-069 changed this:** the subprocess way is gone, K5 was
> rewritten, the engine is the only way.

### 2.8 The plugin engine — DONE (D-050 decision, D-055 implementation) — the Python part void since §2.9

> **D-069:** Everything in this section about Python (finding the
> interpreter, 3.9+, `HEADSHELL_PYTHON`, the declaration in the README) is
> void. The artifact mechanism (item 2, a pinned version + hash) stays, and it
> grew per platform.

D-050's decision was written. Five of the six items are finished, one is open:

1. **The engine was written** — `plugin/runtime.rs`. Finding Python + a version
   check (3.9+), resolving `requires`, downloading the artifact and verifying
   its hash, and on a gap a diagnosis that says what is missing at which step
   (`STEP: PLUGIN_RUNTIME`). A missing dependency now shows without a process
   being opened, in the output of `headshell plugin list`.
2. **`plugin.json` → `requires`** was added. `api` didn't break; adding a field
   doesn't raise the version (§2.1). Old manifests are read with an empty
   list, and a test locks that.
3. **`ytmusic`** dropped its own search for yt-dlp. The
   `HEADSHELL_YTDLP` → `PATH` → `python3 -m yt_dlp` trio was deleted; the
   plugin takes the path from the `requirements` map in the handshake.
4. **`soundcloud` + `echo`** moved to the engine — both write `python3`, the
   engine resolves the interpreter, `requires` is empty.
5. **`torrent` — MOVE CANCELLED (D-056).** D-050 Q3's decision "move it into
   the core as a provider behind a feature" **was reversed**. Torrent stays a
   plugin; `crates/headshell-plugin-torrent` and `plugins/torrent/` stay where
   they are.

   The remaining debt is not the move but **distribution**: the plugin makes
   the user run `cargo build --release`, and that is the only thing that breaks
   D-049. **`TODO: AFTER FIRST RELEASE`** — to be dealt with after the first
   release.
6. **Python 3.9+ was declared** — in the `README` and `CONTRIBUTING`.

> DECISION POINT: how will the engine's private environment be set up?
> **CLOSED — D-055.** `venv` + `pip` **were not chosen**: `ensurepip` isn't on
> every distribution (Debian packages `python3-venv` separately), and there the
> engine couldn't offer the user a way out without root — exactly the place
> D-049 avoids. Instead, **a pinned single-file artifact**: the plugin declares
> a version + an address + a sha256 in its manifest, and the engine downloads
> and verifies it. `pip` is not needed at all.
>
> A measurement had sharpened the question: on this machine the system `pip`
> **is missing** (`python3 -m pip` → "No module named pip") but `ensurepip` is
> there, so a venv could set up a 13 MB environment in 1.4 s. "No pip" and "I
> can't set up a venv" are not the same thing; Debian's package split takes
> away both at once.
>
> **The permission vocabulary (D-040):** the engine's download **doesn't mix
> into** the plugin's `permissions.net` list; it shows on a separate `engine`
> line on the consent screen. If it did mix in, the user would read "this
> plugin connects to github.com"; the one connecting is the engine. D-040's
> wildcard gap didn't close in this round, but it **didn't grow** either.
>
> **Offline:** there are four separate diagnoses, and none resembles another —
> `not installed`, `hash mismatch`, `could not install` (the network),
> `ORPHANED` (the source is 404). The last one is the only case the user can't
> fix, and the message says so.

### 2.9 The plugin engine v2 — QuickJS — DONE (D-069), clean-machine testing TODO

**The problem:** api 1's plugins were Python, and D-050 had declared Python "a
requirement of the project". In practice everyone who wanted to try a plugin
first had to install a runtime (Windows doesn't have one, Debian ships `venv`
as a separate package) — in the user's words, "whoever I sent it to ran into a
problem".

**Done** (`crates/headshell-core/src/plugin/`):

1. **The engine** — `script.rs`: every plugin on its own thread, in its own
   QuickJS runtime. Call time 20 s, loading 5 s, memory 128 MB; a loop is cut
   off even inside `try/catch`. Behind the `plugin-engine` feature (+4 crates,
   ~1.3 MB); the CLI and the desktop turn it on.
2. **`host`** — `host.rs`: `http`, `secrets`, `storage`, `tools`, `log` and
   `console`. Synchronous. The network and tools are forbidden while loading.
3. **The permissions are enforced** — on the request, on every redirect (the
   client doesn't follow redirects; the engine does), on the stream address.
   Wildcards (`*.domain.name`) arrived. The one exception is the tools the
   engine installs, and that is written in every output.
4. **An artifact per platform** — `artifact.rs`: `requires[].assets`, keyed by
   the target the core was built for. The download streams to disk (a 40 MB
   binary). "No release for this platform" is the fifth diagnosis.
5. **The contract is api 2** — `health`, `search`, `resolve_source` are
   exported; `main` in the manifest. api 1 manifests show up as "version
   mismatch", with what to do written down.
6. **The plugins were moved** — `soundcloud` and `ytmusic` to JS; they pass
   their live tests (10/10), and both really play the audio. The `echo`
   fixture to JS.
7. **Torrent was parked** — `parked/`.

Stages: `PLUGIN_LOAD` (manifest/consent), `PLUGIN_RUNTIME` (tool
installation), `PLUGIN_START` (loading the script, checking the exports — the
old `PLUGIN_HANDSHAKE`), `PROVIDER_CALL` (the call). Error types:
`PluginThrew` ("it said no", a message + `main.js:line`), `PluginContract`
("its code can't agree with the engine"), `PluginTimeout`, `PluginCrashed`,
`PluginIncompatible`.

**Testing:** the engine's 73 unit tests with a real QuickJS + a fake network;
in the CLI, the plugin answers in a process whose **environment was emptied
completely** (no `PATH`).

**Clean Linux — the first measurement DONE:** in an `archlinux` container
without Python, yt-dlp or Node, installation, consent, the yt-dlp download,
health, search and stream resolution passed; playback stopped at
`PLAYBACK_OUTPUT` only because the container had no sound card (D-069).

**TODO — clean-machine testing on Windows and macOS.** `plugin install
ytmusic` + `play` in an environment without Python and yt-dlp. D-070 cleared
the way for this (on Windows the app didn't open at all because of `HOME`) and
added a Windows/macOS test job to CI; trying it by hand still hasn't been done
(§3.7).

> DECISION POINT: yt-dlp wants a JS runtime. 2026.08.19 says "JS runtimes:
> none" and carries on with YouTube resolution, but writes that this is
> deprecated (measured). When that road closes, the engine will have to
> download a JS runtime too (deno or `qjs`) through the same `requires`
> mechanism. Not a job today — **ask** when it closes.

### 2.10 The plugin catalog — DONE (D-071)

**The problem:** the plugins sat in `plugins/` in the main repository, and the
user copied them into the data directory with `cp`. Updating a plugin (like
raising the pinned version when YouTube breaks yt-dlp) meant an app release or
copying by hand. The user's request: *"let's keep them in a repo like obsidian
does, and have the plugin list come from the plugins in that repo."*

**Done:**

1. **A separate repository: [`headshell/plugins`](https://github.com/headshell/plugins).**
   The plugins live there as directories; their history was moved over with
   `git subtree split`. The `index.json` at the root carries every plugin's
   manifest and the address + sha256 of its files. The file addresses are
   pinned to the `<name>-<version>` tag: GitHub's raw content cache holds for
   5 minutes (measured, `max-age=300`), and an address pinned to `main` would
   pair the new index with the old file while a new version was being
   published.
2. **The core: `plugin/catalog.rs`.** It reads the index (a broken entry
   doesn't hide the others; each carries its reason), installs the plugin
   (every file's hash is verified, the downloaded `plugin.json` is compared
   with what the index shows, it is prepared in a temporary directory and put
   in place in one step), updates it and removes it. It also produces the
   index (`build_index`): with the same verification as installing — the
   catalog doesn't write its own rule.
3. **The origin record (`origin.json`):** a plugin installed from the catalog
   records which files it brought with which version. An update touches
   **only** a plugin that has a record and whose files are the same as the
   record: it never writes over a plugin placed or changed by hand.
4. **Consent didn't change (the user's decision):** an installed plugin waits
   for consent; if an update grows the permissions, it asks for consent again
   (D-040). A tool (yt-dlp) change **doesn't** ask for consent, but the
   `update` output writes it separately.
5. **Network:** the catalog is read only on an explicit command
   (`plugin catalog`, `install` when the plugin isn't on disk, `update`). The
   address changes with `HEADSHELL_PLUGIN_INDEX`; `https://` is required, plain
   `http` only to this machine.
6. **Surface:** the CLI's `plugin catalog | install | update [<name>] |
   remove <name> | index <dir>`; on the desktop, a catalog section under the
   plugin panel. A new stage: `PLUGIN_CATALOG`.

**Testing:** 27 unit tests in the core (a fake network, a real disk: if the
hash doesn't match nothing is written, if the manifest contradicts the index it
isn't installed, an update keeps `state/`, an update left half-done doesn't
count as a local change, for a plugin placed through a link only the link is
removed). In the CLI, end to end against a server opened on `127.0.0.1`: index
generation → catalog → install → consent → engine → a new version → update →
removal. The live tests of SoundCloud and YouTube Music now install the plugin
**from the live catalog**.

---

# PHASE 3 — GUI and Theme

> **THE NEXT PHASE — D-027.** After Phase 1 closed, this was chosen instead of
> Phase 2: this is where the community's engine is (D-002, D-004), and
> delaying it is expensive. Phase 2 was postponed, not cancelled.

**Goal:** A Tauri desktop interface + a theme system users can write. The theme
ecosystem is this project's distribution channel, not an ornament to be added
later.

### 3.1 GO / NO-GO measurement — DONE (conditional GO)
A prototype in Tauri with a 50,000-row virtualized list + CSS animation + IPC
load. **Measure WebKitGTK on Linux.** The audience is mostly Linux, and
WebKitGTK is the weakest of the three platforms.

> DECISION POINT: Present the measurement result. If it's unacceptable,
> Dioxus/a native Rust GUI is discussed — but in that case the CSS theme
> ecosystem is lost. **CLOSED — D-028: GO, with an environment condition.**

**Measured** (Intel HD 6000 / 2015, WebKitGTK 2.52.6, Tauri 2). The thresholds
were written before the measurement (`spike/tauri-gonogo/THRESHOLDS.md`). All
eight measures are GO; only the "pure CSS" phase is borderline.

Three things came out of this, and they bind the following sections:

1. **On Linux `GDK_BACKEND=x11` + `WEBKIT_DISABLE_DMABUF_RENDERER=1` are
   required.** In the default environment the frame rate drops 2.4× (58.8 →
   23.8 fps). Both are needed **together**; each alone doesn't help, and one
   makes scrolling worse. **D-029: the app sets them up itself** — the first
   thing `main()` does, only on Linux, only if the variable isn't defined.
   Verified: without the outside variable, 23.8 → 55.6 fps. An open snag:
   `set_var` is `unsafe` in Rust 2024 and the workspace says
   `unsafe_code = "forbid"` — to be solved when the GUI package is opened
   (D-029).
2. **§3.2's reasoning changed** — see below.
3. **§3.3 gained a constraint** — see below.

That the culprit is the engine, not the hardware, was established with a
**control experiment**: on the same machine, Firefox draws the same page at
58.8 fps in all four phases. If it were the hardware's ceiling, escaping to a
native Rust GUI wouldn't have saved it either.

### 3.2 The IPC contract — DONE
~~Hundreds of messages per second between the webview and the core = stutter.~~
**D-028 measured this and didn't confirm it:** the IPC round trip p95 is
**1 ms**, 30 Hz polling adds **1 ms** to the frame time, and the bridge carries
**~10,000 events/s**. Batching isn't needed for performance.

The playback position is still **predicted from the anchor** in the webview —
but the reason isn't performance:
- If IPC stalls, the interface doesn't freeze; the prediction keeps walking.
- Phase 4's room primitive is the same type anyway (D-015); two separate
  position concepts aren't kept.

A correct design defended with the wrong reason falls at the first objection;
the contract will be written with this corrected reasoning.

**The core side is ready (D-032).** `playback::LiveSession` — it ties `Player`
and `Session` together; a single `tick()` advances + writes the accumulated
listens + returns a `TickReport`. The shell only drives the loop. The TUI was
moved onto it; the GUI will use the same type, and the dance won't be written a
second time.

Writing now happens **every round**, not on exit: in an interface left open for
hours, a crash would take the whole session's history with it. A record that
can't be written isn't thrown away; it is held and retried; `store_error` and
`listens_pending` make this visible (K9).

**Package layout (D-030):** three packages — `headshell-core`,
`headshell-cli`, `headshell` (GUI). The dependency goes one way:
`headshell-cli` and `headshell` never see each other. If one wants something
from the other, that thing belongs to the core.

#### The shape of the contract

**The contract = the core's surface + serde.** No separate "IPC type" layer is
written: every command already returns an existing core type. A translator
layer would make the two types drift over time, and nothing would catch the
drift. The `--json` output already proves this — the CLI and the GUI get
**the same** data.

**The commands** match the CLI's subcommands one to one, because both are
shells of the same core:

| Area | Commands |
|---|---|
| Library | `search`, `stats`, `sleeve` |
| Import | `import` |
| Identity | `resolve` |
| Provider | `providers`, `provider_test`, `provider_scan`, `servers_list`, `server_add`, `server_remove` |
| Playback | `play`, `toggle_pause`, `stop`, `next`, `previous`, `jump_to`, `set_shuffle`, `set_repeat` |
| State | `anchor`, `queue` |
| Diagnostics | `diag`, `diag_text` (D-072: the text of `DiagReport::render()`) |

There is also `environment`: the data directory, the database path, the music
directories. Not a new "state" type — so that the question "which library did
I look at" isn't left unanswered next to an error message (K9).

**Events** are sent only **when something changed**, not on a timer. The GUI's
Rust side drives the `LiveSession::tick()` loop; a `TickReport` crosses to the
webview only if one of these is present:

- `track_changed`, `listens_recorded`, `store_error`, `finished`, **or**
- **the part of the anchor that feeds the prediction has changed** — the
  state, the rate, the duration, the track ID. `position_ms` is deliberately
  not on the list: predicting it is the prediction's job.

The last item is **D-035**. The first list was written without it, and it
silently froze the interface: the audio started as `Buffering` and moved to
`Playing`; since the transition wasn't sent, the webview stayed with the
`Buffering` anchor it had, and since `Buffering` doesn't advance, the progress
bar froze at 0:00. No error showed. In this rule the real risk is not too many
messages but **missing** ones: the excess is measured, the missing leaves no
trace.

In the silence between, the position is predicted from the anchor.

Long commands (`import`, `resolve`, `provider_scan`, `provider_test`,
`server_add`, `play`) also send `headshell://busy` — that too is a state
change, not a timer.

#### No versioning needed — and that has a limit

Unlike §3.3, here **both sides are inside the same binary**: the webview
assets are packaged with the app and can't be updated independently.
Negotiating a version at run time would be teaching a handshake to a process
that can only talk to itself.

**The limit is here:** if an IPC surface is opened to themes (§3.3), the
contract turns into an *external* contract, and that day the versioning debt
is born. This question must be answered while §3.3 designs the token set —
will a theme be able to call IPC?

#### Drift protection: a shared accuracy set

In the webview the position is predicted without asking the core, so a second
copy of the formula lives in JS. Two copies drift over time, and the drift
starts where nobody notices — the progress bar lies by a few hundred
milliseconds, nobody complains, and then **in Phase 4 the same formula drives
room sync.**

`fixtures/anchor/position_cases.json` is the single source of truth both sides
read (12 cases). `headshell-core/tests/anchor_parity.rs` binds the Rust side;
`headshell/ui/anchor.js` + `headshell/tests/anchor_parity_js.rs` bind the JS
side.

The set's most valuable case was found while writing it: with `rate = 1.001`
at 100 s, the core says **100099 ms**, not 100100 — `100000 × 1.001` isn't
exact in binary, and `as u64` truncates. If JS used `Math.round`, the two
copies would split exactly here. The right counterpart is
`Math.floor(elapsed_ms * rate)`.

The JS run once needed `node`, and without `node` the test **didn't skip, it
failed**: if it could be skipped, it would turn green on a machine where it
isn't installed and prove nothing — a test had been deleted in D-032 for
exactly this reason. D-070 kept the rule and removed its price: `anchor.js` is
now evaluated in the same embedded QuickJS as the core's plugin engine; the
test still never skips, but it asks nothing of the machine.

#### Package — DONE

`crates/headshell/`: the Tauri shell, binary name `headshell-desktop` (D-034).
The Rust side is `main.rs` + `env.rs` (the D-031 environment fix) + `state.rs`
+ `core_thread.rs` + `commands.rs`; the webview is plain static files under
`ui/` — no bundler, no npm, `frontendDist: "ui"`.

**The core lives on its own thread (D-034):** `Session` is `Send` but not
`Sync`, and `import_archive`'s future isn't `Send`, while Tauri wants command
futures to be `Send`. A channel instead of a lock: commands send a closure to
the core thread and wait for the answer, and the tick loop is on the same
thread. The core didn't change for this.

Verified: the window opens, `environment`/`queue`/`anchor` answer, scan and
search results give the same numbers as the CLI's `--json` output, a local file
plays and the progress bar walks from the anchor. The error path was seen too —
with the `STEP: PLAYBACK_RESOLVE` stage, its full chain folded.

### 3.3 The theme API — a versioned contract
Spicetify's biggest headache: the host app changes, and the themes break. To
avoid living through that, a semantic token set (CSS custom properties),
committed slot names and a **versioned theme format** are designed from the
start. Whatever changes inside, this surface stays fixed.

**A constraint from D-028 — the contract must also say what can be animated.**
Even in the corrected environment, `height` / `box-shadow` / `filter` /
`background-position` animations take 58.8 → 47.6 fps; `transform` +
`opacity` don't drop it at all. If the theme author isn't told this, the
difference shows up on **the user's** machine, and the one blamed is the app,
not the theme.

**Language closed (D-036):** token names — and all identifiers in general — are
English. The theme set is this project's most outward-facing surface; the one
consuming it is a theme author we don't know. Comments and interface text
stayed Turkish — until D-073 made them English too (K11 since D-074). While
this decision was applied, `crates/headshell`, the shared accuracy set and the
audio fixtures were translated as well.

> DECISION POINT: Present the token set before writing it. Once it is
> published, a backward-compatibility debt is born.
>
> Four questions to answer: (1) granularity — a narrow semantic set, or a
> layered set with region overrides; (2) the selector promise —
> `data-headshell="..."` attributes, class names, or neither; (3) will IPC be
> opened to themes (D-033 left this here: if it opens, the contract turns into
> an *external* contract, and that day the versioning debt is born); (4) the
> package format and the `api` version field.
>
> **CLOSED — D-037:** (1) a narrow semantic set; (2) class names are part of
> the contract (the existing `.topbar` etc. are now stable; D-036's "internal
> detail" assumption is void here); (3) no, themes are appearance only — no
> IPC; (4) a manifest + an `api` version field, explicitly rejected on a
> mismatch.
>
> **A new open item — "mod" packages.** In D-037 the user proposed a "mod"
> concept, distributed together with theme packages, that changes behaviour.
> It was deliberately postponed. A mod running inside the webview and calling
> IPC carries a security class radically different from the "subprocess +
> JSON-RPC" model K5 requires for provider plugins (it can crash in the same
> webview and freeze the whole interface). A separate round is needed after
> §3.3 closes:
>
> DECISION POINT: How do mods work — a separate process (the K5 model, but
> then the "inside the webview" promise falls), or a limited/permissioned
> webview JS sandbox? Which subset of IPC (if any) is opened? Do they share
> the theme's package format, or a separate one? **Ask.**
>
> **D-069 note:** "the K5 model" is no longer a subprocess but QuickJS
> embedded in the core + the `host` gates. When the mod question opens, there
> is a third option: running a mod not in the webview but in the plugins'
> engine, with a permissioned subset of `host`. The question is still not
> closed.

#### Token set v1 — DONE (D-037)

The `:root` block of `crates/headshell/ui/style.css` is now the contract
itself. Fourteen tokens, each used at least once in the file — no unused
token:

| Token | Value | Meaning |
|---|---|---|
| `--headshell-color-scheme` | `dark` | the parts the engine draws itself (checkbox, caret, scrollbar) |
| `--headshell-bg` | `#12100e` | page background |
| `--headshell-surface` | `#1b1815` | panel background (topbar, sidebar, player, block) |
| `--headshell-surface-raised` | `#221e1a` | interactive/hover background (input, active nav, hover, toast) |
| `--headshell-border` | `#2e2925` | border, divider, progress bar track |
| `--headshell-text` | `#eae2d8` | primary text |
| `--headshell-text-dim` | `#9a8f83` | secondary/dim text |
| `--headshell-accent` | `#ffb454` | brand + interaction accent |
| `--headshell-success` | `#7bd88f` | playing / success state |
| `--headshell-error` | `#ff6b6b` | error state |
| `--headshell-info` | `#79c0ff` | buffering / info state |
| `--headshell-radius-sm` | `6px` | control corner radius (nav, button, input, queue row) |
| `--headshell-radius-lg` | `8px` | panel corner radius (block, toast, dropzone) |
| `--headshell-duration` | `120ms` | the single animation duration (`opacity`/`transform` only, D-028) |

`--headshell-color-scheme` is not the thirteenth but **the fourteenth**: it
wasn't in the first set; it came up while §3.4's light theme was being written
and was added later (D-039). The tokens only change the colours *we* draw; the
checkbox, the text caret and the scrollbar are parts the engine draws itself,
and they stayed dark on a light palette. That was exactly the reference
theme's job.

Hover/focus/disabled got no separate colour token — hover is already expressed
as a combination of existing tokens (`.nav:hover` → `--headshell-text`,
`button:hover` → a `--headshell-accent` border). Right now no rule defines
`:focus-visible` or a disabled state; instead of writing an unused token, this
was left as **a known gap** — the token comes when a real visual rule is
written.

Class names (D-037/2) are now part of the contract: `.topbar`, `.sidebar`,
`.nav`, `.content`, `.queue` (+ `.index`, `.current`), `.player`, `.controls`,
`.now`, `.state.{playing,paused,buffering,stopped}`, `.bar`, `.fill`, `.time`,
`.toast` (+ `.stage`, `.info`), `.block`, `.summary` (+ `.big`, `.label`),
`.table`, `.ranking`, `.dropzone` (+ `.over`), `.busy`, `.brand`, `.check`,
`.row`, `.grid`, `.hint`, `.empty`, `.two-col`, and the theme screen itself:
`.themes`, `.theme` (+ `.name`, `.author`, `.current`), `.tag` (+ `.warn`),
`.rejected` (+ `.id`).

**Adding** a new class doesn't raise `api` — old themes didn't target it. What
raises it is removing an existing one or changing its meaning. The same rule
holds for tokens, and `--headshell-color-scheme` is its first example.

#### Manifest format and loader — DONE

A theme is a directory of two files:

```
<theme-directory>/
  theme.json   # manifest
  theme.css    # only :root { --headshell-*: ...; } — no extra selectors
```

`theme.json`:

```json
{
  "name": "Example Theme",
  "author": "Someone",
  "api": 1
}
```

`api` is the version of the token/class contract in this table. At startup the
app compares it with the `api` number it supports itself; on a mismatch it
**doesn't silently ignore** the theme but rejects it, saying which theme
expects which version (K9). `theme.css` spilling outside `:root` (e.g. writing
`.topbar { ... }` and relying on a new selector) isn't prevented for now — its
engine is CSS, and restricting it is a separate step (see below).

> DECISION POINT (before implementation, small): if `theme.css` writes
> outside `:root` (e.g. targets `.topbar` directly and skips the tokens),
> should it be rejected or left free? Rejecting locks the contract at the
> token level but takes away the theme author's slightest flexibility (e.g.
> adding a special `box-shadow` to a single element — which D-028 doesn't
> recommend anyway).
>
> **CLOSED — D-038: neither reject nor allow freely — flag it.** The loader
> loads it, but labels it in the theme list as "extended / no guarantee". The
> guarantee always applies only to the tokens in `:root`.

**Applied:** `crates/headshell/src/theme.rs` — in the shell, not the core,
because this isn't a domain concept headshell-core would carry but a CSS
mechanism specific to the webview. The mobile bindings won't use CSS custom
properties; the Golden Rule's test "can the core offer this" points at
`crates/headshell` here. Three IPC commands: `themes_list`, `theme_active`,
`theme_select`.

The selection is in `<data_dir>/ui.json`; the schema didn't change, the
database wasn't touched. The store keeps no state; every call reads the disk —
the theme author edits the file, says "refresh the list" and sees the change,
without having to close the app.

Four behaviours are tied to K9 and locked with tests (18 tests):

- **An `api` mismatch is rejected, and both versions are written** ("the theme
  wants version 9, this build offers version 1"). "My theme doesn't show up"
  shouldn't be a diagnostic question.
- **A theme spilling outside `:root` is loaded but flagged** (D-038): in the
  list, "extended · no guarantee".
- **If the selected theme was deleted, it falls back to the default, but with
  the reason.** A silent fallback would mean the user never learns why their
  theme disappeared.
- **A directory that shadows a built-in theme's name doesn't silently win**;
  it is rejected with the reason — which file wins shouldn't be a guess.

The order is deliberate too: **validate first, then write.** Saving a theme
that can't be loaded as the selection would start the app with a broken
selection the next time it opens; a test locks this.

### 3.4 Reference themes — DONE
Prove on at least two different themes that the theme API is sufficient.

**Both are under `crates/headshell/themes/` and ship with the app**
(`include_str!`) — but **they have no privileges**: they go through the same
validation as a theme on disk. If they were privileged, they would prove only
themselves, not that the contract is sufficient.

| Theme | The axis it tests |
|---|---|
| **Daylight** (`daylight`) | colour — the base interface was written dark; with the same contract a light interface comes out |
| **High Contrast** (`contrast`) | **not** colour — `--headshell-radius-*` drops to zero, `--headshell-duration` to `0ms` |

The second theme is deliberately not a second palette: with two palettes the
"two themes" criterion would be met on paper, but the token set's non-colour
axis would never have been tested. If the radius and duration tokens are really
used, it shows in this theme — if they aren't, they are dead tokens. A test
locks this (`the_two_reference_themes_differ_on_more_than_color`).

**And the contract wasn't enough.** Although the light theme applied all the
tokens correctly, the checkboxes, the text caret and the scrollbar stayed dark:
they are drawn not by us but by the engine. The gap was closed as
`--headshell-color-scheme` (D-039). §3.4's "prove it is sufficient" criterion
was exactly for catching this, and it caught it — if two themes hadn't been
written, the gap would have shown up on the first theme author's machine.

**Verified end to end** (2026-09-01, WebKitGTK): the selection is read from
`ui.json` and applied at startup, the theme screen shows all three states —
built-in/extended/rejected — correctly, and a theme asking for `api: 9` stays
in the list with its reason.

---

### 3.5 The shell catches up with Phase 2 + packaging — DONE (D-057)

When Phase 3 was written, Phase 2 had been postponed (D-027), and the order
later reversed: the shell was left unaware of the capabilities that came
afterwards. This section closes that gap and makes the desktop packageable.

**The interface gained Phase 2.** The plugin list, consent/install/disable and
secret entry are now in the interface; they are **the same** core calls as
`headshell plugin …` and `headshell secret …`. The `sleeve` card became visible
too — it was registered in IPC, but `app.js` never called it, so Phase 0.5's
product wasn't on the desktop.

**The plugin state is shown without choosing.** To fit on one line, the CLI
chooses a priority between a manifest problem / a missing artifact / the
consent state; the interface has no such squeeze, and all three stand side by
side. That way no second copy of the choosing logic was born — logic that isn't
copied can't drift.

**Packaging was turned on.** `bundle.active: false` hid three things: 103-byte
placeholder icons, missing `.desktop` data, and a version written in two
places. The `version` field was removed from `tauri.conf.json` — Tauri reads it
from `Cargo.toml`, so there is one source. The packages are produced by
`.github/workflows/release.yml`: a `v*` tag uploads to a draft release, and a
manual trigger only leaves an artifact.

**The interface's silent breakage is tested.**
`crates/headshell/tests/ui_contract.rs`: does every `id` `app.js` looks for
exist in the page, do the sidebar and the `Ctrl`+number shortcut name the same
panels, are D-037/2's class names still reachable, is every declared token
used. There is no type checking in the webview: a typo in an id leaves the
window empty, and the compiler doesn't see it.

**User-facing changes:** a three-step *getting started* card instead of an
empty queue; importing in its own tab (it used to be under "diagnostics") with
its report summarised in numbers; keyboard shortcuts (listed with `?`);
dismissible notices; a diagnostics report copied to the clipboard; text saying
what to do in every empty state.

**Verified locally** (Arch, WebKitGTK): `.deb` and `.rpm` are produced, and the
package contains the icons, a `.desktop` entry with the categories
`AudioVideo;Audio;Music;`, `copyright` + the two license files under
`/usr/share/doc/headshell/`, and the version `0.0.1-beta` coming from
`Cargo.toml`. The license files go in through `bundle.linux.{deb,rpm}.files`:
the `bundle.licenseFile` field doesn't reach the Linux packagers, and that was
seen only when the package was opened up.

**Both debts closed too — measured, not assumed.**

1. **The AppImage is produced in CI.** Locally (Arch) it keeps failing:
   linuxdeploy carries its own old `strip`, which doesn't recognise libraries
   with a `.relr.dyn` section. On the Ubuntu runner the expected behaviour came
   out — `headshell_0.0.1-beta_amd64.AppImage`, 85 MB, sitting in the draft
   release. It stays in a separate step: a failure shouldn't take deb/rpm with
   it.
2. **macOS and Windows were run by hand.** `workflow_dispatch` (run
   35571122203) is green on all three platforms; so is the tag run
   (35597289680). The draft release has nine assets: `.deb`, `.rpm`,
   `.AppImage`, `.dmg`, `.msi`, an NSIS `.exe` and three CLI archives.

> **macOS was arm64-only** — D-070 closed that: the `.dmg` and the CLI archive
> are now a universal binary (`universal-apple-darwin`, `lipo`),
> cross-compiled on the same arm64 runner; a second runner wasn't needed. Not
> verified until the first `workflow_dispatch` run.

### AUR packages

The Arch side is under `packaging/aur/`: three PKGBUILDs and four packages — a
split package that builds from source (`headshell` + `headshell-cli`), and the
two prebuilt ones separately (`headshell-bin`, `headshell-cli-bin` — there is
no shared build there). The desktop entry is in a single file,
`packaging/headshell.desktop`. The packages were built with `makepkg` in an
Arch container, and `namcap` is clean on all three; a full build of the source
package hasn't yet been run on a real Arch machine. The decision and reasoning
are **D-066**; the release publishing order is in `packaging/aur/README.md`.

> **The `-bin` package doesn't work until the release leaves draft:** the
> assets of a draft release can't be downloaded anonymously. The source
> package has no such debt; the tag archive is independent of the draft.

---

### 3.6 Phase 3 status — CLOSED

**458 tests**, clippy and fmt clean.

| Section | Status |
|---|---|
| 3.1 GO/NO-GO measurement | DONE — conditional GO (D-028); the app sets up the environment (D-029/D-031) |
| 3.2 IPC contract | DONE — the core's surface + serde (D-033), events on change (D-035) |
| 3.3 Theme API | DONE — 14 tokens + class names + manifest/loader (D-037…D-039) |
| 3.4 Reference themes | DONE — `daylight` (colour) + `contrast` (radius/duration) |
| 3.5 Phase 2 surface + packaging | DONE — plugins/secrets/sleeve in the interface, bundle on (D-057) |

The document for theme authors: `crates/headshell/themes/README.md`.

**An open decision handed over from Phase 3:** "mod" packages — plugins that
change behaviour, distributed together with theme packages. Deliberately
postponed in §3.3; the decision point text stays there. Since the theme
contract is closed, it can now be opened as a separate round. **It doesn't
leave Phase 3 incomplete:** themes work today as they were designed, and mods
will come beside them, not on top of them.

> The end of this section once said "the next job is **Phase 2**" — the order
> had reversed in D-027, and Phase 3 finished first. **Phase 2 closed after
> that sentence** (§2.6); the sentence was removed once it went stale.

With Phase 3 closed, the remaining job is not code but **distribution**:
publishing the first release. See §3.5 above — the packages are produced, the
AUR packages are written, the release is still a draft.

### 3.7 Platform portability — DONE (D-070), trying Windows/macOS by hand TODO

**The problem:** the code had been written and tested only on Linux, and it
stayed tied to Unix in three places and to a single machine in a few:

1. **The data directory** looked only at `HOME`; a standard Windows doesn't
   define `HOME`. The CLI gave an error and stopped; the desktop closed
   **without saying anything** (the Windows release build has no console).
2. **The music directory list** was split on `:` — `C:\Music` would be cut in
   two.
3. **The Subsonic salt** was read from `/dev/urandom`; on Windows it fell back
   to a weak fallback every time (saying so, but falling back).
4. **The tests** opened directories in `/tmp` and never deleted them: on a
   development machine, 1,100 directories, 1.2 GB — and on that machine `/tmp`
   is a tmpfs, that is, memory. The YouTube Music tests' cache could hide a
   dead download address on that machine.
5. **A test needed `node`** (§3.2).
6. **Line endings** weren't preserved: a checkout on Windows with
   `core.autocrlf` on would break the snapshots.
7. **The Linux packages** were built on Ubuntu 24.04 and needed glibc 2.39 —
   they didn't open on Debian 12 (measured). **The macOS package** was
   arm64-only.

**Done:**

- The data directory per system: Linux/BSD `~/.local/share/headshell`, macOS
  `~/Library/Application Support/headshell`, Windows
  `%LOCALAPPDATA%\headshell` (the user's decision). `HEADSHELL_DATA_DIR` and an
  explicitly defined `XDG_DATA_HOME` come first on every system. The
  resolution is in a pure function; all three systems' branches are tested on
  every machine.
- The music list goes through `std::env::split_paths`; the usual music
  directory per system.
- The salt: if there is no `/dev/urandom`, a `RandomState` keyed with the
  operating system's randomness.
- If the desktop can't open, the error is shown in a **window** (on every
  system).
- The tests use self-deleting directories; the integration tests are in
  `target/tmp`. The yt-dlp cache checks the hash and that the address is alive
  on every run.
- `anchor_parity_js` runs in the embedded QuickJS; `node` isn't needed.
- `.gitattributes`: LF everywhere, binary fixtures without conversion.
- A Windows and macOS job in CI: clippy + the tests. The Windows counterpart of
  the tool tests uses a copy of `cmd.exe`.
- The release pipeline: Linux on 22.04 (glibc 2.35), macOS a universal binary.
- **The BSDs can be built, not tried (canary):** the plugin engine's bindings
  are generated there at build time with `libclang` (the user's decision:
  "let them be buildable, but write them down as not tested").

**Testing:** the three gates on Linux. A cross-compile to the Windows target
(with Zig) — the core and the CLI, tests included, clippy clean: the first
compile of the `cfg(windows)` code. It was measured that the CLI built in an
Ubuntu 22.04 container opens on Debian 12. That the startup error window
carries the text was verified separately with the `url` version Tauri uses and
with `decodeURIComponent` in QuickJS; the window's content couldn't be captured
as an image on this machine.

**TODO:** trying it **by hand** on Windows and macOS — the CI job runs the
tests but doesn't open a window on a desktop or play audio. §2.9's "on a clean
machine, `plugin install ytmusic` + `play`" item is here too.

### 3.8 Interface redesign — DONE (D-072)

**The problem:** the interface had fallen behind the recent changes and made
no room for the coming phases. After D-069/D-071 the plugin panel was three
sections stacked on top of each other (installed, catalog, secrets), and every
plugin had five buttons regardless of its state. The interface was below what
the CLI shows: statistics had no albums, years or durations, importing had no
fingerprint and local-key links; resolution and `diag` printed raw JSON. A flat
list of eight tabs would only get longer when Phase 4's rooms and the mods
arrived.

**Done:**

1. **The skeleton** — a full-height sidebar with four groups: *listen* (now
   playing, library), *history* (stats, sleeve, import), *sources* (providers,
   plugins), *system* (appearance, diagnostics). A future section is not a new
   flat tab but a new row in a group: rooms go into *listen*. Today there is no
   placeholder for any of them (K10). Sleeve has its own section (D-004). In a
   narrow window the sidebar collapses to icons.
2. **The motion layer** — `ui/motion.js`: springs (damping ratio + response),
   interruptibility, velocity handoff, momentum projection, rubber-banding; no
   dependency. `transform` and `opacity` only (D-028). The response is three
   times `--headshell-duration`; `0ms` means no motion, and
   `prefers-reduced-motion` turns off positional motion.
3. **The gaps were closed** — year bars in statistics (which are also a year
   picker), albums and durations; the chain's five links in importing;
   resolution is readable; diagnostics is the core's `render()` text
   (`diag_text`); in the theme list, a preview of each theme in its own
   colours (`theme.rs`).
4. **Plugins** — *installed / catalog / secrets* tabs; buttons by state
   ("approve" for one awaiting consent, "install the tools" for one with a gap
   that installing will fix); in the sidebar, the number of plugins waiting for
   something from the user. The catalog is still read only with a button
   (D-071).

**The contract didn't change:** the fourteen tokens and their values are the
same, and no class was removed from `CONTRACT_CLASSES` or changed its meaning —
`api` 1. The layout changed; extended themes that assume positions (no
guarantee) may feel it, and that is written in the theme guide.

**Testing:** `motion_js.rs` (9 tests, embedded QuickJS; a deliberate sign error
put into the spring formula was caught by three of them), `ui_contract.rs` +3
(the icon set, the markup ban, inline `style`), `theme.rs` +5 (the preview).

**Verification:** in a Debian 13 container, with the same WebKitGTK as the host
(2.52.6), Xvfb and a null audio device — the host's screen was locked. Seven
flaws showed only in screenshots and were fixed (D-072).

**Still open:** the "playing" state itself wasn't seen (the null device doesn't
wait in real time — seen in §3.9); the Windows and macOS webviews weren't
tried; volume isn't there because the core has no command for it (D-033) —
seeking is, since §3.25.

### 3.9 Now playing as a sheet — DONE (D-075)

**The problem:** "now playing" was one section among nine, reached from the
sidebar, and the player bar — where the playing track is — led nowhere. A
modern player opens a detailed view from its bar (the user's reference: YouTube
Music).

**Done:**

1. **The bar opens a sheet over the content row** — a click outside its
   buttons, the chevron at its end, a drag upwards — and so do the sidebar row,
   `Ctrl`+`1` and starting a track. The section under it keeps its place;
   `Esc`, the handle, a drag down or another section close it. A track the
   queue moves on to does not open it.
2. **The sheet is the "now playing" section**, not a copy of it: a turntable in
   place of the missing cover art (the record turns while playing, the arm
   follows the position from groove to groove), the track, and the queue with
   its own scroll. An empty queue shows "getting started" there.
3. **The motion layer** got a vertical drag and `rotate`, and two faults in its
   drag were fixed: the distance past the threshold was lost, and a release
   after a pause was thrown.

**Not done:** cover art (a core capability — by D-033 a CLI subcommand first;
done in §3.10), lyrics (the same — done in §3.12) and related tracks (no data
in the core), seeking (done in §3.25) and
volume (§3.8).

**Testing:** `motion_js.rs` +2 — the spring's linearity (the sheet, its scrim
and the chevron move on proportional springs) and the drag's two faults; both
fail when the fault is put back.

**Verification:** the §3.8 container with a PulseAudio null sink instead of
ALSA's null device: it is paced in real time, so the "playing" state was seen
this time — the three themes, a 900×600 window, drags, flicks, pause and stop.

### 3.10 Covers — DONE (D-076)

**The problem:** the "now playing" record had a plain label and the queue plain
rows: the core had no covers (§3.9's "not done").

**Done:**

1. **The chain is in the core** (`artwork/`). First where the track lives, not
   gated: a local file's tags (ID3 `APIC`, FLAC `PICTURE`, MP4 `covr`, Vorbis
   `METADATA_BLOCK_PICTURE` — symphonia gives all four) and then its folder
   (`cover`/`folder`/`front`), Subsonic's `getCoverArt`, Jellyfin's item and
   album `Primary` image, a plugin's `artwork(id, size)`. Then, **only online**:
   the identity chain's recording at MusicBrainz, its release titled like the
   track's album — and when the recording is on no such release, a search for
   the album itself — and the Cover Art Archive's front for it or its release
   group. No Spotify (K4), no service that wants a key.
2. **Online is a choice:** the CLI's `--online`, the desktop's
   `HEADSHELL_ONLINE=1` — since §3.15 a setting, the variable deciding one
   launch over it. Offline, a cover that was not looked up says so —
   "not looked up" and "not found" are different answers (K9). MusicBrainz is
   asked at most once every 1.1 s by identity and covers together.
3. **The cache** is `<data>/artwork/`: images named by their content and an
   `index.json`; one answer per album, "not found" believed for 30 days.
4. **`headshell artwork`** walks a query's tracks or a file through the chain
   and reports every track's answer and a summary; `--out` writes the covers.
   The desktop's `artwork` mirrors it; `artwork_queue` and `artwork_images` are
   the playing queue's side.
5. **The queue is looked up in the background**, the playing track first;
   playback never waits. The tick names the covers whose answer arrived, and
   the interface asks for their images as `data:` URIs (the CSP allows no
   other). The label becomes the cover — round, larger, turning with the
   record — and every queue row gets a square thumbnail or a small record in
   its place; a note under the queue says what could not be looked up.
6. **Plugins, api 3:** the manifest must say `"artwork": true|false`; a plugin
   that gives covers exports `artwork(id, size)` and fetches the image itself
   through `host.http`'s new binary mode (K5: the core fetches no address a
   plugin gives). A plugin's failed or missing cover falls back to the chain.
   The catalog's `soundcloud` 0.3.0 and `ytmusic` 0.5.0 give covers.

**Not done:** covers in the player bar and anywhere else (not asked); an
in-app switch for online (done in §3.15); an MP4 fixture (symphonia's `covr` path is read by the same code, but
no fixture holds it).

**Testing:** the chain on canned answers (the namesake-recording trap among
them: it fails when the album guard is taken out), the cache, the image
checks and resizing, the worker, Subsonic/Jellyfin/plugin covers on a fake
server, the embedded-cover FLAC and MP3 fixtures; `headshell artwork --json`
has a snapshot; live: MusicBrainz + the Cover Art Archive, and each catalog
plugin's cover (they skip without a network, D-043).

**Verification:** the container of §3.9: a local library with covers in FLAC
and MP3 tags, a folder cover, a coverless made-up band and a coverless real
recording, offline and with `HEADSHELL_ONLINE=1`.

### 3.11 Reading the queue ahead — DONE (D-077)

**The problem:** with YouTube Music, "next" waited 5.6–7 s and Enter ~10 s
(release build, measured). Nearly all of it was yt-dlp resolving the address
(4.7 s); the audio is cheap. D-024 read the next track ahead, but inside
`tick`: a round held the core's thread 4.7 s, and pause waited that long.

**Done:**

1. **A read-ahead thread** per playing session (`playback::prefetch`): the
   next track and the one after it, always, and a third that statistics pick
   — three at most (the user's decision). Each one's address is resolved and
   the first 512 KB of its stream fetched. The player hands the plan over and
   never waits for it; `tick` takes no time.
2. **Memory only** (the user's decision): no audio is written to disk. An
   address is trusted for 8 minutes — SoundCloud signs for eleven, YouTube for
   six hours (measured). A head is kept while its track is planned.
3. **Starts begin from what is ready:** "next" starts from the head, "previous"
   from the address kept; a track being resolved at that moment is waited for,
   not resolved twice. An address that no longer opens is resolved once more;
   a stream begun from its head whose address went stale gets a new one while
   the head plays.
4. **The user's calls first:** a plugin runs one call at a time; a call made on
   the user's behalf goes before the read-ahead's and the covers', and a tool
   a background call is running is stopped for it (`PluginPreempted`: "gave
   way", not "failed").
5. **The third track** is guessed from how the user listens
   (`prefetch::likely`): the queue's continuation for someone skipping
   through it, a track or artist they often play for someone who picks from
   it. The session's habits (skips, picks, going back) and the history's plays
   and skips (`SqliteLibrary::listening_profile`, by the play rule) weigh it;
   the report says why in a sentence.
6. **Every HTTP stream waited for its whole file** (found on the way):
   symphonia 0.6 reads a source's end first when told its length. The source
   no longer reports it; opening the 7.5 MB m4a went from 1.77 s to 0.1–0.6 s.
7. **`headshell prefetch "<query>" [--all]`**: what a playing session does for
   its queue, as a command, timed. The desktop mirrors it as `prefetch`;
   `prefetch_queue` is the playing queue's plan and counts.

**Measured** (release, YouTube Music, `examples/latency_probe.rs`): next 6.42 s
→ 0.02 s, previous 7.02 s → 0.16–0.59 s, a far track 6.95 s → ~5 s, Enter 9.73 s
→ ~7 s, three "next"s half a second apart 0.01–0.03 s each, the longest round of
`tick` 4.71 s → under 5 ms.

**Not done:** the first track after Enter still waits for the search and the
address — there is nothing to read ahead; yt-dlp's unpacked builds may cut its
~1.15 s start (not measured). Lifetimes a provider could declare (the core
trusts 8 minutes for all). Resuming a stream that breaks mid-way (not after a
head).

**Testing:** the read-ahead thread against a provider answering from memory
(order, in-flight waits, failures asked once, giving way, lifetimes, stale
streams, heads kept and dropped with the plan); heads and the rest on a local
server (a range from exactly that byte of exactly that file, a server that
ignores ranges gives no head, a stale address replaced, no new address is an
error, not a short track); the plugin gate and a stopped tool (Unix and
Windows); the third track's guess; the listening profile; a real local queue
played with next and previous; `headshell prefetch --json` has a snapshot.

**Verification:** `examples/latency_probe.rs` in a Debian 13 container with a
PulseAudio null sink, before and after, and the live plugin tests.

### 3.12 Lyrics — DONE (D-078)

**The problem:** the sheet had one list beside the record, the queue; the
reference's lyrics tab was missing because the core had no lyrics (§3.9). The
user's ask: synced lyrics, fetched as LRC, **for exactly the recording that
plays**.

**Done:**

1. **The chain is in the core** (`lyrics/`). The file's own first — a `.lrc`
   beside it with its name, then its tags (ID3 `SYLT`/`USLT`, Vorbis
   `LYRICS`, MP4 `©lyr`); they win even unsynced. Then the server's or the
   plugin's own (`Provider::lyrics`: OpenSubsonic `getLyricsBySongId`,
   Jellyfin `/Audio/{id}/Lyrics`, a plugin's `lyrics(id)`), synced ones
   winning. Then **LRCLIB**, chosen by the recording's length.
2. **"Exactly the recording" is a rule, not a hope.** LRCLIB holds ten to
   twenty timed versions of a well-known song; the one taken is synced, its
   artist and title match, and its length is within ±2 s of the recording's
   (the decoder's length where known) — the nearest first, the album's
   versions only when the album has a synced one (a festival take ran the
   album version's length to the second). Without one, the nearest synced
   version is shown **marked**, with how far it is, and the user shifts it by
   ±0.5 s; the shift is kept per track. Without any, unsynced words.
3. **When LRCLIB is asked is the user's rule:** a local file's and a server's
   track only while a lyrics view is on screen — opening it is the asking; a
   plugin's track the moment it starts, since it plays from the internet
   anyway. The file's and the server's own lyrics are always read. "Not
   asked", "none" and "failed" are three answers (K9); LRCLIB busy is said,
   after two retries.
4. **The cache** is `<data>/lyrics/`: LRCLIB's answers (a version of the
   length for good, anything less for 30 days) and the user's shifts.
5. **`headshell lyrics "<query>" [--all] | --file <audio> [--print]
   [--shift MS]`**, with `--json` and a snapshot. The desktop mirrors it as
   `lyrics`; `lyrics_now`, `lyrics_view` and `lyrics_shift` are the playing
   track's side, and the tick says when its answer changed.
6. **The interface:** *up next | lyrics* tabs beside the record, and a
   button that grows the lyrics to the whole sheet (`L`, `Esc`). The line
   being sung is estimated every frame from the anchor (`lyrics.js`, bound to
   the core's `line_at` by `fixtures/lyrics/line_at_cases.json`); following it
   is one interruptible spring on the list's transform, and the user's own
   scroll takes over at once.
7. **Plugins, api 4:** the manifest must say `"lyrics": true|false`; a plugin
   with `true` exports `lyrics(id)` and returns LRC text, parsed by the core's
   own parser. The catalog's `ytmusic` 0.6.0 gives YouTube Music's own timed
   lyrics; `soundcloud` 0.4.0 says it has none.

**Not done:** word-by-word (karaoke) timing — LRCLIB's word timing comes as
YAML, a new parser; seeking to a line with a click — done in §3.25 (issue
#1); translations; an in-app way to ask LRCLIB again for a track whose
answer is kept (the CLI's cache lives 30 days).

**Testing:** the LRC and `SYLT` readers; the version choice on real LRCLIB
data (the festival trap fails when the album guard is taken out); LRCLIB's
client on canned answers (the search, the exact query, a busy server); the
chain's weighing and its network rule; the cache; the worker; the providers
on a fake server; the echo plugin's lyrics through the engine; the fixtures'
`.lrc`, LRC tag, `USLT` and `SYLT`; `headshell lyrics --json`; the parity set
on both sides; every page script parsed. Live: LRCLIB, and the catalog
plugin's lyrics (they skip without a network, D-043).

**Verification:** the §3.9 container with the host's WebKitGTK and a
PulseAudio null sink: a made-up song with a `.lrc` followed line by line, words
without timing, a track with none, a real recording LRCLIB has at its length,
and one it has only seconds off — marked, shifted, kept; the whole-sheet view,
`L`, `Esc` and a hand scroll.

### 3.13 Context menus — DONE (D-079)

**The problem:** a right click opened the engine's own menu — "Back",
"Reload" (which rebuilt the interface and lost the open section, the search
and the scroll), "Open Image in New Window" on a cover, and in debug builds
"Inspect Element". None of it was the app's, and none of it was about the
thing under the pointer.

**Done:**

1. **The engine's menu never opens**, in any build. In its place every element
   offers what can be done with it: a track (play it, find its artist, copy
   it, resolve its identity), the player (the transport, shuffle, repeat, the
   lyrics), a sidebar row (the section's own work without opening it), a
   plugin's card (its own buttons, from one list), a notice, the lyrics, a
   year, a text box (cut, copy, paste, select all). An empty spot opens
   nothing.
2. **Nothing new underneath (K1, D-033):** every row calls a function the
   interface already had; no IPC command and no CLI subcommand was added.
   What each element offers is written where it is drawn (`h(…, { menu })`).
3. **It behaves as the platform's menus do** (`ui/menu.js`, with
   /apple-design): it grows out of the pointer and stays inside the window;
   press, move and let go picks a row in one gesture, a still release never
   picks; a press outside only closes it; the Menu key and Shift+F10 open the
   focused element's menu, and every key is the menu's while it is open; a
   row that cannot be undone asks first.
4. **Paste reads the clipboard:** WebKitGTK refused it, so the main window is
   built in `setup` with Tauri's clipboard access (`create: false` in the
   config) — no new crate.

**Not done:** queue editing — "play next", "add to the queue", "remove from
the queue" — needs the core (the queue has no insert or remove, and the
read-ahead would follow them): the user's choice, its own step. An album in
its order: the core keeps no track numbers. ("Add to the queue" landed in
§3.14.)

**Testing:** `tests/menu_js.rs` — the placement (no opening point puts a menu
outside the window; from the keyboard it does not cover its element), the
release rule, the type-ahead, the separators; `ui_contract.rs` reads
`menu.js` too.

**Verification:** the §3.9 container at 1180×780 and 900×600: every kind of
menu opened, picked from with the pointer, by press–drag–release and from the
keyboard; the clipboard read back with `xclip`; a click outside swallowed; a
notice held while its menu was open.

### 3.14 Play from a chosen provider, suggestions, a song's menu — DONE (D-080)

**The problem:** only local files are in the catalog; servers and plugins
were asked only when it found nothing, so with local music there was no way to
play from YouTube Music. The play box offered nothing while typed in, and a
song could not be added to the queue.

**Done:**

1. **Play from a chosen provider** (`provider use`, the providers section's
   "play from" card): it is searched first — its scanned catalog, or its own
   search — and the others only when it finds nothing, their results named.
   The choice is kept in `preferences.json`; one that cannot be kept is said.
2. **Suggestions while typing** (`suggest`): the tracks whose title begins
   with the text, the artists, the other matches; the chosen provider's, or
   every other one's under its name. The catalog at once, a server or a
   plugin after a pause — asked off the thread that plays.
3. **Every song's menu:** play now, add to queue, find the artist / album,
   copy, resolve — and "soon" placeholders: play next, add to a playlist…,
   start a radio, like. "Add to queue" is `Player::enqueue`.
4. **The CLI:** `headshell suggest`, `provider use`; the TUI's search line
   (`/`, Enter plays, Tab adds).

**Not done:** play next (the gapless handover must be taken back first);
playlists, radios, likes.

**Covers in the suggestions** (an addendum): the cover worker takes what is on
screen beside the queue, and asks only where each track lives — never the
third parties for a list that changes at every pause.

**Testing:** the ranking; the order and the fallback with fake providers;
`enqueue`; the TUI's keys and rows; `suggest` and `provider use` through the
CLI (a snapshot).

**Verification:** the §3.9 container with the echo plugin: suggestions from
the catalog and from the plugin as a fallback, picked with the pointer and
the keyboard; adding to an empty and a full queue; choosing the plugin to
play from.

### 3.15 Settings: the desktop's online choice — DONE (D-082)

**The problem:** the desktop asked MusicBrainz and the Cover Art Archive only
when it was started with `HEADSHELL_ONLINE=1` (§3.10). Started from its icon it
never did, and nothing on screen said how to change that but a variable.

**Done:**

1. **A choice kept in `preferences.json`** (`lookup`), offline until chosen.
   `HEADSHELL_ONLINE` decides one launch over it; who decided is always said.
2. **`headshell online [on|off]`** shows and writes it. It is the desktop's: the
   CLI goes online only with `--online`, run by run (the user's choice).
3. **A settings section**, the system group's third row (`Ctrl`+`0`): what
   online asks, a switch, and a note on the variable's hold, an online import's
   cost and the lyrics' and plugins' own rules. The switch takes effect at once
   — the cover worker starts over in the new mode — and a choice saved from the
   CLI while the app runs takes effect when the settings are read.

**Testing:** who decides, as a table; the choice beside `play_from`; the CLI's
four states and a CLI run staying offline with the desktop online (a
snapshot); `ui_contract.rs`.

**Verification:** the §3.9 container: off → on with a real album's cover
arriving without a restart, off again, the CLI's change picked up while the app
ran, a launch held by the variable, the row's menu, the pressed switch, 900×600.

### 3.16 A simpler interface; the debug mode — DONE (D-083)

**The problem:** the interface showed the developer's details to everyone: a
notice's `STEP:` code first, raw JSON, provider and plugin ids, capability
flags, `api` versions, hashes, zero counts, decision numbers in a note, and a
whole diagnostics section.

**Done:**

1. **Three layers:** what the user acts on in front; what explains it — K9's
   summaries among them — one level deeper for everyone; the developer's
   details only in the debug mode. Nothing was removed: the debug mode shows
   what the interface showed before.
2. **The debug mode:** a switch at the bottom of settings, kept in `ui.json`
   beside the theme; `DEBUG=1` (exactly `1`) holds a launch on. It also raises
   the log to `debug`.
3. **Notices** name their stage in plain words (all 25 of the core's), the code
   and the chain under "details". Partial-success counts show plainly and only
   when not zero; the import report leads with what changed in the library.
4. **The source label** only for tracks not from the local files, by the
   provider's name.

**Testing:** the shared `ui.json`, the variable's values, every stage's words
(`ui_contract.rs`).

**Verification:** the §3.9 container, every section before and after, the
mode on, off and held by the variable, 900×600.

### 3.17 The interface speaks to the listener — DONE (D-084)

**The problem:** after §3.16 the words still explained how the app works — the
engine, the catalog's repository, the data directory, the CLI — and named
things as the code does: providers, plugins, consent, secrets, scan.

**Done:** every section says in a warm sentence what it is for; how it works
is a paragraph for the debug mode. Everyday names (sources, services, keys,
browse, allow, revoke, check for new music, songs, plays). Notices say what to
do; the core's cause is in their details. The ids, the theme contract and the
CLI are unchanged.

**Verification:** the §3.9 container, every section, three layout faults fixed.

### 3.18 Music folders chosen in the app — DONE (D-085)

**The problem:** the local files came only from `HEADSHELL_MUSIC_DIRS` or the
system's music folder; the interface could only point at a variable.

**Done:** the core keeps the folders (`preferences.json`), the variable still
decides a run that sets it; `headshell music-dirs [add|remove]`; sources →
music folders with *add a folder…* and a remove button per folder, a change
checking for new music at once.

**Testing / verification:** the order and the changes in the core, the CLI end
to end through a scan, the container — a folder added through the dialog and
taken off.

### 3.19 Performance and network debts — TODO (measured 2026-10-02)

A test round on `cbbe2b3` (release build; CLI, TUI under tmux, the desktop in
the Xvfb container; a fake Subsonic server, a CONNECT proxy that slows, stalls
and cuts real services, hand-made bad plugins). Nothing here is fixed yet; each
item is a measured debt, its likely cause and a direction, not a decision. The
raw notes are outside the repository (`/srv/rust/hs-perf/RESULTS.md` on the
machine they were taken on).

**Freezes the user feels — first:**

1. **One core thread holds every desktop command.** A play search against an
   unreachable server (an away-from-home NAS: 30 s timeout) held a stats view
   that takes ~3 s for **32.5 s**; pause and next wait in the same line. The
   busy notice shows, the waiting view says nothing. Long or network-bound
   work (play search, provider test, import, stats) needs to leave
   `core_thread` or be cancellable (`state.rs`, `commands.rs`).
2. **Slow suggestions starve every IPC command.** `SuggestPlan::finish` runs
   off the core thread, but a server's or plugin's search blocks a Tauri
   worker synchronously (ureq; a plugin call even runs when its future is
   created). With the play-from source hanging, six typing pauses left a stats
   view at **28.8 s** (one pause: 2.2 s) — 4 cores, 4 workers. Run them on a
   blocking pool, cancel the stale ones, cap how many are in flight.
3. **A lost audio device hangs playback.** With the sound server killed
   mid-track, `play` never returned, the TUI showed `▶ (playing) 0:02`
   forever, no error, no recovery when the server came back (Bluetooth or USB
   output going away, PipeWire restarting). The cpal error callback only
   records an error; nothing reads it as "the output is gone".
4. **A stalled stream blocks next and quit.** `HttpMediaSource::read` waits
   without looking at a stop flag and `AudioEngine::drop` joins the decoder:
   next 5 s into a stall waited **17.6 s**, quit 9.2 s (worst case the 30 s
   idle timeout). After the timeout the rest of the track is skipped silently;
   the decode error only reaches `tracing::warn!`, not the screen or `diag`.
5. **The TUI freezes during a slow suggestion.** `tui.rs` awaits `suggest`
   inside its draw loop: ~21 s without redraw or keys against a plugin that
   times out; ticks (track hand-over, listens) wait too.

**Data and time:**

6. **`import --online` is all or nothing.** `Resolver::resolve_all` is
   sequential and its first error aborts the import before anything is
   written: a MusicBrainz stall at track 19 of 40 saved **0** listens (offline
   all 80 would have been). ~1.24 s per distinct track → ~7 h for 20,000.
   Write what is resolved, fall back to the local key on failure, resume.
7. **The offline import waits on the disk.** 400k listens: 211 s wall, 41 s
   CPU, 387 MB; importing the same file again (all duplicates) 202 s. 50k:
   26.7 s on ext4, 4.3 s on tmpfs. `set_resolution` runs one autocommit
   `UPDATE` per distinct track (`library/mod.rs`) — one transaction, and skip
   rows that did not change.
8. **A zip bomb takes the memory.** A 2 MB zip holding 2 GB of JSON peaked at
   2,059 MB RSS (`ZipArchive::read_entry` reads an entry whole, no cap). A
   per-entry limit well above a real export file (~15 MB) is enough.
9. **A second writer fails at once.** While an import holds the write lock
   (~4 min for 400k) every other writer gets `database is locked` in 0.0 s —
   no busy timeout; the CLI importing while the desktop records listens is
   the everyday case.
10. **Stats load the whole history.** `all_listens` into memory for every
    view, a year filter too: 1.1–1.7 s and ~190 MB for 400k (the desktop's
    view: ~3 s, peak 368 MB) with no loading sign on the page.

**Plugins:**

11. **Three timeouts kill a plugin for the session.** `MAX_STARTS` counts the
    whole session and never resets: after three timeouts a query the plugin
    answers at once fails with `PLUGIN_START` until restart, and "fell over
    every time" is not true. Count consecutive failures, or within a window.
12. **`host.http` ignores the call's budget.** A fixed 15 s timeout per
    request: two 12 s requests overran the 20 s call, the caller gave up at
    22 s and the engine was dropped. Bound each request by the time left.
13. **A provider's answer is not clamped to `limit`.** A plugin returning
    200,000 rows made a 200,000-track queue (`hits_from`); suggest took 2.5 s
    and 346 MB.
14. **QuickJS running out of memory reads as `threw a value that is not an
    error object: Null"null"`** — it should say "out of memory (128 MB)".
15. When the chosen source fails, suggest asks every other provider — a
    misbehaving plugin among them runs on every failed keystroke.

**Smaller:**

16. The TUI exits when the queue ends even while the search line is open.
17. Idle desktop with nothing played: 7.6 % CPU in `headshell-desktop`, 15.8 %
    in the web process (Xvfb, software painting — re-measure on a real
    session); the decode thread polls every 5 ms while idle.
18. Covers online: ~6 s and ~420 KB of MusicBrainz JSON per album; a slow
    link (30 KB/s + 300 ms) took 21 s for one cover. Since D-094 the answers
    are asked for gzip — re-measure.
19. A read-only data directory refuses even `stats`.
20. A WAV starts ~1–1.5 s after play (FLAC: 0.05 s) — the codec's packets,
    not the network.

**Measured and fine** (to keep that way): corrupt `library.db`,
`preferences.json`, `servers.json`, truncated and noisy audio, a server's
500/401/HTML/oversized/reset/refused/DNS answers — all fast, staged errors;
search 10–40 ms, local suggestions 20–30 ms, a 20,000-file scan 7 s (0.7 s
unchanged), next/previous with the read-ahead 10–30 ms, the window in 0.5 s.

**Measured since:** YouTube Music and YouTube under a bad network (2026-10-15,
issue #1): searches ran past the plugin budget on a lossy link, uncompressed,
and googlevideo cut downloads halfway — both fixed in D-094.

**Not measured yet:** the catalog install under a bad network, a session left
open for hours (leaks), Windows/macOS, the desktop redrawing a very long queue.

### 3.20 Music videos — DONE (D-086)

**The problem:** a song's video was out of scope (§1.4), and the only way to
play YouTube was yt-dlp — the gray route.

**Done:** a *music · video* switch on the turntable: the suitcase lid swings
down and the video plays on its inside from the second the music was at, and
back. Videos come from the track itself (a local video file, a plugin item)
or from the provider chosen for videos — searched for the song. Roles: main,
music, video (`provider use --for`, `provider follow`). Plugin api 5:
`video` required, `resolve_video(id)`. The `youtube` plugin plays through
YouTube's official player; the page is served from `http://localhost:<port>/`
for it, local video files from a shell server. `headshell video`. Settings →
*music videos* turns all of it off: the window is the record player it was.
The settings are grouped — listening, internet, advanced.

**Testing / verification:** the core's clock, listens, roles, matching and
contract; the CLI end to end; the shell's server and ACL list; the plugins
live against a mirror; the container — the lid both ways, YouTube and a local
file, the plinth's corner, the window's corner.

### 3.21 For you: recommendations — DONE (D-087)

**The problem:** the history said what the listener loves, and nothing used it to say what to play next.
The ask was a page of songs per source, worked out by an algorithm of our own from genres and nearby songs —
reachable by nobody but the user, and resettable from settings.

**Done:** *for you* between *now playing* and *library* (Ctrl+2; settings moved to Ctrl+,). The taste is
read from the history each visit — plays fading over 90 days, skips taking back half — never stored; songs
played in the same sittings as the loved ones, the genres played most, and the favourite artists' unheard
songs — the best of them on a shelf of square tiles at the top, scrolled sideways. A scanned source is read,
not asked; a server is asked for similar songs (`getSimilarSongs`, Jellyfin's
instant mix) and a genre's songs, a plugin for `related(id)` (api 6) — only by its own track's id. Genres from
the tags, the servers, and MusicBrainz when online (schema v3, `track_genres`). "Not for me" in a song's
menu; settings → *recommendations* → *start over* forgets what was learned and reads the taste from now on,
deleting no listen. `headshell recommend [--from] | dismiss | reset`.

**Testing / verification:** the pure taste, sittings and scoring; visits over fake sources (catalog, server,
plugin, a failing one); the session's dismiss and reset; the library's migration; Subsonic and Jellyfin fakes;
the echo plugin through QuickJS; the CLI end to end; the container — the page per source, covers, "not for me",
the reset in settings.

### 3.22 YouTube Music's fast path — DONE (D-088)

**The problem:** a track took 3.4–4.1 s to start inside the engine (4.5–5.5 s from the shell), nearly all
of it yt-dlp's process around a single InnerTube call; the read-ahead (§3.11) hid it for "next", not for the
first song or a search the user plays.

**Done:** `ytmusic` 0.9.0 asks InnerTube's `player` endpoint itself, as the visionOS client yt-dlp uses
without a JS runtime — 0.2–0.3 s, the same address, no new permission. The visitor id the bot wall hands
back is kept in the plugin's storage. yt-dlp stays as the fallback with the last word; an error carries both
reasons; `provider test` says whether the fast path works.

**Testing / verification:** a live test with yt-dlp left out; the missing-track test asks for both voices;
9/9 against a local catalog mirror; an in-engine probe, 0.8.0 against 0.9.0, and a broken client falling
back.

### 3.23 Better sources: Premium, Qobuz, Spotify — TODO (the user's choice, 2026-10-09)

The research and the reasons are in D-088. **Waits for §3.24** (the user's
decision, 2026-10-12): a source's quality options become a required part of
the plugin contract — how many, their names, what each does, and their words
in which languages — so the language engine comes first. In order:

1. **YouTube Music Premium** — 141 (AAC 256) through yt-dlp's `web_music` client and the user's cookies;
   that client needs a JS runtime, so the engine installs one (`qjs`) through `requires`. Verified with the
   user's Premium account. Opus (251/774) waits for a decoder — a new dependency, to be asked.
2. **A Qobuz plugin** — the subscriber's unencrypted FLAC; signed in with a token (the password login sits
   behind a captcha). The app id is read from the web player, and the user applies for an official one at
   api@qobuz.com. No account to test with: its live tests skip, and are reported as skipped.
3. **Spotify through librespot** — the user's own Premium, playback only, nothing decrypted written to disk.
   It needs D-026 rewritten for Spotify first (PLAN §0.1), a helper program outside the core (K4), a way for
   its audio to reach the core, and the catalog entry the user chose (`headshell/plugins`). New accounts may
   hit librespot's open "audio key" error.

### 3.24 The language engine — IN PROGRESS (D-089)

**The problem:** every word was written into the surfaces in English; the
user asked for a language engine — every word from a local folder, the
language list being what that folder holds.

**In steps, each one whole:**

- **A. The engine — DONE.** `i18n/` in the core: the built-in `en.json`, the
  data directory's `locales/` scanned (a file that can't be used listed with
  its reason), the system's language, `{placeholders}`, CLDR plurals, a
  missing key in English, two-voice diagnostics. `preferences.json`'s
  `language`; `headshell language [use <code> | system]`; the window's
  `languages`, `language_set`, `locale_bundle`, `locales_folder_open`; the
  settings' language group, first on the page.
- **B. The window — TODO.** Every word of `index.html` and the scripts to
  keys.
- **C. The CLI and the TUI — TODO.** Their output and `--help`.
- **D. Diagnostics — TODO.** The error chain, `headshell diag` and warnings
  in both voices.
- **E. Plugins — TODO.** A plugin's `locales/` and the required quality
  declaration (§3.23).

### 3.25 Moving in the track — DONE (D-090)

**The problem:** GitHub issue #1 (a listener's list of suggestions), points 3
and 2: clicking the progress line opened or closed now playing instead of
moving in the song, and the arrow keys should move 5 seconds, with Ctrl
changing the song. The line had only ever been a picture — the bar is the
sheet's handle (D-075) — and the core had no seek the user could reach
(§3.8, §3.9).

**Done:**

1. **The core:** `Player::seek` and `seek_by` (`SEEK_STEP_MS`, 5 s). A
   position past the track's end ends it and the queue moves on as after an
   ending; while a video is shown its clock moves and the shell moves its
   player. `TickReport` says `moved`, and `track_changed` now counts from the
   last report — a command between two rounds is reported by the next one.
2. **The TUI:** ←/→ move 5 s, Ctrl+←/→ (and `n`/`b`) change the song.
3. **The window:** `seek`, `seek_by`. The line is a slider with its own strip,
   cut out of the bar's handle: a press puts it under the pointer, a drag
   carries it, the sound jumps on release, Esc puts it back. Its knob is always
   shown while a track can be moved in. ←/→ and Ctrl+←/→ as in the TUI; a held
   key's steps add up while a jump is on its way.
4. **A lyric line plays from where it starts** (issue #1, point 1): a click on
   a synced line moves there and following picks up from it; the line under
   the pointer comes up a little. A coarse jump lands just before the line, so
   the clicked line stays lit until the track reaches it.

**Testing:** the containers' end (FLAC, Ogg, MP3) without a device; a real
file moved, stepped, paused, past its end, and a video's clock moved
(`playback_local.rs`); the jump reaching the window (`core_thread.rs`); the
TUI's keys; the window's step against the core's (`ui_contract.rs`).

**Verification:** the §3.9 container: a click at three quarters, a drag
(the line and the time follow, the sound waits), a drag upwards that doesn't
pull the sheet, the arrow keys, Ctrl+→, a paused track that stays paused, the
end of the last track and of one in the middle of the queue; a click on a
lyric line (0:37 lit, the line under the pointer brighter). A video's seek
was not seen (no network or plugin in the container).

### 3.26 SoundCloud's HLS streams — DONE (D-091)

**The problem:** SoundCloud stopped giving plain streams: on 2026-10-15, 20 of
20 searched tracks offered only HLS (AAC in MP4 fragments; some only
encrypted). The plugin refused HLS, so nothing from SoundCloud played — and
the live play test hid it as a skip.

**Done:** the core plays an unencrypted HLS media playlist (`playback/hls.rs`):
the init file and the fragments as one stream, its length from the playlist,
a seek opening the fragment that holds the second and dropping the packets
before it. Encrypted, master and byte-range playlists are refused with their
reason. `soundcloud` 0.7.0 picks the best unencrypted AAC variant and declares
the streams' host; an encrypted-only track says so. The live play test skips
only for a missing device or network, and now seeks too.

**Testing:** the playlist parser (SoundCloud's shape, relative addresses, the
refusals); a generated HLS fixture from a local server — whole, opened at 3 s
without the first fragment fetched, past the end; live: 7/7 against a local
catalog mirror, a SoundCloud track played and moved to 30 s.

### 3.27 Links — songs DONE (D-095), playlists TODO

**The problem:** GitHub issue #1, point 5 (and issue #2): paste a YouTube link
— something not on YouTube Music too — and play it; ideally any link.

**Done (songs):** plugins name the links they take as templates with `{id}`
(plugin api 7, `links` required); the core reads a pasted link against them
(`link.rs`), the plugin music is played from first, and asks that plugin's
`link(id, template)` for the song. `play`, `suggest`, the TUI and the play
box take a link; a link no source takes is a `LINK_OPEN` error naming who
takes links. `ytmusic` 0.10.0 (YouTube Music, YouTube, youtu.be) and
`soundcloud` 0.8.0 take links; `youtube` 0.3.0 takes none.

**Later (the user's call, 2026-10-15):**

- **A playlist's link** — a YouTube Music/YouTube playlist, an album, a
  SoundCloud set: `link` already returns an array; the core refuses more than
  one track today ("only a song's link"). Open: queue them all or show them as
  a list to pick from, a cap on a long playlist, and the plugins' templates
  (`playlist?list={id}`, `sets/`).
- **A video's link** through the `youtube` plugin (the video player rather than
  the sound alone) — today a YouTube link plays its sound through `ytmusic`.
- **SoundCloud's short links** (`on.soundcloud.com/…`, what its app shares):
  they redirect, and following them needs a new host in the plugin's
  permissions — a new approval for every user.
- **Links from outside the app** (a `headshell://` scheme, opening a link
  with headshell) — not asked yet.

### 3.28 The video button — DONE (D-096)

**The problem:** GitHub issue #2: the video button played nothing for songs
that have a video. Measured: with YouTube Music as the main provider the
video role fell to the local files; with YouTube chosen, the artist's audio
upload (a still picture) won over the video on its length, a mislabelled
video was refused, and a long title searched badly.

**Done:** the video role falls to the first video plugin; ties keep the
provider's order; the search leaves the release labels off; `youtube` 0.3.0
reads the uploads' own titles (YouTube's search, same address), leaves the
audio uploads out and puts the official video first. A song without an
official video says so.

---

# PHASE 4 — Rooms (the backend starts here)

**Goal:** Listening together in sync. The gap Discord music bots left behind.

**Precondition:** It is in scope because of D-002. But running a server creates
a cost that requires no revenue — before this phase starts, it must be proven
that a user base exists. If Phase 0.5 and 3 didn't bring users, don't enter
Phase 4.

### 4.1 The anchor protocol
A single primitive:
```
{ track: CanonicalId, anchor_wall_time: T, anchor_position: P, rate: f64, state: Playing|Paused }
```
The client computes its own position: `pos = P + (now - T) * rate`.
An anchor broadcast, not an event broadcast — a late joiner settles into place
with a single message, and packet loss corrects itself.

### 4.2 Clock sync
An NTP-style offset: `offset = ((t2-t1) + (t3-t4)) / 2`. The median of several
measurements, repeated periodically.

### 4.3 Scheduled start
Providers open with different latencies (local ~20ms, remote ~800ms). Not
"start now" but "start at T+2s" — everyone buffers and seeks beforehand. On
track transitions the next track is announced in advance.

### 4.4 Drift correction
The tolerance is wide: a 50-100ms difference between different homes isn't
noticed. Soft correction (a 0.1% change in rate, or a micro-seek). Snapcast's
~1ms target isn't needed.

### 4.5 v1: a single provider, v2: mixed
In v1, if everyone is on the same provider, canonical resolution isn't needed —
`provider_track_id` is enough. Mixed providers are left to v2. This lets rooms
ship before the identity layer matures.

### 4.6 The relay server
It carries only signals (K3). One websocket per participant, a few bytes per
second. P2P is **unnecessary** for signalling — a single source of truth brings
simplicity. P2P is discussed only if voice chat is added later.

> DECISION POINT: Is voice chat in scope? The advice: **no**, users already
> talk on Discord. If it is to be added, the cost of WebRTC + TURN must be
> calculated. **Ask.**

---

# PHASE 5 — The Social Graph

Friendships are **not declared, they are derived**: "the people you often
listen with" accumulate from rooms. Don't open with an empty "add a friend"
screen — the cold start problem.

---

# PHASE 6 — Mobile

**An approved goal (D-001).** Kotlin/Swift bindings with `uniffi`.

The implementation is in this phase, but **the constraint holds from today**:
it must come without changing the core. If it doesn't, the core was designed
wrong (K7).

**An early-warning mechanism:** from Phase 1 on, try generating the `uniffi`
scaffolding in CI — it doesn't even need to compile; it only asks the question
"can this API be expressed?" on every commit. Otherwise violations pile up for
months and come out all at once.

> **DECISION POINT CLOSED — D-052 / D-053.** The warning proved right:
> `PlayOptions<'a>` piled up exactly like this, carrying a lifetime on the
> public surface for months.
>
> What was put in place today is **not real scaffolding generation** but the
> cheap filter that comes before it: `crates/headshell-core/tests/k7_surface.rs`
> looks for lifetimes in public types and closures in public signatures, and it
> runs in CI's third gate (D-053).
>
> **D-069 note:** the plugin engine (`rquickjs-sys`) comes with ready bindings
> for the desktop targets but not for Android/iOS; in those builds the
> `bindgen` feature (libclang at build time) will be turned on. The engine
> itself suits mobile — api 1's subprocesses could never have worked on iOS.
>
> The real `uniffi` check is still this phase's job, and it needs two things:
> marking ~60-80 types in the core with `#[derive(uniffi::Record)]`, and
> rewriting the four traits below — `uniffi` can't express
> `Pin<Box<dyn Future + Send + 'a>>` in the return of a trait method:
> `Provider`, `HttpClient`, `MetadataLookup`, `FingerprintLookup` (the D-052
> debt).

---

# APPENDIX — Streaming platforms

The answer to the question "which platforms can we play from" lives here.

> **The API information here was not verified.** Platforms change their
> interfaces without warning. Before a plugin is written, the relevant row is
> tested again — the NEVER DO item "don't assume an API shape you don't know"
> applies here too.

## The dividing line is legal, not technical

The question isn't "is there an official API". The question is: **is the
audio protected with DRM?**

- **No DRM** → a plugin can resolve the stream directly. Using an undocumented
  API may break the terms of service; that is a risk, but not a separate
  offence.
- **DRM** (Widevine, FairPlay, Deezer's Blowfish) → opening the stream is
  **circumventing a technological protection measure.** In most countries that
  is a separate article of law (DMCA §1201, EU 2001/29 art. 6), forbidden
  independently of copyright infringement. **It is not written in this
  project.** Because of D-002 this is a product to be published; there is no
  personal-use exemption — the same thing K4 says about Spotify.

For platforms with DRM two ways remain: driving the platform's **own** player
(`CONTROL`), or taking metadata only.

## The table

| Platform | Capability | Reasoning |
|---|---|---|
| **SoundCloud** | `SEARCH BROWSE STREAM` | No DRM. There is an official API, but key applications have been closed/intermittent for years; the practical way is yt-dlp. §2 K5's reference plugin is this one anyway. Some tracks (Go+, private) can't be accessed — the plugin **counts and reports** them, it doesn't silently skip them (K9). |
| **Qobuz** | `SEARCH BROWSE STREAM` | No DRM; FLAC downloads to the subscriber unencrypted. No official public API; it is known through reverse engineering. A subscription is required. The cleanest source for a hi-res catalog. |
| **YouTube Music** | `SEARCH BROWSE STREAM` | No DRM; the address comes from InnerTube's `player` endpoint, yt-dlp as the fallback (D-088). **The highest maintenance cost:** YouTube actively makes it harder (nsig, PO tokens). That is exactly why it is a plugin — when it breaks, the core stays up. |
| **YouTube** (videos) | `SEARCH VIDEO` | **The official route** (D-086): YouTube's own player, the IFrame embed — nothing downloaded, no stream resolved, YouTube's ads and counting kept. Its rules are the window's to keep: visible while it plays, nothing over it, never under 200 × 200. Search through InnerTube (the one gray part, the user's choice). Needs the page served over `http(s)` — a custom scheme sends no `Referer` ("Error 153"). |
| **Tidal** | `SEARCH BROWSE` | The official developer API gives catalog/search. Full streaming depends on the partner program; the high-quality tiers have DRM. Tidal Connect is a certified-device programme, not open to everyone. |
| **Apple Music** | `SEARCH BROWSE` | MusicKit is **official and legitimate** — with a developer token + a user token the catalog and the user's library are fetched. But playing needs the MusicKit runtime: it isn't on Linux, and WebKitGTK has no FairPlay. `STREAM` could be opened in Phase 6's iOS/macOS versions. |
| **Deezer** | `SEARCH BROWSE` | The public API is official and good for metadata. The stream is encrypted with Blowfish → a protection measure → the other side of the line. |
| **Spotify** | `CONTROL` | K4. A separate repository, a separate package; invisible in the core's dependency tree. The Web API's player endpoints need Premium. librespot breaks the ToS. Metadata isn't pulled or written to the database — history comes through the export (K2). |
| **Amazon Music** | — | No public API, Widevine. Not even metadata can be fetched. |
| **Pandora** | — | No public API since 2011, locked to the US. Also **the model doesn't fit:** we address tracks, Pandora addresses stations. |
| **Idagio** | — | No public API, DRM. Also a different data model (work / movement / performance), not tracks. Classical music identity is a job in itself — not a provider job but an `identity/` job. |
| **Tencent (QQ/Kugou/Kuwo)** | — | Locked to mainland China; a Chinese phone number and payment method are required, and the streams are encrypted. **But this is exactly K5's reason for existing:** someone in that region can write the plugin themselves. We provide the protocol, not the list. |

## The import side doesn't recognise this line

This is the whole point of K2: **an export is a legal right; terms of service
can't restrict it.** Even the table's "closed" rows give listening history.

Those that provide an export under the GDPR/CCPA: Spotify (done, §0.2), Apple,
Google/YouTube (Takeout), Deezer, Qobuz, Tidal, SoundCloud, Amazon. Pandora and
Tencent are weaker — on request, and their formats are undocumented.

So: **three platforms we can play from, ten platforms we can take the listening
identity from.** The product was the second — CLAUDE.md's first sentence.

---

# NEVER DO

- Don't put business logic in the CLI
- Don't pull history/a library from a provider API — use the export
- Don't stream audio from a server
- **Don't write code that decrypts a DRM-protected stream** (D-026) —
  Widevine, FairPlay, Deezer's Blowfish. Circumventing a protection measure is
  an article of law separate from copyright infringement; because of D-002
  there is no personal-use exemption. See APPENDIX — Streaming platforms
- Don't add a Spotify dependency to the core
- Don't cross a phase boundary (don't write code "because it'll be needed
  later")
- Don't leak a type `uniffi` can't express into the core API
- Don't delete or overwrite raw `listen` records
- Don't add a dependency without permission
- Don't silence a broken test
- Don't assume a format/API shape you don't know — ask or verify
- Don't write code, comments, messages or commits in any language but English
  — not even when the conversation is in Turkish (K11)

---

# GLOSSARY

| Term | Meaning |
|---|---|
| **canonical id** | A provider-independent track ID (preferably an MBID) |
| **provider** | An audio source: local, Subsonic, SoundCloud, torrent… |
| **plugin** | A provider written in JS, running in the QuickJS embedded in the core (api 2, D-069) |
| **host** | The single gate the engine gives a plugin: `http`, `secrets`, `storage`, `tools`, `log` |
| **artifact** | A tool the plugin declares, which the engine downloads per platform and verifies the hash of (yt-dlp) |
| **listen** | A single listening event: track + timestamp + duration + source |
| **anchor** | `{track, wall_time, position, rate, state}` — the single primitive of room sync |
| **resolve** | Matching a track to a canonical ID, and from there to a provider ID |
| **accuracy set** | `fixtures/identity/cases.json` — hand-labelled matching tests |
| **role** | The part a provider plays (D-086): **main** (the play box's first, what the others follow), **music**, **video** |
| **surface** | Who makes the playing track's sound (D-086): the audio engine, or the shell's video player |
