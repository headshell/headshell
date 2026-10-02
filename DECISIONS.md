# DECISIONS.md — The Decision Log

Every answered question is written here. The same question is not asked twice.
Read this file before deciding anything.

---

## D-001 — Is mobile a target?
**Date:** 2026-08-28
**Decision:** Yes. The implementation comes in later phases, but **the API design will be mobile-compatible from today.**
**Reasoning:** Adding it later would mean rewriting the core API from scratch.
**Consequence:** Invariant Rule K7 is binding. A public signature that can't be expressed with `uniffi` is not accepted.

---

## D-002 — A personal tool, or a product to be published?
**Date:** 2026-08-28
**Decision:** It will be published. A community around it is the aim.
**Consequence:**
- Phase 4 (rooms, a server) is in scope.
- A license decision is needed (see D-005, open).
- K4 is applied strictly on Spotify — there is no personal-use exemption.
- Public repo hygiene is needed: README, CONTRIBUTING, a code of conduct, release notes.

---

## D-003 — Is there a local music archive?
**Date:** 2026-08-28
**Decision:** No. Fixtures can be produced for testing.
**Consequence:**
- Phase 1 (playback) **can't be dogfooded** by the developer. This affects the phase order (see D-004).
- When Phase 1 starts, small royalty-free test fixtures will be produced first
  (Creative Commons / public domain recordings, short).

---

## D-004 — Sleeve and the community goal, the phase order
**Date:** 2026-08-28
**Decision:** Sleeve is an explicit goal. The December year-end card season (Spotify Wrapped®) sets the calendar.
**Consequence:**
- **A new Phase 0.5 was added:** generating a shareable Sleeve card.
- Reasoning: Phase 0's output is terminal text. Terminal text doesn't spread. The
  community goal needs a shareable visual artifact, and that wasn't in the plan.
- Phase 0.5 lives in the core (the GUI and mobile will use the same generator) and is driven from the CLI.

---

## D-005 — License
**Date:** 2026-08-29 · **Status:** CLOSED — **MIT OR Apache-2.0**
**Decision:** MIT OR Apache-2.0. The placeholder in Cargo.toml turned into a real decision.
**Reasoning:** The Rust ecosystem standard; it maximises adoption. Since plugins
speak through a subprocess + JSON-RPC (K5), the core's license barely binds
plugin authors legally — copyleft's real benefit doesn't materialise here.
**Consequence:**
- In Phase 0.5.4 the `LICENSE-MIT` and `LICENSE-APACHE` files go into the repo root.
- If Phase 4's server component becomes a separate crate/repo, AGPL is
  considered there **separately** — this decision covers only the core/CLI.

**Applied (2026-08-29).** Both license files are in the repo root; in MIT the
copyright holder is `enaimami`. `CONTRIBUTING.md` says a contribution agrees to
be published under the dual license. The `license` field in Cargo.toml was
already right; it is now a decision, not a placeholder.

---

## D-006 — The `MetadataLookup` generic (report Finding 3)
**Date:** 2026-08-28 · **Status:** APPLIED (2026-08-28)
**Decision:** Because of D-001 this **is a K7 violation today**, and it will be fixed.
**Current state:** `Session::import_archive<L: MetadataLookup>` and
`Session::resolve_track<L: MetadataLookup>` carry a generic parameter. `uniffi` can't express generics.
**Direction:** `Arc<dyn MetadataLookup>` instead of the generic. `uniffi` can model this as a
**callback interface** (`#[uniffi::export(with_foreign)]`), so it passes as a trait
implemented in a foreign language (Kotlin/Swift).
**Note:** K7's first wording said "no trait objects"; that was too strict and was corrected.
`Arc<dyn Trait>` is the way uniffi supports. What is forbidden is **generics** and **lifetimes**.

**Implementation:** To be `dyn`-compatible, `MetadataLookup` returns
`LookupFuture<'a, T> = Pin<Box<dyn Future<..> + Send + 'a>>` instead of
`-> impl Future` (the hand-written form of the `async-trait` macro — no new
dependency added). `Send + Sync` was added to the trait. `Resolver<L>` dropped
its generic too: `Resolver { lookup: Arc<dyn MetadataLookup> }`.
`session::default_lookup()` now returns `Arc<dyn MetadataLookup>` rather than a
concrete `OfflineLookup`, so the caller sees no signature change when the source
changes. No generic parameter is left on the core's public surface.

---

## D-007 — The repeated first line in the `diag` error chain (report Finding 1)
**Date:** 2026-08-28 · **Status:** APPLIED (2026-08-28)
**Decision:** It's a bug; it will be fixed.
**Cause:** `diag/mod.rs:233` starts the chain with `err.to_string()`. Since the
Display of `crate::Error` is `#[error("ADIM: {stage}")]` (`STEP:` today, D-073),
`error_chain[0]` becomes a copy of the header already printed from `failed_at`.
**Fix:** Let the chain start not from `err` itself but from `err.source()`.
`error_chain` should contain only the real causes.

**Implementation:** `diag::error_chain` starts with an empty `Vec` and walks from
`err.source()`. The test `error_chain_does_not_repeat_the_stage_header`
verifies both that the chain contains no line starting with the stage header
and that the header appears exactly once in the `render()` output.

---

## D-008 — The two meanings of the "plays" label (report Finding 2)
**Date:** 2026-08-28 · **Status:** APPLIED (2026-08-28)
**Decision:** Not the label but **the concept** will be fixed.
**Cause:** `library search` counts raw listens (no threshold), while `stats` counts
those above the `min_ms_played` (30 s) threshold. The same fixture shows "18 plays" and "9 plays".
**Fix:** A single shared concept is defined in the core:
- `play_count` → a "counted" play that passed the threshold (the scrobble convention: ≥30 s or half the track)
- `listen_events` → the raw event count, visible only in `diag` and debugging
Both surfaces call the same computation; the SQL isn't written separately in two
places. This keeps the same bug from being born again.

**Implementation:** The rule is defined in one place, in `model::PlayRule`;
`PlayRule::counts` applies the scrobble convention (≥ the threshold **or** half
the track — the half-track branch works only if the duration is known).
`StatsQuery::play_rule()` and `ListenStore::search(.., rule)` use the same
instance. The SQL side isn't written by hand: `library::play_predicate_sql(rule)`
turns the rule into an SQLite expression in a single place, and the test
`sql_play_rule_agrees_with_rust` locks, on a real SQLite, that Rust and SQL give
the same answer in 11 edge cases.

**Naming:** `SearchHit.plays` → `SearchHit.play_count`. The raw event count left
the user surface; it is written only to the diagnostic counters, as
`SearchOutcome.listen_events` (`search.play_count`, `search.listen_events`).
`headshell library search` takes `--min-ms` like `stats`, so that both surfaces
can be driven with the same threshold.

**Measured effect:** in the `spotify_extended_mini` fixture, the `Creep` row went
from `plays: 18` to `play_count: 9` — exactly what `stats` counts.

---

## D-009 — The identity accuracy set is insufficient
**Date:** 2026-08-28 · **Status:** APPLIED (2026-08-28)
**Decision:** 15/15 = 100% is not a valid signal. The set will be grown with hard cases.
**Reasoning:** CLAUDE.md defines this number as "the project's most important
metric". 100% on an easy set means nothing was measured.
**Case classes to add:**
- Remaster / Deluxe / Anniversary editions (must count as the same recording)
- Live recordings (must be **separate** from the studio version)
- Covers (must **not** match — negative cases)
- `feat.` / `ft.` / `with` variations
- Turkish characters and transliteration in artist names (Müslüm Gürses / Muslum Gurses)
- Classical music: the composer/performer distinction
- Different artists with the same name
- Long/short (radio edit) versions
**Target:** at least 60 cases, at least 15 of them negative (ones that must not match).

**Implementation:** The set was grown from 15 cases to **69 cases**; 21 are
negative (must not match). Every case carries a `class` label, and next to the
overall rate the test prints a **per-class** breakdown too — the overall rate can
hide a collapse in a single class. The test also protects the set itself: if it
drops below 60 cases or 15 negatives, it fails.

The schema grew by two fields: `class` and `expect_method`. The second was needed
to express cases like "an ISRC that isn't in the catalog is still an authority,
but it ties to the ISRC identity, not to an MBID".

**First measurement (the set grew, the algorithm old): 62/69 = 89.9%.** So D-009
was right — 15/15 = 100% meant nothing had been measured. The errors weren't
scattered; they gathered in two real flaws:

1. **The `live` class was 1/4.** `normalize::strip_edition_suffixes` treated
   "live" as a droppable version suffix; `Creep (Live at Glastonbury)` was tied
   to the studio recording **with 100% confidence**. That was exactly the
   behaviour D-009 explicitly forbids.
2. **`cover` 3/5, `remix` 1/2.** The title weight (0.6) could reach the
   threshold on its own: `The Rock Tribute Band - Karma Police` was tied to the
   Radiohead recording with 0.90 confidence.

**Fixes:**
- The tags were split in two. `REISSUE_MARKERS` (remaster, deluxe, mono, radio
  edit…) are re-releases *of the same recording*; they're dropped.
  `VARIANT_MARKERS` (live, remix, acoustic, karaoke, demo, unplugged, cover,
  instrumental, reprise) are *a different recording*; they aren't dropped. In a
  suffix containing both, like `(Live Version)`, the variant wins.
- `fuzzy::similarity` got two separate rules on top of text similarity:
  **a variant mismatch** (`live` on one side and not the other → ×0.5) and
  **an artist floor** (`ARTIST_MIN_SIMILARITY = 0.7`; below it → ×0.5).
  The penalty is for a mismatch, not for presence: two live recordings aren't
  penalised against each other.

**Last measurement: 68/69 = 98.6%.** Per class, only `radio_edit` was 2/3 — that
case wasn't a flaw but an unanswered question; it was decided with D-010, and the
set reached **69/69 = 100%**. The test threshold went 90% → 95% → **97%**.

> This 100% is not the 100% D-009 criticised: the set is 69 cases, 22 of them
> negative, and the `live` / `cover` / `classical` / `same_title` classes were
> the places where the algorithm really failed. Still, 100% on a single set
> means the set is exhausted: when the network and real MusicBrainz data arrive,
> new error classes must be added — the rule is the same: write the case first,
> see the test fail.

---

## D-010 — Should a radio edit count as the same recording as the original?
**Date:** 2026-08-28
**Decision:** **No — a separate recording (Option B).** The canonical ID is at the
recording level; MusicBrainz too treats a radio edit as a separate recording.
**Reasoning:** The duration penalty is the identity chain's strongest signal for
catching live recordings and covers; it's part of what took the `live` class from
1/4 to 4/4 in D-009. Loosening it for a user convenience we can't measure would
spend accuracy already earned. "Show different recordings of the same song as a
single row" is a separate layer (work / release-group grouping), and it's solved
in the right place when MusicBrainz data arrives — Phase 0's job is to build the
identity *correctly*.
**Consequence:** The code didn't change; the case
`Underworld - Born Slippy .NUXX - Radio Edit` was relabelled as
`expect_mbid: null`. **Accuracy 69/69 = 100%**, 22 negative cases. The test
threshold went 95% → **97%** (a single-case regression fails the test).

**A notable nuance:** The two other radio/extended edit cases in the set stayed
positive, because their durations are unknown. The labelling encodes the
question "what should the chain do with this evidence", not "what is this song":
if the duration difference proves an edit, the match is rejected; if there's no
evidence, the chain gives its best guess with fuzzy confidence. The distinction
is deliberate; it should be measured again when the network and real duration
data arrive.

**Context:** The one failing case in the D-009 set:
`Underworld - Born Slippy .NUXX - Radio Edit` (240 s) doesn't tie to the 570 s
recording in the catalog. The cause isn't a flaw but a conflict of rules: "radio
edit" counts as a re-release suffix and is dropped (the title matches exactly),
but the 330 s duration difference triggers the `DURATION_MISMATCH_MS` penalty and
pulls the score below 0.88. Radio edits with a small duration difference (e.g.
`Aerodynamic (Radio Edit)`, duration unknown) do match.

**Option A — let the radio edit merge with the original.** The user says "I
listened to the same song"; they want to see a single row in the statistics.
Implementation: the duration penalty is loosened if one of the titles carries a
length suffix.
*Pro:* it fits user intuition, and the track count on the Sleeve isn't split.
*Con:* the duration penalty is the identity chain's strongest signal for catching
live recordings/covers; loosening it puts at risk the `live` and `cover` classes
just won in D-009.

**Option B — let it count as a separate recording (today's behaviour).**
MusicBrainz too treats a radio edit as a separate *recording*; our canonical ID
is at the recording level. The case's label is corrected to `expect_mbid: null`,
and the rate becomes 69/69.
*Pro:* the identity chain stays consistent at the recording level; no signal is
loosened.
*Con:* the same song may appear as two rows in the statistics.

**Chosen: B.**

---

## D-011 — Rasterizing the Sleeve card
**Date:** 2026-08-29
**Question:** How will `headshell sleeve --out card.png` produce the PNG? (PLAN 0.5.3 decision point)
**Decision:** **resvg, behind an optional `render-png` feature.** The core always
produces SVG; the PNG conversion is compiled only when the feature is on. The CLI
turns the feature on; the mobile bindings don't.
**Reasoning:** The dependency tree must stay small (K7/mobile); the resvg tree
(tiny-skia, fontdb, rustybuzz) is large. With the feature off the tree doesn't
grow at all; with it on, the done criterion (`--out card.png`) is met. Since SVG
is always produced, the GUI/mobile can get the card without producing a PNG if
they don't want one.
**Consequence:**
- `headshell-core` → `resvg = { version = "0.48", optional = true }`,
  `[features] render-png = ["dep:resvg"]`.
- `headshell-cli` turns headshell-core on with `render-png`.
- The feature-gated code is in `sleeve/png.rs`; `render_svg` is unconditional.

**Applied (2026-08-29).** `sleeve::write_card` decides the format by the
extension: `.svg` always works, and `.png` with the feature off gives an
**explicit** error, "not in this build, use SVG" — it never silently writes the
wrong format. Rasterization goes through `usvg` + `tiny-skia`; since
`usvg::Options::default()` comes with an empty fontdb, the system fonts are
loaded by hand and sans-serif is tied to a concrete family (otherwise no text was
drawn at all).

**Measured effect:** the tree is **56 crates** with the feature off, **114** with
it on. The decision's reasoning was confirmed: the 58-crate difference stays
outside the mobile bindings.

## D-012 — The Sleeve card's design
**Date:** 2026-08-29
**Question:** Is the card a preview of the theme system (Phase 3), or a fixed design? (PLAN 0.5.3 decision point)
**Decision:** **A fixed design, with named internal constants.** The colours and
dimensions are gathered into named constants, but no versioned token contract is
opened to the outside.
**Reasoning:** Once a theme token set is published, a backward-compatibility debt
is born (PLAN 3.3 — the Spicetify lesson). Giving birth to a contract before the
token set is designed would be pulling Phase 3 forward without a plan. Gathering
the constants under names makes moving them to tokens later a small job.
**Consequence:** The dimensions are a parameter (`CardSize`), the
colours/spacings are named module-internal constants (`palette`, `metrics`). They
move into the token set when the theme API is designed in Phase 3.

**Applied (2026-08-29).** `palette` has five colours (ground, text, dim, accent,
bar track), `metrics` ten measures. Both are a `mod` inside `sleeve/svg.rs`, not
public — so nobody can depend on these names today, and moving them into the
token set in Phase 3 creates no backward-compatibility debt. That was the point
of the decision.

---

## D-013 — The version number and the copyright holder
**Date:** 2026-08-29
**Question:** What number will the first public release carry, and who holds the copyright in LICENSE-MIT?
**Decision:** The version is **`0.0.1-beta`**, the copyright holder **`enaimami`**
(a pseudonym). The repo was `git init`-ed, the first commit was made with the
user's name and email, and the `v0.0.1-beta` tag was put on the Phase 0.5
closing.
**Note:** The user said "0.0.1 Beta"; since Cargo's semver doesn't accept the
spaced form, `0.0.1-beta` was written — the same meaning, valid semver.
**Consequence:** The version field is in one place, in `workspace.package`; both
crates take it from there. Since the snapshot tests already treat
`headshell_version` as a variable and normalise it, a version bump doesn't break
the tests — and it won't in later bumps either.

---

## D-014 — The order after Phase 0.5: playback first
**Date:** 2026-08-29
**Question:** Will Phase 1 (playback) come first, or Phase 3 (GUI + theme)? (the PLAN's Phase 1 decision point)
**Decision:** **Phase 1 first — playback.** The user's reasoning: "let's sort out a
player first so we can build things on top of it."
**Reasoning:** Playback is a *foundation*; the GUI and the theme are built on
top of it. In the reverse order there would be no live state for the GUI to show
(the playing track, the queue, the position), and the theme system would be
decorating an empty space. Also, from Phase 1 on `headshell` starts producing the
scrobble — history no longer forms at any provider, which is the project's real
claim.
**Accepted risk:** Because of D-003 the developer has no local archive; this
phase **can't be dogfooded**. In return it's verified with royalty-free fixtures
and tests; the GUI may not make it to the December year-end card window
(Spotify Wrapped®).
**Consequence:** Phase 3 (GUI + theme) was left until after Phase 1. Phase 1's
first job is the §1.1 provider trait design — the signatures **are presented
before they're written** (K7: expensive to reverse).

---

## D-015 — The outer surface of playback state: an anchor + polling
**Date:** 2026-08-29
**Question:** How will the playback state be exposed to the GUI/mobile/TUI?
(PLAN §1.1; asked before writing the signatures, because of K7)
**Decision:** **An anchor + polling.** `Player::anchor()` returns a
`PlaybackAnchor`; the consumer computes the position itself:
`pos = position_ms + (now - wall_time) * rate`. No observer/callback, no
continuous position notifications.
**Reasoning:** Three things arrive at the same place. (1) PLAN 3.2 already asks
for this: "the playback position is predicted from the anchor in the webview,
not asked of the core continuously" — hundreds of IPC messages per second mean
stutter. (2) Phase 4's room sync primitive (`anchor`) is **exactly the same
thing**; the type written today is reused in rooms tomorrow, and two separate
state models aren't kept. (3) For `uniffi` a plain record is the safest way — a
callback interface is supported too, but used for the position it would create
bridge traffic on mobile.
**Consequence:** The core exposes the type
`PlaybackAnchor { track, wall_time, position_ms, rate, state }`. Discrete events
(a track ended → producing a `listen`) are handled inside the core; they don't
leak to the consumer as an event stream.
**Note:** If an "event missed" problem comes up in a later session (for example
if the GUI notices late that a track has ended), `drain_events()` **can be
added** — placed next to the anchor surface without breaking it. We're not
adding it today: K10, don't write it before it's needed.


**Applied (2026-08-29).** `PlaybackAnchor { track, wall_time, position_ms,
rate, state, duration_ms }` — a plain record for `uniffi`. The `position_at(now)`
formula stays in the core so the GUI/TUI/mobile don't write it three times. The
`rate` field is 1.0 or 0.0 today; when Phase 4's drift correction (PLAN 4.4)
pulls it to values like 1.001, the surface won't change.

**`Buffering`** was added to `PlayState` (it wasn't in the first design):
`Paused` is the user's decision, `Buffering` is the pipeline waiting. Merging
the two would tell the user "you paused", when they didn't.

Note: D-015's item "if events are missed, `drain_events()` can be added" is
still valid and still unnecessary — the end of a track is handled inside the
core (`Player::tick`), and no event stream leaks to the consumer.

---

## D-016 — The audio pipeline: symphonia + cpal, by hand
**Date:** 2026-08-29
**Question:** `rodio` (it wraps symphonia+cpal; a queue/mixer/gapless ready-made),
or by hand as PLAN §1.4 wrote?
**Decision:** **symphonia (decoding) + cpal (output), by hand.** The way the PLAN
wrote.
**Reasoning:** In Phase 4, the rooms' drift correction needs to move the playback
rate by 0.1% (PLAN 4.4); you only get there if you command the pipeline. Being
limited to what `rodio` gives would mean tearing it out later. The dependency
tree stays small too (K7/mobile).
**Accepted cost:** Resampling, format conversion and gapless are our job — Phase 1
will take longer.

**Applied (2026-08-29).** Decoding on a background thread, output in the cpal
callback; a ring buffer between them. The audio callback **never blocks** — if
the lock can't be taken, silence is written, with no waiting (blocking produces
crackle).

The position is computed **from the frames handed to the output**, not from the
decoded frames: the difference between them is a buffer's worth of time, and
looking at the decoded ones would put the progress bar ahead of the audio.
Measured with a real file: the 1 s fixture advanced in 96ms/100ms steps and
ended at 999ms.

Resampling is nearest-neighbour (including mono→stereo copying). Phase 1's goal
was to produce correct audio; if high-quality resampling is needed, it's
measured separately. With the `audio` feature off, the queue and the anchor still
compile; only the audio output drops out — server/mobile builds don't link
against ALSA.

---

## D-017 — The first provider: local files
**Date:** 2026-08-29
**Question:** In Phase 1, the local file provider first (§1.2), or Subsonic/Jellyfin (§1.3)?
**Decision:** **Local files.**
**Reasoning:** D-003 says "whichever environment can really be tested should
come first". This machine has `ffmpeg`, `flac` and `lame` installed —
royalty-free test fixtures (FLAC/MP3/OGG + samples with broken tags) can be
produced, so D-003's dogfood constraint is partly overcome. The
decoding/output pipeline the Subsonic client will also use gets built locally
first.
**Consequence:** §1.3 (Subsonic/Jellyfin) comes after the local provider works.
It's assumed the user has **no** running Subsonic/Jellyfin server; if they do,
the order is reconsidered.

**Applied (2026-08-29).** `LocalProvider`: recursive scanning, tag reading with
symphonia, deriving from the file name if there are no tags.
`SEARCH|BROWSE|STREAM` — no `CONTROL`.

The fixtures were produced (`fixtures/audio/`, 84 KB): `ffmpeg` sine tones,
royalty-free. Tagged FLAC/MP3, an untagged OGG, an untagged FLAC in a
subdirectory and a deliberately broken file. D-003's constraint was thus partly
overcome — the audio pipeline is tested with real files, on a real device.

Two security decisions: `resolve_source` **rejects a path that isn't in the
index** (it isn't an arbitrary file-reading surface), and a file deleted after
indexing gives an explicit error, not a silent `None`.

---

## D-018 — A persistent provider catalog (schema v2)
**Date:** 2026-08-30
**Question:** The local index was in memory, and every `headshell play` call
scanned the disk from scratch. Where will it be written?
**Decision:** **A separate table** in SQLite: `provider_tracks` + FTS5. Added as
schema v2; the v1 tables didn't change.
**Reasoning (this is the real decision):** `tracks`/`listens` and the catalog
**have different lifetimes**. `tracks` is "what you listened to" — it's derived
from raw events and, by the rules, is never deleted. `provider_tracks` is "what
you can play" — it's a mirror of the source, and when a file is deleted its row
must go too. Merging them into one table would mean also deleting the history of
a file you deleted from disk; that would trample the project's most basic promise
(your history belongs to you).
**Consequence:**
- The `CatalogStore` trait is separate from `ListenStore`.
- `headshell play` **doesn't scan**; `headshell provider scan` runs once.
- Incremental scanning: the tags of a file whose `mtime_ms` stamp hasn't changed
  aren't read again. `ScanSummary.unchanged` counts this (K9).
- `replace_catalog` drops the rows that aren't in the source and reports how many
  rows were dropped.
- The migration doesn't break existing installs: the test
  `migrating_a_v1_database_keeps_its_listens` sets up a v1 database, upgrades it
  to v2 and verifies that the listens are still there.

**A side effect — security.** `LocalProvider::resolve_source` used to look in the
in-memory index; the index is now in a table the provider doesn't see. A **root
check** was put in its place: the path is `canonicalize`d and checked for being
under the scanned roots. `<root>/../../etc/passwd` is rejected
(`resolve_source_rejects_traversal_out_of_the_roots`). A path that can't be
resolved (a deleted file) is rejected too — accepting something suspicious
would be a file-reading hole; Session explains the situation to the user.

---

## D-019 — Phase 1.3's scope: Subsonic **and** Jellyfin
**Date:** 2026-08-30
**Question:** Should the remote provider be Subsonic (OpenSubsonic) only, or
Jellyfin's own API too?
**Decision:** **Both.** Two separate providers, two separate credential models.
**Reasoning:** In most Jellyfin installs the Subsonic plugin isn't turned on;
saying "we support Jellyfin, but install a plugin first" is not supporting it.
What the two clients have in common (HTTP transport, the server registry,
credential storage, the stream source) is shared anyway; only the endpoints and
the JSON shapes diverge.
**Consequence:** Shared plumbing under `provider/remote/` + `subsonic.rs` +
`jellyfin.rs`. Jellyfin also doesn't have to be the plugin boundary's customer
in Phase 2; the plugin boundary will be tested with its own reference plugin.

---

## D-020 — The network transport layer: the trait in the core, the client behind a feature
**Date:** 2026-08-30
**Question:** There's no HTTP/TLS in `headshell-core`'s dependency tree today. How
should the first network call come in?
**Decision:** **Option C.** The `net::HttpClient` trait in the core, the
Subsonic/Jellyfin logic in the core; the concrete client behind the
`http-client` feature (the same pattern as `audio` and `render-png`). The CLI
turns the feature on; mobile doesn't — it provides its own transport as
`Arc<dyn HttpClient>`.
**Reasoning:** The convention already said "everything that touches the network
sits behind a trait, so that tests can use a fake". Without the trait boundary the
network logic can't be tested, and mobile can't use its own HTTP stack.
**Consequence:**
- The crate choice inside the feature is secondary and reversible: **`ureq` 3.4**
  (`default-features = false`, `rustls`). `reqwest` wasn't chosen because it
  would make `tokio` a permanent dependency of the core — the rule that the
  public API stays runtime-independent rules it out.
- `ureq` is a blocking API; `UreqClient::send` blocks inside the async signature,
  and this **is documented**. Since the core doesn't choose a runtime, it can't
  call `spawn_blocking`; whoever wants a non-blocking transport (GUI, mobile)
  provides their own `HttpClient`. The trait boundary makes this trade-off
  reversible.
- The tree measurement (as D-011 did); I'm writing down the procedure so it can
  be measured again:
  `cargo tree -p headshell-core --no-default-features [--features F] --prefix none |
  sed 's/ (\*)//' | sort -u | wc -l`.

  | Feature | Crates |
  |---|---|
  | none | **51** |
  | `http-client` | **69** (+18) |
  | `audio` | 82 (+31) |
  | `render-png` | 101 (+50) |

  So the mobile bindings don't carry the 18-crate TLS tree (ureq, rustls, ring,
  webpki…). **The first version of this line said "56 → 112"; it was a guess,
  not a measurement, and it was wrong** — the decision doesn't change, but the
  size of its reasoning does: `http-client` is **the cheapest** of the three
  features, not the most expensive.

---

## D-021 — Where the server registry and the credentials live
**Date:** 2026-08-30
**Question:** Where will the server address + user + password be written?
**Decision:** **`servers.json` in the data directory**, with `0600` permissions on
Unix. The schema doesn't change (SQLite v2 stays as it is).
**Reasoning:** A credential isn't library data; keeping it in the same file as
`library.db` would mix up backup and sharing behaviours. The OS keyring
(`keyring`) is a new dependency and fragile on headless Linux — it will be
reconsidered together with the plugin permission model in Phase 2.
**Consequence:**
- **Subsonic:** the password is **not written to disk in plain text**. At
  registration a random salt is generated and `token = md5(password + salt)` is
  stored; every request goes with the `u/t/s` triple. This is Subsonic's own
  authentication route, not something made up.
- **Jellyfin:** the password is turned into an access key once, with
  `AuthenticateByName`; what is stored is the key. The user can also give an API
  key directly — then the network isn't touched at all.
- **No dependency was added for md5**; RFC 1321 is implemented in the core
  (`provider/remote/md5.rs`, locked with the RFC's own test vectors). Reasoning:
  the tree must stay small, and md5 here isn't a security primitive but a wire
  format Subsonic imposes.
- The salt's entropy comes from `/dev/urandom`; if it can't be read, a clock +
  address based fallback is used, and that is **not silent** — it goes into the
  registration note.

### Addendum — off the disk, but not out of error texts (2026-09-26)

D-076 found it while writing Subsonic's cover call: every Subsonic request carries the
`u/t/s` triple in its query string, and an error that named the address carried the
triple with it — a server answering `500`, a server that can't be reached, a stream
that won't open. That text goes to `headshell diag`, which is made to be pasted into a
bug report, and to the interface's notices. The token isn't the password, but with
the salt beside it, it makes requests as the user.

Now such an address is named without its query string. Subsonic's calls name their
endpoint (`https://server/rest/search3`), on the network path and the status path
alike; a stream that fails to open — Subsonic's, or a plugin's, whose signatures
travel the same way — is named by its host and path. They still say where it failed
(K9); the key stays out. Only error texts changed: the requests carry what they
carried. The Subsonic test fails when the fix is taken out; a second test refuses a
stream from a local server and looks for the key in the error.

---

## D-022 — Phase 1.3's test path
**Date:** 2026-08-30
**Question:** There's no running Subsonic/Jellyfin server at hand. How will 1.3
count as "done"?
**Decision:** Automatic verification **with a fake HTTP server brought up in the
test** (`std::net`, no new dependency). The user will also set up a real server
(Navidrome / Jellyfin) with Docker; until that verification is done, 1.3 stays
explicitly marked **"code done, not verified on a real server"**.
**Reasoning:** D-003's dogfood constraint comes up again here. A fake server locks
the protocol's shape but doesn't show a real server's quirks (redirects,
transcoding, date formats); writing that we know this is better than writing
"DONE" as if we didn't (K9).
**Consequence:** The fake server talks to the **real** `UreqClient` — so the
transport layer is tested too, not only the parser. The end-to-end test serves the
fixture FLAC over HTTP and plays it: the `AudioSource::HttpStream` path really
produces audio (if there's no audio device, the test skips itself, writing the
reason to `stderr`).

### Verification — 2026-08-31: DONE

The second half the decision asked for is complete. Two real servers were set up
in Docker, and `headshell` connected to both:

| Server | Version | Result |
|---|---|---|
| Navidrome (OpenSubsonic) | 0.63.2 | register → verify → search → **stream** → scrobble |
| Jellyfin | 10.11.11 | register (password→key) → verify → search → **stream** → scrobble |

The fixture FLAC really played from both servers and showed up in the
`headshell stats` output — the remote half of Phase 1's done criterion ("it
plays from local **and remote** sources") is no longer an assumption.

Paths tested as well: a wrong password (on both, the registration **was not
written**), Jellyfin `--api-key` (a credential without going to the network), the
remote provider showing up in `provider list`.

**The one flaw the fake server hid and reality showed:** when registration
verification failed, the outer sentence said "its server **could not be
reached**". When Navidrome rejected the wrong password with
`HTTP 200 + status:"failed"`, the message turned into: *"could not be reached: …
rejected the request: Wrong username or password"* — contradicting itself and
sending the user off looking for a network error. The two causes of being
unhealthy (unreachable / rejected) can't be merged into one sentence; the outer
text now says only "**could not verify**", and `detail` carries the reason (K9).
Locked with a regression test
(`a_subsonic_failure_arrives_with_http_200_and_still_fails`).

**The verification procedure** (so it can be repeated):

```bash
docker run -d --name nav -p 14533:4533 \
  -v "$PWD/fixtures/audio:/music:ro" -v nav-data:/data \
  -e ND_DEVAUTOCREATEADMINPASSWORD=password123 deluan/navidrome:latest

HEADSHELL_PASSWORD=password123 headshell provider add subsonic \
  --url http://127.0.0.1:14533 --user admin --name nav
headshell provider test nav && headshell play "Test" --all && headshell stats
```

In Jellyfin the setup wizard is passed through the API (`/Startup/Configuration`,
`/Startup/User`, `/Startup/RemoteAccess`, `/Startup/Complete`), then `/music` is
added as a library with `/Library/VirtualFolders`.

**Still untested:** HTTPS/TLS (both were run over plain HTTP), redirects behind a
reverse proxy, server-side transcoding, a large library (tested with 4-5 tracks)
and Subsonic implementations other than Navidrome (Airsonic, Gonic, LMS).

---

## D-023 — Which stage a rejected request belongs to
**Date:** 2026-08-31
**Question:** When the server returns `HTTP 401`, which `Stage` should the error
be reported with? Every non-2xx code used to be `NETWORK_REQUEST`.
**Decision:** **`401` and `403` → `PROVIDER_CALL`**; every other code (`404`,
`429`, `5xx`…) stays `NETWORK_REQUEST`.
**Reasoning:** K9's distinction: "I couldn't reach it" and "it said no" are
different diagnoses with different fixes. A `401` isn't a transport error — the
connection was made, the request went out, the server read it and rejected it.
Saying `NETWORK_REQUEST` sends the user off to fiddle with their network, when
what they need to do is fix their credential. The flaw was seen against a real
Jellyfin (the D-022 verification): a wrong password was reported as
`ADIM: NETWORK_REQUEST`.

`404` and `5xx` were deliberately left in the transport layer: with them, which
layer the error comes from can't be known without reading the body, and guessing
produces a wrong diagnosis.

**Consequence:**
- The rule is in one place: `net::stage_for_status`. Both
  `HttpResponse::error_for_status` and `UreqClient::open_stream` (a 401 received
  while opening a stream) call it.
- A passing unit test **deliberately** changed
  (`jellyfin::an_http_error_is_reported_with_its_status_and_body`); as §0.1
  requires, it was asked first.
- The same class of flaw was fixed in two more places:
  - The `remote::prepare_server` verification error now says "**could not
    verify**", not "could not be reached" (the D-022 verification section).
  - The `headshell provider test` heading is "**UNAVAILABLE**", not
    "UNREACHABLE": `ProviderHealth.reachable == false` has two causes, the
    heading picks the word that covers both, and the `note` line says the
    reason. The `ProviderHealth` API didn't change — this is a presentation
    decision, the CLI's job (the Golden Rule).

**A side finding — a panic risk was closed (K8).** `error_for_status` clipped the
body with `String::truncate(200)`; if `truncate` lands in the middle of a
character boundary, it **panics**. A server's error message in Turkish (or any
multi-byte text) could have brought the core down. `net::clip`, which aligns to
the boundary, was put in its place, locked with a test.

---

## D-024 — Gapless: a single output, tracks fed in order
**Date:** 2026-08-31
**Question:** How will the gap between tracks be closed? A new `AudioEngine` (a
new cpal stream + a new decoder) was set up for every track; that was the gap.
**Decision:** **A single output, sequential feeding.** The cpal stream and the ring
buffer **stay open** between tracks; the decoding thread lives as long as the
engine, and when a track ends it takes the next one from the queue and keeps
writing into **the same buffer**.
**Reasoning:** The alternative considered was "two engines, prepare ahead": the
existing structure would have been kept, but two cpal streams would have had to be
open at the same time, and in some ALSA/WASAPI configurations the second can't be
opened — gapless would **silently** not work. The audience is mostly Linux; a
feature that breaks silently is worse than a feature that doesn't exist.
**Consequence:**
- **The position accounting rests on slices.** Since the buffer now carries
  samples of several tracks side by side, the position can't be read from a
  single counter. Every track is a `Span`: where it started in output frames, how
  many frames it wrote, its duration. The playing track is the slice
  `frames_played` falls into.
- `start_frame` **isn't known** while queueing (`None`): where it will start
  depends on how many frames the track before it writes, and that isn't clear
  until that track ends. Only **started** slices count as "playing" — otherwise
  the transition would be announced early and the interface would show a track
  not yet played.
- **A transition requested by the user isn't gapless.** `next`/`jump_to` set up
  the engine again: playing audio read ahead would mean playing a track the user
  didn't choose. Gapless is only for a natural end.
- **Reading ahead is an improvement, not a dependency.** If the next track can't
  be opened (a provider error), `tick` falls back to the old way: set up the
  engine again. There's a gap, but playback doesn't stop. The error goes to the
  log (K9).
- `Queue::peek_after_finish` tells what's next **without moving** the cursor. At
  the end of the queue, when wrapping with `RepeatMode::All`, it returns `None`:
  wrapping regenerates the shuffle, and which track comes next can't be known
  without moving the cursor. The price is one gap per round; better than decoding
  a made-up track ahead.
- Measurement: 4 fixture tracks (5 s of audio in total) played end to end in
  **5.47 s** — the total cost of the three transitions in between is too small to
  measure.

**A side finding — a scrobble inconsistency was fixed.** Since the duration of
untagged files was `None` in the catalog, `PlayRule`'s "half of the track" branch
couldn't work and the rule fell back to the 30 s threshold: a 1 s track listened
to from start to finish produced no scrobble. The duration is now read **from the
container** (`AudioEngine::duration_of`), and it goes into both the rule and **the
record**. Going into the record too is a must: otherwise the CLI said "4 listens
recorded" while `stats`, applying the rule again, showed 2. D-008's rule didn't
change — the data given to it was fixed. Locked with a test
(`every_queued_track_produces_a_listen_that_stats_also_counts`).

**A side fix.** `play_file`/`play_http` open the source **before the device**: a
broken or missing file should say `PLAYBACK_DECODE` in an environment without an
audio output (CI) too; a "no device" error would hide the real cause.

> **D-077 changed where the next track comes from.** `tick` no longer resolves it
> itself — that held the core's thread for as long as the provider took (4.7 s for
> yt-dlp, measured) — but hands it to the engine once the read-ahead thread has it
> ready. A transition the user asks for still sets the engine up again, as above; it
> starts from the address, and the stream's head, read ahead.

---

## D-025 — Directory watching: a dependency-free staleness probe
**Date:** 2026-08-31
**Question:** Changes are picked up only by running `headshell provider scan` by
hand. Should the `notify` crate be added for directory watching?
**Decision:** **No — no dependency was added.** Instead,
`headshell provider scan --if-stale`: the provider is asked a cheap question, and
it scans only if needed.
**Reasoning:** Scanning was already incremental (D-018, the mtime stamp), and its
expensive part, reading tags, was already skipped for unchanged files; what was
missing was the question "is it worth scanning". `notify` grows the core tree (K7,
mobile binary size) and behaves differently per platform. Directory stamps work
the same way everywhere.
**Consequence:**
- The question is on the trait: `Provider::catalog_changed_since(since_ms)` →
  `Option<bool>`. The three answers are three different things (K9):
  `Some(true)` changed, `Some(false)` unchanged, **`None` I don't know**. The
  default is `None` — a remote provider offers no cheap stamp, and saying
  "unchanged" would be wrong. Again a default trait method instead of a downcast
  (the same reasoning as `scan_catalog`): so plugins (Phase 2) can give their own
  stamps.
- **"I don't know" is a reason to scan.** Skipping because we don't know would
  make a file the user added invisible. So would a catalog that was never
  scanned.
- The local provider walks only **directories**; it doesn't `stat` files: the
  question isn't "what changed" but "is it worth scanning". If there's an
  unreadable directory, the answer is `None` — there may be a change there.
- **What it doesn't see is written down explicitly:** retagging a file in place.
  The file changes, the directory stamp doesn't. Catching that would mean
  `stat`-ing every file, which is the incremental scan itself. In that case a
  plain `headshell provider scan` is needed, and the command's help says so.
- The schema **didn't change**: the question "when was it last scanned" comes
  from the `MAX` of `provider_tracks.scanned_at`. If it was never scanned, `None`
  — not zero; "I looked in 1970" would show everything as stale.
- `ScanReport` gained two fields: `scanned` (did it run) and `reason` (why).
  Skipping **isn't silent**: the CLI prints "scan skipped (local: unchanged)".

This isn't watching but a probe that looks when triggered. If real-time watching
is needed, it'll be looked at again in Phase 3 together with the GUI's event
loop — there will be a loop there anyway.

---

## D-026 — Circumventing DRM is forbidden at the level of an invariant rule
**Date:** 2026-08-31
**Question:** The streaming platforms (Qobuz, Tidal, Deezer, Apple Music, YouTube
Music, SoundCloud, Spotify, Amazon, Pandora, Idagio, Tencent) weren't named in the
phases. Which of them can be supported?
**Decision:** The dividing criterion is **not** "is there an official API" but
**"is the audio protected with DRM"**. Code that decrypts a DRM-protected stream
isn't written in this project — and that isn't a preference but **a NEVER DO
item** (in both PLAN.md and CLAUDE.md).
**Reasoning:** Circumventing a protection measure is an article of law
**separate** from copyright infringement (DMCA §1201, EU 2001/29 art. 6): it
counts as a violation even if you have the right to the work itself. D-002 says
this is a product to be published, so there's no personal-use exemption — the
general form of what K4 does for Spotify on a single platform.
The reason for writing it as a rule is separate: if it stayed as reasoning, the
debate would open again with every new platform ("let's add Deezer too").
**Consequence:**
- A new section, **PLAN §2.5**, and an expanded **APPENDIX — Streaming
  platforms** (in place of the old "APPENDIX — Spotify"; the Spotify content was
  kept as the `CONTROL` row).
- The classification: **streamable** SoundCloud / Qobuz / YouTube Music;
  **metadata only** Tidal / Apple Music / Deezer; **`CONTROL` only** Spotify;
  **closed** Amazon Music / Pandora / Idagio / Tencent.
- **Deezer is official and open on the metadata side, encrypted on the stream
  side** (Blowfish). That puts it not in the same box as Tidal but on the other
  side of the line.
- **What rules Apple Music out is technical, not legal:** MusicKit is official
  and legitimate, but playing needs the MusicKit runtime — it isn't on Linux,
  and WebKitGTK has no FairPlay. `STREAM` could be opened in Phase 6's iOS/macOS
  version; that's why the row wasn't deleted.
- **A model mismatch rules out Pandora and Idagio too:** Pandora addresses
  stations, we address tracks. Idagio addresses works/movements/performances —
  that's an `identity/` job, not a provider job.
- **Tencent stayed on the list (as "closed").** That's exactly K5's reasoning:
  someone in that region writes the plugin themselves. We provide the protocol,
  not the list.
- **None of them enters the core.** The reason isn't only K5: these APIs break
  without warning, and when they break the music that is playing must not stop;
  only that plugin should fail.
- A "**this API information was not verified**" warning was put at the top of
  the APPENDIX — the relevant row will be tested again before a plugin is
  written.

The real finding isn't in the table but under it: **three platforms we can play
from, ten platforms we can take the listening identity from.** Importing doesn't
recognise this line, because an export is a legal right that terms of service
can't restrict (K2). Even the "closed" rows give history. And the product was the
second one anyway.

---

## D-027 — The order after Phase 1: Phase 3 (GUI + theme)
**Date:** 2026-08-31
**Question:** D-014 only said "Phase 1 first"; it didn't settle what came after.
Is it Phase 2 (the plugin boundary) next, or Phase 3 (GUI + theme)?
**Decision:** **Phase 3.** The first job is the §3.1 GO/NO-GO measurement.
**Reasoning:** The PLAN's own warning: "this is where the community's engine is
(D-002, D-004); delaying it is expensive." The theme ecosystem is this project's
distribution channel, not an ornament to be added later.
**Consequence:**
- §3.1 measures first, then decides: a virtualized list + CSS animation + IPC
  load in Tauri, **on Linux/WebKitGTK**. The measurement is done inside `spike/`
  — outside the workspace, throwaway, with no dependency entering the core.
- If the measurement comes out unacceptable, Dioxus/a native Rust GUI is
  discussed; the price is losing the CSS theme ecosystem. The decision is made
  **after** the measurement, with numbers.
- Phase 2 was postponed, not cancelled. The debts handed over to it stay: keyring
  (D-021), real-time watching (D-025), TLS/reverse proxy/transcoding
  verification (D-022).
- **Phase 2's reference plugin (§2.2) was chosen already, as SoundCloud.** Its
  only reason isn't the catalog: it's the only candidate that needs no
  subscription, so the only one that can run in CI and on someone else's
  machine. Qobuz's catalog is cleaner, but without a subscription it can be
  neither developed nor tested; YouTube Music's maintenance cost (nsig, PO
  tokens) would mean wrestling with the platform while trying to test the
  protocol. The reference plugin's job is to prove the protocol, not to offer a
  catalog.

---

## D-028 — §3.1 GO/NO-GO: Tauri accepted, with an environment condition
**Date:** 2026-08-31
**Question:** PLAN §3.1 — is Tauri acceptable on Linux/WebKitGTK under the load of
a 50,000-row virtualized list + CSS animation + IPC?
**Decision:** **GO.** But on Linux it's **unacceptable** without
`GDK_BACKEND=x11` and `WEBKIT_DISABLE_DMABUF_RENDERER=1` being set.

**The measuring machine is weak on purpose:** Intel HD 6000 (Broadwell GT3, 2015),
4 cores, 8 GB, Wayland, WebKitGTK 2.52.6, Tauri 2 (431 crates, an 11 MB binary).
The thresholds were written in `spike/tauri-gonogo/THRESHOLDS.md` **before the
measurement**; setting thresholds after seeing the numbers wouldn't be measuring
but fitting the decision to the measurement.

**The numbers (the corrected environment, two independent runs):**

| Measure | GO threshold | Result |
|---|---|---|
| 50k scrolling + disciplined CSS, median frame | ≤ 18 ms | **17 / 17 ms** |
| ” p95 | ≤ 25 ms | **21 / 22 ms** |
| ” worst frame | ≤ 120 ms | **23 / 26 ms** |
| IPC round trip p95 (1000 samples) | ≤ 5 ms | **1 / 1 ms** |
| Frame cost of 30 Hz IPC | ≤ 2 ms | **1 / 1 ms** |
| Rust → JS event stream | ≥ 2000/s | **10,417 / 11,765** |
| Until the window shows | ≤ 1500 ms | **271 / 267 ms** |
| Peak RSS (50k rows loaded) | ≤ 250 MB | **185 / 183 MB** |
| 50k scrolling + **naive** CSS, median | ≤ 18 ms | 21 / 21 ms — BORDERLINE |

**The reasoning and four findings:**

1. **The virtualized list isn't a problem.** 50,000 rows, node-pooled
   virtualization, continuous scrolling: 58.8 fps, zero dropped frames. This was
   the part of the measurement that passed most easily — this wasn't where the
   fear belonged.

2. **In the default environment CSS animation drops the frame rate 2.4×**
   (58.8 → 23.8). If this weren't fixed, it would have been a NO-GO.

3. **The culprit is not the hardware but the engine's path — told apart with a
   control experiment.** On the same machine, on the same page, on the same GPU,
   **Firefox 154 draws 58.8 fps in all four phases** (naive CSS included). This
   distinction decided the outcome: if it were the hardware's ceiling, the
   PLAN's alternative (a native Rust GUI) wouldn't have saved it either, because
   it would hit the same GPU. Without running the control, the wrong decision
   would have been made. The control isn't a separate page but **the same
   `index.html`** — a control measuring a different page wouldn't be comparable.

4. **The single-variable fix was misleading.**
   `WEBKIT_DISABLE_DMABUF_RENDERER=1` **alone makes scrolling worse**
   (52.6 → 30.3 fps); `GDK_BACKEND=x11` alone doesn't rescue the CSS phase
   (23.8 → 26.3 fps). Only both together help. An investigation that tried the
   variables one at a time and stopped would say "there's no workaround" and give
   a NO-GO.

**Consequence:**
- Phase 3 goes on; §3.2 (the IPC contract) is the next job.
- **The environment fix is a distribution job, not the user's.** How it will be
  applied (the app sets it itself, a wrapper script, conditional on the driver)
  is a separate decision — open.
- **§3.2's fear wasn't confirmed at this scale.** "Hundreds of messages per
  second = stutter" didn't happen: 30 Hz polling adds 1 ms to the frame, and the
  bridge carries ~10,000 events/s. Batching **isn't needed for performance**.
  Predicting from the anchor is still the right design, but its reasoning
  changed: (a) if IPC stalls, the interface doesn't freeze, (b) Phase 4's room
  primitive is the same type anyway (D-015). §3.2 will be written with this
  corrected reasoning — a correct design defended with the wrong reason falls at
  the first objection.
- **A constraint that goes into §3.3:** even in the corrected environment,
  `height` / `box-shadow` / `filter` / `background-position` animations take
  58.8 → 47.6 fps; `transform` + `opacity` don't drop it at all. The theme
  contract **has to say** which properties can be animated — if it doesn't, the
  difference shows up on the user's machine.

**Not measured (written down on purpose):** one machine and one driver
(Mesa/Broadwell); Nvidia, AMD and newer Intel weren't tried. `GDK_BACKEND=x11`
needs XWayland; a setup without XWayland wasn't tried. Resizing, multiple
windows, high DPI and 4K weren't measured (the pixel count grows ~9×). Since
WebKit rounds `performance.now()` to 1 ms, there's no sub-ms resolution.

The details and the reproduction procedure: `spike/tauri-gonogo/RESULTS.md`.
The spike is throwaway; the numbers that need to stay are here.

---

## D-029 — The app sets up the environment fix itself
**Date:** 2026-08-31
**Question:** D-028 measured that Linux needs `GDK_BACKEND=x11` +
`WEBKIT_DISABLE_DMABUF_RENDERER=1`. Who will set this up — the app, a wrapper
script, or the user?
**Decision:** **The app itself**, as the first thing `main()` does, only on Linux
and only **if the variable isn't defined**.
**Reasoning:** It's something the user shouldn't have to know. A wrapper script
would leave out anyone who runs the binary directly from a terminal; a
conditional measurement (computing frames at startup and deciding) would add a
startup delay and the flicker of restarting the webview.

**Verified, not assumed.** There was a question to ask: GDK reads `GDK_BACKEND`
during `gtk_init`, and WebKit reads `WEBKIT_DISABLE_DMABUF_RENDERER` when the web
process is born — both **after** Tauri's setup. Is the first line of `main()`
early enough? Measured: with no outside variable given, the app setting them up
itself took it **23.8 → 55.6 fps** (the CSS phase). It was early enough.

**Consequence:**
- The user's own setting **isn't overridden**: someone who deliberately gives
  `GDK_BACKEND=wayland` isn't interfered with. Every variable set or skipped is
  written to the log — silently changing the environment makes debugging
  impossible (K9).
- **An open snag — the `unsafe` conflict.** In Rust 2024 `std::env::set_var` is
  `unsafe`. The workspace says `[workspace.lints.rust] unsafe_code = "forbid"`,
  and `forbid` **can't be overridden** with an `allow` at the package level;
  `headshell-core` and `headshell-cli` both inherit it with
  `[lints] workspace = true`. When the GUI package is written there are three
  options: (a) turning the workspace rule into `deny` (then it can be
  overridden, but the protection weakens), (b) leaving the GUI package without
  `lints.workspace = true` (it loses the other rules too), (c) not using `unsafe`
  at all, setting up the environment and **re-running itself** (`exec`). This
  decision will be made in §3.2, when the GUI package is really opened — there's
  no package today.
- **This fix was measured on one machine** (Mesa / Broadwell / Wayland). Whether
  it's needed or harmless on other drivers is unknown. `GDK_BACKEND=x11` needs
  XWayland; what happens on a pure Wayland setup without XWayland wasn't tried.
  That's why it isn't unconditional; it's set "if not defined": the escape hatch
  stays open.

---

## D-030 — Package layout: three packages, one direction
**Date:** 2026-08-31
**Question:** Where should the GUI package live — inside the workspace, in a
separate workspace, or in a separate repository?
**Decision:** The same repository, the same workspace, **three packages**:
`headshell-core`, `headshell-cli`, `headshell` (GUI). They **behave like**
separate packages, but they don't have to be separate repositories.
**Reasoning:** Separability is a property of the structure, not the file layout.
What matters is this: `headshell-cli` and `headshell` **can't work** without
`headshell-core` — all the base operations are there (K1). For them to be
separable when wanted, it's enough that the dependency direction is one-way from
today.
**Consequence:**
- The dependency direction: `headshell-core` ← `headshell-cli`,
  `headshell-core` ← `headshell`. **`headshell-cli` and `headshell` never see
  each other.** If one wants something from the other, that thing belongs to the
  core — the Golden Rule at the package level.
- A separate repository, not for now: the core still changes fast, and keeping
  two repositories in step is expensive at this stage.
- The known price: `cargo test --workspace` will build the Tauri tree too.
  Measured — a full build of Tauri is ~70 s, 431 crates in the lock file.
  Bearable; if it becomes unbearable, it's separated with `default-members`.

---

## D-031 — The environment fix without `unsafe`: `exec`
**Date:** 2026-08-31
**Question:** D-029's open snag. `std::env::set_var` is `unsafe` in Rust 2024, the
workspace says `unsafe_code = "forbid"`, and `forbid` can't be overridden with an
`allow` at the package level. Should the rule be loosened, or should the package
leave lint inheritance?
**Decision:** **Neither.** The environment is set up and the process restarts
itself with `exec`. `CommandExt::exec` is a safe call; there's no need for
`set_var` at all.
**Reasoning:** Loosening a workspace-wide safety property for a single line is
disproportionate. Taking the package out of lint inheritance would lose all the
other rules too. `forbid` stays intact in all three packages.
**Consequence:**
- **The loop guard comes from the structure, not a flag:** only **missing**
  variables are set; in the child's eyes nothing is missing, so it doesn't `exec`
  a second time. A separate "already restarted" flag isn't needed.
- `exec` replaces the process image and keeps the PID — desktop/service
  integration doesn't break.
- Verified: with no outside variable given, the CSS phase is **58.8 fps**,
  startup 352 ms. The restart has no measurable cost.
- `exec` returns only **if it fails**; in that case it carries on without the fix
  and writes the reason to the log — instead of silently running slow (K9).

---

## D-032 — The `tick` loop and listen recording moved into the core
**Date:** 2026-08-31
**Question:** In which layer should the regular `tick()` and writing accumulated
listens to the store live — should each shell wire it up itself, or should the
core provide it?
**Decision:** **The core.** A new type, `playback::LiveSession`, ties `Player` and
`Session` together; a single `tick()` call advances, writes and returns a
`TickReport`. The TUI was moved onto it.
**Reasoning:** K1's test: the TUI wrote the same dance once (tick → take_listens
→ record_listens), the GUI would write it a second time and mobile a third. A
shell that forgets to write listens to the store **silently loses history** —
nothing makes the loss noticeable.
**Consequence:**
- **The behaviour changed: listens are now written every round, not on exit.**
  In the old version `tui.rs` wrote only when the loop ended; since a CLI session
  is short, the problem didn't show, but in an interface left open for hours a
  crash or a `kill` would take the whole session's history with it. In most
  rounds there's nothing to write, and the store isn't touched at all.
- **If writing fails, the records aren't thrown away but held**, and retried in
  the next round. Since `take_listens` pulls the records out of the player, if
  they weren't held there'd be nowhere to take them back from.
- **A store error doesn't make `tick` an `Err`:** the audio keeps playing, and
  bringing the session down would increase the data loss. But it doesn't stay
  silent either — `TickReport` carries `store_error` and `listens_pending`, and
  the TUI shows both (K9).
- **D-015 was kept:** no observer/callback; the shell drives the loop itself.
  **K7 was kept:** no closure parameter, generic or lifetime leak.
- What `track_changed` doesn't catch was written down on purpose: when
  `RepeatMode::One` restarts the same track, neither the track nor the position
  changes. That's also what's right for the shell — the anchor says it started
  over, and `listens_recorded` says there's a new listen.
- **A test was removed because it proved nothing.** Testing the claim "if the
  store can't write, no record is lost" with a really broken store was tried:
  the data directory was made read-only, but SQLite kept writing through its open
  file handle and the test **turned green while testing nothing**. The decision
  was pulled out into a pure function (`absorb`) and tested directly. A green
  test whose condition can't be met is worse than no test — because it makes you
  think it's covered.

---

## D-033 — The IPC contract: the core's surface + serde, no versioning
**Date:** 2026-08-31

> **D-070 (2026-09-24):** The JS copy's test now runs not with `node` but with the
> embedded QuickJS (`anchor_parity_js.rs`; `anchor_parity.mjs` was deleted). The
> rule "it doesn't skip, it fails" stands as it was; only what it asks of the
> machine is gone.
**Question:** §3.2 — how should the contract between the webview and the core be
defined, how should it be versioned, and how do we make sure the anchor
prediction in the webview doesn't drift from the core?
**Decision:** There is **no** separate IPC type layer. Every command returns an
existing core type, passed with `serde`. **No version negotiation.** Drift is
locked with `fixtures/anchor/position_cases.json`.
**Reasoning:**
- **A translator layer makes the two types drift over time**, and nothing
  catches the drift. The `--json` output already proved that the CLI and the GUI
  get the same data; making up a second shape would break that proof.
- **Versioning is unnecessary because both sides are inside the same binary.**
  The webview assets are packaged with the app and can't be updated
  independently. A handshake at run time would be teaching a handshake to a
  process that can only talk to itself. **It must not be confused with §3.3's
  theme API** — that's an external contract and has to be versioned.
**Consequence:**
- The command list matches the CLI's subcommands one to one: `search`, `stats`,
  `sleeve`, `import`, `resolve`, `providers`, `provider_test`, `provider_scan`,
  `servers_list`, `server_add`, `server_remove`, `play`, `toggle_pause`, `stop`,
  `next`, `previous`, `jump_to`, `set_shuffle`, `set_repeat`, `anchor`, `queue`,
  `diag`.
- **Events are sent only when something changed, not on a timer.** The GUI's
  Rust side drives the `LiveSession::tick()` loop; if a `TickReport` carries
  `track_changed` / `listens_recorded` / `store_error` / `finished` it crosses to
  the webview, and if it doesn't, nothing is sent. In the silence between, the
  position is predicted from the anchor. (D-028 measured that we can carry
  10,000 events per second — so this isn't a performance measure but a
  preference for not sending needless messages.)
- `TickReport` gained `Serialize`.
- **The question left open was handed over to §3.3:** will an IPC surface be
  opened to themes? If it is, the contract turns into an *external* contract,
  and the versioning debt is born that day. It must be answered while the token
  set is designed.

**Drift protection, and what was found there.** In the webview the position is
predicted without asking the core, so a second copy of the formula will live in
JS. Two copies drift over time, and the drift starts where nobody notices: the
progress bar lies by a few hundred milliseconds, nobody complains, and then **in
Phase 4 the same formula drives room sync.** `fixtures/anchor/position_cases.json`
(12 cases) is the single source of truth both sides read; `tests/anchor_parity.rs`
binds the Rust side.

While the set was being written, a case broke the test, and **what was wrong was
the expectation, not the code**: with `rate = 1.001` at 100 s the core says
**100099 ms**, not 100100 — `100000 × 1.001` isn't exact in binary
(100099.999…) and `as u64` truncates. If JS used `Math.round` it would say 100100,
and the two copies would split exactly here. The right counterpart is
`Math.floor(elapsed_ms * rate)`; it's written in the fixture. This is the set's
most valuable case, and it was found before any JS was written.

A second test protects the set itself: if the hard cases (Buffering, rate 0, the
clock jumping back, clipping to the duration) are deleted, the test breaks. An
accuracy set that covers only the easy path isn't a lock.

---

## D-034 — In the GUI the core lives on its own thread
**Date:** 2026-08-31
**Question:** It came up while writing the `headshell` package: Tauri wants every
async command's future to be `Send`. Can `LiveSession` be kept as shared state
behind a `Mutex`?
**Decision:** **No.** The core settles on its own thread; commands send a closure
there and wait for the answer with a `oneshot`. No lock.
**Reasoning:** A measured constraint, not a preference:
- `Session` **is `Send` but not `Sync`** — the SQLite connection carries a
  `RefCell`. `Mutex<Core>` doesn't help, because carrying the lock across an
  `.await` needs `Core: Sync`.
- The future `Session::import_archive` returns isn't `Send` anyway
  (`Box<dyn ExportArchive>`). Even if the lock problem were solved, this would
  remain.
- The solution is not to move the core: **no core type crosses the thread
  boundary**; only job closures and serialisable results cross. The core didn't
  change — the constraint was met on the shell's side.
**Consequence:**
- **No enum variant per command.** The channel carries
  `Box<dyn FnOnce(&mut Core) -> ...>`; every command closes over its own
  `oneshot`. A 23-variant message type would be another disguise of the
  translator layer D-033 rejected.
- **The tick loop is on the same thread too.** With `tokio::select!` the job
  queue and a 200 ms timer sit side by side; there's no race between commands
  and `tick()` — the channel puts them in order.
- **The known price:** while a long `import` runs, the playback controls wait in
  line. So it doesn't stay invisible, long commands send a `headshell://busy`
  event and the interface writes which job is running (K9). Found acceptable: the
  audio keeps playing on its own thread anyway.
- **The binary is named `headshell-desktop`.** The package is named `headshell`
  as in D-030, but `headshell-cli` already produces a binary named `headshell`,
  and two outputs with the same name in one workspace collide. The CLI's name is
  promised to the user (`headshell import ...`), so the GUI was the side that
  changed.
- **There's an error envelope, no data envelope.** `headshell_core::Error` can't
  be serialised (its source chain is `dyn Error`); commands return
  `{ stage, chain }` — the same as what the CLI prints to `stderr`. What D-033
  forbade was writing twins of the data types; this isn't within its scope.

---

## D-035 — The event list was incomplete: state changes must be sent too
**Date:** 2026-08-31
**Question:** D-033 had said events would go only on `track_changed` /
`listens_recorded` / `store_error` / `finished`. Is this list enough?
**Decision:** **It isn't.** They must also be sent when the part of the anchor
**that feeds the prediction** (state, rate, duration, track ID) changes. The list
grew by this item.
**Reasoning — the rule broke not at the desk but when it ran.** The first version
applied D-033's list to the letter, and the interface **silently froze**: after
`play`, the webview takes an anchor once, and at that moment, since the engine
hasn't filled the buffer yet, the state is `Buffering`. When the buffer fills,
the engine moves to `Playing`, but since that transition doesn't count as
"notable", it isn't sent. The webview stays with the `Buffering` anchor it has,
and since `Buffering` doesn't advance, the progress bar freezes at 0:00. **No
error shows** — the audio plays, and the interface looks as if it doesn't. It
wasn't noticed until a real track was played on screen.
**Consequence:**
- The rule was put like this: the webview **walks** the position itself with
  `position_ms + (now - wall_time) × rate`; when that formula's input or context
  changes, the prediction goes wrong. `position_ms` itself is deliberately **not**
  on the list — predicting it is the prediction's job.
- The decision was pulled out into a pure function
  (`core_thread::worth_sending`) and tested. One of the tests locks exactly this
  bug: `the_buffering_to_playing_transition_must_be_sent`.
- **The general lesson:** in a "send only on change" rule the real risk isn't
  too many messages but **missing messages**. The excess costs performance and
  gets measured; the missing silently freezes the interface and leaves no trace
  anywhere.

---

## D-036 — The naming language: the outer surface in English, the inside in Turkish
**Date:** 2026-08-31

> **D-073 (2026-09-25):** The text-language half of this decision (comments,
> documents and interface text in Turkish) was replaced — everything is English
> now. The identifier half stays as it is.

**Question:** It came up while presenting §3.3's theme token set: should the token
names be Turkish? And the real question behind it — which name is written in
which language in this project?

**Decision:** The distinction isn't the code language but **who reads it**.

- **All identifiers are English.** Functions, types, variables, CSS classes,
  HTML ids, JSON keys, fixture file names, theme tokens.
- **Comments, documentation and user-facing text stay Turkish.** `PLAN.md`,
  `DECISIONS.md`, doc comments, CLI help text, interface text, the `ADIM:`
  prefix. These are the localisation axis, not the naming axis.

**Reasoning:**
- The line was already there in practice, just not written down: all of
  `headshell-core` and the CLI subcommands were English (`PlaybackAnchor`,
  `Queue::view`, `headshell provider scan`); the only Turkish was inside the GUI
  shell (`.ust`, `dikkate_deger`). An unwritten rule gets applied inconsistently
  six months later.
- The theme token set will be this project's **most outward-facing** surface —
  what consumes it isn't code I wrote but a theme author I don't know. Saying
  `--headshell-yuzey` would limit theme authorship to those who know Turkish; a
  narrowing with no benefit at all.
- CSS's own words are English; `background: var(--headshell-arka2)` mixes two
  languages in one line and makes you pause while reading.
- There's also the accent problem: the `sanatçı`/`sanatci` pair is a source of
  silent bugs in a file name or a JSON key.

**Consequence — the renaming done together with this decision:**
- `crates/headshell` (Rust): `duzelt→fixup`, `baslat→spawn`, `calis→run`,
  `tur→run_tick`, `Onemli→Notable`, `dikkate_deger→worth_sending`,
  `calistir→run_on_core`, `cekirdek_dustu→core_thread_gone`.
  The diagnostic stage `ADIM: ORTAM_DUZELTME` → `ADIM: ENV_FIXUP` (in the same
  spelling as the core's `CONFIG_LOAD`/`IDENTITY_RESOLVE` vocabulary).
- `crates/headshell/ui`: all CSS classes, HTML ids and JS names
  (`.ust→.topbar`, `.uyari→.toast`, `capa→anchor`, `cagir→call`…). The CSS
  variables are English too, but **deliberately without the `--headshell-`
  prefix**: that prefix is the promise itself, and §3.3 will hand it out.
- The shared accuracy set (`fixtures/anchor/position_cases.json`): the keys
  (`vakalar→cases`, `capa→anchor`, `beklenen_ms→expected_ms`) and the case names.
  This file is read from two languages, and whoever grows the set shouldn't need
  to know Turkish. The `needle` list on the Rust side was translated too.
- The audio fixtures: `etiketli.flac→tagged.flac`, `bozuk.flac→corrupt.flac`,
  `Baska Sanatci - Ogg Parca.ogg→Other Artist - Ogg Track.ogg` etc.
- `examples/calma_denemesi.rs→playback_probe.rs`.

**The single deliberate exception — embedded tags.** The tags of `tagged.flac`
and the mp3 became `Test Artist`, but the titles are `Sine 440 ünïcode` /
`Mp3 Track ünïcode`. The accents aren't a leftover of a language but **the very
thing being tested**: the tags are UTF-8, and this is the only proof of that
decoding path. The test says so too.

**A side lesson — the renaming really broke a test.** `cli_json`'s gapless test
said `play "Sanat" --all`, and catching all four fixtures depended on two of
them having a Turkish file name and two a Turkish tag. When the names were
translated, the common token disappeared and the test saw 2 tracks instead of
4. The query became `"Artist"`; the fixtures are now `Test Artist`,
`Other Artist`, `Dir Artist` — the commonality is **deliberate and visible**, not
derived from a language accident.

---

## D-037 — The theme token set: a narrow set, class names in the contract, no IPC, a manifest + `api`
**Date:** 2026-09-01
**Question:** §3.3's DECISION POINT had left four questions: granularity, the
selector promise, whether IPC is opened to themes, the package format. They were
to be asked before writing, because once published, a backward-compatibility debt
is born.

**Decisions:**

1. **Granularity: a narrow semantic set.** No region-specific overrides —
   Spicetify's fragility came exactly from layered/wide slot sets (§3.3's own
   reasoning). ~15-20 tokens, including the state variants (hover/focus/disabled),
   but no separate variable per region.
2. **The selector promise: class names.** My advice was to limit it to CSS custom
   properties (leaving the class names as an internal detail after D-036); the
   user chose to make the class names part of the contract. **Consequence:** the
   existing class names in `crates/headshell/ui/style.css` (`.topbar`, `.toast`,
   `.queue-item` etc.) are no longer an internal detail but a stable surface a
   theme author can target. This **overrides** D-036's assumption that "class
   names are an internal detail and change without notice" — renaming CSS now
   counts as a break. The warning at the top of `style.css`, "this is not the
   theme contract", falls with this decision.
3. **IPC for themes: no.** Themes only change appearance; the internal IPC
   contract D-032/D-033 won, with "no versioning needed", stays intact.
   **Note:** the user also proposed a "mod" idea that changes behaviour —
   something downloadable together with theme packages, with its own section.
   This was **deliberately postponed** and not designed today: a mod running JS
   inside the webview and calling IPC carries a security class radically
   different from the "subprocess + JSON-RPC, if the plugin crashes the core
   doesn't" model K5 requires for provider plugins — a mod running inside the
   same webview can crash/freeze the whole interface. It was written into
   PLAN.md as a separate DECISION POINT (see the end of §3.3); the design happens
   that day.
4. **The package format: a manifest + an `api` version field.** A small manifest
   (`name`, `author`, `api`) + a CSS file. On loading, the app checks the `api`
   version; on a mismatch it **doesn't silently ignore the theme but rejects it
   explicitly and says why** — in line with K9's diagnostics culture and D-035's
   lesson ("in this rule the risk isn't too many messages but missing ones").

**Consequence — the work was carried to the rest of §3.3:** the concrete token
list, the manifest schema, moving `style.css` onto tokens, the theme
loading/validation code and §3.4's two reference themes. This decision only
closes the four questions; the implementation happens in a separate step.

---

## D-038 — A theme spilling outside `:root`: don't reject it, flag it
**Date:** 2026-09-01
**Question:** While the token set was being written (the implementation of
D-037), a small fifth question came up: if `theme.css` spills outside `:root` and
targets a selector directly (e.g. `.topbar`), what should the loader do — reject
it, or leave it free?
**Decision:** **Neither — flag it.** The loader doesn't reject such a theme; it
loads it, but labels it explicitly in the theme selection list as "extended / no
guarantee". The surface the contract **guarantees** is always only the
`--headshell-*` tokens in `:root` — across an `api` version bump, backward
compatibility is committed only for them.
**Reasoning:** The user wanted a middle way instead of a strict binary choice —
"one blocks creativity, the other blocks a consistent interface." Rejecting shuts
off the theme author's slightest flexibility (a small special touch on a single
element); leaving it free makes the contract meaningless in practice. Labelling is
the exact counterpart of K9's "don't swallow it silently, say it" principle: the
risk isn't hidden, it's made visible to the user, but it isn't prevented either.
In line with D-035's lesson — what's problematic is silent harm, not explicit
information.
**Consequence:** When the loader is written (the remaining §3.3 work, not yet
started), an "is there an out-of-scope rule" scan will be added next to the
manifest validation — without parsing CSS, a simple check that looks for any
selector outside the `:root { ... }` block is enough.

---

## D-039 — The token set was incomplete: `--headshell-color-scheme`
**Date:** 2026-09-01
**Question:** Not asked — **measured** while §3.4's reference themes were being
written. D-037's narrow semantic set (thirteen tokens) wasn't enough to express a
light theme: although the `daylight` theme applied all thirteen tokens correctly,
the checkboxes, the text caret and the scrollbar stayed dark.

**Cause:** The tokens only change the colours **we draw**. The engine draws the
checkbox, the caret and the scrollbar, and the engine's only input is CSS's
`color-scheme` property — not a colour value but the answer to "is this interface
light or dark". No colour token can stand in for it.

**Decision:** A fourteenth token, `--headshell-color-scheme` (`dark` | `light`).
It's used in `style.css` as
`html { color-scheme: var(--headshell-color-scheme); }`.

**`api` didn't go up, and that's deliberate.** The rule: **adding** a new token
doesn't raise the version — old themes didn't write it, so they get the default
and keep working. What raises it is removing an existing token or changing its
meaning. `--headshell-color-scheme` is the first example of this rule; the rule
was written into PLAN §3.3 and `src/theme.rs`.

**What's really worth recording isn't the finding but how it was found.** §3.4
says "prove on at least two different themes that the API is sufficient"; the
criterion was there not to prove but to **find where it isn't enough**, and it did
exactly that. That's also why the second theme (`contrast`) isn't a second
palette: by dropping the radius and duration tokens to zero, it tests the axis
**other than** colour. If two palettes had been written, the "two themes"
criterion would be met on paper, and the missing token would show up on the first
theme author's machine — with the app blamed, not the theme (D-028's same
reasoning).

**The loader's small decisions** (none of them irreversible, so they weren't
asked; recorded so they aren't debated a second time):
- The selection is kept in `<data_dir>/ui.json`. The database schema wasn't
  touched: a theme isn't listening data but an interface preference.
- The built-in reference themes are embedded with `include_str!`, but **without
  privileges** — they go through the same validation as a theme on disk.
- A disk theme carrying the same name doesn't shadow the built-in; it's rejected
  with the reason. Silent shadowing would make you guess which file won (K9).
- The theme commands **don't enter** the core thread: a theme isn't a core
  concept, and there's no reason not to be able to change the interface's theme
  while a long `import` runs.
- The diagnostic stage is `CONFIG_LOAD`. Adding a `THEME_LOAD` stage that only
  the GUI needs to the core would leak a concept belonging to the shell into the
  core's diagnostic vocabulary.

---

## D-040 — The plugin permission model: declaration + consent, enforcement later
**Date:** 2026-09-01

> **D-069 (2026-09-24):** "enforcement later" closed. Plugins run in an embedded
> QuickJS and can reach the outside only through the engine's gates; the network
> permission is **enforced** on every request, every redirect and the stream
> address, wildcards (`*.domain.name`) arrived, and the file permission concept
> went away (a plugin has no file access). What follows is the model up to that
> day.
**Question:** (PLAN §2.1) Will a plugin's network/file access be restricted?

**Decision:** **Declaration + consent.** The plugin declares its permissions in
its manifest, the user approves them on first load, and the consent is recorded.
**No jailing at the operating-system level** — and the user is told exactly that.

**Reasoning.** Three things are true at once:
1. The plugin runs as a subprocess **with all of the user's privileges**. The
   declaration isn't a firewall but a **contract**: "this plugin says it will do
   these things". Presenting it as security would make people trust a protection
   that doesn't exist — the security version of a silent `unwrap_or_default()`.
2. Real jailing (Landlock, `bubblewrap`) exists only on Linux. macOS and Windows
   have no counterpart; the model collapses there, and the
   "permitted/unpermitted" distinction changes meaning by platform. Phase 2's done
   criterion is **a language-independent reference plugin**, not an operating
   system jail.
3. Being able to add it later depends on the protocol being split at the right
   place today — so the real job is to get the permission **names** right.

**Consequence:**
- In the manifest, `permissions: { net: [host…], fs: [path…] }`. `net` entries
  are host names (`api.soundcloud.com`), `fs` entries are path prefixes. The names
  were chosen in a form that can later be turned into a Landlock/bwrap rule — that
  is, **machine-readable, not descriptive**.
- The consent is kept in `<data_dir>/plugins.json` together with a digest of the
  permission set. If the plugin grows its permissions, the digest changes and
  **consent is asked for again**; if it shrinks them, it isn't.
- The one thing the core itself hands out is narrowed: the plugin sees its own
  data subdirectory (`<data_dir>/plugins/<name>/`) and **only its own** secrets
  (D-042).
- `headshell diag` and `headshell provider list --json` report the declared
  permissions **and** that they aren't enforced. The user knows what they're
  trusting (K9).
- When enforcement comes, the `api` version doesn't go up: the manifest fields
  stay the same; what changes is what the core does with them.

---

## D-041 — Phase 2's scope: §2.1 + §2.2, the rest a separate round
**Date:** 2026-09-01
**Question:** (PLAN §2.2) How many plugins will be written in this phase?

**Decision:** Only **§2.1 (the protocol) + §2.2 (the Python SoundCloud
reference)**. §2.3 (AcoustID), §2.4 (torrent) and §2.5 (streaming platforms) are
outside this round.

**Reasoning:** Phase 2's "counts as done" criterion is exactly this already — "a
non-Rust reference plugin works, and the core can reject it on a version mismatch
without crashing". A second provider proves nothing new about the protocol; it
only has the first provider's mistakes written twice. Chromaprint (§2.3) is a
native C library, and `librqbit` (§2.4) is a big job on its own; both depend on
the protocol being right, and they're cheaper if done **after the protocol
settles**.

**Consequence:** §2.3/§2.4/§2.5 aren't cancelled; they're queued. Phase 2 is
reconsidered when the protocol closes; if the first plugin shows a gap in the
protocol (as D-039 did for themes), we don't move on to the next one until that
gap is closed.

---

## D-042 — Secrets: a single concept, file-based, namespaced; still no `keyring`
**Date:** 2026-09-01
**Question:** (handed over from D-021) Credential storage was to be revisited
together with the plugin permission model. Will `keyring` be added?

**Decision:** **No.** D-021's reasoning still holds (a new dependency, fragile on
headless Linux). Instead, **a single concept of secrets** is defined:
`<data_dir>/secrets.json`, `0600` on Unix, **namespaced** —
`{"plugin:soundcloud": {"client_id": "…"}}`.

**Reasoning:** Plugins need secrets too (the SoundCloud `client_id`), and the
`servers.json` pattern was about to be repeated by hand a second time. A second
copy is the copy that forgets the first copy's permission tightening.

**Consequence:**
- `servers.json` **stays where it is**: what's in it is a server record (address
  + kind + user), and the secret is a field of that record. Migrating would mean
  disturbing Phase 1 from its working state; there's no gain.
- In the handshake the core passes the plugin **only its own namespace**. A
  plugin doesn't ask for, and can't see, another plugin's secret.
- Secret values **don't go into** the log or `headshell diag`; the key name and
  "present/absent" are written instead. The diagnostics report is text that gets
  copied and pasted (K9) — it can't carry a token.
- The `keyring` door didn't close: since **reading** a secret goes through a
  single place, putting a keyring behind it later is a one-file job.

---

## D-043 — The SoundCloud plugin: the client_id from three sources, live tests in the default run
**Date:** 2026-09-01
**Question:** Where will §2.2's reference plugin get its `client_id`, and how will a
network-bound plugin be tested under the rule "don't write network-bound tests"?

**Decision (Q1 — the client_id):** **Three sources, in this order.** The user's
secret (`plugin:soundcloud` / `client_id`) → the cache on disk
(`<data_dir>/plugins/soundcloud/state/client_id.txt`) → **discovery** from
SoundCloud's web client. `health()` reports which one was used.

**Reasoning:** The advice was "only the user provides it" (scraping is fragile,
and the reference plugin's job is to prove the protocol, not to offer a
catalog). The user chose all three: zero setup friction, but without working
around the key of a user who gives their own. That's why the order is
deliberate — if there's a secret, discovery is never attempted.

**Consequence:**
- On a 401/403: if the source is *discovery/the cache*, the key is refreshed
  once and the call retried; if the source is *the secret*, it **isn't
  refreshed** — the user is told that their own key was rejected. Silently
  changing what the user gave would send them looking for the error in the wrong
  place.
- Discovery isn't a documented endpoint and can break without warning. What
  happens when it breaks **is written down**: an explicit error + "give your own
  client_id".
- Discovery is done **on the first real call**, not in the handshake: the
  handshake's timeout is 5 s, and the protocol says "don't go to the network".

**Decision (Q2 — the test path):** **The live tests are in the default run.**
`crates/headshell-core/tests/plugin_soundcloud.rs` connects to the real
SoundCloud and runs with `cargo test --workspace`.

**Reasoning:** My advice was "a fake server + the live test behind `--ignored`";
the reasoning was that a test turning red should mean *our* code broke. The user
saw this objection and chose the opposite: learn *that day* on the day the plugin
breaks. The decision is the user's, and its price is accepted — when SoundCloud
goes down, the package turns red.

**The rule written on top of this — the "don't write network-bound tests"
sentence in `CLAUDE.md` was updated.** Its new form: *network-bound tests may be
written; being unable to reach the service isn't a failure.* The distinction is
K9's own:
- **Can't reach it** (no DNS/TCP) → the test skips itself and writes the reason to
  `stderr`. The same procedure as the audio device tests.
- **Reaches it and gets the unexpected** → the test fails. "I couldn't reach it"
  and "it said no" are different diagnoses with different fixes.

**The scope answered itself:** api 1's methods are limited to `search` +
`resolve_source`; there's no wire format for `browse`. The plugin declares
`SEARCH | STREAM`.

**Measurement (2026-09-01, a sample of 200 tracks):** **99%** of the tracks have
a `progressive` (plain HTTP MP3) variant; **1%** offer only HLS. No HLS decoder
was written; an explicit error is returned for that 1%. The 4 tracks with
`policy: SNIP` are 30 s previews — `[preview]` is appended to the title, because
api 1 has no field to carry it and the user shouldn't be surprised while playing.

---

## D-044 — A freshly submitted job counts as "not finished" immediately (a flaw the live test found)
**Date:** 2026-09-01
**Question:** Not a decision, but the flaw D-043's live run exposed, and its fix.
It's recorded because the same family repeated a second time (D-035).

**The flaw:** `headshell play` queued the SoundCloud track, then said
`listens recorded: 0` and quit **instantly**. No error, no warning, a clean
`headshell diag`. The audio never played.

**The cause:** `AudioEngine::open()` starts the cpal stream right away; the
callback sees an empty buffer + `idle` and sets `Stopped`. Then `play_source`
opens the source (`open_source`), queues the job and sets `idle = false` — but it
**left correcting the state to the first callback.** In the window between, the
state is `Stopped`, and `Session::play`'s loop looks at exactly that: "it
finished before it started".

**Why the window showed now:** for a local file `open_source` takes a few
milliseconds; for an HTTP stream, **seconds** (it downloads 4 MB). The flaw had
been there since Phase 1 and showed only with a remote source.

**The fix:** `play_prepared` sets the state to `Buffering` **immediately** as it
queues the job. It was the thing already done for `idle` (there was even a
comment about it in the code), not done for `state`. `Buffering` is "the pipeline
waiting" (D-016), and that's exactly what needs to be said.

**Regression:** `a_freshly_queued_track_is_never_reported_as_stopped` — it opens
the engine, **waits** for the callback to set `Stopped` (asserting the
precondition), then submits a job and reads the state **without sleeping**.

**The lesson — for the third time:** in D-035 the GUI froze (a missing state
event), in D-022 "could not be reached" and "could not verify" were merged, and
here the CLI quit silently. All three are *missing* signals; the excess gets
measured, the missing leaves no trace. And all three were found not by a fake
server but by **a real run**.

---

## D-045 — The §2.3-2.5 round: scope, order and the Chromaprint path
**Date:** 2026-09-01
**Question:** D-041 postponed §2.3/§2.4/§2.5 and said "a separate round". That
round opens now: what will it cover, in what order, and where will the
fingerprint come from?

**The fact measured before the round — the middle of the chain is dead.**
`session.rs`'s `default_lookup()` always returns `OfflineLookup`;
`impl MetadataLookup` exists only for `OfflineLookup` and `StaticLookup`. So
every record imported today gets its identity either from an ISRC (if the export
has one) or from `LocalKey`: **the 2nd and 3rd links of the K6 chain don't work
at all**, and `authoritative_ratio()` is a fixed number, not a measured one. This
finding changed what §2.3 is — PLAN §2.3 only said "AcoustID".

**Decision (Q1 — §2.3's scope): MusicBrainz first, then AcoustID.** A real
`MetadataLookup` (MusicBrainz; an ISRC query + a recording search, honouring the
rate limit, behind the `http-client` feature) is written, and the accuracy rate on
`fixtures/identity/cases.json` is **really measured for the first time**. Then
AcoustID is added as the chain's 4th link.

**Reasoning:** AcoustID only works when an audio file is at hand. Imported history
has no files — for the overwhelming majority of those records the only authority
is MusicBrainz. The order of impact is the reverse of what the PLAN wrote; that's
why the order changed, not the scope.

**Decision (Q2 — Chromaprint): the `rusty-chromaprint` crate.** The advice was an
`fpcalc` subprocess (it doesn't add a single line to the dependency tree, and it's
the boundary K5 already uses). The user chose pure Rust: the PCM already comes
from symphonia, and nothing is asked of the user to install.
**The accepted price:** a new dependency (FFT included) and the fingerprint being
tied to the `audio` feature — with `audio` off the 4th link drops out, and because
of K9 that's reported not silently but as "no fingerprinting in this build".

**Decision (Q3 — the round's width): all three in this round.** §2.3 + §2.4
(torrent) + §2.5 (streaming platforms). The advice was "only §2.3" (three separate
dependency decisions and three separate live test surfaces open at the same
time); the user chose to close Phase 2 entirely.

**Order:** §2.3 → §2.4 → §2.5. Each goes through its own gates (`test`, `clippy`,
`fmt`) and gets its own commit; at the end of the round the §2.6 table is updated.

**Two sub-decisions still open in this round, to be asked when their turn comes:**
1. **Where §2.4 torrent lives** — the PLAN implies the core by saying
   "`librqbit`", but K5 says "providers are subprocess plugins". The
   contradiction will be decided before code is written; the tree size will be
   measured and presented at that point.
2. **Which platform(s) for §2.5** — the APPENDIX table has three candidates
   given `STREAM` (SoundCloud was written, Qobuz needs a subscription, YouTube
   Music is the most expensive to maintain). How many, and which, will be asked
   when §2.4 is done.

### D-045 addendum — §2.3's MusicBrainz half: three flaws the live run found

**Date:** 2026-09-01. `crates/headshell-core/src/identity/musicbrainz.rs` +
`tests/identity_musicbrainz.rs`. The chain's 2nd and 3rd links now work;
`headshell --online resolve "..."` connects to the real MusicBrainz.

All three came up **on a real response**; a synthetic catalog couldn't have shown
any of them. This is the fourth repetition of D-044's lesson.

**1. A live recording is marked not in the title but in the note.**
`variant_markers` looked only at the title. In the real catalog, a
`Radiohead — Creep` search returns 191 recordings, most of the first page is live,
and **none of them says "live" in the title** — they're all plain `Creep`, and the
distinction is in the `disambiguation` field. The measured result: the 1994
Astoria recording, since its duration is 12 s close to the studio one, was a
**"perfect hit" with 1.00 confidence**. The fix: a `Candidate.disambiguation`
field, and a `context_b` parameter for `fuzzy::similarity`.
- The second question this opened: should two *different* live recordings merge?
  No (D-010). `Creep (Live at Glastonbury)` and `live, 1994-05-27: Astoria` carry
  the same marker but aren't the same performance. The rule: if a distinguishing
  word the user gave (`glastonbury`) finds no counterpart in the candidate's text,
  penalise. One way only — a detail the candidate knows in addition isn't a
  contradiction.

**2. The same query gave two different MBIDs in two runs.** In the real catalog a
tie isn't the exception but the rule, and `max_by` surrendered to the order the
server sent on a tie; that order isn't fixed. In the identity layer, this meant
the same track getting a different canonical ID tomorrow. The fix: a deterministic
ordering — the score, then `tiebreak_rank` (a recording without a note is the
default; one with a known duration is preferred), and as a last resort the MBID
order.

**3. A tie was reported as "100% confidence".** While a deterministic but
**arbitrary** choice was being made among 25 equivalent candidates, the output
claimed a perfect hit. The fix: `Resolution.tied_candidates`; on a tie the method
can't be `Mbid`, and the confidence is clipped below the perfect-hit threshold.
The CLI says this on a separate line, and `diag` counts it as
`identity.tied_candidates`.

**Public signatures that changed** (§0.1's "an API signature will change" trigger;
all three within Phase 2, the `uniffi` line not yet set up):
- `Candidate` → a `disambiguation: Option<String>` field,
- `Resolution` → a `tied_candidates: usize` field (`serde(default)`; old records
  count as singular),
- `fuzzy::similarity` → a 7th parameter, `context_b: Option<&str>`.

**The accuracy set: 97.2% → 100% (72/72).** The set grew from 69 to 72 cases,
the catalog from 20 to 22 entries; the new class `mb_disambiguation` imitates the
real MusicBrainz form (a plain title, the distinction in the note).
`ACCURACY_FLOOR` was left at 0.97: pinning it at 100% would break the test with
every new hard case. **It doesn't mean the set got easier — it's a debt: the set
needs to be made harder.**

**The CLI wiring:** a global `--online` flag, **off by default**. The choice is in
the core (`session::LookupMode` + `lookup_for`); the CLI only turns the flag into
a mode (the Golden Rule). The default being off is deliberate: importing an export
shouldn't silently connect anyone to the network, and since MusicBrainz accepts
one request per second, an `import --online` of thousands of tracks would take
hours.

**The live tests are in the default run** (the same as D-043's decision):
`tests/identity_musicbrainz.rs`, 6 tests; without a network they write the reason
and skip. In a build with `http-client` off they skip too, and **say so**.

---

## D-046 — §2.3's AcoustID half: where the key comes from, where the link ties into the chain
**Date:** 2026-09-02
**Question:** D-045 chose the fingerprint path (`rusty-chromaprint`) but left two
things open: where will the client key AcoustID wants come from, and where will
the chain's 4th link be tied in when a `TrackRef` has no file?

**Decision (Q1 — the key): an embedded default + a user override.** The order:
the secret store first (`identity:acoustid` / `api_key`, D-042's infrastructure),
otherwise the key embedded in the build. What the user sets **always** wins. The
rejected option was "the secret store only": a 4th link that doesn't work out of
the box is in practice a link that never works.
**The price paid:** an embedded key is visible in the repository, and if it's
abused AcoustID can revoke it. That's why the user override was written in the
same round — if the key falls, nobody is locked out.
**Today's state:** `EMBEDDED_API_KEY` is **empty**, and empty on purpose. Putting
a made-up string there would come back as "invalid key" on the first live call
and make you look for the flaw in the fingerprint rather than the key. Until a
key is obtained in the project's name from `acoustid.org/new-application` and
written there, the link works only with the user's own key, and a call without a
key is rejected **saying what to do**.

**Decision (Q2 — the connection): a separate entry point,
`Resolver::resolve_file(path)`.** No `source_path` field was added to `TrackRef`.
The reason: that field would change the signature of a public type (a §0.1
trigger), touch every place that produces a `TrackRef`, and make import records
**that have no file** carry an empty field forever. `resolve()` and `TrackRef`
didn't change at all.
**The price paid:** two entry points; the caller has to know which to use.

**The order is kept (K6).** `resolve_file` first tries the three text links from
the file's **own tags**; it asks the audio only if the result falls to
`LocalKey`. The fingerprint is the most expensive link (the whole file is
decoded) and unnecessary when the first three work —
`a_tagged_file_never_reaches_the_fingerprint_link` measures this.

**Two failures are kept apart (K9).** **Failing to produce** a fingerprint (the
file is too short, the packets are broken) isn't an error: the reason is logged
and the chain ends with the local key the text side found. **Failing to ask**
AcoustID, on the other hand, is propagated — reporting an install whose key isn't
set up as "nothing matches" would make you look for the flaw in the file.

**Public signatures that changed:**
- `identity::FingerprintCandidate` (new), `identity::FingerprintLookup` (a new trait),
- `Resolver::with_fingerprint_lookup`, `Resolver::resolve_file` (new),
- `Session::resolve_file`, `Session::fingerprint_lookup_for` (new),
- `net::HttpRequest::post_form` (new) — a fingerprint takes thousands of
  characters in base64 and doesn't fit in a URL; a truncated URL would look like
  "no match".
- `net::RateLimiter` moved from musicbrainz to `net` (AcoustID has a quota too).

**CLI:** `headshell resolve --file <path>`. With `--online` off the chain ends
with three links, and that isn't a flaw but a configuration.

**Feature:** `fingerprint` (defined in D-045) is now **on** in `headshell-cli`.
The CLI is the surface every core capability is tested through; growing its tree
is a deliberate price.

### D-046 addendum — two things the live run found

**1. My own flaw: an invalid key comes with `400`, not `200`.** The first version
read the body **after** the status code, so the real service's rejection was
reported as `ADIM: NETWORK_REQUEST` — a diagnosis that sends the user to check
their network, when what they need to do is fix their key. The same distinction
D-023 set up for `401`/`403`. The fix: the body is parsed first; if it's
AcoustID's own answer, `IDENTITY_RESOLVE`, otherwise (a proxy page, a maintenance
screen) it's handed over to the status code. **The fake client couldn't have shown
this** — my test assumed `200` and was green. The fifth repetition of D-044's
lesson.

**2. Not my flaw, but the flaw D-045 said was "closed" is open: MusicBrainz search
is unstable between runs.**
`the_same_query_always_yields_the_same_canonical_id` failed again in a live run
and showed two different MBIDs. D-045 made the choice deterministic **within the
set**; what was measured is that the set itself isn't fixed. The
`mb_stability_probe` probe: the same `Radiohead — Creep` search, twice in a row,
returns 25 candidates, and in some runs **the number of shared candidates is
zero**. MusicBrainz serves search from several index replicas; within a replica
the order is fixed, across replicas the top 25 are completely different. So a
deterministic ordering **can't solve** this problem: the set to be ordered is
different each time.
**Consequence:** an ambiguous query with no duration and no ISRC got a canonical
ID that depended on which replica answered. Unacceptable for the identity layer.

**Decision (Q3 — no authority is claimed under ambiguity).** Without
distinguishing evidence no MBID is returned; the chain ends with the local key.
Two rejected options: *paging* (fetching every match to fix the set — every
ambiguous query would take ~9 seconds, and bulk resolution would stop being
practical) and *work-level identity* (conceptually the most correct, but it
changes K6's definition "canonical = recording MBID"; postponed).
**The measured result:** `Radiohead — Creep` (without a duration) is now
`local:b51521e93103eefa` in every run — the same even in a run where the candidate
sets **don't intersect at all**. `tied_candidates` (2 or 7) keeps on record how
weak the evidence is: "no candidates" and "the candidates couldn't be told apart"
produce the same identity but aren't the same diagnosis.

### D-046 addendum (2) — the second flaw the rule exposed: the duration evidence was thrown away

After Q3 was applied, `Şebnem Ferah — Sil Baştan` lost its authority **even though
a duration was given**. The probe showed why: three candidates — durations 309,
313 and 315 s, the query 309 s — **all three got exactly 1.0000.**

The cause is in `fuzzy::similarity`: the base score is already 1.0 when the text
matches exactly, the duration bonus gets swallowed in the `clamp`, and since the
duration difference is read in three bands (≤3 s / 3–15 s / ≥15 s), 0 s and 4 s
inside a band can't be told apart. So although the duration was really known, it
**wasn't being used** in choosing the identity.

**The fix isn't in the scoring but in tie-breaking.** The score formula wasn't
touched (the accuracy set is tuned to it); the duration difference was added to
the ordering as the third criterion, after `tiebreak_rank` and before the MBID
order, and `count_tied` now counts those that share the score, the rank **and**
the duration difference. An unknown duration is `u64::MAX`: "I can't claim
closeness", it falls to the end.

**The measured effect — this is a fix, not a trade-off:**
- The accuracy set is **72/72 = 100%** (unchanged).
- `Şebnem Ferah — Sil Baştan` (with a duration) now gives
  `e0a22727-1fcf-4e3a-81a3-b65623b2c53e` with the `mbid` method — **the very
  recording the ISRC link gives for the same query.** Before the fix `1eaab31f…`
  was chosen, so the chain was picking *the wrong recording*, and nobody had
  measured that.
- `Radiohead — Creep` (with a duration, 238 s) resolves with a single winner
  (`tied 1`).

**The remaining risk — and it happened.** Whether a query with a duration reaches
a single winner depends on the winning candidate being in that replica's top 25.
`Radiohead — Creep` (238 s) resolves with `tied 1` in most runs, but in some runs
the 238 s recording isn't in the set and the chain falls back to the local key
again. So the duration evidence **doesn't guarantee stability; it only provides it
most of the time.**

This broke the live test and showed that the test was measuring the wrong thing:
`a_live_take_does_not_win_over_the_studio_take` said "a candidate must always be
returned". That isn't the invariant — the invariant is **that the live recording
can't win.** No winner coming out doesn't break that invariant, and it's the right
behaviour under D-046's rule. The test now recognises both acceptable outcomes and
writes which one happened; the only thing it rejects is a live recording being
chosen.

**Not closed in this round:** fixing the set itself (paging) or moving the
identity to the work level. Both would let ambiguous queries get authority; each is
a separate decision. *(Paging: decided in D-081 — not every match, but the first
hundred in one response, read only while the outcome can still change; a tier that
does not fit claims nothing, which extends Q3 rather than reversing it.)*

### D-046 addendum (3) — a run with a real key: the match path had never worked

**Date:** 2026-09-02. The user gave an AcoustID key, and the link ran **with a real
key** for the first time. Three things were measured; the third was a flaw.

**1. The fingerprint we produce is valid.** AcoustID accepted the fingerprint of
the 15 s synthetic fixture and returned 0 candidates — the right result for a
synthetic sound. The compression, the URL-safe base64 and the form body are right.

**2. "It accepted it" isn't an empty claim — it was checked.** If the service
accepted every string, the test in item 1 would measure nothing, and it would stay
green even if our compressor broke. A deliberately broken string was sent:
`code 3, invalid fingerprint`. The check is now a permanent test.

**3. The flaw: the `duration` field comes as a decimal; it had been written as
`Option<u32>`.** The real response sends `"duration": 309.0`. `serde_json` can't
decode a decimal value into a `u32` — so **the first real match would have fallen
over with a JSON error before the match could be parsed.** The chain's 4th link
looked like it "worked" and had never worked: the path where a match *isn't found*
had been tested, not the path where one *is*.

What hid the flaw was a hand-written fixture: I had put `"duration": 238` (an
integer), because I assumed the schema without measuring it. **This is the sixth
repetition of D-044's lesson**, and the first time the fake data *itself* was the
source of the flaw — not a fake server, a fake **body**.

**Fixes:**
- The field is `Option<f64>`, converted through `duration_secs_to_ms`. A
  meaningless value (negative, `NaN`, infinite, longer than 24 hours) is `None` —
  since the duration is now a tie-breaker (see addendum 2), a made-up duration
  would tie the identity to the wrong recording.
- The fixture is no longer written by hand: `fixtures/identity/acoustid_lookup.json`
  was taken from the live service, and the unit tests read it with `include_str!`.
- **A frozen fixture turning into a lie is a separate class of flaw, and that was
  closed too:** `the_committed_fixture_still_matches_what_the_service_sends`
  fetches the live response and compares it with the fixture *field by field, by
  type*. The values can change (the catalog lives); the shape can't. The detection
  itself was verified too — when the fixture's duration is turned into an integer,
  the test fails naming exactly that field.

**About the key:** the user's key sits in `.env` (gitignored) and was used in the
tests as `HEADSHELL_ACOUSTID_KEY`. `EMBEDDED_API_KEY` is **still empty** —
embedding that key in the source would make it public, and for a personal key kept
in the secrets file that's a decision the user makes separately. It wasn't done
without asking.

---

## D-047 — §2.4 torrent: a subprocess plugin, a localhost stream, Torznab search
**Date:** 2026-09-02
**Question:** PLAN §2.4 says "`librqbit`" and reads as if it puts it inside the
core; K5, on the other hand, says "providers are subprocess plugins". The
contradiction was to be closed before any code was written (one of the two
sub-decisions D-045 left open).

**The tree cost, measured before the decision** (`cargo tree -e normal`, unique
crates):

| | crates |
|---|---|
| `headshell-core` today (`fingerprint` on) | 77 |
| `librqbit` 9.0.1 alone (`--no-default-features`) | 223 |
| **new** ones coming if added to the core | **+179** (only 35 shared) |

So the core goes 77 → 256, **3.3 times**. And this tree goes to mobile too through
`uniffi` — directly the subject of K7 and of the rule "the tree must stay small
(mobile binary size)".

**Decision (Q1 — where it lives): a separate workspace crate, a subprocess
plugin.** `crates/headshell-plugin-torrent`, an independent Rust binary using
`librqbit`, talking to the core over §2.1's JSON-RPC protocol. `headshell-core`'s
tree stays at 77. The contradiction closed in K5's favour; PLAN §2.4's advice of
"`librqbit`" stands, its **place** changed.

To be honest, it isn't free: since the workspace shares a single `Cargo.lock`,
those 179 crates go into the lock, and `cargo test --workspace` builds them. What
doesn't change is `headshell-core`'s **own** dependency tree — which is also what
the mobile binding will carry. The measurement can always be verified with
`cargo tree -p headshell-core`.

**Decision (Q2 — how the audio is delivered): a sequential HTTP stream on
127.0.0.1.** The plugin serves `librqbit`'s `ManagedTorrent::stream(file_id)`
stream (`AsyncRead + AsyncSeek`, which sets the piece priority according to the
read position) from a small HTTP/1.1 server bound only to the local interface,
and `resolve_source` returns `HttpStream`. **Not a single line of the protocol
changed.**

The alternative was "download it fully, then return `LocalFile`": simple, but
`headshell play` would block for minutes, or a progress notification would have
to be added to the protocol.

It doesn't break K3: nothing is relayed; the stream is read on the user's own
machine from data they fetched themselves. The server binds to `127.0.0.1` and
carries in its path a random token that lives as long as the process — so another
process on the same machine can't find the addresses by trying them.

**Decision (Q3 — scope): playing + search.** The advice was "playing only"
(search means a separate scraper for every indexer, more expensive to maintain
than the SoundCloud plugin). The user wanted search too.

**Decision (Q3b — where search comes from): Torznab (Prowlarr/Jackett).** The way
that removes the objection itself: one standard XML API, one parser. The user
chooses which indexers are queried in their own Prowlarr/Jackett; when a site
breaks, **not our code** but their indexer definition gets updated. No
site-specific scraper sits in the repository.

The price is that the user does some setup, and under K9 that price isn't hidden:
if Torznab isn't configured, `search` returns not a silent empty set but an
explicit error saying "not configured — `headshell secret set plugin:torrent
torznab_url ...`". "I couldn't find it" and "I didn't look" are different
diagnoses.

**Torznab returns a *release*, not a track** — and that doesn't fit the wire
format's `WireTrack` directly. It was solved without growing api 1, in two steps:
1. `search "<query>"` → releases; each one's `id` is the infohash.
2. `search "<infohash>"` → the audio files inside that torrent; the `id`s are
   `<infohash>/<file index>`.

In a release with a single audio file, `resolve_source("<infohash>")` plays
directly. If there are several files it **doesn't guess**: it returns an error
saying which files there are and to search for the infohash (K9 — "I don't know
which one" is better than silently picking the first file).

**New dependencies:** `roxmltree` (parsing the Torznab RSS) and `reqwest` — both
are already in the workspace lock (`roxmltree` from resvg, `reqwest` from librqbit
and Tauri), so they add no new name to the lock. Neither enters `headshell-core`.

### D-047 addendum — three things the build found

**Date:** 2026-09-02.

**1. The order in name parsing was wrong (three flaws, one cause).** While
extracting the artist/title from a release name, I split off the year first, then
the noise, and the artist last. The result: in `Van Halen — 1984`, taking the year
emptied the title and the split failed, **losing the artist too**; in scene names
(`Portishead.Dummy.1994.FLAC`) the year was never found because it sat behind the
trailing `FLAC`; and Sigur Rós's album `( )` was counted as noise and deleted for
being "empty parentheses". The right order: **the artist first, then the noise,
the year last.** The claim "it's all noise" now requires at least one word.

**2. A flaw: the budget didn't cover `add_torrent` — and it was in production
too.** I had put the timeout only around `wait_until_initialized`. But for a
magnet, what resolves the metadata is `add_torrent` itself: if no peers are found,
it waits there **forever**. On a cold magnet the plugin fell into the core's 20 s
call timeout, and the user saw "the plugin hung" and never learned why — exactly
what K9 forbids. The budget now wraps both, and when it runs out it returns an
error explaining the peer/tracker state.

**Only a real run showed this.** The unit tests were green; the flaw came out only
when a real HTTP request went to the stream server, and since that request had no
record in the catalog, it produced a bare magnet. A fake session would have said
"it returned right away". The seventh repetition of D-044's lesson. Regression
test: `a_source_with_no_peers_gives_up_within_the_budget_and_says_why`.

**3. The end-to-end test uses no peers — and that's deliberate.** The test
produces a torrent and puts its data in the download directory; `librqbit`
verifies the hash and counts it as complete. That way what's tested is our code:
the file list from the metadata, opening the stream, answering `Range`, checking
the token. A test that depends on peers would sometimes pass depending on the
state of the network, and it would mix up the two failures D-043 wants to tell
apart. **What isn't tested is, explicitly: downloading from peers.** That's the
job of `librqbit`'s own test suite.

**A topic not closed — the permission vocabulary can't say "a random peer".**
`plugin.json` declares only two DHT entry points; a torrent client, however,
connects to trackers and peer addresses that can't be known in advance, and also
to the Torznab address the user gave. D-040's vocabulary ("a list of hosts, no
`*`") can't express that. I preferred leaving the declaration incomplete and
saying so in the `description` over pretending a restriction exists that doesn't.
Widening the vocabulary is a separate decision, and it wasn't asked.

**An unmeasured cost was measured: the disk.** D-047 said "the workspace shares a
single `Cargo.lock`, those 179 crates go into the lock and
`cargo test --workspace` builds them", but gave no number. Here's the number: on
this machine `target/` grew to 45 GB and **the disk filled up** — the run failed
not with a compile error but with `No space left on device` and a `Bus error` in
the linker. `target/debug/incremental` alone was 12 GB (pure cache; deleting it
loses no output). After it was deleted, the round passed clean.

This isn't a flaw but a measured cost: the torrent plugin doesn't grow
`headshell-core`'s tree (it stayed at 77, verified with
`cargo tree -p headshell-core`), but it grows **the workspace's build load**. A
developer machine may need `CARGO_INCREMENTAL=0` or a regular `cargo clean`; in
CI, this must be taken into account when setting aside disk for a single
`--workspace` run.

## D-048 — §2.5 YouTube Music: search from InnerTube, the stream from yt-dlp, format 140
**Date:** 2026-09-09
**Question:** The only section left in Phase 2 was §2.5, and the open
sub-decision was "which platform, how many". Of the three candidates given
`STREAM` in the APPENDIX table, SoundCloud was written in D-043; Qobuz and YouTube
Music were left.

**Decision (Q1 — the platform): YouTube Music, a single plugin.** Qobuz needs a
subscription; without one the plugin **can't be run live** — and the one shared
lesson of every round from D-044 to D-047 was "only a real run showed the flaw".
Writing a plugin that can't be tested would do the opposite of that lesson.
YouTube Music is the only candidate that doesn't need a subscription.

It was accepted from the start that this round isn't a **protocol** round: with
SoundCloud (Python) and torrent (Rust), the protocol had already been proven in
two languages and two different ways of delivering audio. This is a **product
value** round.

### Measured before writing

The PLAN's APPENDIX says "the API information here was not verified; before a
plugin is written, the relevant row is tested again". Four measurements were made,
and **three of them changed the path the PLAN implied.**

**Measurement 1 — yt-dlp's own search isn't enough.** The APPENDIX says "the way
is yt-dlp". But the results of
`yt-dlp --flat-playlist -J "music.youtube.com/search?q=..."` look like this:

| field | state |
|---|---|
| `title` | present |
| `id` | present |
| artist | **missing** |
| duration | **missing** |
| album | **missing** |

On top of that the list is dirty: among 20 results there are channel pages,
playlists, "10 hours ... for sleep with rain", "slowed + reverb" and a "30min
Loop". K6's 3rd link (fuzzy matching) wants **artist + title + duration**; a
provider returning `duration_ms: 0` can never enter that link.

**Measurement 2 — InnerTube's "Songs" filter gives exactly what's wanted.**
`POST music.youtube.com/youtubei/v1/search`, the `WEB_REMIX` client context,
`params: EgWKAQIIAWoKEAoQCRADEAQQBQ==` (the songs filter). **No key needed**
(HTTP 200, 676 KB). All 20 rows are songs, and every row has the artist, the
album, the duration (`4:11`) and the `videoId`.

**Decision (Q2 — where search comes from): InnerTube, not yt-dlp.** The division
of labour: InnerTube gives the metadata, yt-dlp resolves the audio. Each does
what it's strong at.

**Measurement 3 — `bestaudio` can't be played.** `headshell-core`'s symphonia
features: `mp3, flac, vorbis, isomp4, aac`. So **there's neither a webm container
nor an opus decoder.** yt-dlp's `bestaudio` selection gives format 251
(opus/webm, 136 kbps) — it downloads, it doesn't play. The available audio
formats:

| id | container | codec | kbps | can the core decode it |
|---|---|---|---|---|
| 139 | m4a | mp4a.40.5 | 49 | yes |
| **140** | **m4a** | **mp4a.40.2 (AAC-LC)** | **130** | **yes** |
| 249/250/251 | webm | opus | 52/69/136 | **no** |

**Decision (Q3 — the format): `140/bestaudio[ext=m4a]`, not `bestaudio`.**
There's a loss of quality (130 kbps AAC instead of 136 kbps opus), and the price
is paid knowingly: lower quality that plays is better than higher quality that
doesn't. The day opus/webm comes to symphonia, this line changes by one word.

**Measurement 4 — and this round's real finding: YouTube throttles a plain GET.**
The same `videoplayback` address, the same file (4,054,677 bytes), three forms of
request:

| request | code | speed |
|---|---|---|
| plain GET | 200 | **32 KB/s** |
| the `&range=0-` query parameter | 200 | 7.7 MB/s |
| the `Range: bytes=0-` header | 206 | **8.0 MB/s** |

**250 times.** A plain GET downloads a 130 kbps track at 2× real time —
technically it plays, but the smallest ripple on the line cuts the audio, and the
cause showed up nowhere.

**Decision (Q4 — how the throttling is lifted): the `Range: bytes=0-` header.**
The protocol already has a field that carries it (`source.headers`), and the
core's `UreqClient::open_stream` counts a 206 as 2xx and reads the
`Content-Length` that `Content-Range` gives — **not a single line changed.** The
query parameter form works too, at nearly the same speed; the header was chosen
because it's the standard mechanism and it doesn't tamper with the address.

This has nothing to do with circumventing a protection measure (D-026 / NEVER
DO): there's no encryption and no DRM; a standard HTTP header that the server
itself supports and answers with `206` is sent.

### Decision (Q5 — yt-dlp not as a library but as a subprocess)

`pip install yt-dlp` would be a repository dependency; instead, the plugin calls
`yt-dlp` **as a subprocess** and reads its `-J` output. Three reasons:

1. There's no Python dependency in the repository — the SoundCloud plugin's rule
   (the standard library only) is kept.
2. When it breaks, the message the user sees is **yt-dlp's own message** (K9).
   Not "something went wrong" but "Sign in to confirm you're not a bot".
3. The user updates yt-dlp with their own package manager. What YouTube breaks
   isn't repaired by us but by yt-dlp — and that's the very answer to the
   APPENDIX's "the highest maintenance cost" warning.

The order yt-dlp is looked for: `HEADSHELL_YTDLP` (a path) → `yt-dlp` on `PATH` →
`python3 -m yt_dlp`. If none exists, `health` says `reachable: false`, and
`search`/`resolve_source` return an error that writes how to install it — not a
silent empty result.

### The permission declaration is incomplete again, on purpose again

D-047's unclosed topic came up here again. The audio address sits on a host that
**changes with every resolution**, like `rr6---sn-u0g3jxaa-n5fz.googlevideo.com`;
D-040's vocabulary doesn't accept wildcards (no `*`). `music.youtube.com` and
`www.youtube.com` were declared, and `*.googlevideo.com` was explained in the
`description`. I again preferred leaving the declaration incomplete to pretending
a restriction exists that doesn't — but now **two** plugins do this, so widening
the vocabulary isn't a one-off trouble.

### D-048 addendum — two things the build found

**Date:** 2026-09-09.

**1. A flaw: the search results weren't in relevance order.** InnerTube's tree is
deep and changes without warning, so I collect the rows not by memorising the
path but by **searching for** the `musicResponsiveListItemRenderer` key. The walk
was written with a stack, and `stack.pop()` gave the children in reverse order —
so the document order was completely scrambled.

The visible result was this: for the query `nujabes aruarian dance`, **the track
searched for wasn't in the first five results at all**; instead came "Island",
"Luv (sic)", "Horizon" and two guitar covers. All valid tracks, all correctly
formatted — just the wrong five. A unit test couldn't have seen this, because
every field it would check was filled. The fix is one word: the children are
pushed onto the stack **in reverse**.

This directly concerns K6's 3rd link too: `resolve` scores the first candidates
coming from the provider, and if the right candidate never gets into the list, the
chain can never see it.

**2. The test was claiming the wrong thing — and the live run showed that too.**
To test the order, I first wrote "ask the same query with limits 3 and 12; the
short list must sit at the head of the long list, in the same order". It failed.
The cause wasn't on our side: **YouTube doesn't give the same order to the same
query in two calls.**

Measurement (the same query, five consecutive runs):

| position | stability |
|---|---|
| 1st result | **the same 5/5** |
| 2nd result | 4/5 |
| 3rd result | 4/5 |

The test was rewritten to claim the one thing that's stable: for an exactly
matching query, **the first result must be the track searched for**. That also
catches the flaw in item 1 (while it was broken, the first result was "Island"),
and it doesn't count as a guarantee something the service doesn't guarantee. The
same lesson D-045 learned with MusicBrainz: a test that demands stability from a
service unstable between runs tests the service, not its own code.

**Also fixed:** the assert that looked for the error message with
`err.to_string()` was looking in the wrong place. `Error`'s `Display` writes only
`ADIM: PROVIDER_CALL`; the cause chain is in `chain_text()`, and that's also what
the CLI and the GUI show the user. The test now looks there and verifies **two
things at once**: the message carries yt-dlp's own sentence **and** it says which
stage it was in.

**End-to-end proof:** `headshell play "hopeless_0taku_guitar Aruarian Dance Guitar
with Rain"` — the 69-second track played from start to finish in 88 seconds (the
~19 s between is yt-dlp's resolution and the first buffering), exit code 0, and
the listen record was written to the database. That was the proof §2.5 was
looking for.

## D-049 — The plugin dependency contract: no plugin asks for root
**Date:** 2026-09-09
**Question:** As §2.5 was finishing, the user asked: if a plugin leans on a tool
installed on the system, can that plugin count as "working"? "Every plugin has to
bring its dependencies on its own, or write a script that installs them without
taking root on the system. Otherwise won't there be tons of trouble on every
operating system?"

The question came out of D-048's yt-dlp, but **it isn't a single plugin's
problem.** What came out when it was measured is this: the three plugins have
three separate dependency contracts, and none of them is written down.

| plugin | the real contract today | what it asks of the user |
|---|---|---|
| `soundcloud` | `python3` on `PATH` | usually ready on Linux/macOS; **missing on Windows** |
| `ytmusic` | `python3` **+ yt-dlp** | a package manager → **root** |
| `torrent` | the `exec` target (`./headshell-plugin-torrent`) **isn't in the repository** | `cargo build --release`, that is, **a Rust toolchain** |

So the plugin with the heaviest dependency isn't the one written in this round:
to run a binary, torrent first makes you install a compiler.

**Decision: no plugin may ask the user for root privileges or a system-wide
install.** A plugin either **brings its dependencies itself**, or it offers **a
procedure that installs them without root**. "Install this with your package
manager" isn't an installation procedure; it opens a separate support surface for
every distribution and every operating system, and that surface is carried not by
the plugin author but by us.

**Where the rule must not slip.** D-048 had deliberately leaned on "the user
updates yt-dlp with their own package manager": YouTube breaks yt-dlp regularly
and yt-dlp does the repair. If this rule is read as "freeze a copy into the
repository", **we take over** that maintenance burden — which is exactly what the
APPENDIX's "the highest maintenance cost" warning describes. The rule's three
conditions must be met at once: **no root + every operating system + able to stay
current.**

**The good news, measured:** the diagnostics side is already up. A missing
dependency doesn't silently turn into "no results"; `headshell provider test` says
UNAVAILABLE in both cases and writes the reason (`PLUGIN_HANDSHAKE` + the operating
system's error for a missing `exec`, `health`'s sentence for a missing yt-dlp). K9
is met at the reporting level; what's broken is **installability**, not
visibility.

**The bad news, measured, and this round's own flaw:** `plugins/ytmusic/main.py`
says `pacman -S yt-dlp` when yt-dlp is missing. Outside Arch that's **wrong
advice** — an example, in our own code, of the "tons of trouble on every operating
system" the user described. The message was made independent of the operating
system (see the D-049 addendum).

### What the decision doesn't cover, to be asked

The rule says what, not **how**. All four are separate decisions, and none was
made in this round:

1. **Can a plugin download an executable at run time?** yt-dlp is distributed as a
   single-file zipapp (~3 MB, no `pip` needed); the plugin downloading it into its
   `data_dir`, which the protocol already lets it write to, and keeping itself
   current would meet all three of the rule's conditions. But that means "the
   plugin pulls a binary from the network and runs it", and **the permission
   vocabulary can't express that** — the third time D-040's gap is stepped on
   (a random peer in torrent, a changing `googlevideo.com` in ytmusic, a
   downloaded binary here).
2. **If there's no download: who writes the rootless install script, and in what
   language?** The script can't be Python — Python itself is one of the
   dependencies.
3. **How will the torrent plugin be distributed?** A prebuilt release output per
   platform, or will it stay "build from source"? Its state today is what breaks
   the rule most heavily.
4. **Will `python3` keep being assumed?** Defensible on Linux and macOS, not on
   Windows.

**There's also an addition to ask about:** a machine-readable `requires` field in
the manifest. Today a missing dependency is only discovered after the process is
started (`health`) or when it can't be started (`PLUGIN_HANDSHAKE`); a declared
list of requirements would let `headshell plugin list` say "missing: yt-dlp"
without even opening a process. It doesn't break the api — by D-039/§2.1's rule,
**adding doesn't raise the version.**

## D-050 — The plugin engine: the runtime is the host's job, not the plugin's
**Date:** 2026-09-09

> **D-069 (2026-09-24):** Q1 (the runtime is Python) and the Python part of Q2
> are **void**. The principle "the runtime is the host's job" stands, but the
> runtime is now **inside** the host: an embedded QuickJS. Q4 (K5 stays as it is)
> is void too — K5 was rewritten. What follows is the reasoning of that day.
**Question:** D-049 set the rule ("a plugin can't ask for root"), but how it would
be applied was open. The user gave the direction: *"Let the program have its own
plugin engine, and let the scripts that run go through it. If you say Python, fine,
let it be Python; it's enough to say Python is required when building. Let each
plugin not bring its own separate packages or things that make you struggle...
don't let them have such long arms."*

This is the right place, and a measurement confirms it: **`python3` is today the
requirement of three of the four plugins, and it's written nowhere** — not in the
`README`, not in `CONTRIBUTING`, not in `Cargo.toml`. So the project already has a
run-time requirement in practice; what's missing is **owning** it.

**Decision (Q1 — the engine): `headshell` has a single plugin engine, the runtime
is Python, and it is declared once as `headshell`'s own requirement.** It's written
in the build/installation document. Plugins are scripts running on top of that
engine. A plugin has nothing to do with the questions "which Python", "is it
installed", "how is it installed" — those are the host's questions.

The interpreter **isn't embedded.** The user's sentence already says so ("it's
enough to say it's required"), and embedding would be expensive: the binary size,
and the trouble of carrying CPython on mobile. The system Python is enough, as
long as it **is declared and, if missing, said so explicitly.**

**Decision (Q2 — packages): the engine has its own separate environment.** The
knot was here: saying "there's Python" doesn't bring yt-dlp — yt-dlp isn't an
interpreter but a **package**. The solution isn't left to the plugin:

- The plugin **declares** what it wants in `plugin.json` (`requires`).
- **The engine does** the installing, into its own private environment in the
  data directory.
- The system isn't touched, root isn't asked for, and the user's Python install
  isn't polluted.
- The plugin itself installs nothing, downloads nothing, and doesn't call `pip`.
  That's exactly D-049's "don't let them have long arms" condition.

This meets all three of D-049's conditions at once: **no root** (the private
environment is in the user's data directory), **every operating system** (one
way, no distribution-specific command), **able to stay current** (the package is
updated by the engine; D-048's yt-dlp reasoning is kept — when YouTube breaks it,
the repair is still done by yt-dlp, not by us).

**Decision (Q3 — torrent): its function moves into the core; D-047's *place* is
reversed.** — **CANCELLED: see D-056 (2026-09-19).** This sub-decision was never
applied; torrent stays a plugin, and D-047 is in force. What follows is the
cancelled reasoning; Q1/Q2/Q4 weren't affected. The reason was consistency:
torrent is a Rust binary, not a script, and can't go through the engine — and its
state today is what breaks D-049 most heavily (it makes the user run
`cargo build --release`).

D-047's measurement still holds and isn't ignored: `librqbit` adds **+179 crates**
to `headshell-core`'s tree (77 → 256, 3.3 times), and that tree will go to mobile
through `uniffi`. That's why the move's **shape** is a feature gate:

```toml
torrent = ["dep:librqbit"]   # off by default
```

The repository has done this three times already (`audio` D-016, `http-client`
D-020, `fingerprint` D-045): putting a heavy capability behind an explicit gate.
The desktop build opens the gate and the user builds nothing; the mobile and
server builds don't open it, and **`cargo tree -p headshell-core` still says 77.**
An unconditional move was possible too and wasn't rejected — it would mean paying
a measured price for no reason.

It doesn't break K5: K5 requires *plugins* to be subprocesses, while torrent
stops being a plugin and becomes a built-in provider — like `local`, Subsonic and
Jellyfin (Phase 1).

**Decision (Q4 — K5 stands):** "plugins can be written in any language" doesn't
change. The subprocess + JSON-RPC boundary stays open to everyone; the engine
becomes **not the only way but the supported way that needs no installation**.
Every plugin distributed in the repository goes through the engine. The reasoning
in the APPENDIX's Tencent row ("someone in that region can write the plugin
themselves — we provide the protocol, not the list") thus stays unbroken.

### The work this decision changes

None of it was done in this round; all of it is §2.8's scope:

1. **The engine will be written:** finding Python, a version check, setting up the
   private environment, resolving `requires`, and on a gap a diagnosis that says
   **what is missing at which step** (K9). Today a missing dependency is only
   discovered after the process is opened.
2. **A `requires` field in `plugin.json`** — `api` doesn't break; adding doesn't
   raise the version (§2.1's rule).
3. **`ytmusic`** drops its own search for yt-dlp (the `HEADSHELL_YTDLP` → `PATH` →
   `python3 -m yt_dlp` trio is deleted), says `requires: ["yt-dlp"]` and uses what
   the engine gives.
4. **`soundcloud`** and `echo` move to the engine — both are stdlib, `requires` is
   empty.
5. ~~**`torrent`** stops being a plugin; `crates/headshell-plugin-torrent` goes into
   the core as a provider behind a feature, and `plugins/torrent/` goes away.~~
   **CANCELLED — D-056.** It wasn't done and won't be.
6. **`python3` is declared as a requirement in the `README`/`CONTRIBUTING`.**

### Not asked, left open

- **How is the private environment set up?** `venv` + `pip` lean on the system
  Python, but `pip` doesn't come with every distribution (on Debian `python3-venv`
  is a separate package). How the engine solves this, and what it says if `pip` is
  missing too, is a separate decision.
- **Package verification.** If the engine pulls packages from the network, version
  pinning, hash verification and how that shows in the permission vocabulary are
  open — the fourth time D-040's gap is stepped on.
- **Offline installation.** What does the engine do without a network; "not
  installed" and "could not install" are different diagnoses (K9).

---

## D-051 — Document ownership: every fact lives in one file
**Date:** 2026-09-09

> **D-074 (2026-09-26):** The rules are K1–K11 now. The language moved from the
> code conventions (CLAUDE.md) to the invariant rules (PLAN.md, K11); the rest of
> the table below stands.

**Question:** Measured at the start of the cleanup: the invariant rules were
written **three times**, in `CLAUDE.md`, `PLAN.md §2` and `CONTRIBUTING.md`; the
workspace tree, the code conventions and the commands twice each. The copies had
drifted, and the drift wasn't silent — they contradicted each other:

| contradiction | CLAUDE.md said | reality |
|---|---|---|
| K7 | "no trait objects" | D-006 loosened this: `Arc<dyn Trait>` and `async fn` are allowed |
| phase numbers | Phase 3 = rooms | PLAN: Phase 3 = GUI, Phase 4 = rooms, 5 = social, 6 = mobile |
| the current phase | "Currently Phase 0" | Phases 0–3 closed, §2.8 open |
| the workspace tree | a nonexistent `sync/` listed | `net/`, `sleeve/`, `session.rs`, `headshell-plugin-torrent`, `plugins/` missing entirely |
| the CLI surface | 8 commands | in reality more, including `sleeve`, `scan`, `server`, `library` |

The most dangerous was K7: **a void wording of a rule said to be inviolable** sat
in the file read in every session. The README's roadmap had also taken Phase 5 for
"Mobile" and skipped the Social Graph entirely.

**Decision: every fact has a single owner file. A file that isn't the owner
doesn't repeat that fact; it points to the owner.**

| fact | owner |
|---|---|
| the working protocol, the invariant rules (K1–K10), the phase plan, NEVER DO, the glossary | `PLAN.md` |
| the workspace tree, commands, the CLI test surface, code conventions, diagnostics practice, test layout | `CLAUDE.md` |
| the contributor process (the three gates, the accuracy set, the license) | `CONTRIBUTING.md` |
| the reasoning behind a decision | `DECISIONS.md` |

**Reasoning — why the rules aren't in CLAUDE.md:** the text of a rule carries
meaning together with its reasoning (the answer to "why was K7 loosened?" sits next
to the rule). Text with reasoning is long, long text doesn't fit into the file read
in every session, and when it's shortened to fit, it drifts. That was exactly what
drifted.

**Reasoning — why the tree and the commands aren't in PLAN.md:** `CLAUDE.md` is
read automatically in every session; `PLAN.md` isn't. Putting the operational
knowledge the agent needs every day into the file that isn't read would mean
having an 87 KB file opened in every session. Ownership was split not "by
importance" but **by frequency of use**.

**The only thing that stands in two files:** the full text of K1 (the Golden Rule)
and the K1–K10 heading index. An index is a list of headings; it has no body to
drift. K1, since it's the rule most often broken while writing code, stands inside
the summary, marked "full text in PLAN.md §2".

**Applied:** `CLAUDE.md` was rewritten (the rules → an index + an anchor, the tree
pulled back to reality, the CLI surface completed, the phase status removed
entirely); `PLAN.md §3` dropped the convention/tree/command copies and turned into
an anchor; `CONTRIBUTING.md`'s rule and convention copies turned into anchors; the
phase numbers in `PLAN.md` K10 and the `README.md` roadmap were corrected.

**The one antidote to drift:** the phase status now lives **only** in the `DONE` /
`TODO` marks in PLAN.md's phase headings. No other file writes a sentence about
"which phase we're in now".

---

## D-052 — K7's limit: "public" is what uniffi exports
**Date:** 2026-09-09
**Question:** Once D-051's document cleanup was done, K7 was checked in the code,
and three separate findings came out. All of them said "there's a
lifetime/generic", but the three weren't the same thing:

| finding | what | decision |
|---|---|---|
| `PlayOptions<'a>` (`session.rs`) | a lifetime in a public record | **a violation — fixed** |
| `impl Into<String>` / `impl AsRef<Path>` in 24 public constructors | ergonomic generics | **outside the rule — they stay** |
| `ProviderFuture<'a,T>`, `HttpFuture<'a>`, `LookupFuture<'a,T>` | a dyn-compatible async trait | **a known debt — tracked** |

**Decision 1 — "a public signature" = the surface `uniffi` will export.**
`Session` methods, the types that cross those signatures, and the traits modelled
as callback interfaces. If a type crosses this surface, it can't carry a lifetime.

**Reasoning:** `uniffi` only looks at the marked item. `PlayOptions` will be
exported as a record, and `uniffi` can't express `&'a str` in a record field —
that's a real obstacle. But `ProviderTrackId::new(id: impl Into<String>)` doesn't
have to be exported: in Phase 6 a `#[uniffi::constructor] fn create(id: String)`
is added next to it, and existing Rust callers don't break. A broad reading would
turn 24 signatures into `String` and add `.to_owned()` at every call site; no
gain, a loss of ergonomics.

**Decision 2 — `PlayOptions` carries an owned `String`.** `query: &'a str` →
`query: String`; `Copy` went away. The price is one short text copy per command.

**Decision 3 — the rule is now checked in the code.**
`crates/headshell-core/tests/k7_surface.rs`: if a public `struct`/`enum`/`type`
takes a lifetime, or a public signature takes a closure parameter, the test fails
and says which file:line with `ADIM: K7_SURFACE` (K9).

**This isn't real `uniffi` scaffolding generation**, and it doesn't claim to
replace it. The real check needs ~60-80 types in the core to be marked with
`#[derive(uniffi::Record)]`; that work belongs to Phase 6 (K10), and because of
the debt below it would turn red from day one. The cheap filter comes first.

**The known debt:** `uniffi` can't express `Pin<Box<dyn Future + Send + 'a>>` in
the return of a trait method. `Provider`, `HttpClient`, `MetadataLookup` and
`FingerprintLookup` are written like that today — not by accident, but because
it's the only macro-free way to a dyn-compatible async trait (needed because
D-006 allowed `Arc<dyn Trait>`). In Phase 6 all four will be rewritten for
`uniffi`'s own async machinery. The test keeps them in the `BOXED_FUTURE_ALIASES`
list; **the list is a record of debt, not an exemption** — adding a new name grows
the debt, and is asked about first.

**A side finding — the check itself was flawed.** In the first version, stripping
the test module cut everything after the first `#[cfg(test)]` wholesale; every
public type defined *after* the test module stayed outside the check. It came out
when an injected violation wasn't caught. It was changed to brace-counting block
skipping, and both violation classes were verified by injection: it's trusted not
because it's green, but because it can turn red.

## D-053 — CI: the repository had none; the three gates now run on a machine
**Date:** 2026-09-09
**Question:** In the Phase 6 section of PLAN.md, an unanswered DECISION POINT was
sitting: *"try generating the `uniffi` scaffolding in CI… when will it be added?
Advice: right away."* When asked, the answer was "now".

When measured, the real gap came out: **the repository had no CI at all.** The
three gates (`fmt`, `clippy`, `test`) were written in CONTRIBUTING.md and ran only
if run by hand.

**Decision:** `.github/workflows/ci.yml` — the three gates on push and PR. The K7
surface check (D-052) runs inside the third gate.

**System dependencies:** `cpal` links against ALSA, the Tauri shell against
WebKitGTK; without `libasound2-dev` + `libwebkit2gtk-4.1-dev` and friends,
`--workspace` doesn't build.

**Network tests (D-043) run in CI.** Being unable to reach something isn't a
failure, so no separate "skip" switch was added: in CI only SoundCloud and
MusicBrainz really run; AcoustID skips itself for lacking a key, `ytmusic` for
lacking yt-dlp, `torznab` for lacking configuration. A network test turning red
means not "I couldn't reach it" but "I reached it and got the unexpected" (K9) —
and that's something that should be known anyway.

**The second job is `core-alone`, and it's red today.** In the `--workspace` run,
`headshell-cli` and `headshell` turn on `headshell-core`'s `audio`/`http-client`
features and hide the dead code in the default build. Mobile (Phase 6) will build
the core without these features. There are three flaws today: `net::network_err`
is dead, `net::fake::last_request` is dead, and an unmet lint expectation in
`playback/player.rs:228`. That's why the job is a **report** with
`continue-on-error: true`, not a gate. When the three are fixed, that line must be
removed.

**Not verified:** CI has never run — this commit itself will be the first run. The
system dependency list and the WebKitGTK version (`4.1`) on `ubuntu-24.04` were
chosen only by reading, not on a local machine.

---

## D-054 — Debt cleanup: what the documents claimed and what the code does had diverged
**Date:** 2026-09-18 · **Status:** APPLIED (2026-09-18)

**Question:** "Clean up the features that don't exist, the wrong plans, the
features written wrong." This wasn't a decision question but a **measurement**
question: which claim doesn't hold?

**Method:** the three gates were run (all three were clean already), then every
claim in the documents was checked against the code one by one — the CLI
subcommands against `main.rs`, the output samples against `output.rs`, the TUI
mock-up against `tui.rs`'s drawing, the secret store against `secrets.rs`, the
phase statuses against the code. The code had not a single
`TODO`/`FIXME`/`unimplemented!`; all of the debt was in the documents, and part of
it was **wrong**, not missing.

### 1. CI's masked red closed — the job is now a gate

D-053's `core-alone` job stood as a report with `continue-on-error: true`. All
three flaws were fixed:

- `net::network_err` — only `ureq_client` (`http-client`) and the fake client in
  the tests call it. `#[cfg(any(feature = "http-client", test))]` was added; in the
  default build it's now **absent**, not silenced.
- `net::fake::last_request` — its only callers are the AcoustID tests behind
  `fingerprint`. `#[allow(dead_code)]` + a reason: it isn't dead but
  **conditional**. Writing the feature name here would tie a general test helper
  to a single feature.
- `playback/player.rs` — the `#[expect(unused_variables)]` was never met, because
  the `let _ = (source, item);` right below it already silences the lint. The
  expectation was removed; one of two belts came off.

While measuring, a **fourth** flaw came out: when `--features http-client` is
turned on alone (`audio` off), the `Resp::bytes` and `fixture` helpers of
`tests/remote_http.rs` die. Where the debt piled up wasn't the feature
*combination* but a feature turned on alone. That's why the CI job turned into
`core-features`: the default + every feature one by one + all at once.

### 2. The README's four wrong claims

- **"Stored on disk as an encrypted key/token" — was wrong.** Nothing is
  encrypted. For a remote server the password itself really isn't written to disk
  (for Subsonic a `salt` + `md5(password+salt)`, for Jellyfin an access key is
  derived), but the stored token stands in for the password to reach that server,
  and it sits as plain text in `servers.json`, `0600` on Unix. The plugin/AcoustID
  secrets are in a separate file (`secrets.json`), the same way. This isn't a flaw
  but D-042's deliberate decision; the flaw was **describing it as if it were
  encrypted.** A security promise being wrong is worse than describing a feature
  that doesn't exist: the user decides based on it when sharing their disk. The
  text was rewritten to say what it protects and what it doesn't.
- **The sample `headshell stats` output was made up** — emoji headings,
  percentages and `─────` bars. `output.rs` prints no such thing. The sample was
  regenerated exactly with the formatter's alignment and replaced.
- **The TUI mock-up didn't match the real drawing** — a duration and an album
  column per row, a `Queue (3/10)` counter, `[Repeat: ALL]` badges. The real queue
  row is only `artist - title`. The mock-up was redrawn following the `draw_*`
  functions.
- **`headshell provider scan` was described as "incremental".** Without a flag
  it's a full scan; what's incremental is `--if-stale`, and that looks at the
  directory stamp and doesn't see a file retagged in place. In CLAUDE.md the same
  command was written as `--incremental` — no such flag ever existed.

Also: the repository address was the placeholder `username/headshell`; the
`python3`/`yt-dlp` requirement was written in no user document (item 6 of PLAN
§2.8 admitted this itself); the command table was missing
`provider remove/servers`, `plugin disable/enable/forget`, `secret list/remove`.

### 3. There is no `headshell provider search` command

`plugins/torrent/README.md` described the two-step search flow with
`headshell provider search`. `ProviderCommand` = `list | test | scan | add |
remove | servers`; `search` was never written. The plugin's search works through
`headshell play` (`Session::queue_from_search`; if the catalog is empty, it asks
every provider that can stream).

**And a real CLI gap came out here:** `output::play` prints only
`artist - title`; it doesn't print the provider track ID. Torrent's release→file
choice (`<infohash>/<index>`) requires seeing the ID, so the documented flow
**can't be followed** from the human output; `--json | jq` is a must. It wasn't
fixed: changing the output breaks the snapshot tests, and it's a product decision.
It was written into PLAN §2.6 as an open debt, and the README shows the `--json`
route.

### 4. Stale plan items

- **§0.3's format example used the number `D-007`**, and the repository had a real
  D-007 on a completely different topic (the diag error chain). A reader looking up
  the example found the wrong decision. The example was changed to `D-NNN` and
  fitted to the field format DECISIONS.md really uses.
- **In the closed Phase 0, three decision points still said "Ask."** The answers
  to two were in the code: the schema was written and versioned with
  `user_version`; the accuracy floor is `ACCURACY_FLOOR = 0.97` (today's
  measurement 72/72 = 100%). The third — Last.fm/ListenBrainz importing — asked
  "in this phase or Phase 2", and **both phases closed; it was written in
  neither.** The question is stale: it's no longer "in which phase" but "will it be
  done". It was marked as ownerless.
- **In five places K4 had been written instead of K5.** "Subprocess + JSON-RPC" is
  K5; K4 is the Spotify rule. D-050's own text wrote it right (K5) and then wrong
  (K4), two paragraphs apart.

### 5. A wire value was taken for interface text (D-036)

`RepeatMode`'s `Display` prints `off`/`all`/`one` — right for JSON and IPC. But
both the TUI's queue heading and the desktop interface's button tooltip showed that
string **to the user**. D-036: identifiers English, interface text Turkish. Each
shell got its own label mapping (`kapalı/tümü/tek`; `off/all/one` again since
D-073); the wire value didn't change, and a test locks that.

**Consequence:** 433 tests pass, 7 skip themselves and write the reason. The three
gates + `core-features` are clean.

**Not done (deliberately):** `EMBEDDED_API_KEY` is still empty (D-046), the plugin
engine still isn't written (D-050), the permission vocabulary still doesn't accept
wildcards (D-040), the `play` human output still doesn't print the ID. All four
stand as open debts in PLAN §2.6 — this round was opened to **count** them, not to
close them.

---

## D-055 — The plugin engine: a pinned artifact, no `pip`, and four separate diagnoses
**Date:** 2026-09-19 · **Status:** APPLIED (2026-09-19)

> **D-069 (2026-09-24):** §2 (finding the interpreter, `HEADSHELL_PYTHON`) is
> gone. §1 (a pinned artifact + a hash) and §3 (the diagnoses) stand, but the
> artifact is now declared **per platform**, and there's a fifth diagnosis: "no
> release for this platform".

**Question:** D-050 decided on the engine but left open **how** the private
environment would be set up (PLAN §2.8's decision point): `venv` + `pip` lean on
the system Python and don't come with every distribution, and if packages are
pulled from the network, the questions of version pinning + hash verification +
the permission vocabulary were unanswered.

### 1. The environment: a pinned single-file artifact (Q1)

A measurement that sharpened the question: **on this machine there's no system
`pip`** (`python3 -m pip` → "No module named pip"), but `ensurepip` is there, so
`python3 -m venv` can set up a 13 MB environment in 1.4 s and put `pip 26.2.1` in
it itself. "No pip" and "I can't set up a venv" aren't the same thing.

But Debian packages `python3-venv` separately, and there both fall at once — no
rootless way out is left for the engine to offer the user. That's exactly the
place D-049 avoids.

**Decision: no `venv`/`pip`. The plugin declares a pinned artifact in its
manifest; the engine downloads it and verifies its sha256.**

```json
"requires": [{
  "name": "yt-dlp", "version": "2026.08.19",
  "url": "https://github.com/.../yt-dlp",
  "sha256": "1fa6733c…"
}]
```

All four fields are required, `url` has to be `https://`, and if the hash doesn't
match the file **isn't put in place**. Without pinning and verification, "an
install that asks for no root" would only be a trust problem that moved house.

We validate the declaration **on loading**, not on installation: with a `requires`
that has no version or no hash, the plugin isn't listed at all. Left to
installation time, the flaw would come out only when the user typed the command.

**The accepted price:** only things distributed as a single file can be
installed. A future plugin wanting `requests` can't use this way. The total need of
all today's plugins is one — yt-dlp, which is a zipapp anyway.

**A `kind` field was deliberately not put into the schema.** Adding a distinction
"so it can also express a pip kind later" would be writing a single-variant enum.
Adding a field doesn't break `api` (§2.1); it's added when the need arises.

### 2. The interpreter: found once, and the choice isn't silently changed

The plugin writes `"exec": ["python3", "./main.py"]`; the engine resolves the bare
`python3`/`python` name (with a 3.9+ condition). `HEADSHELL_PYTHON` → otherwise
`python3` → `python`.

**While measuring, a flaw came out and was fixed:** in the first version
`HEADSHELL_PYTHON` was only put at the head of the candidate list. When it was
pointed at the wrong thing, the engine silently fell back to `python3` and reported
"python3 (3.14.7)" — the user would think their own choice had been applied. Now an
explicit choice is evaluated **separately**, and its failure is final: there's no
falling back, and the reason is said.

### 3. Four separate diagnoses (K9)

An artifact "not being ready" isn't a single thing, and the four need four
different things:

| state | what it means | who fixes it |
|---|---|---|
| `not installed` | never installed | the user — `headshell plugin install <name>` |
| `hash mismatch` | on disk, but doesn't verify | the user — reinstall |
| `could not install` | the network couldn't be reached | nobody — try again tomorrow |
| **`ORPHANED`** | the source said 404/410 | **the plugin author** — the address is dead |

The orphan concept was the user's proposal ("when links go stale, let's leave them
orphaned, okay?"), and it sits in the right place: a pinned address will surely die
one day, and that day telling the user "check your network" would be wrong advice.

**Being orphaned isn't written to disk.** A GitHub outage returns 5xx, not 404, but
if it were written, a single bad moment would brand a plugin orphaned permanently.
The diagnosis is said when it's measured; it isn't remembered.

**If the source of an installed artifact dies, nothing happens:** the file is on
disk, its hash matches, it keeps working. Being orphaned is a problem only for a
user who hasn't installed it yet — so publishing a new version is the plugin
author's responsibility.

### 4. The permission vocabulary: a separate line, not mixed in (the fourth step on D-040)

**Decision: the engine's download doesn't go into the plugin's `permissions.net`
list.** On the consent screen it shows on a separate `engine` line — what will be
downloaded, from where and with which sha256.

Reasoning: the download is done by the engine, not the plugin. If it mixed into the
same list, the user would read "this plugin connects to github.com", and that would
be **wrong information**. D-040's wildcard gap didn't close in this round, but it
**didn't grow** either.

### 5. The trigger: a separate command

`headshell plugin install <name>`. Downloading during `approve` would make consent
expensive; downloading on first use would delay `headshell play` with an
unexpected download. A separate command is explicit, scriptable, and free to run a
second time (for an installed artifact the network isn't touched at all — measured:
1.27 s → 0.19 s).

The command **doesn't wait for** `--online`. That flag exists to prevent implicit
network access ("importing an export doesn't silently connect anyone to the
network"); here, downloading is the command itself, not its side effect.

### 6. A dependency: `sha2` was added (after asking)

The measured price is **+8 crates** (51 → 59 unique; `cfg-if` was already in the
tree), and this tree goes to mobile through `uniffi`. `0.10` was chosen, not
`0.11`: the Tauri shell already locks `0.10.9`, so there's no second copy in the
workspace.

It **wasn't put** behind a feature gate. A closed gate would mean being unable to
give the "hash mismatch" diagnosis, and that diagnosis going silent would destroy
the reason verification exists at all.

Writing SHA-256 by hand was on the table too (~90 lines, locked with the NIST
vectors, zero dependencies), and it wasn't rejected — the user chose the crate
whose maintenance is someone else's.

### The measured result

A real run: the engine downloaded yt-dlp 2026.08.19 (3,072,469 bytes), verified its
hash exactly against the published `SHA2-256SUMS`, gave it the execute bit, and
`ytmusic` played audio with it from the live YouTube Music. All four diagnoses were
provoked by hand and verified (404 → orphaned, 503 → unreachable, a broken file →
hash mismatch, a wrong sha → not put in place).

**450 tests pass**, 7 skip themselves and write the reason. The three gates and the
six combinations of `core-features` are clean.

### Not done (deliberately)

**`torrent` wasn't moved into the core** (D-050 Q3). Being a Rust binary, it can't
go through the engine, and it still makes the user run `cargo build --release` —
it's now the only thing breaking D-049. The move is a round of its own: `librqbit`'s
+179 crates, the `torrent = ["dep:librqbit"]` gate, tearing out
`crates/headshell-plugin-torrent`. Item 5 of PLAN §2.8 is open.

---

## D-056 — Torrent isn't moving into the core: D-050 Q3 cancelled
**Date:** 2026-09-19 · **Status:** APPLIED (2026-09-19)

**Question:** The user, right after D-055: *"let's drop the business of torrent
going into the core. In fact, if the torrent part isn't written much, let it stay
at `//TODO:AFTER FIRST RELEASE` for now."*

**Decision: D-050 Q3 is cancelled. Torrent stays a plugin; D-047 is in force.**

### The conditional part was measured, and it didn't hold

The user's second sentence depended on a condition — "if it isn't written much".
The measurement:

| | lines |
|---|---|
| `crates/headshell-plugin-torrent/src/` | 2,335 |
| its tests | 647 |
| number of tests | 56 |

It isn't unwritten: it's a working plugin that searches against a live Torznab in
D-047 §2.4, downloads sequentially with `librqbit` and streams over `127.0.0.1`. So
**there's nothing to delete**, and the condition isn't met. That's why the first
sentence (an unconditional cancellation) was applied, and the second was applied not
as deleting the code but as **postponing the remaining debt**.

### Why D-050 Q3 was taken, and why it's being reversed

The reason for taking it was consistency: torrent is a Rust binary, not a script,
and can't go through the plugin engine (D-055) — and its state today is what breaks
D-049 most heavily. As the solution, it was going to be moved into the core with a
`torrent = ["dep:librqbit"]` gate.

The reason for reversing it is **the price of what's being cancelled**: the
architecture D-047 measured had paid off (the `headshell-core` tree stayed at 77,
mobile clean), the plugin worked, and the move meant tearing out 3 thousand lines.
There's nothing to gain from tearing out a working architecture for the sake of
*consistency* before the first release.

**D-049's violation wasn't solved by this decision; it was postponed.** We don't
hide it: torrent still makes the user run `cargo build --release`, and after D-055
it's the **only** plugin breaking the rule.

### The open question is no longer architecture but distribution

With the move cancelled, D-049's real question remains: how does a Rust binary
reach the user without making them install a compiler? There are two ways, and
neither was chosen — a prebuilt release output per platform (it brings versions,
signing and a CI job), or staying with "build from source" (today's state).

**`TODO: AFTER FIRST RELEASE`.** The mark is in three places: the module header of
`plugin/lib.rs`, the top of `plugins/torrent/README.md`, PLAN §2.8 item 5.

This is the **first** `TODO` deliberately put into the repository. D-054 had
measured "the code had not a single `TODO`/`FIXME`/`unimplemented!`", and that
preference goes on: the mark points not to *a missing implementation* but to **a
postponed decision**, and next to it is written what was postponed and why.

---

## D-057 — The desktop shell: packaging turned on, the Phase 2 surface came to the interface
**Date:** 2026-09-19 · **Status:** APPLIED (2026-09-19)

**Question:** The user: *"could you give the desktop interface a hand? Let it be
ready to deploy, and a bit more user-friendly."* — Three things were unclear: the
scope (just polish, or the IPC surface too), a dependency for the file dialog, and
pinning down the packaging identity.

**Decision (all three are the user's answers):**

1. **Scope: polish + packaging + the Phase 2 surface.** The interface gains plugin
   and secret management, and the `sleeve` card becomes visible.
2. **`tauri-plugin-dialog` was added** — a dependency belonging to the shell; it
   doesn't enter the `headshell-core` tree.
3. **`tune` / `dev.tune.desktop` were pinned.** PLAN §1's "the project name is
   open" line stays; what's pinned is the packaging identity.
   *(This item deliberately keeps the old name: the name became `headshell` with
   **D-058**. Paths like `tune-core` elsewhere in the log were renewed for the new
   name, but a decision talking about the name itself makes D-058 meaningless if it
   isn't read with the name of the day it was made.)*

### The interface had never seen Phase 2

When Phase 3 was written, Phase 2 had been postponed (D-027). The order later
reversed, and nobody went back to look at the shell: the SoundCloud and YouTube
Music plugins worked, but **they couldn't be installed or approved from the
interface**. The only way to enter a secret (the AcoustID key, the Torznab token)
was the CLI. For a desktop user, that meant the feature didn't exist.

The second example of the same gap was quieter: the `sleeve` command was
**registered** in IPC, yet `app.js` never called it. All of Phase 0.5's product —
the project's distribution hook — didn't show on the desktop, and no test said so.

### The plugin state: a copy that doesn't choose doesn't drift

The CLI chooses a priority among three things with `status_text()`: a manifest
problem, an artifact the engine must install (D-055), the consent state (D-040).
It chooses because it has to fit on one line.

The interface has no such squeeze, so **the choosing logic wasn't copied**: all
three are shown side by side. Copied logic drifts over time; logic that isn't
copied can't. The same principle as the `--headshell-*` tokens (D-033's anchor
formula exception is deliberately the only one, and it's locked with the accuracy
set).

### `bundle.active: false` had hidden three things

With packaging off, none of them showed:

* The icons were a 32×32, **103-byte** single-colour placeholder.
* The `.desktop` entry had no category, no description and no license file.
  *(A correction, 2026-09-20: the first two closed; **the license hadn't
  closed.** `bundle.licenseFile` was written, but Tauri gives it only to the
  Windows/macOS packagers; the `.deb` and `.rpm` produced carried no license file
  at all — including the `/usr/share/doc/<package>/copyright` Debian policy asks
  for. All three were added with `bundle.linux.{deb,rpm}.files`: `copyright`,
  `LICENSE-MIT`, `LICENSE-APACHE`. The package was opened up and verified.)*
* The version was written in two places — `tauri.conf.json` said `0.0.1` while the
  workspace was at `0.0.1-beta`. The `version` field **was removed**; Tauri reads
  it from `Cargo.toml`, so there's one source.

The icon is now generated from `icon.svg` (`icons/README.md`) and uses the default
theme's palette — but it isn't part of the theme and doesn't change when the user
changes the theme.

Packaging runs through `release.yml` on two triggers: a `v*` tag (uploads to a
draft release) and a manual run. It doesn't run on PRs — it's a ten-minute job, and
the three gates already run on every PR.

The local run produced a `.deb` and an `.rpm`; they contain the icons, a `.desktop`
entry with the categories `AudioVideo;Audio;Music;` and the right version. **The
AppImage failed**, and the cause isn't the configuration but the host: linuxdeploy
comes with its own old `strip` and doesn't recognise Arch's libraries with a
`.relr.dyn` section. That's why the AppImage is a separate step in CI — it isn't
masked; if it fails the job turns red; the split only keeps an AppImage failure
from taking deb/rpm with it. No problem is expected on the Ubuntu runner, but it
**wasn't verified**.

### The interface's silent breakage is now tested

The webview has no type checking: a typo in `$("playQuery")` returns `null`, blows
up at startup and **leaves the window empty** — `cargo test` wouldn't see it.
`tests/ui_contract.rs` holds four ties: every `id` looked for exists in the page,
the sidebar and the `Ctrl`+number shortcut name the same panels, D-037/2's class
names are reachable, every declared token is used.

While the test was being written, its first version failed on `.panel`: the class
carries no rule in `style.css`, but it's in the DOM and a theme can target it. The
criterion was corrected from "does it have a rule in the CSS" to **is it
reachable**.

### Only a screenshot caught one bug

The shortcut window came up **open** on every launch. The markup was right
(`<div id="helpSheet" class="sheet" hidden>`), the JavaScript was right, the
commands were right: since `.sheet { display: flex }` is more specific than the UA
stylesheet's `[hidden] { display: none }` rule, the `hidden` attribute did
nothing. The same flaw was in `importSummary` too (`.summary` is `flex` as well).

Neither the compiler, nor clippy, nor a test looking at IPC could have seen this —
the bug was in CSS specificity. Actually opening the app and looking caught it.

The fix is one rule (`[hidden] { display: none !important }`) and a regression test
next to it. The `!important` is deliberate: a theme shouldn't be able to bring back
a hidden element.

### What user-friendliness amounted to

* An empty queue is no longer an empty list but a three-step **getting started**
  card.
* Importing moved out of the "diagnostics" tab into its own tab — the first thing
  to do was sitting under the most diagnostic tab. Its report is also a summary in
  numbers rather than raw JSON; the raw form stays folded.
* Keyboard: space, the arrow keys, `/`, `Ctrl`+1…8, `Esc`, `?`. Since an
  undiscovered shortcut might as well not exist, `?` opens a list.
* Notices can be dismissed; the diagnostics report can be copied to the clipboard.
* Empty states (no provider, no server, no secret, no listens) now write what to
  do — the "I didn't look" and "I couldn't find it" distinction holds in the
  interface too.

---

## D-058 — The project name: the `tune` placeholder became `tonearm`
**Date:** 2026-09-20 · **Status:** APPLIED (2026-09-20)

**Question:** The first release line was opened (the user: *"the first release
line"*). Pushing a tag pins the name in practice — the name of the downloaded
package, the `.desktop` entry, the data directory, the repository address. PLAN §1,
on the other hand, had said since day one *"Still open: the project name (`tune`
is a placeholder)"*. Will the name close now, or wait for 1.0?

**Decision:** Now. The name is **`tonearm`**, and the GitHub repository
`enaimami/tonearm`.

### Why `tonearm`

A tonearm doesn't choose the record — it reads whatever you put on. That's already
the project's one-sentence thesis: wherever the audio comes from (a local file,
Subsonic, SoundCloud, YouTube Music, torrent), the layer on top stays the same. The
name describes the architecture. That record enthusiasts in Turkey already say
"tonarm" is a second gain.

Availability was measured, not remembered: `crates.io` is free, the biggest clash
on GitHub is a 2★ vinyl noise simulator, `enaimami/tonearm` is free. The candidates
eliminated, and why: `earmark` (895★ Elixir markdown), `phono` (2743★ Phonograph —
the same field, confusing), `deepcut` (428★ Thai tokenizer), `stylus` / `cadence` /
`groove` / `motif` (taken on crates.io or the name of established audio software),
`sidetrack` (both a 62★ library and a negative connotation). The finalists
`refrain`, `longplay` and `dubplate` were clean in the measurement; the choice was
made by the metaphor.

### Why today, and not yesterday

The name change touched 110 files and ~950 lines. Part of it is **contract**:

* the theme tokens `--tune-*` → `--tonearm-*` (D-037's 14 tokens),
* the diagnostics report's JSON key `tune_version` → `tonearm_version`,
* the environment variables `TUNE_*` → `TONEARM_*` (11 of them),
* the packaging identity `dev.tune.desktop` → `dev.tonearm.desktop`,
* the data directory `~/.local/share/tune` → `~/.local/share/tonearm`.

None of them had been published. The same change **after** a tag is pushed would
leave every installed user's library ownerless and break every theme written. This
was the one moment its price was zero — D-057 turning packaging on started this
moment, and the tag would have closed it.

The data directory on the development machine was moved by hand (the library,
secrets and plugin records were kept); `tonearm stats` gives the same numbers after
the move.

### How far the log is touched

In the older records of this file, **paths and identifiers** like `tune-core` were
renewed for the new name: a decision log exists to be read, and a record pointing
at a nonexistent directory can't be read.

The one exception is **D-057's item 3**, because that item is a decision about the
name itself (*"`tune` / `dev.tune.desktop` were pinned"*). Read without the name of
the day it was made, the decision becomes meaningless. The old name stays there,
with a note next to it. The distinction is this: a record that *uses a name* is
renewed; a record that is *about a name* is frozen.

### The debt that closed alongside

The MusicBrainz client identity was sending the placeholder
`https://github.com/kullanici-adi/tune` — D-054 had flagged it and nobody had
closed it. It now sends the real repository address. MB's rule asks for a
reachable contact address; the repository was chosen over an email, because an
email would be distributed as plain text inside the binary.

**455 tests, three gates clean.** The name change didn't change behaviour: the
tests passed in the same number before and after the renaming.

> **This record was frozen (D-065).** The name later became `headshell`, and the
> `tonearm` occurrences in this record **weren't renewed** — because this record
> doesn't *use* a name, it's *about* one. Written with the new name, the sentence
> "the name `tonearm` was chosen" would become meaningless. The rule itself is
> inside this very record: *a record that uses a name is renewed; a record that is
> about a name is frozen.*
>
> That the availability measurement here was incomplete is written in D-065 too:
> crates.io and GitHub were checked; **the AUR and Codeberg weren't.**

## D-059 — `--tui` checks the terminal before the audio device; that's how it was seen that CI had never been green
**Date:** 2026-09-21 · **Status:** APPLIED (2026-09-21)

**Question:** Which error should `headshell play --tui` give in an environment with
no terminal? The test (`the_tui_refuses_to_start_without_a_terminal`) expected the
terminal refusal; CI got a sound card error and failed.

**Decision:** The order was reversed. On the `--tui` path the terminal is checked
**first** (`tui::require_terminal`), then the audio output is opened.

### Why this order

`--tui` needs two resources: a terminal and an audio output. The terminal is free to
check; the audio output opens a hardware resource. In the reverse order, on a
machine without a sound card, the user saw an ALSA error even though they had typed
`--tui` — a wrong diagnosis. What K9 asks for is an error that describes what the
user did.

The check is done with `enable_raw_mode` itself, not with a separate `is_terminal`
criterion: if the two criteria drift apart, "it passed the check, it failed while
opening" is born. The raw mode is given back right away, and the screen isn't
switched — so the output of a slow provider search isn't lost on the alternate
screen.

### The real finding: CI had never been green

D-053 set up CI. Since that day there had been **two runs** (`a2cf5b6`, `c84d75a`),
and **both were red**. The commit messages said "455 tests, three gates clean"; the
measurement had been made on the development machine, and that has a sound card.
This single test passed on every machine with a sound card and failed on every
machine without one.

The lesson comes not from the rule itself but from where it was measured: the claim
"three gates clean" is incomplete without saying **where** the gates were run.

### How it was verified

Since the development machine has a sound card, the failure couldn't be reproduced
directly. `ALSA_CONFIG_PATH=/dev/null` imitates the runner having no sound card and
gives exactly CI's output. It was measured in both directions:

| Condition | Without the fix | With the fix |
|---|---|---|
| A sound card | ok | ok |
| No sound card (CI's state) | FAILED | ok |

A side gain: the test **now really runs** in CI. It used to get stuck on the audio
device and never reach the terminal refusal — so it never tested its claim.

### The debt opened alongside

`playback_local::a_real_file_plays_and_the_position_advances` is unstable on this
machine (3 failures in 6 runs): `snd_pcm_avail_delay → I/O error (5)`. The only
audio output is HDMI, and writing to the idle line gives intermittent EIO.
`engine_for` skips two cases — no device, the device couldn't be opened — but this
is a third: the device opened, then was pulled out from under it. Since CI has no
device at all, it's skipped there. A separate round; not within this decision's
scope.

## D-060 — The plugin engine: the temporary download name must be specific to the run
**Date:** 2026-09-21 · **Status:** APPLIED (2026-09-21)

**Question:** D-059 closed CI's first red, and a second came out from under it: two
`plugin_ytmusic` tests were failing. What was the cause, and where does it belong?

**Decision:** A product race. `Engine::install` writes the temporary file with a
**run-specific** name: `<artifact>.<pid>-<nanoseconds>.downloading` (the suffix was
`.indiriliyor` until D-073).

### The race

The download had two steps — first a `.downloading` temporary file, then a
`rename` (D-055). The temporary name was fixed: `<artifact>.downloading`. Two runs
installing the same artifact at the same time write the same file; when one takes it
away with `rename`, the other gets ENOENT in `make_executable`'s `metadata` call.

This isn't a test flaw. In real use too, if two `headshell plugin install` commands
run at the same time, the same thing happens. The reason it showed in the tests is
that `plugin_ytmusic`'s five tests run in parallel and all share the same fixed
cache directory — so what the test did was realistic, not flawed.

`.downloading` stays **as the suffix**: the check that recognises a half-done
download looks at it. Also, the temporary file is now deleted on the error paths of
`make_executable` and `rename` — an installation left half-done shouldn't leave
files lying around.

### Why it was never seen on four cores

On the development machine, with a clean cache, all three of three runs passed; on
CI's two cores it failed. A timing-dependent failure not showing doesn't mean "it
isn't there" — it means **it wasn't measured** (K9's distinction holds here too).

That's why the regression test wasn't left to timing:
`installing_the_same_artifact_concurrently_does_not_collide` installs the same
artifact into the same directory with eight threads. It was measured
deterministically — when the fix was reverted, **all three of** three runs failed;
with the fix, they passed.

**456 tests, three gates clean** (455 + this regression test).

### Left open

`plugin_ytmusic`'s second test failed in CI with `PROVIDER_CALL`. Whether because
the artifact couldn't be installed or because YouTube blocks CI addresses — the
distinction will only be seen in the run after this fix. Under D-043 the second is
a legitimate red and needs a separate decision.

### Addendum — the clock was no tiebreaker on macOS (2026-09-26)

D-075's CI run went red on macOS, in this decision's own regression test, with the
error this decision fixed: `rename` got ENOENT. The run-specific name was
`<pid>-<nanoseconds>`; the eight threads share the pid, and the nanoseconds were to
tell them apart. macOS reads the wall clock in microseconds, so two threads that
started in the same microsecond got the same name, and the first `rename` took the
second's file. D-075 did not touch this code; the macOS runs before it were green by
timing. A process-wide counter joins the name: `<pid>-<nanoseconds>-<n>`.

"Measured deterministically" above was true on Linux, whose clock counts nanoseconds;
the measurement didn't see another system's clock. D-076 found the same kind of
per-process name in its own cover cache before it shipped.

## D-061 — YouTube's bot wall: cookies go through the secret store; a run without cookies skips
**Date:** 2026-09-21 · **Status:** APPLIED (2026-09-21)

**Question:** The question D-060 left open was answered. `plugin_ytmusic`'s stream
resolution failed in CI, and the cause was:

```
yt-dlp: ERROR: [youtube] qYcoJpqCha4: Sign in to confirm you're not a bot.
```

YouTube puts up a bot wall for data centre addresses. Which side of D-043 is this —
"I couldn't reach it", or "I reached it and got the unexpected"?

**Decision (the user's):** Cookies are given. The `cookies` secret in the
`plugin:ytmusic` namespace is passed to yt-dlp as a cookie file; in CI the secret
comes through `YTMUSIC_COOKIES` → `HEADSHELL_TEST_YTMUSIC_COOKIES`.

### Why through the secret store, not an environment variable

Cookies reach the plugin **from the secret store** (D-042), and the test writes them
there and gives them that way. So the path tested is the same path the user lives
through: `headshell secret set plugin:ytmusic cookies`. If the test had its own back
door, what was being tested wouldn't be the product.

The plugin writes the cookies to a `0600` temporary file and deletes it on exit —
yt-dlp can read cookies only from a file, but no lasting copy of an account session
is left on disk.

### Why a run without cookies skips

GitHub gives no secrets to PRs coming from forks. There the cookies always arrive
empty, and if we insisted, the test would stay red permanently — CI's red would
lose its meaning at that moment, and that's its only job.

The distinction was set up like this:

* **If cookies were given**, getting past the wall is our job; failing to is a
  reason to **fail**. That's the strictness on `master`.
* **If no cookies were given**, there's nothing measurable: the service didn't let
  us look, and said nothing about the product. The test skips and writes the reason
  to `stderr` (D-043).

The match is **narrow**: only the bot wall's own signature (`not a bot`). Every
other refusal still fails the test — otherwise a real regression would hide behind
this door. A broad rule of "skip if there's a PROVIDER_CALL error" wouldn't be
applying D-043 but cancelling it.

### The price is written down explicitly

A cookie is an account session. Anyone with write access to the repository can leak
it, and PRs opened from the same repository see secrets. yt-dlp's own documentation
says the account can be restricted. That's why the cookie is taken **from a
throwaway account**, not a personal one; and when it expires, the test turns red
again — that red doesn't say "the product broke", it says "the cookie died".

### Where it came from

The cause was only seen once the test's error message was fixed: the test wrote
`{err}`, and `Error`'s `Display` by design prints only `ADIM: {stage}`. Three
candidates, three stage names, zero causes — an error message that swallows the
diagnosis, in a project built on K9. Switching to `chain_text()`, the cause came out
on the first run.

**456 tests, three gates clean.**

## D-062 — The same race in a second place: atomic placement into the shared cache
**Date:** 2026-09-21 · **Status:** APPLIED (2026-09-21)

**Question:** The CI run after D-061 failed again, but **with a new error**: not the
bot wall, but `yt-dlp is not installed`. The artifact had just been installed; why
doesn't it look installed?

**Decision:** `cached_ytdlp` now places into the shared cache atomically: it first
copies to a run-specific `.installing` name, then `rename`s.

### The chain

`std::fs::copy` isn't atomic. The other test running in parallel saw the **half
written** file in the cache with `cached.exists()` and counted it as ready; it
copied it into its own data directory, the engine's hash check didn't match, the
artifact never got into `ready_paths`, and the plugin said "yt-dlp is not
installed".

The error message was right — the artifact really wasn't ready. What was misleading
was that the step installing it looked successful.

### This is the same as D-060

D-060 fixed the fixed temporary name in the engine. A second copy of the same
pattern sat three functions up, in the test helper, and that round missed it: while
the fix was being written, nobody looked at "where else is there a non-atomic
placement". When a race is found, the question to ask isn't "did I fix this place"
but **"how many other places share the same pattern"**.

As a rule: every file put into a shared directory is put there with `rename`.
`rename` is atomic on the same file system — the file either doesn't exist or is
complete; its half-written state is visible to no reader.

### The limit of verification — explicitly

This race was never triggered on the development machine (three runs with a clean
cache, all three passed), so the fix **wasn't proven locally.** The proof is in
reading: `copy` isn't atomic, `rename` is. No deterministic check like D-060's
regression test was written here — what would be tested is the test's own helper,
and a test testing it would fall below what it tests.

The real exam is CI. This says D-059/D-060's lesson once more: **"it passed locally"
is a measurement result, and its scope is as wide as the place it was measured.**

**456 tests, three gates clean** (measured under CI's condition of no audio device;
`playback_local` is unstable on this machine because of EIO on the HDMI line,
D-059).

## D-063 — The MSI version is written separately, but isn't allowed to drift
**Date:** 2026-09-21 · **Status:** APPLIED (2026-09-21)

**Question:** While §3.5's second debt was being closed — macOS and Windows
packaging were run by hand for the first time — Windows failed:

```
Error failed to bundle project: `optional pre-release identifier in app
version must be numeric-only and cannot be greater than 65535 for msi target`
```

Windows Installer doesn't accept a pre-release label; `0.0.1-beta` is invalid for
the MSI target. How should the version scheme change?

**Decision (the user's):** `bundle.windows.wix.version = "0.0.1"`. `Cargo.toml`
stays `0.0.1-beta`, and the beta label is kept everywhere it's visible. Drift is
made impossible with a test.

### The tension with D-057, and how it was closed

D-057 had decided that the version lives **in one place**: the `version` field was
deliberately removed from `tauri.conf.json` that day, with the reasoning "if it were
written in two places, one would drift". `wix.version` brings that second place
back.

Since the reasoning is valid, the rule wasn't cancelled; **it was enforced**:
`crates/headshell/tests/bundle_contract.rs` checks that `wix.version` equals the
Cargo version with the pre-release suffix dropped, and that its three fields stay
within Windows's limits (the first two ≤255, the rest ≤65535). It's written in two
places, but it can't diverge: if it does, the gate turns red. Measured — when
`wix.version` was deliberately made `0.0.2`, the test failed and wrote what had
diverged from what.

The option "drop the `-beta` suffix entirely" was rejected: the package version
itself should say it's beta; that information shouldn't stay only in the README.

### Why the dry run paid off

PLAN §3.5 said "it must be run by hand with `workflow_dispatch` before the first tag
is pushed", and that's exactly what paid off. Going with a tag, the `draft` job would
have been skipped because of `needs: bundle` (it showed `skipped` in the run), the
release would never have been created, and the tag would have had to be deleted and
pushed again.

The same run closed §3.5's **first** debt too: the AppImage is produced without
problems on the Ubuntu runner. It couldn't be tried locally on Arch because of
linuxdeploy's old `strip`, and it had been written as "not an expected problem, but
not verified" — now it's verified.

### The fifth member of the family

The same pattern as D-059, D-060, D-061, D-062: the rule was right, and the
measurement had never been made on that platform. `bundle.active` was `false` for a
long time; D-057 turned it on, but Windows was never tried. `bundle_contract.rs`
takes this measurement from packaging day and carries it into every gate run.

**458 tests, three gates clean** (456 + two new checks).

## D-064 — The `wrapped` feature became `sleeve`: the brand is someone else's
**Date:** 2026-09-21 · **Status:** APPLIED (2026-09-21)

**Question:** The shareable year card feature had been named `wrapped` since day one
(D-004, Phase 0.5). The user stopped at the first release line: *"let's rename
wrapped from scratch, because leaving it simply as wrapped will create legal
problems."*

**Decision:** The feature is named **`sleeve`**.

### Why it's a problem

**Wrapped** is the brand of Spotify's year-end feature, and this project is in the
same field — on top of that, it imports the Spotify export, so the two will be seen
side by side. The neighbours of the same trap were ruled out too: Apple's
**Replay**, YouTube Music's **Recap**, YouTube **Rewind**. What's safe is not to
resemble a branded name but to describe the job.

### Why `sleeve`

A record sleeve: the thing you show others, with the information written on it. The
artifact produced is a card image that exists to be shared anyway, so the metaphor
describes the job itself — and it stays in the same family as `headshell`.

### The attribution rule (the user's correction)

When referring to another company's brand in text, it's written **with its owner
and its mark**: not a bare "Wrapped" but **Spotify Wrapped®**. A bare brand name
gives the impression of using that name as if it were your own feature; writing it
with its owner makes the attribution explicit. Referring to someone else's product
by name is fine — the problem is making it the name of your own feature.

The five "Wrapped" occurrences left in the repository are all in this form, and all
deliberate.

### The bug a blind replace was caught making

There were 167 occurrences, and not all of them were ours. A plain find-and-replace
broke two places, both sentences describing **Spotify's limit**:

* `sleeve/data.rs` — "where it parts from the provider's 12-month memory",
* `docs/index.html` — the row in the comparison table's **Spotify column**.

In both, sentences slandering our own product were born, like "Sleeve is offered
once a year". The lesson: when a name is changed, every occurrence is sorted into
three categories — our feature, someone else's product, unclear. The third is made
explicit; the second isn't touched.

### The contract surface

What changed isn't only internal naming: the CLI subcommand (`headshell sleeve`),
the `--json` keys, the diagnostic counters (`sleeve.*`), the diagnostic stage
(`ADIM: SLEEVE_RENDER`), the desktop IPC command, the HTML ids and a CSS class.
None of them had been published — the draft release is standing, and there are no
installed users. D-058's logic holds here too: this was the moment its price was
zero.

The snapshot test caught the change — since the counter keys are written
alphabetically, `sleeve.*` moved ahead of `stats.*`. They were regenerated, and it
was verified that the difference is only the name and the order, and not a single
value moved.

**458 tests, three gates clean.**

## D-065 — The project name is `headshell`: D-058's measurement was incomplete
**Date:** 2026-09-21 · **Status:** APPLIED (2026-09-21)

**Question:** The first release draft was ready, and the next job was the AUR
package. What came out when the name was measured there wasn't a coincidence:
**`tonearm` is taken on the AUR** — 1.5.0-1, *"Unofficial native GTK4 / Adwaita
music streaming client for TIDAL"* (codeberg.org/dergs/Tonearm), actively
maintained, with a `tonearm-git` version too.

So in the same field — a Linux desktop music client — the same name, the same
distribution channel.

**Decision:** The name is **`headshell`**. The GitHub organisation is `headshell`.

### Why D-058 missed it

D-058 had written availability as *"measured, not remembered"*, and it was right —
but **two channels had been measured**: crates.io and GitHub. The AUR and Codeberg
hadn't been checked, and the clash was exactly there. That's why D-058's sentence
"the biggest clash is a 2★ vinyl noise simulator" turned out wrong.

The lesson is the same as this session's general lesson (D-059…D-063): **a
measurement doesn't cover anything outside the channel it was measured in.** "The
name is free" means "free in the places I looked", and until where was looked is
written down, the claim is incomplete.

In this round five channels were measured — crates.io, the AUR, Codeberg, GitHub
repository search, the GitHub user/organisation namespace — and more than 20
candidates were scanned. `headshell` is completely free in four of them; the only
clash on GitHub is a 1★ repo.

### Why `headshell`

The removable head piece at the end of the arm that carries the stylus. **Whatever
cartridge you fit, the connection is the same** — you fit a Shure, you fit an
Ortofon, and neither the arm nor the record changes.

`tonearm` said "I don't choose the record"; `headshell` is the same idea looking at
the provider side: "every cartridge fits me". The thesis didn't change; the metaphor
settled one step further into place. Even the icon didn't need to change — the
**head** in the sentence written in the commit when it was drawn, *"a spindle, an
arm, a head, and a stylus coming down onto the outer groove"*, was the headshell
itself already.

### D-058 was frozen

D-058 doesn't *use* a name; it's *about* one. By the rule it set itself, it's
frozen: the `tonearm` occurrences in it weren't renewed, because the sentence "the
name `tonearm` was chosen" would become meaningless if written with the new name. A
note saying the measurement was incomplete was added next to it.

### The contract surface

1087 occurrences, 111 files. Among what changed, the parts that are contract: the
crate names (`headshell-core`, `-cli`, `-plugin-torrent`), the binary names
(`headshell`, `headshell-desktop`), the environment variables (`HEADSHELL_*`), the
theme tokens (`--headshell-*`, D-037's 14 tokens), the diagnostics JSON key
(`headshell_version`), the packaging identity (`dev.headshell.desktop`), the data
directory (`~/.local/share/headshell`), the MusicBrainz User-Agent and a CSS class.

None of them had been published: the draft release is still a draft, and there are
no installed users. D-058's argument "the one moment its price is zero" held a
second time, and this time it was used before the window closed.

The snapshot test caught it again: `headshell_version` moved alphabetically ahead
of `os`, 11 snapshots were regenerated, and it was verified that the difference is
only the name and the order.

**458 tests, three gates clean.**

## D-066 — The AUR: three PKGBUILDs, four packages; both built from source and prebuilt
**Date:** 2026-09-21 · **Status:** APPLIED (2026-09-21)

> **D-070 (2026-09-24):** The gap "the `.dmg` is arm64 only" closed — the macOS
> package and the CLI archive are a universal binary. The Linux packages are built
> on 22.04 (a glibc 2.35 floor); the behaviour of the `-bin` packages didn't change.

**Question:** How will the first release's Arch side be distributed? D-065 came up
exactly as this job was starting (the name was found taken when measured on the
AUR), and the package was left half done.

**Decision:** **Three PKGBUILDs, four packages** under `packaging/aur/`:

| PKGBUILD | Package(s) | From |
|---|---|---|
| `headshell/` | `headshell` + `headshell-cli` | The tag archive, built from source |
| `headshell-bin/` | `headshell-bin` | The release's `.deb` |
| `headshell-cli-bin/` | `headshell-cli-bin` | The release's CLI archive |

**Only the source package is split.** The two binaries come out of a single
`cargo build`; with separate PKGBUILDs a user installing both would build ~500
crates twice. On the `-bin` side there's **no** shared build — the two packages
share no work; they only copy files. A split package's reason to exist is a shared
build; imitating a reason that doesn't exist had two prices: a user installing only
the CLI downloaded the 10 MB `.deb` too, and namcap gave a `splitpkgmakedeps` error
(the rule: the global `makedepends` must cover the sub-packages' `depends` — on the
`-bin` side those dependencies belong only to run time and aren't needed for
building). Once they were separated, both went away.

### Why there's a `-bin` too

The source package builds the Tauri shell; the price is measured in minutes.
`release.yml` already produces every platform's binary, and the CLI archive's
**file name has no version** — that fixed name was put there exactly for this, so
the AUR line stays the same in every release. So the `-bin` package was a decision
implied but not written in this repository; now it's written.

The `.AppImage` isn't used: it's 85 MB and carries its own libraries. An Arch
package should link against the system libraries, so the desktop binary is taken
out of the `.deb`.

### `cargo tauri build` isn't used

That's a bundler; it produces `.deb`/`.rpm`/`.AppImage`. `makepkg` already installs
the Arch package; calling the bundler would both repeat the work and make
`tauri-cli` a build dependency. A plain `cargo build` is enough; in return, the
`.desktop` entry and the icons are installed by hand.

### The `.desktop` entry moved into the repository

Both PKGBUILDs install `packaging/headshell.desktop`; for this the `-bin` package
downloads the source archive too (1.2 MB — the icons and licenses come from there
as well). Two hand-written copies would drift apart: this log has two records of
the same failure, in D-062 and D-063.

`StartupWMClass=headshell-desktop` **was measured, not made up**: the `.deb` in the
draft release was opened and the entry Tauri generates was read — the window class
is derived from the binary name. The half-done draft said `headshell`; had it
stayed like that, the running window wouldn't have matched the launcher icon. The
icon names are `headshell-desktop.*` for the same reason.

### The version on a single line

`pkgver=0.0.1_beta`; the upstream spelling is derived with
`_pkgver=${pkgver//_/-}` (`-` is the pkgrel separator in an Arch version). In the
half-done draft the version was written in two places — `pkgver` and `_srcdir` —
and that was the same bug caught on the MSI side in D-063.

### The package texts are English

The `pkgdesc` and the `.desktop` `Comment` are English. D-036 said "the user reads
the text → Turkish", and the `.deb`'s description really was Turkish (it comes from
tauri.conf.json). The AUR packages deliberately split off: the audience reading
there is international. **A known and accepted inconsistency**, not a missed one.
(D-073 made all of it English, and the inconsistency closed.)

### The torrent plugin doesn't go into the package

The engine looks for plugins under `~/.local/share/headshell/plugins/<name>/`, and
the `exec` in `plugin.json` is relative to that directory; it doesn't see a binary
under `/usr/bin`. A system-wide plugin directory is a core decision — PLAN §2.8
item 5 and D-049, after the first release.

### The order: the tag first, then the PKGBUILD

The `-bin` package **doesn't work until the release leaves draft**: a draft
release's assets can't be downloaded anonymously; only those with access to the
repository see them. The source package has no such debt; the tag archive is
independent of the draft.

`.SRCINFO` isn't kept in this repository: it lives in the AUR repository, and
`makepkg --printsrcinfo` generates it there. A copy here would silently go stale
when the PKGBUILD changed.

### The packages were really built — namcap gave three findings

This machine is Debian; there's no `makepkg`. The packages were built in an
`archlinux:base-devel` container (`podman`), and since the tag didn't yet carry the
fixes, `source=` was fed a tag-equivalent archive produced from the working tree.
The method is written in `packaging/aur/README.md` — it had to be repeatable,
because what namcap says couldn't be found by guessing:

1. **`gcc-libs` is unnecessary.** It was written in all four packages; namcap says
   "included, but may not be needed" — it's a member of `base` and implicitly
   satisfied anyway. Removed.
2. **The `-bin` packages were producing a `-debug` package** and re-`strip`ping
   binaries coming from upstream. The symbols are meaningful where they were built,
   not here → `options=('!strip' '!debug')`.
3. **`x86_64` was hard-coded.** The architecture-specific sources moved into the
   `source_x86_64` / `sha256sums_x86_64` arrays, and the architecture in the CLI
   archive's name was turned into `$CARCH`.

The result: `namcap` is clean on all three PKGBUILDs. The warnings left on the
packages are informational — "implicitly satisfied" dependencies (`glib2`, `dbus`,
`cairo`, `libsoup3`, `gdk-pixbuf2`; all coming through `webkit2gtk-4.1` + `gtk3`)
and "ELF file is unstripped", which is `!strip`'s own consequence.

A full build of the source package (`makepkg -s`, Tauri + ~500 crates) **wasn't
run** — it was started and stopped so as not to tie up the machine.
`makepkg --printsrcinfo` parsed the PKGBUILD, `namcap` passed clean, and the file
paths of the `package_*()` functions were verified by running them into a fake
`$pkgdir`; but `build()`/`check()` should run once on a real Arch machine.

### A side finding: `enaimami/headshell` is a 404

Measured while the package's `url` was being written — D-065 moved the repository
to the `headshell` organisation, but `README.md` (two places) and
`packaging/copyright` kept carrying the old address, and that address doesn't
redirect; **it gives a 404**. The `copyright` file sat as a dead address inside the
`.deb` and `.rpm` in the draft release. All three were fixed.

### A side finding: PLAN §3.5's two debts had already closed

The debts "the AppImage wasn't verified" and "macOS/Windows weren't run by hand"
turned out closed once the runs were measured: `workflow_dispatch` run 35571122203
is green on all three platforms, so is tag run 35597289680, and the draft release
has nine assets. The PLAN didn't know this — the text was updated to the
measurement. The real gap left: the `.dmg` is arm64 only.

**458 tests, three gates clean.**

## D-067 — The `Makefile`: a shortcut layer, not a rule layer
**Date:** 2026-09-21 · **Status:** APPLIED (2026-09-21)

**Question:** The commands are written in three places (CLAUDE.md, CONTRIBUTING.md,
ci.yml), and they're typed by hand. Does a `Makefile` make them runnable from one
place, or does it become a fourth source of truth?

**Decision:** There's a `Makefile`, but it **sets no rules** — it runs the existing
commands. The definition of the three gates stays in PLAN.md §0.4 and the command
surface in CLAUDE.md; if they contradict it, they win, and that's written at the
top of the Makefile.

The target names are English, the text Turkish (D-036; everything English since
D-073). The version is read from `Cargo.toml`, not written into the Makefile a
second time — Arch's `_` spelling is derived from it too (D-063's same lesson).

**What it gains:** `make gates` runs the gates **in ci.yml's order** (fmt → clippy
→ test; the cheapest fails first — CLAUDE.md's list was alphabetical, CI's was
deliberate). `make core-features` runs ci.yml's second job locally: the core on its
own, every feature one by one, all at once (D-054). Until today that job ran only
in CI. `make aur-test PKG=…` builds an AUR package end to end in an Arch container
and `namcap`s it.

### `LC_ALL=C` isn't an ornament

`make help` collects the targets from its own source with `grep`, and its first
version silently listed them incompletely: 13 of 16 targets showed. What the three
missing ones — `cli`, `clippy`, `diag` — have in common is that they're the only
three targets with an **`i`** in their name.

The cause is the range `[a-zA-Z0-9_-]`: character ranges use the locale's
collation order, and in `tr_TR.UTF-8` `i` and `ı` are separate letters, and the
`a-z` range leaves `i` out. The pattern now runs with `LC_ALL=C`.

The bug didn't show for a while because in the interactive shell `grep` was wrapped
in a shell function and found all 16 lines; the `/usr/bin/grep` that `make` calls
found 13. **When the same command gives two results in two shells, what you're
measuring is not the command but the environment.**

This project's developer works in a Turkish locale, so the trap can be set up once
more: when writing letter ranges in shell scripts, use either `LC_ALL=C` or
`[[:alnum:]]`.

### Addendum: `aur-test` was pinned to the container

In the first version the target called `podman` directly, and on an Arch machine it
failed with `podman: Böyle bir dosya ya da dizin yok` (No such file or directory,
in the Turkish locale) — while there the container is **unnecessary**; `makepkg` is
already there. The container was a need of the Debian box this Makefile was written
on, and that need had leaked into the target's definition.

Now the engine is chosen automatically (native if there's `makepkg`, otherwise the
container) and can be forced with `ENGINE=`. If a tool is missing, the target says
what's missing and which command installs it — K9.

A second bug in the same round: the missing-tool messages used backticks inside
double quotes, so instead of printing `updpkgsums` they **ran** it.

---

## D-068 — The landing page is published from GitHub Pages (a placeholder for now)

**Date:** 2026-09-23 · **Status:** APPLIED (2026-09-23)

**Question:** `docs/index.html` had sat in the repository for months but was
published nowhere — so nobody saw it. Should a separate repository, a separate
generator (SSG) and a separate distribution pipeline be set up for a site?

**Decision:** No. GitHub Pages publishes **directly from the `docs/` folder of the
`master` branch**. No generator, no npm, no extra workflow — the page is a
single-file plain HTML already, on the same line as `crates/headshell/ui` (no
bundler; there wasn't one there either).

**Reasoning:** Setting up a distribution pipeline for a placeholder is more
expensive than the placeholder. As long as the `docs/` source is the very thing
that's published, the page doesn't go stale: a fix goes in the same commit and
doesn't wait to be copied into a separate repository.

**Consequence:**
- The address: <https://headshell.github.io/headshell/>
- From now on `docs/` is **a published surface**. Every file put there is public;
  `writing-plugins.md` (then `eklenti-yazma.md`) is served from the same root too.
- Three dead links were fixed in the same round: the "Source Code" button and the
  two license links went not to the repository but nowhere
  (`href="https://github.com"`, `href="LICENSE-MIT"` — the second a 404 from the
  site root).
- The page is a placeholder: the version badge says `v0.0.1-beta`, and there's no
  download link. It will be rewritten after the first tag.

**Addendum — turning it on couldn't be written into the repository.** At first, a
workflow (`actions/configure-pages`, `enablement: true`) was supposed to turn Pages
on itself, so the setting would stay written in a file in the repository. It didn't
work: both the local token and the workflow's `GITHUB_TOKEN` give a 403 on the
`Create Pages site` call (`Resource not accessible by integration`) — neither has
the Pages permission.

The workflow was reverted. The source is chosen **once** in the repository
settings: Settings → Pages → *Deploy from a branch* → `master` / `/docs`. From then
on it's automatic: every commit going to `docs/` is published, spending neither a
workflow nor CI minutes.

## D-069 — The plugin engine moved to QuickJS: no Python, permissions enforced, torrent parked

**Date:** 2026-09-24 · **Status:** APPLIED (2026-09-24)

**Question:** The user: *"With Python, whoever I sent it to for testing ran into
some kind of problem. We'll write the plugin system from scratch."* D-050 had tied
the engine to Python ("it's enough to say it's required"); in practice Windows has
no Python, Debian ships `venv` as a separate package, the versions don't line up —
and everyone who wanted to try a plugin first had to install a runtime. Four options
were presented: an embedded JS engine (QuickJS), WebAssembly, prebuilt subprocess
binaries per platform, purely declarative plugins.

**Decision (the user's):** Three items.

1. **The plugin process moves to QuickJS.** Plugins are written in JS and run in
   the engine embedded in the core.
2. **Torrent doesn't come with us**; it'll be looked at much later.
3. **A platform binary for yt-dlp:** "yt-dlp publishes for every kind of processor
   from arm to i386; it's enough to match the system's type and find the right
   one."

Next: it will be tested on different operating systems, on machines where
**nothing is installed** (a separate round).

### 1. The engine: `rquickjs` 0.14 (QuickJS-NG), the `plugin-engine` feature

Measured before the dependency was added (D-047's method):

| | |
|---|---|
| the core's tree | **51 → 55 unique crates** (+4: `rquickjs`, `rquickjs-core`, `rquickjs-sys`, `allocator-api2`; `hashbrown`/`foldhash`/`equivalent` were already there) |
| stripped binary | **+~1.3 MB** (an empty binary 350 KB → 1.66 MB) |
| cold build | +~60 s (QuickJS-NG's C source) |
| build requirement | a C compiler — `rusqlite`'s `bundled` already wanted one |

It's behind a feature gate (`plugin-engine`), because the core is complete without
plugins too. The CLI and the desktop turn it on. In a build with it off, plugins
are discovered, listed and approved, and their tools are installed — only the first
call says "no plugin engine in this build" (K9).

`rquickjs` 0.14 needs **Rust 1.87**; the last version that fit 1.85 was 0.11. The
workspace's `rust-version` went **1.85 → 1.87** (CI is `stable` anyway). With the
floor raised, clippy had `% 3 == 0` in `sleeve/svg.rs` turned into
`is_multiple_of` — a method stable in 1.87.

A mobile (Phase 6) note: `rquickjs-sys`'s ready bindings cover all the desktop
targets but not Android/iOS; there the `bindgen` feature (libclang at build time)
will be needed. Not a job today; a note to keep an eye on.

### 2. Contract api 2: exported functions

api 1's wire protocol (JSON-RPC, a handshake, `shutdown`) is gone. The script is an
ES module; it exports `health()`, `search(query, limit)`, `resolve_source(id)`. The
names are the same as api 1's method names — the documentation, the trait and the
plugin use the same name. Values pass between JS and Rust as JSON, and api 1's data
formats (`WireTrack`, `HealthResult`, `AudioSource`) were kept as they were.
`search` now returns an array directly rather than `{tracks: […]}`, and
`resolve_source` a source or `null` directly rather than `{source: …}`.

The manifest: `exec` → `main` (inside the directory, `.js`). Two api 1 fields **are
rejected**, not ignored: `exec` and `permissions.fs`. api 1 manifests don't show as
"broken" but as **"protocol version mismatch: api 1 … install the api 2 (QuickJS)
version"** — discovery looks at the version first.

A single-source manifest: since there's no handshake, the capabilities are only in
the manifest. When the engine starts, it checks that the function of every declared
capability is exported; if one is missing, it's a contract violation, and it isn't
retried.

### 3. `host`: the plugin's single gate, all synchronous

`host.http` (get/post/request), `host.secrets.get/file`, `host.storage`,
`host.tools.run`, `host.log`, and `console` → `host.log`. No event loop and no
timers; an `async function` can be written, and the engine resolves the promise. A
synchronous API was chosen deliberately: it's the shortest path for the plugin
author, and an asynchronous API can **be added** later (without breaking the api),
while the reverse would break it.

- **`host.secrets.file(k)`** — yt-dlp reads cookies only from a file (D-061). The
  gate is narrow: the plugin can have a file written with only its own secret, not
  with content of its own; `0600`, deleted when the engine shuts down.
- **`host.storage`** — for SoundCloud's `client_id` cache (in api 1,
  `state/client_id.txt`). Private to the plugin, a 1 MB cap; a broken store isn't
  reset, it's reported as an error.
- Absence returns `null`, not `undefined` (`rquickjs` turned `None` into
  `undefined`; the first test run showed it, and it was fitted to the contract).
- While the module loads, the network and tools are **forbidden** (the enforced form
  of api 1's rule "the handshake must not go to the network").

### 4. The permissions are enforced now (D-040's "enforcement later" closed)

`PERMISSIONS_ENFORCED` `false` → `true`. Since a plugin can reach the outside only
through `host`, the declaration is no longer a contract but a boundary:

- `permissions.net` is checked on every request and **on every redirect**. The HTTP
  client given to plugins doesn't follow redirects
  (`UreqClient::without_redirects`); if it did, a permitted address could carry the
  plugin somewhere unpermitted and the engine wouldn't see it. A test locks this: on
  a redirect to an unpermitted address, the second request never goes out.
- **The stream address** `resolve_source` returns is checked too: the plugin picks
  the address, and the core fetches it (K3); without the check, a plugin could send
  the core to an address it didn't declare. `local_file` is rejected.
- **Wildcards arrived** (D-040's gap): `*.googlevideo.com`, `*.sndcdn.com`. They
  cover only subdomains (not the apex); a bare `*` and a single-label wildcard are
  rejected. The consent comparison takes wildcards into account.
- The address parser is sceptical: no URL crate was added, and every form it doesn't
  understand (backslashes, percent-encoding, IPv6, userinfo tricks) is rejected —
  the place where two parsers disagree is the escape hatch of a permission check.

**The only thing not enforced is the tools the engine installs**: yt-dlp is a
separate process and isn't jailed. Every list and consent output writes this.

### 5. Tools: an artifact per platform

`requires[].assets`: a platform key → `{url, sha256}`. The key is
`<os>-<arch>[-musl]`, Rust's `std::env::consts` names, and it comes **from the
target the core was built for** — the system isn't probed at run time. The list of
recognised keys is closed; a typo makes the manifest invalid.

yt-dlp 2026.08.19's release list **was measured** (the GitHub API), and it departed
from the user's "from arm to i386" expectation in two places:

| platform | release |
|---|---|
| linux x86_64 / aarch64 (glibc + musl) | a single file ✓ |
| macOS | a single universal binary (Intel + Apple Silicon) ✓ |
| Windows x86_64 / x86 / ARM64 | a single file ✓ |
| **linux armv7** | **only a zip** (multi-file) — not declared |
| **linux i686** | **none at all** |

On these two platforms the plugin doesn't load, and the status line says "no release
for this platform" — it **doesn't suggest** an install command, because installing
won't fix it. Zip support (for armv7) wasn't added: D-055's rule "a `kind` field was
deliberately not put in"; it's added when the need arises, without breaking the api.

The self-contained binary is **~40 MB**, and the HTTP client's in-memory path cut it
off at 32 MB. The download now **streams** to disk, with the hash computed as it
streams (`ArtifactSource`); the artifact download has no overall timeout but
per-stage limits (40 MB on a slow connection exceeds 30 s). The file name is
`<name>-<version>-<platform>` (+ `.exe` on Windows). The tool is verified again by
its hash on first use — a binary changed after installation doesn't run.

**A measured warning:** yt-dlp 2026.08.19 says `JS runtimes: none` and carries on
with YouTube resolution without a JS runtime, but writes that this is
**deprecated**. It works today (the live tests passed). When it closes, the engine
will have to download a JS runtime too (deno or `qjs`) through the same `requires`
mechanism.

### 6. The isolation trade-off

In api 1 the plugin was a separate process; if it died, the core lived. In api 2 the
plugin is on its own thread, in its own QuickJS runtime, but **in the core's address
space**. Everything JS can do — an infinite loop (cut off even inside `try/catch`,
measured), a memory overflow (a 128 MB cap, turned into an exception), deep
recursion — doesn't bring the core down, and each is tested. The only thing that
could bring it down is a flaw in QuickJS's own C code. In return: no installation,
enforced permissions, and an engine that can go to mobile (iOS doesn't allow
spawning subprocesses — api 1 could never have gone there).

The stage and error names follow: `PLUGIN_HANDSHAKE` → **`PLUGIN_START`**,
`PluginRpc` → **`PluginThrew`** (a message + `main.js:line:column`), and a new
**`PluginContract`** ("the plugin said no" and "the plugin's code can't agree with
the engine" are different diagnoses).

### 7. Torrent was parked

`crates/headshell-plugin-torrent` and `plugins/torrent` → `parked/`, in the
workspace's `exclude`. It used the core's `plugin::protocol` types, and those types
went away; the code wasn't deleted, it doesn't compile. The open questions of its
return are in `parked/README.md`. `librqbit`'s 179 crates left the workspace lock
too.

### What was invalidated

- **D-050 Q1** (the engine is Python) and the Python part of **Q2** → void. The
  principle "the runtime is the host's job" stands; the runtime is now **inside**
  the host.
- **D-055 §2** (finding the interpreter, `HEADSHELL_PYTHON`) → gone. **§1** (a
  pinned artifact, the hash) and **§3** (the four diagnoses) stand; they grew per
  platform, and a fifth diagnosis was added (`no release for this platform`).
- **D-040** "enforcement later" → enforced for the network; the file permission
  concept went away.
- **K5** was rewritten (PLAN §2).
- **D-047/D-056** (torrent stays a plugin) → torrent was parked.

### Testing

- The engine's 73 unit tests with a real QuickJS and a fake network: permissions,
  wildcards, redirects, the stream address, the network ban while loading,
  timeouts (a loop, loading, inside `try/catch`), a memory overflow, a missing
  export, a wrong return shape, the secret file being `0600` and deleted on
  shutdown, the store, running tools/their hash/their time.
- CLI: in a process whose **environment was emptied completely** (`env_clear`, no
  `PATH`), the plugin answers `provider test echo`.
- Live: SoundCloud 5/5, YouTube Music 5/5 — both really played the audio; yt-dlp
  from the Python-free Linux binary (`yt-dlp_linux`).
- **A clean Linux (the first measurement):** an `archlinux:latest` container — no
  `python3`, `python`, `yt-dlp` or `node`; nothing was installed except the
  binary's own dependency, `alsa-lib`. `plugin approve` → `plugin install ytmusic`
  (the engine downloaded `yt-dlp_linux` and verified its hash) → both providers
  "available" in `provider test` (yt-dlp 2026.08.19 answered from the engine's
  binary) → `play` passed the search and **the stream resolution**, and stopped at
  `PLAYBACK_OUTPUT` (the container has no sound card; the player doesn't open the
  audio before resolving the source).
- The workspace: 400 tests pass, 3 skip themselves (no AcoustID key), 1 is ignored.
  `playback_local::a_real_file_plays…` fails **intermittently** on this machine
  (an ALSA `snd_pcm_avail_delay` I/O error); at the `HEAD` before the change it
  failed 1 in 5 runs too — not this round's flaw, a separate job.

### Left open

- **Testing on clean machines** (the user's next step): `plugin install ytmusic` +
  `play` on Windows and macOS in an environment without Python/yt-dlp installed.
  Linux's container measurement was done (above); a real desktop (with a sound
  card) and the Windows/macOS binaries were never run.
- yt-dlp's need for a JS runtime (above).
- linux armv7 (a zip) and linux i686 (no release).
- Torrent's return to the new engine (`parked/README.md`).

### D-069 addendum — the artifact download was cut off at 30 seconds; so was the audio stream (2026-09-25)

The CLI in the `v0.0.2-beta` draft was tried in a Python-free Debian 12 container.
`plugin install ytmusic` failed with this error (then in Turkish): `the source could
not be reached: the download was cut off at 1307282 bytes: timeout: receive
response`. At the time, downloading from GitHub was slow (~43 KB/s). The download
client's design was right — no overall time limit, 15 minutes for the body — its
implementation wasn't.

**The cause is in ureq 3.4; not in its documentation but in its source**
(`timings.rs`): a stage's time limit is checked in the next stage too, and counted
from the moment that stage *ended*. So `recv_response` (30 s) limited the body too,
from the moment the headers arrived: every artifact that didn't download in 30
seconds was cut off. yt-dlp is ~35 MB; fitting it into 30 seconds needs ~1.2 MB/s
(~10 Mbit/s). In CI and in the first container measurement the connection was fast,
so it didn't show.

A second thing came out in the same investigation: in ureq, a deadline that has
passed isn't an error but becomes a 1-second read limit (`NextTimeout::not_zero`).
No total time limit cuts off a body that keeps flowing; the "15 minutes" budget was
never enforceable. The test checking this failed one run in three, when the
deadline coincided with the moment a byte arrived.

The fix (`net::ureq_client::long_body`):

- Waiting for the headers is limited with `timeout_send_request`: the same rule
  carries it to waiting for the headers, not to the body.
- The body has no total time limit. Since `recv_body` is counted afresh on every
  read, it's **a silence limit** (30 s). The size caps are enforced while reading
  (128 MB for an artifact, 256 MB for a stream). A slow but flowing download is no
  longer cut off; a dead connection fails within 30 seconds at the latest.
- **The audio stream moved to the same client too** (`for_streams`). Since
  v0.0.1-beta it had used `UreqClient::new()`, and the 30 s overall limit covered
  the body too: a track that didn't download entirely in 30 seconds (a FLAC from a
  remote Subsonic, anything over a slow connection) would be cut off in the middle.
  Not a bug this release introduced, but its root and its fix are the same.

Testing: three tests against a local server — a slow body, a server that never
answers, a body that goes silent in the middle. With the old settings the first
fails, with exactly the production error (`Timeout(RecvResponse)`); the other two
test that the fix didn't break the protections. On the real network, the fixed
build installed yt-dlp in 51 seconds and verified its hash. If the rule changes
with ureq, these tests fail.

## D-070 — Platform portability: Windows, macOS and Unix-likes; tests leave no trace on the machine

**Date:** 2026-09-24 · **Status:** APPLIED (2026-09-24)

**Question:** The user: *"For it to work on both DOS (Windows) and Unix/Unix-like
systems, check both the files and, in general, the files you write to `/tmp`,
because we may have become dependent on the machine."* Known before the scan: the
only platform proven to work today was x86_64 Linux; the macOS and Windows packages
built but had never been opened.

### Decision (the user's) — four questions

1. **The Windows data directory:** `%LOCALAPPDATA%\headshell` (not Roaming: 40 MB
   tools and a growing database shouldn't be carried in a roaming profile).
2. **The macOS data directory:** `~/Library/Application Support/headshell` (the
   platform's convention; since it was never run on macOS, there's no data to move).
3. **Delete the test leftovers in `/tmp`** — deleted.
4. **The BSDs:** *"Let them be buildable, but written down as canary or not
   tested."*

### What the scan found — the code

| # | Finding | Its effect | Fix |
|---|---|---|---|
| 1 | The data directory only from `HOME` | A standard Windows doesn't define `HOME`: the CLI gives an error, **the desktop closes without saying anything** (the release build has no console; `stderr` goes nowhere) | A place per system; a pure function whose branches for all three systems are tested on every machine |
| 2 | `HEADSHELL_MUSIC_DIRS` split on `:` | `C:\Müzik` is cut in two | `std::env::split_paths` (`;` on Windows) |
| 3 | The usual music directory from `HOME` | On Windows local music is never found | `%USERPROFILE%\Music`, on macOS `~/Music` |
| 4 | The Subsonic salt from `/dev/urandom` | On Windows always the weak fallback (saying so) | A `RandomState` keyed with the operating system's randomness |
| 5 | The startup error only to `stderr` | A silent close on Windows — a K9 violation | The error in a window; on every system |
| 6 | The artifact file's `.exe` from the machine it was built on | The same artifact's name would change by machine | From the platform key |

**The startup error window.** If the core can't open, a window showing only
`startup-error.html` is opened with the same Tauri context, the main window being
closed. The text travels in the address's `#` part (percent-encoded; encoded by
hand because URL parsers strip line breaks): no IPC, no core, and the page's CSP
doesn't allow inline scripts. Verification: the `url` 2.5.8 Tauri uses keeps the
encoded fragment exactly in both the `tauri://localhost` and the
`http://tauri.localhost` (Windows) forms; decoding it back is tested with
`decodeURIComponent` in QuickJS. The window really opened on this machine
(760×460), but its content couldn't be captured with `xwd` — WebKit's drawing read
as black both ways. No visual verification was done.

### What the scan found — machine dependence

- **The tests were filling up `/tmp`.** 1,100 directories, 1.2 GB — and on this
  machine `/tmp` is a tmpfs, that is, memory (it was 60% full; 9% once deleted). No
  test deleted the directory it opened; each YouTube Music test copied the 40 MB
  yt-dlp into its own directory and left it. Now the unit tests open self-deleting
  directories with `crate::test_support::TempDir`, the integration tests with
  `tests/support` (in a failing test too: `Drop` runs on a panic); the integration
  tests' root is Cargo's `target/tmp`. Measured: 0 entries in `/tmp` before and
  after a full run.
- **The yt-dlp cache could hide a dead address.** If the lasting cache in `/tmp`
  existed, a download was never attempted; if the pinned address died (orphaned,
  D-055), this machine would be green and a clean machine red. The cache is now in
  `target/tmp`, its hash is verified again on every run, and the address is asked
  whether it's alive (without reading the body). The tests don't copy the 40 MB;
  they make a hard link.
- **A test needed `node`** (`anchor_parity_js`, D-033). The rule was right ("if
  there's no node, don't skip, fail"), but its price was making the machine running
  the test install a runtime. `anchor.js` is now evaluated in the embedded QuickJS;
  the test still never skips and asks nothing of the machine. It was measured to
  fail when a deliberate drift (`Math.floor` → `Math.round`) was put in (`expected
  100099, found 100100`). `rquickjs` entered the desktop crate only as a test
  dependency; no new crate entered the tree.
- **Line endings weren't preserved.** A Windows checkout with `core.autocrlf` on
  would break the snapshots. `.gitattributes`: LF everywhere, binary fixtures without
  conversion. The repository had no CRLF files; the bytes didn't change.
- **The Linux packages wanted glibc 2.39.** They were built on Ubuntu 24.04;
  `v0.0.1-beta` doesn't open on Debian 12 with `GLIBC_2.39 not found` (measured).
  The release pipeline was moved to 22.04. Measured: the CLI built in a 22.04
  container wants at most `GLIBC_2.35`, and it opens on a Python-free Debian 12 and
  runs the live SoundCloud plugin with `echo`.
- **The macOS package was arm64 only** (D-066). The `.dmg` and the CLI archive are
  now a universal binary, cross-compiled on the same runner. Not verified until the
  first `workflow_dispatch` run.

### The BSDs — buildable, NOT TRIED (canary)

`rquickjs-sys` carries no ready bindings for FreeBSD/NetBSD/OpenBSD/DragonFly; for
those targets the same dependency was written once more with the `bindgen` feature,
and the bindings are generated at build time with `libclang`. Measured:
`bindgen`/`clang-sys` show up only in the FreeBSD target's tree, not on
Linux/Windows/macOS. 4 build-time packages entered the lock (bindgen, cexpr,
clang-sys, prettyplease). It couldn't be built from here (no BSD system headers and
no `libclang`); whether audio (cpal) and the desktop shell work there is unknown.
In the documents: "not tried (canary)".

### Testing

- Linux: `fmt` and `clippy` clean; **410 tests passed**, 1 failed: the intermittent
  ALSA test (`playback_local`, recorded in D-059), while a container build was
  filling the CPU. On its own it passed 5 of 5 runs.
- **A cross-check against the Windows target** (`x86_64-pc-windows-gnu`, Zig 0.16.0
  for the C code, downloaded with its hash verified): the core and the CLI, tests
  included, `clippy -D warnings` clean. The first compile of the `cfg(windows)`
  code. The tests couldn't be run (no Windows/Wine); the desktop crate can't be
  built this way (it needs the Windows resource compiler).
- A Windows and macOS job was added to CI (clippy + tests). **It hasn't run yet** —
  for it to run, the change has to be pushed.

To repeat the Windows cross-check (no MinGW needed): `rustup target add
x86_64-pc-windows-gnu`, download Zig, give as `CC_x86_64_pc_windows_gnu` a wrapper
that filters out the `--target=x86_64-pc-windows-gnu` argument `cc` passes and calls
`zig cc -target x86_64-windows-gnu`, then `cargo clippy --target
x86_64-pc-windows-gnu -p headshell-core -p headshell-cli --all-targets`. The
verification tools were removed from the machine once the job was done.

### What was invalidated

- **D-033**'s ruling "if there's no `node`, the test fails": the rule was kept,
  `node` went away.
- **D-066**'s note "the `.dmg` is arm64 only": a universal binary.
- The single source of the data directory was `HOME` (the §0 era): it's per system
  now.

### Left open

- Trying it **by hand** on Windows and macOS: the window, audio,
  `plugin install ytmusic`.
- The first run of CI's Windows/macOS job.
- When the Ubuntu 22.04 runner retires, the glibc floor should be kept in a
  container, not left to the runner's version.
- A real build on the BSDs.

### Addendum — the first run of Windows CI hung; the same trap was in the Unix terminal too (2026-09-24)

The Windows job passed clippy in three minutes and stayed locked in the test step
for over an hour; macOS finished the same job in seven minutes. The log doesn't open
until the job ends, and the local token couldn't cancel the run either (403).

When `headshell provider add` can't find the password in `HEADSHELL_PASSWORD`, it
opens the no-echo prompt; the prompt uses crossterm's raw mode. crossterm opens the
terminal not from standard input but directly — on Windows the console buffer
(`CONIN$`), on Unix `/dev/tty`. The test tied the child's standard input to nothing
and counted that as "no terminal"; but the child still reaches its parent's
terminal. On the Linux and macOS runners there's no controlling terminal: raw mode
fails right away and the CLI says what to do — which is what the test expected. On
the Windows runner the processes have a console: raw mode opened, and
`event::read()` waited for a key that would never come. The `--tui` test carried the
same assumption.

There's no log from the Windows side; the cause was worked out from the code. The
same mechanism was reproduced on Unix: under `script` (a pseudo-terminal — the
situation of a developer running `cargo test` from a terminal), the test didn't
finish in 60 seconds and was killed. So these two tests hung in a developer's
terminal too; the reason they passed locally was that the run was done from a shell
without a controlling terminal.

**The product behaves correctly** — asking the user at the terminal for a password is
the right thing (ssh and sudo ask from the terminal too, even when standard input is
a pipe); for scripts there's `HEADSHELL_PASSWORD`. What was wrong was the test's
assumption. The fix:

- **Windows:** the CLI tests start the binary detached from the console
  (`DETACHED_PROCESS`); the precondition is always met.
- **Unix:** there's no safe std way to detach the child from the terminal
  (`CommandExt::setsid` is still unstable in 1.98, `pre_exec` needs `unsafe`, and
  the workspace forbids that). If `/dev/tty` can be opened, the two tests write the
  reason and **skip**; in CI there's no controlling terminal, and there they really
  run. In a pseudo-terminal both skipped; in a shell without a terminal both ran and
  passed.

The CI and packaging jobs got time limits (`timeout-minutes`): the next hang fails
without waiting 6 hours, and its log opens. The hung run (36041064570) can't be
cancelled with the local token; it will close with GitHub's 6-hour limit or by being
cancelled from the interface.

## D-071 — The plugin catalog: the plugins are in `headshell/plugins`, and the app reads the list from there

**Date:** 2026-09-25 · **Status:** APPLIED (2026-09-25)

**Question:** The user: *"the plugins are sitting embedded right now. Instead, let's
keep them in a repo like obsidian does, and have the plugin list come from the
plugins in that repo."* The plugins weren't embedded in the binary, but they sat in
`plugins/` in the main repository, and the user copied them into the data directory
with `cp`. Updating a plugin — like raising the pinned version when YouTube breaks
yt-dlp (D-048) — meant an app release or copying by hand. Three sub-questions were
asked: the repository's structure, whether consent would cover the tools, and where
the live tests go.

**Decision (the user's):**

1. **A single repository: `headshell/plugins`.** The plugins live there as
   directories, and `index.json` is generated from them. Obsidian's exact model (the
   list in one repository, every plugin in its own repository) wasn't chosen. The
   user first asked, "is there any problem on the GitHub side with opening this many
   repos?" The answer: there's no obstacle on GitHub (the number of public
   repositories in a free organisation is unlimited, Actions are free for public
   repositories, and download traffic doesn't depend on the number of
   repositories); the cost is in maintenance (separate CI, separate secrets and
   per-repository access with a fine-grained token in every repository). In
   Obsidian the reason for separate repositories is that the plugins belong to other
   authors; here both have the same owner. Since the index format leaves the file
   address free, this choice **is reversible**: if a plugin moves into its own
   repository later, the client doesn't change.
2. **Consent keeps covering only the network permissions.** My advice was that a
   tool change (yt-dlp's address or hash) should ask for consent again too — an
   update from the catalog can silently change a binary that isn't jailed. The user
   chose today's state. In return: the `update` output writes every tool change
   **separately** ("tool changed: yt-dlp 2026.08.19 → …, no consent asked"); it
   isn't silent.
3. **The live tests stay in the main repository and install the plugin from the
   live catalog.** A broken release in the catalog turns the main repository's CI
   red — D-043's principle of "the day the plugin breaks is learned that day".

**Decisions told to the user, with no objection:** the catalog is read only on an
explicit command; every file is verified with the sha256 in the index; the file
addresses are pinned to the version tag; `plugin install <name>` downloads from the
catalog if the plugin isn't on disk (a superset of today's behaviour), and
`catalog`, `update`, `remove`, `index` come alongside it; it never writes over a
plugin installed by hand or changed locally; no new dependency.

### The index (schema 1)

```json
{ "schema": 1, "url_template": "…/refs/tags/{name}-{version}/{name}/{path}",
  "plugins": [ { "manifest": { …plugin.json… },
                 "files": [ { "path": "main.js", "url": "…", "sha256": "…" } ] } ] }
```

The entry carries the manifest **itself**, not a summary: the permissions shown in
the catalog and those of what's installed come from the same source, and the
downloaded `plugin.json` is compared with it. The file list is exactly
`plugin.json` + the script; `origin.json` and `state/` are the engine's names, and a
file in the catalog can't write to them. The schema rule is `api`'s: adding doesn't
raise it, and an unknown schema isn't read and says "update headshell".

The index **isn't written by hand**: `headshell plugin index <catalog-repo>` passes
every manifest through the same validation as installation (`PluginManifest::parse`
— `load` was built on top of it; the catalog doesn't write its own rule; D-057's
"copied logic drifts") and computes the hashes. `--check` writes nothing; if the
index isn't up to date, it says **which plugin** is different.

### Why a tag, and not `main`

The index sits on `main`, the files on the `<name>-<version>` tag. Measured:
`raw.githubusercontent.com` returns `Cache-Control: max-age=300`. With file
addresses pinned to `main`, within five minutes after a new version is published, a
client could get the new index with the old file (or the reverse), and installation
would say "hash mismatch" — a scary, temporary error the user can't fix. Since a
tagged address never changes, the cache can't break it. Both the
`refs/tags/<tag>/…` and the `refs/heads/<branch>/…` forms were measured (200); a
nonexistent tag is a 404. That's why `{version}` is required in the template.

A version's tag isn't moved: installed copies recorded that version's hash. The
catalog repository's CI checks on every push that the tag exists and that the files
are the same as the tag's, then really installs every plugin from the published
catalog.

### Whose files get touched

A plugin installed from the catalog has an origin record in its directory
(`origin.json`: the catalog, the version, file → sha256). An update touches only a
plugin that has a record and whose files are the same as the record; the others get
a "skipped" that writes the reason. A directory placed by hand (a developer's
working copy, maybe a link) shouldn't be deleted by an update. A file left by an
update that stopped halfway — carrying the **new** hash from the catalog — doesn't
count as a local change, so that the next `update` can complete it.

Installation is prepared in the data directory, **outside** the plugin directory,
and put in place with a single `rename`: an installation that stops halfway doesn't
leave a half plugin, and discovery doesn't take the prepared directory for a plugin.
An update is atomic file by file (the script, the manifest, the origin record last)
and doesn't touch `state/` (the plugin's `host.storage`). Removal first forgets the
consent, then deletes the directory — the reverse order could leave an approved but
half-deleted plugin — and if it's a link, it removes only the link. Secrets aren't
deleted; the names of those left are in the report.

### The network

The catalog is read only on an explicit command: `plugin catalog`,
`plugin install` (if the plugin isn't on disk) and `plugin update`. On the desktop, a
"fetch the catalog" button; no request when the panel opens. No background update
check either. `--online` isn't required: downloading is the command itself (D-055's
reasoning). `HEADSHELL_PLUGIN_INDEX` gives another catalog; the address must be
`https://`, plain `http` only for `127.0.0.1`/`localhost` — since the index carries
the hashes, it's the root of trust, and anyone on the path can change what comes
over plain HTTP.

"I couldn't reach the catalog" (`NETWORK_REQUEST`) and "the catalog is broken / the
file is missing / the hash doesn't match" (a new stage, `PLUGIN_CATALOG`) are
different diagnoses (K9); a file's 404 is reported not as a network problem but as
the catalog maintainer's flaw.

### A flaw found: a plugin approved on the desktop couldn't be played

The desktop shell builds the provider registry at startup, and only the server
commands refreshed it. `plugin_approve` / `enable` / `disable` / `forget` didn't
refresh it: a plugin approved from the interface couldn't be played until the app
was reopened. Since the catalog's install → approve → play flow goes exactly through
this path, it was fixed: every command that changes a plugin's state (including the
new `install`, `update`, `remove`) refreshes the registry. The playing track isn't
affected; the player keeps its own copy of the registry.

### The public API

- `Session::install_plugin(name)` → `install_plugin(name, http)` and `async`:
  downloading from the catalog takes the HTTP client from the caller
  (`add_server`'s pattern, a K7-compatible `Arc<dyn HttpClient>`). The CLI and the
  desktop were updated; there's no mobile yet.
- `PluginInstallReport` gained three fields: `fetched` (what came down from the
  catalog; `null` = the catalog wasn't read), `permissions`, `consent`.
- New: `plugin_catalog`, `update_plugins`, `remove_plugin`, `build_plugin_index` and
  their reports.

### What moved

- `plugins/soundcloud` and `plugins/ytmusic` → `headshell/plugins`, their history
  kept with `git subtree split`. A `.gitattributes` (LF) was put into the catalog
  repository: a file turned into CRLF on Windows would produce a different hash.
- `docs/writing-plugins.md` (then `docs/eklenti-yazma.md`) §9–10 (the usage notes
  of SoundCloud and YouTube Music) → the plugins' own READMEs; in the guide their
  place was taken by the catalog section. Every fact in one place (D-051).

### Testing

- `plugin::catalog`: 27 unit tests, a fake network and a real disk — if the hash
  doesn't match, nothing is written and no temporary directory is left; if the
  manifest contradicts the index, it isn't installed; broken entries (api 3, a path
  leaving the directory, plain http, a file writing to `state/`, the same name twice)
  are reported with their reasons without hiding the others; an update keeps
  `state/`; plugins placed and changed by hand are skipped; a half update is
  completed; a plugin pulled from the catalog is said so; only the link itself is
  removed; a name carrying `../` can't delete outside the directory.
- CLI: end to end with the real binary against a server opened on `127.0.0.1` —
  `plugin index` → `--check` → `catalog` (a snapshot) → `install` → `approve` → the
  engine answers → a new version (`--check` says what's out of date) → `update` →
  `remove`.
- The real plugins were installed from a local mirror before publishing: after
  approval SoundCloud is "available" on the live service (the client_id through
  discovery), and YouTube Music was installed, with the engine downloading and
  verifying yt-dlp.

### Left open

- The catalog repository's CI builds headshell from source; once `plugin index` is
  in a release, it should switch to the release binary.
- There's no channel telling the user about a plugin pulled from the catalog
  **because it was malicious** (Obsidian's `community-plugins-removed.json`). Today
  it only says "installed from this catalog, but no longer on the list".
- No review process for third-party plugins was written; the catalog repository's
  README has only the rules.

## D-072 — The interface redesign: a full-height sidebar, a motion layer, two additions in the shell

**Date:** 2026-09-25 · **Status:** APPLIED (2026-09-25)

> **D-075 (2026-09-26):** "Now playing" became a sheet that rises out of the player
> bar; the sidebar row opens it. The "playing" state, not seen here, was seen there.

**Question:** The user: *"redesign the interface to fit the recent changes and the
coming ones"* (with Apple's fluid interface principles). Three things were asked:
the skeleton, how far to touch the Rust side, the way to verify.

**Decision (the user's; the recommended option in all three):**

1. **A full-height sidebar** with four groups: *listen* (now playing, library),
   *history* (stats, sleeve, import), *sources* (providers, plugins), *system*
   (appearance, diagnostics). The counter-option was keeping today's frame and only
   renewing the surface.
2. **Small additions in the shell**, without touching the core and the CLI: a
   `diag_text` command and a colour preview in the theme list.
3. **Verifying by opening the window**, with a temporary data directory.

### The interface was behind the recent changes

- **After D-069 and D-071 the plugin panel was three sections stacked on top of each
  other** (installed, catalog, secrets), and every plugin had five buttons
  regardless of its state: "approve" on an approved plugin, "install the tools" for
  an artifact whose platform isn't supported. Now there are tabs, the buttons come
  from the state, after installation it switches to the "installed" tab where the
  result lives, and the number of plugins waiting for consent or installation from
  the user is in the sidebar.
- **The interface was below what the CLI shows.** Statistics had no albums, years or
  durations; importing had no fingerprint and local-key links; search had no album
  and duration. Resolution and `diag` printed raw JSON — while the documentation of
  `DiagReport::render()` says "the GUI will show the same text". All were added; the
  data from the core, the formatting from the core's own text.
- **The getting started card said "give the Spotify/Apple/Google export archive"**;
  today only Spotify's export is read (`import::parsers`). The text was fixed, and
  the plugin catalog is there too, as a music source.
- **The controls were emoji** and didn't take the theme's colour (they're drawn with
  the engine's colour font). In their place, an icon set painted with
  `currentColor`.
- **The drawing loop wrote to the DOM on every frame even while paused.** Now it
  runs only while playing and writes only when something visible changes.
- **The secret form's example said `acoustid`**; the core's namespace is
  `identity:acoustid` (`acoustid::SECRET_NAMESPACE`). A mistake left over from the
  old interface; the skip message in a test run showed it.

### The skeleton: a future section is a new row in a group

Phase 4's rooms go into *listen*, Phase 5's social graph into *history*, mods into
*system*; the flat list doesn't grow. Today there's no placeholder for any of them
(K10). Sleeve came out from under the statistics into its own section: the project's
distribution hook (D-004) was sitting below a scroll. The shortcuts are
`Ctrl`+`1`…`9`; when the window drops below 1000 px, the sidebar collapses to icons.

### Motion: `ui/motion.js`

The web counterpart of Apple's fluid interface principles (WWDC 2018), without
dependencies: a two-parameter spring (damping ratio + response, a closed-form
solution), interruptibility (a new target starts from the on-screen value and
velocity), velocity handoff, momentum projection, rubber-banding. Where it's used:
the sidebar's selection indicator, segmented controls, directional section
transitions, notices (they come from below, close when dragged to the right, and the
rest slide to make room), the shortcut window (grows from the button that opens it),
the statistics bars. Selection happens the moment the pointer goes down; buttons
shrink the moment they're pressed.

- **`transform` and `opacity` only** (D-028). A review found a `color` transition on
  `.nav`, and it was removed.
- **The duration is the theme's:** the spring response is three times
  `--headshell-duration`. `0ms` means nothing moves (High Contrast);
  `prefers-reduced-motion` turns off positional motion, and opacity stays. No new
  token.
- **Deliberately no translucent, blurred material.** A `backdrop-filter` over
  scrolling content means re-blurring on every frame; D-028 didn't measure it on
  WebKitGTK, and it wasn't assumed without measuring. The top bar is flat; when
  content goes under it, a scroll-edge shadow appears.
- **An element driven by a spring must not have its own `transform` in the
  stylesheet.** An element at rest drops its inline transform (so the layer doesn't
  blur the text); a bar with `scaleX(0)` in the stylesheet fell back to it and
  vanished once it settled at full size. The rule was written into `motion.js`.

### Two additions in the shell

- **`diag_text`** — the core's `render()` text; `diag` keeps giving the same report
  as JSON, folded in the interface. `sleeve_svg`'s reasoning: if the formatting were
  done here or in JS, the block pasted when reporting a bug would diverge from the
  CLI's.
- **`ThemePreview`** — the theme's `:root` tokens, and for the ones it doesn't write,
  the default theme's (`style.css` itself, with `include_str!`). A conditional value
  inside `@media` isn't counted; comments and `!important` don't mix into the value.
  A theme isn't a CLI concept (§3.3), so it has no subcommand.

### The theme contract didn't change

The fourteen tokens and their values are the same; no class was removed from
`CONTRACT_CLASSES` or changed its meaning — at `api` 1. New classes **weren't
taken** into the contract: the promise given to theme authors didn't grow. The
layout changed: the sidebar is full height, `.topbar` sits above the content column,
and `.state` is a dot instead of a glyph (its colour still changes with
`.state.playing { color }`). An extended theme that assumes positions (D-038, no
guarantee) may feel this; it was written into the theme guide.

### Testing

- `tests/motion_js.rs` — 9 tests, `motion.js`'s pure arithmetic in the embedded
  QuickJS (`anchor_parity_js.rs`'s way, D-070). It was measured that the tests
  aren't empty: three of them caught a deliberate sign error put into the
  derivative of the spring velocity.
- `tests/ui_contract.rs` +3 — every icon used is defined in the set (an undefined
  icon gives no error; it draws an empty square); no script writes a string as
  markup (the descriptions in the catalog are remote data, D-071; written with
  `innerHTML` they'd bring markup into a page with access to IPC); no inline `style`
  in the pages (the CSP `style-src 'self'` silently ignores it).
- `src/theme.rs` +5 — the preview.

### Verification: the host's screen was locked; WebKitGTK in a container

The first attempt opened the window in the XFCE session, but the image was black
from start to end: `xset` said "Monitor is Off" and `light-locker` was running — in
the locked session X never drew the new window (the user: *"the MacBook's screen is
off"*). Verification moved into a Debian 13 container: the same WebKitGTK as the
host (2.52.6), Xvfb, a null ALSA device, the same binary, a temporary data
directory; navigation with `xdotool`. The container and the image were deleted
afterwards.

Seven flaws showed only in screenshots and were fixed: the tabs in the sidebar were
centred (the general `button` rule's `justify-content`); the year bars had zero
width (`align-items: center`); a bar that settled at full size vanished (the rule
above); `.row label` shrank the search box with an icon to 13 px; in the light
theme, the brand mark's record was the same colour as the background; new listens
left the statistics and the card stale; the consent notice printed the core's wire
value ("approve").

Seen end to end: the keyboard (`Ctrl`+number, `/`, `?`, `Shift`+`Enter`, `Esc`), the
queue and playback (listen recording included), search, error and info notices,
dismissing by dragging, the three themes, a narrow window, the live catalog →
install → approve.

**Not seen:** the "playing" state itself — the null device doesn't wait in real
time, tracks end hundreds of times faster, and the state falls to "buffering";
Windows (WebView2) and macOS (WKWebView); the fallback colours for old WebKit that
doesn't know `color-mix` exist only in the code.

### Deliberately not done

- **Seeking and volume:** the core has no command for them; adding a capability to
  the interface first requires a CLI subcommand (D-033).
- **The landing page** (`docs/index.html`) is black and blue; the app and the icon
  are amber. The page is a placeholder and will be rewritten after the first tag
  (D-068); the two identities should be merged that day.

## D-073 — Everything in English: code, text and documents; Turkish snapshots of the documents

**Date:** 2026-09-25 · **Status:** APPLIED (2026-09-25)

> **D-074 (2026-09-26):** This decision became invariant rule K11 (PLAN.md §2).
> Its text moved there from CLAUDE.md's code conventions; the consequence below
> that calls CLAUDE.md the owner no longer holds.

**Question:** While the organisation's profile README was being written, the user
said: *"make the language English, guaranteed. English for every repo, please. In
fact, sit down now and rewrite everything written in Turkish in English."* And on
publishing: *"when it's done, push all of it, and also save the Turkish ones as
[file_name].tr.[extension]."*

**Decision:**

1. **Everything is English** — identifiers (already so since D-036) and text alike:
   comments and doc comments, CLI help and output, the interface, error and
   diagnostic messages (the `ADIM:` prefix is now **`STEP:`**), test names and test
   data, fixtures, the workflows, the packaging, the documents. This replaces
   D-036's text-language half. Both repositories: `headshell/headshell` and
   `headshell/plugins`.
2. **The documents keep a Turkish snapshot next to them** as
   `<name>.tr.<ext>`: README, CONTRIBUTING, CLAUDE, PLAN, DECISIONS, the plugin
   guide, the subdirectory READMEs, the landing page and the sample card. Every copy
   carries a note at the top: the English text is canonical; the copy is as it
   stood on 2026-09-25, and keeping it current isn't guaranteed. The README and the
   landing page link the two languages to each other; inside a Turkish copy, links
   to other documents point to their Turkish copies, so the anchors keep working.
3. **No `.tr` copies of code.** A `tests/*.tr.rs` would be built by cargo as a test;
   code has a single language. The `.desktop` entry keeps Turkish the freedesktop
   way (`Comment[tr]=`), not as a separate file.
4. **Historical commit messages stay as they are.** Rewriting history would change
   every hash, and the release tags point at them; new commits are English.
5. **Real data stays as it is:** artist and track names (Şebnem Ferah, Müslüm
   Gürses, Ezhel's Geceler), the Turkish title deliberately in the accuracy set
   ("a Turkish title not in the catalog"), the Turkish characters the normaliser is
   tested with, and the `Müzik` folder the music directory search looks for.

**Reasoning:**
- The line D-036 drew — "the user reads the text, so it's Turkish" — assumed a
  Turkish-speaking audience. The project is public (D-002), its plugin catalog
  invites authors we don't know (D-071), and the AUR packages had already split off
  into English (D-066's "known and accepted inconsistency"). A reader without
  Turkish couldn't read a single error message, the plugin guide or the reasoning
  in this log.
- D-036's own argument for identifiers — two languages in one line make the reader
  pause — held for text too: an English identifier in a Turkish sentence in a
  Turkish comment above English code.

**How it was done, and what the doing found:**
- **The code was translated with tooling that can only change text.** A lexer masks
  everything outside string literals and comments and verifies, file by file, that
  the masked code didn't change and that every format placeholder survived. Then a
  normalised comparison of every changed Rust file against `HEAD` — strings emptied,
  comments dropped, both sides through rustfmt — showed only the intended changes:
  renamed identifiers and `tracing` field names, test data, rustfmt re-wrapping and
  the few changes English itself needed (below).
- **English needs plurals; Turkish didn't.** "4 çalma" is "4 plays" but "1 play". A
  `count(n, noun)` helper in the CLI and `countOf(n, noun)` in the interface; a test
  locks the singular. The label columns in the CLI output got wider.
- **The card speaks English.** Month names, `since 1 January 2023 (2 years)`, a
  comma as the thousands separator (a test was renamed to
  `thousands_separator_is_a_comma`). `docs/sample-card.svg` was regenerated from the
  fixture archive with the English core; the Turkish card stays as
  `docs/sample-card.tr.svg`.
- **Looking for Turkish letters isn't enough.** Plenty of Turkish is plain ASCII:
  `tracing` field names like `dosya`, `hata`, `yol`, `surum`, `yeni`, test values
  like `kapali`, `yanlis`, `sir`, `karma`, a test domain `kotusndcdn.com`. They
  didn't show in a search for `çğıöşü`; a scan of every word against an English word
  list found them, and a second scan of the identifiers alone (code without its
  strings and comments) confirmed nothing was left. The temporary suffixes became
  `.downloading` and `.installing`.
- **An identifier renamed in two repositories is a release order.** The SoundCloud
  plugin's health text changed (`client_id source: secret | cache | discovery`), and
  a live test in this repository asserts it while installing the plugin from the
  live catalog. So the catalog went first — soundcloud 0.2.1 and ytmusic 0.4.1,
  tagged and indexed — and this repository after it; in the reverse order, CI would
  have tested the new assertion against the old plugin.
- **Renamed files:** `docs/eklenti-yazma.md` → `docs/writing-plugins.md`,
  `docs/ornek-kart.svg` → `docs/sample-card.svg`, and in the spike
  `ESIKLER.md` → `THRESHOLDS.md`, `SONUC.md` → `RESULTS.md`,
  `kontrol.py` → `check.py`, `olc.sh` → `measure.sh`, `varyant.sh` → `variant.sh`.
  The landing page's anchor ids became English (`#felsefe` → `#philosophy`, …). The
  Makefile's targets and the plugins' identifiers were already English.
- **A flaky test was checked, not assumed.** The ALSA playback test
  (`playback_local`, D-059) failed more often on the translated tree than on `HEAD`
  at first. The two prebuilt test binaries were run alternately, 20 times in each
  order: whichever ran second — right after the other released the device — failed
  more (13/40 translated, 9/40 `HEAD` in total). The flakiness is the device's, and
  the code-level comparison above shows no playback logic changed.

**Consequence:**
- CLAUDE.md's language rule was rewritten (it's the owner, D-051); PLAN §3.3's
  language note and CONTRIBUTING point here.
- The `.tr` copies are snapshots: when the English changes, they don't. If a Turkish
  text matters again, it's translated again from the English, not patched.
- Nothing in the core's behaviour changed apart from the text it prints: the
  diagnostics report's JSON keys, the IPC contract, the theme tokens and class names
  stayed exactly as they were (they were English already).

## D-074 — The language became an invariant rule: K11, everything in English

**Date:** 2026-09-26 · **Status:** APPLIED (2026-09-26)

**Question:** The user asked: *"add a rule to the main rules saying everything is
written in English, or check whether there is one. Every part added to the code
will be in English."* There was one — D-073 — but as the first bullet of
CLAUDE.md's code conventions, not among the invariant rules: PLAN.md §2 ended at
K10.

**Decision:**

1. **K11 — Everything in the repositories is written in English.** D-073's text
   moved from CLAUDE.md's code conventions into PLAN.md §2, which owns the rules
   (D-051). CLAUDE.md and CONTRIBUTING carry it in their rule index and point to
   it; PLAN §3 and §3.3 point to it; NEVER DO got its line.
2. **The language of the conversation doesn't change it.** Written into the rule
   itself, and into CLAUDE.md as "a common mistake about K11".
3. **What stays as it is didn't change;** the rule lists it: real names, Turkish
   characters that are a test's input, the entries a platform format localises
   itself, the `*.tr.*` snapshots and the link to them, and the commit messages
   from before D-073.
4. **The Turkish snapshots weren't patched.** `PLAN.tr.md`, `CLAUDE.tr.md` and
   `CONTRIBUTING.tr.md` still end at K10: they stand as of 2026-09-25, and a
   snapshot is translated again, not patched (D-073).

**Reasoning:**
- The user asked for it among the main rules, not among the conventions, and the
  difference is real: a broken convention is a note in review; a broken invariant
  rule means stopping and asking before writing (§0.1).
- The pull toward breaking it doesn't stop: the conversations this code is
  written in are held in Turkish. That is why the rule says outright that the
  conversation's language doesn't carry over.
- The text moved rather than being copied. A rule's owner is PLAN.md, and a rule
  written in two files drifts — the reason D-051 exists.

**Checked, not assumed:** no commit came after D-073, so the tree is the one
D-073's two scans cleared. A new scan for Turkish letters outside the `*.tr.*`
files found them only in what K11 keeps as it is: artist, track and album names
in tests and fixtures, the normaliser's character table and its tests, the
encoding and multi-byte tests, the `Müzik` folder, the `Comment[tr]=` entry, the
`Türkçe` language links, and the Turkish quoted in this log.

**Consequence:** nothing in the code changed. The rules are K1–K11; D-051 and
D-073 carry a note.

## D-075 — Now playing became a sheet that rises out of the player bar

**Date:** 2026-09-26 · **Status:** APPLIED (2026-09-26)

**Question:** The user, with two screenshots of YouTube Music: *"I'm going to ask you
for an interface that opens up — I took a little inspiration from YouTube Music,
because every modern player has this: why shouldn't we? Look, wherever I touched the
bottom part, a detailed area like this opened. It also comes up when any song starts
playing. We definitely need this — please do it together with /apple-design."*

**Decision:**

1. **"Now playing" is a sheet over the content row, not a section inside it.** It
   rises out of the player bar and goes back into it; the section under it keeps its
   place and its scroll; the sidebar and the top bar stay usable, as in the
   reference. It opens from a click anywhere on the bar outside its buttons, the
   chevron at the bar's end, the sidebar row, `Ctrl`+`1`, starting a track (the play
   box, the library, "queue all") and a drag up on the bar. It closes from the same
   click and chevron, `Esc`, its handle, any other sidebar row and a drag down. A
   track that starts on its own — the queue moving on — does not open it: the sheet
   comes when the user starts something; it does not take the screen back unasked.
2. **The old "now playing" section was not kept beside it.** The sheet *is* that
   section — the same card, the same queue, the same "getting started". Two places
   showing one queue would have been D-051's drift inside the interface. D-072's
   skeleton stays: the row is where it was, `PANELS` and the shortcuts are unchanged.
3. **The core has no cover art, so a turntable stands in for it** — the app's own
   picture (the brand mark is a record and a tonearm). The record turns while
   playing (33⅓: 1.8 s a turn, fifteen times the theme duration) and stops where it
   is when paused. The tonearm reads the record the way the progress bar reads the
   anchor, from the lead-in groove to the run-out, and goes back to its rest when
   stopped; its angles were computed from the drawing's geometry, not guessed.
4. **The motion is `motion.js`'s.** A critically damped spring that starts from the
   on-screen value; a drag follows the pointer 1:1, the release hands its velocity to
   the spring, and where the momentum carries the sheet decides open or closed. The
   scrim under the sheet darkens with its height and the chevron on the bar turns
   over with it: three springs with proportional starts, which stay in step only
   because the spring is linear — a test holds that. Reduced motion: a fade in
   place. A theme duration of `0ms` (High Contrast): no motion.

**Not done, and why:**
- **Cover art** is a new capability for the core (local tags, Subsonic
  `getCoverArt`, Jellyfin images, plugins), and by D-033 it needs a CLI subcommand
  first. The turntable holds its place until then.
- **Lyrics, related tracks, comments** — the reference's other tabs: the core has
  none of them. The sheet has one list, the queue.
- **Seeking and volume** are still missing, for D-072's reason.
- **No translucent material** (D-072): the sheet is flat; the depth is the scrim and
  the shadow.

**Found while verifying: two faults in the drag the notices use too.**
- **The first move delivered can be far past the threshold.** WebKitGTK merges the
  moves that arrive within a frame, and the drag started measuring from that move: the
  sheet trailed the pointer by 86 px for the whole drag. It now measures from where
  the threshold was passed.
- **A release after a pause was thrown.** A pointer held still sends no moves, so the
  velocity at release was the last move's, and a careful drop on the open side closed
  the sheet. The release is now a sample too.
- Both are locked down in `motion_js.rs` with a fake element, and it was measured that
  the tests are not empty: putting either fault back fails the drag test, and a
  non-linear term in the spring fails the linearity test.

**Found while verifying: two WebKit specifics.**
- `user-select` only works in WebKit with its prefix; a drag up from the bar selected
  the whole sheet. The bar and the turntable carry `-webkit-user-select`, and a press
  on the bar outside its buttons no longer begins a selection.
- WebKitGTK paints overlay scrollbars above everything: the covered section's bar
  showed through the sheet. While the sheet is out, the content under it does not
  scroll; its position is kept.

**Verification:** D-072's Debian 13 container with the host's WebKitGTK (2.52.6), but
with a PulseAudio null sink instead of ALSA's null device. It is paced in real time
(20 s of audio in 20.4 s), so this time the "playing" state was seen — D-072 couldn't.
Seen: opening and closing by click, chevron and `Esc`; frames in mid-motion (the sheet
rising from behind the bar, the scrim, the chevron flattening, the sidebar indicator
moving); a drag held and let go (back open), a flick down (closed), a flick up on the
bar (open); pause (the arm stays on the record), stop (the arm goes to its rest), a
track change (the arm swings back to the lead-in); the three themes; a 900×600
window; the reduced-motion fade — forced in `motion.js` for the check and put back,
since WebKitGTK didn't take the GTK setting in the container.

**The contract didn't change:** no token was added or changed, no class left
`CONTRACT_CLASSES` — `api` 1. The layout changed: `.main` is a two-row grid whose
second row `.content`, the scrim and the sheet share, and `#panel-now` is no longer
inside `.content`. It is written into the theme guide.

## D-076 — Covers: a chain in the core, online only by choice; plugin api 3

**Date:** 2026-09-26 · **Status:** APPLIED (2026-09-26)

**Question:** The user: *"Bring album and single covers to the record on the 'now
playing' page and to every track in the queue."* With a source rule — the provider's
own cover first (Subsonic `getCoverArt`, Jellyfin's `Primary` image, a plugin's);
then the Cover Art Archive through the recording's MusicBrainz release, the official
release matching the track's album preferred; for a local file its embedded picture
first, then an image in its folder — and with rules: every step that goes online
behind `--online`, off by default, and "I didn't look because I'm offline" kept
apart from "I looked and found none" (K9); no Spotify (K4) and no service that wants
an API key without asking; a plugin's cover through `host`, the core never fetching
an address a plugin gives (K5); the queue looked up in the background, the playing
track first, playback never waiting; a cache in the data directory, "not found"
included, an album asked once, MusicBrainz at most once a second with a meaningful
`User-Agent`; a summary of what came from where; a round cover turning with the
record, square thumbnails in the queue, `data:` URIs only, motion only in
`transform` and `opacity`. Ten questions were asked first (§0.2); the answers are the
decisions below.

**Decision:**

1. **A trait method and a capability.** `Provider::artwork(id, size)`, whose default
   says "cannot" (with the stage `ARTWORK_READ`), and `Capabilities::ARTWORK`. The
   local provider, Subsonic, Jellyfin and plugins answer it.
2. **The cache is files and a JSON index**, not the library: the listening history
   is irreplaceable, a cover is not. `<data>/artwork/` holds images named by the
   sha256 of their bytes and an `index.json` with one answer per album (artist and
   album normalised as the identity chain does; a track without an album is its own
   key). "Not found" is believed for 30 days. A torn write (temporary name, then
   rename), two writers (the index is read again and merged before every write) and
   orphans (removed after an hour's grace) are handled, not hoped away.
3. **No new crate; PNG out.** `zune-jpeg` and `png` were already in the tree through
   `resvg`; they sit behind the new `artwork-resize` feature. The label is 320 px, the
   thumbnail 96, downscaled by area averaging — every source pixel counts, where a
   bicubic filter samples and aliases at ten to one. A JPEG that already fits the
   label is kept as it came. Without the feature a cover up to 2 MB passes as it is
   and a larger one is refused with the reason. The limits are 16 MB and 8000 px, and
   the format (JPEG, PNG, GIF, WebP) is read from the bytes, not the name.
4. **Plugin api 3.** The manifest **must** say `"artwork": true` or `false` — a
   missing field is rejected, so "gives none" is a statement, never a guess. A plugin
   with `true` exports `artwork(id, size)` and returns `{ mime?, data }`, the image in
   base64, which it fetched itself with `host.http.request({ …, binary: true })` — the
   new binary mode; read as text, an image is destroyed. A plugin's `null` or error
   hands the track to the rest of the chain. api 2 manifests no longer load: a
   required field changes what a manifest means, it doesn't add to it (the guide's
   §7).
5. **Phase 3, §3.10.**
6. **The desktop's `--online` is `HEADSHELL_ONLINE=1`,** read by the core as
   `HEADSHELL_MUSIC_DIRS` is. *(Since D-082 a saved choice — settings, or `headshell
   online on|off` — with the variable deciding one launch over it.)* It takes `1` or
   `0`; anything else stops startup with the reason — a `true` that went unrecognised
   would leave the user offline without a word. It means what `--online` means in the CLI: covers, but `resolve` and
   `import` go online with it too (an online import is slow — one request a second —
   as it is in the CLI). The environment block shows it.
7. **Where the track lives is not gated:** its tags, its folder, its server and a
   plugin's own service are asked offline too — the track already plays from there.
   MusicBrainz and the Cover Art Archive only online.
8. **Online, the identity chain runs** to find the recording; nothing is written back
   into the history.
9. **The tick carries keys; the interface pulls images.** `TickReport::artwork_ready`
   lists the covers whose answer arrived — found or not, so a row can show why it has
   none — and `artwork_images` gives one size at a time: a queue of thirty albums
   needs thirty thumbnails but one label.

**The flow.** One background thread per session (`headshell-artwork`). The core hands
it the queue when the queue changes; it walks the playing track, then the ones after
it, then the ones already played; what the cache knows is settled at once; an album is
asked once and its other rows get the same answer. The thread may block on the network
and on the MusicBrainz limiter — one request every 1.1 s, shared with the identity
chain — and the core thread never does.

**The CLI and the IPC (D-033).** `headshell artwork "<query>" [--all] | --file <audio>
[--out <dir>]`, with `--json` and a snapshot. The report gives every track's answer —
`found` and its source, `not_found` and what was looked at, `not_checked_offline`,
`failed` and the chain — and a summary: tracks, embedded, folder, provider, Cover Art
Archive, not found, not looked up offline, failed. A failed link makes the run a
partial failure; "not found" doesn't. The desktop mirrors it as `artwork`;
`artwork_queue` and `artwork_images` are the playing queue's side, as `queue` and
`anchor` are.

**Checked in the code before writing, as asked:**
- symphonia 0.6 gives embedded pictures as `Visual`s in the metadata log: ID3v2 `APIC`,
  FLAC `PICTURE`, MP4 `covr` and Vorbis `METADATA_BLOCK_PICTURE`. An MP3's ID3 tag is
  a revision of its own, before the container's, so every revision is read. No reader
  applies `limit_visual_bytes`; the size limit is ours.
- The MusicBrainz search the identity chain uses brings no releases. `releases_of`
  (the recording with `inc=releases+release-groups`) was added.

**Found while verifying:**
- **A namesake recording lent its compilation's cover.** Portishead's "Sour Times",
  album *Dummy*: MusicBrainz has 77 recordings of that name. Given the file's length,
  the identity chain matched one whose only release was a compilation, and *its*
  cover came back for *Dummy* — a wrong answer that looks right. "The release
  matching the album, preferred" became **required**: with an album, only a release
  titled like it is taken (edition suffixes aside), and when the matched recording is
  on none, the album itself is searched by its title and artist. Among the candidates:
  an official one, the artist's own (no secondary type such as compilation or live),
  an album before an EP before a single, the earliest. A unit test on canned answers
  holds the trap — it fails when the rule is taken out — and a live test asks the real
  services.
- **The default `User-Agent` named a stale address** (`github.com/enaimami/headshell`);
  it names `github.com/headshell/headshell` now, and so do the tests'.
- **A Turkish placeholder**, `kapak`, in `fixtures/audio/cover.jpg` — K11's leftover.
- **The cover cache's temporary name was per process.** The worker and an `artwork`
  command are two threads of one process: every write has its own name now. The same
  kind of fault turned D-075's macOS CI red in the artifact installer (the D-060
  addendum).
- **Subsonic's credentials travel in the query string,** and an error that names the
  address carries them into `headshell diag`, which is made to be pasted. The cover
  call names the endpoint instead, on both the status and the network path. The other
  Subsonic calls still name the address — **a separate fix, not made here**: it
  changes existing error texts, and it is asked about first.
- MusicBrainz answered `503` once during a run; the existing retry took it.

**The release order.** The catalog's plugins speak api 2 until `headshell/plugins`
publishes `soundcloud` 0.3.0 and `ytmusic` 0.5.0 (api 3). Until then this repository's
live plugin tests fail — they install from the published catalog, and this core
refuses api 2. Once they are published, the released app (0.0.2-beta, api 2) lists them
as incompatible: its installed copies keep working, updates wait for the next release.
Listing both versions under one name is not a way out: the released app refuses a name
listed twice. The catalog can't go first either: its CI builds headshell from this
repository's `master`, and an api 2 core rejects api 3 manifests. The order is this
change, then the catalog, then a release — and between the first two, this
repository's live plugin tests are red for as long as the catalog takes.

**Verification:** the §3.9 container (Debian 13, the host's WebKitGTK, Xvfb, a
PulseAudio null sink) with a library of seven tracks: covers in FLAC and MP3 tags, a
folder cover, a made-up band without one and a real recording without one. Offline:
the three kinds of cover on the label and in the rows, the two without in placeholders
and the note saying why, the plain label back on the coverless track. With
`HEADSHELL_ONLINE=1`: the real recording's album cover from the Cover Art Archive, the
made-up band "not found", the environment block saying so. The live plugin tests passed
against a local mirror of the catalog carrying the api 3 plugins.

## D-077 — Reading the queue ahead: three tracks made ready in memory, the user's calls first

**Date:** 2026-09-26 · **Status:** APPLIED (2026-09-26)

**Question:** The user, after screenshots in which Enter took 10–16 s to sound with
YouTube Music: *"Write us a nice, solid cache engine. Especially when there are several
songs, as with Shift+Enter, give each of the other songs a probability of being played,
based on statistics, then download a little of the likeliest one and continue from
there. These waits are unacceptable. The likeliest should always be the next song, but
let's try to predict the one after it too. What do you think?"* The waits were measured
before anything was written, and four questions were asked (§0.2); the answers are the
decisions below.

**Measured first** (the host and a Debian 13 container; YouTube Music, "Let It Happen",
a 7.57 MB m4a):

| step | time |
|---|---|
| the InnerTube search | 1.3 s |
| yt-dlp resolving the address | 3.5–5.2 s (1.15 s of it yt-dlp starting; ~2.8 s of CPU) |
| the stream's first byte / first 256 KB | 0.1–0.3 s / 0.26 s |
| the whole file | 2.4–3.3 s |

The 10–16 s of the screenshots was a debug build: there the first yt-dlp call of every
engine start hashes the 40 MB binary with an unoptimised sha256 (~7 s; 0.15 s
optimised) — the plugins section filling late came from the same hash. **The expensive
part is the address, not the audio**: "download a little" is cheap and enough, and what
has to happen ahead of time is the resolving.

**Decision (the user's):**

1. **Memory only.** Addresses and heads are kept in memory; no audio is written to
   disk. A persistent cache would be downloading YouTube's audio and keeping it: the
   appendix draws the legal line at DRM, but a kept copy widens the terms-of-service
   risk, and it costs disk. Local files and Subsonic/Jellyfin are fast already; the
   read-ahead is mostly for plugins.
2. **Three tracks at most** — "three in total is enough": the next one and the one
   after it, always; a third that statistics pick.
3. **The user's calls first.** A plugin runs one call at a time; a call made on the
   user's behalf goes before the calls made ahead of time, and a tool one of those is
   running (yt-dlp) is stopped for it. The alternative, a second engine per plugin for
   the background, would have run in parallel, but `host.storage` is not written for
   two engines: each keeps its own copy and writes it back whole, and the temporary
   name is per process.
4. **This order:** the quick wins first — reuse the address already resolved, take the
   resolving off the core's thread, keep addresses, measure in a release build — then
   the statistics.

**Found on the way: every HTTP stream waited for its whole file.** symphonia 0.6 reads a
seekable source's end before its start when it knows the length: trailing tags (ID3v1,
APE), and for Ogg the last page. `HttpMediaSource` downloads front to back, so a read at
the end waited for the last byte: opening the 7.5 MB m4a took 1.77 s, and a 40 MB FLAC
from Subsonic would wait for its whole download. The source keeps its length for
`seek(End)` — an MP4 whose `moov` sits at the end needs that — but no longer reports it
through `byte_len`. Lost is what symphonia derives from the end: an Ogg stream's length
and the estimate for an MP3 without a Xing header; the provider's metadata gives both.
A test with a server that holds a file's end back for three seconds fails with the old
behaviour (it waited 3.006 s).

**Found on the way: D-024's read-ahead held the core's thread.** `tick` resolved the next
track itself, synchronously: a round took 4.7 s, and in the desktop pause and next waited
behind it. A "next" the user pressed also threw the resolved address away and ran yt-dlp
again.

**The design.**

- **`playback::prefetch`**: one read-ahead thread per playing session
  (`headshell-prefetch`). The player hands it the plan whenever the queue or its
  position changes, and never waits for it. The plan: the next track, the one after
  it, and the third (`Likely`); under repeat one the playing track comes first
  (`Again`), for the gapless repeat.
- **What is kept:** a track's address, how long resolving it took, and the first
  512 KB of its stream. An address is trusted for 8 minutes: YouTube signs for six
  hours, SoundCloud for eleven minutes (both measured), and the core does not know which
  provider signs for how long. A head is kept only while its track is planned.
- **Starts:** a start takes the track if it is ready and begins from its head; if the
  thread is resolving it at that moment, the start waits for it rather than resolving
  it a second time; otherwise the start resolves it, as before, and the address is kept
  for "previous" and a replay. A kept address that no longer opens is resolved once
  more. The next track goes to the engine for the gapless transition (D-024) once it is
  ready.
- **Heads:** a range request for the first bytes, kept only when the server cut it as
  a range and stated the file's length (`206` + `Content-Range`): the rest is fetched
  later from where the head ends, and two bodies not cut from the same file cannot be
  joined. The rest has to come as a `206` from exactly that byte of a file of exactly
  that length; anything else is an error, not a guess. A plugin's own `Range` (YouTube's
  `bytes=0-`, D-048) gives way to these.
- **A stale address after a head:** the stream asks for a new one through its handle;
  the player has the read-ahead thread resolve that track before anything else and
  hands the new address over; the head plays meanwhile. Once: a second failure is an
  error, not a silently shortened track.
- **A skipped track's download stops** — it used to run to the file's end.
- **Calls made ahead of time** (`provider::ahead_of_time`): a background worker marks
  its thread, and nothing in `Provider` changes. The cover worker (D-076) is marked too:
  the twenty lookups of a "queue all" no longer hold a call the user makes.
- **The plugin gate:** a call for the user waits only for the call already running,
  and asks that one, if made ahead of time, to give way; the engine's `host.tools.run`
  stops the tool, and `host.http` refuses a new request. The call made ahead of time
  ends as `ErrorKind::PluginPreempted` — "gave way", counted apart from "failed" (K9) —
  and is asked again later. A cover that gave way stays pending and is looked up again
  instead of showing a failure.

**The third track** (`prefetch::likely`). A track beyond the next two is needed before
the thread has moved on in two ways: the user skips the next two quickly — the queue's
continuation is needed — or picks a track from the queue. The first is weighed with how
often the user skips: in this session, and in the history of the next two tracks or
their artists. The second with how much the user listened to each track, and a quarter of
its artist's plays. Before anything is known a quarter of tracks count as skipped (worth
four observed tracks) and a tenth of moves as picks (worth ten). Without history the
third is the continuation; for someone skipping through a list of versions it stays the
continuation; a track played forty times pulls it to itself. The report says why in one
sentence ("you skipped 5 of the 5 tracks you left this session", "you played it 40
times"): a guess nobody can check is worth less than one that explains itself.

- **The history** comes from `SqliteLibrary::listening_profile`: counted plays and the
  listens that stopped before the play rule, by the same predicate `stats` uses; the
  artist is found by its part of the grouping key — a range the key's index answers,
  so a name that merely begins the same is not counted. No schema change. Only imports
  bring skips: the app's own playback writes counted plays only, so the session's habits
  are the strong signal and the history the weak one.
- **The habits** — skipped, finished, picked, went back — are counted by the player and
  carried across the players of one session: a new search replaces the player, not the
  listener.

**The CLI and the IPC (D-033).** `headshell prefetch "<query>" [--all]`, with `--json`
and a snapshot: the track `play` would start and the two after it, each one's address
resolved and head fetched, timed, and a summary. The desktop mirrors it as `prefetch`;
`prefetch_queue` is the playing queue's plan, counts and habits. A hand-run probe,
`examples/latency_probe.rs`, drives a `LiveSession` the way the desktop's core thread
does and prints what the user waits for.

**Measured** (release build, the Debian 13 container, a PulseAudio null sink, YouTube
Music, "let it happen" with every match queued):

| | before | after |
|---|---|---|
| Enter → sound | 9.73 s | 6.95–8.14 s |
| next → sound | 6.42 s | 0.01–0.02 s |
| next again | 5.66 s | 0.01–0.03 s |
| previous | 7.02 s | 0.16–0.59 s |
| a far track in the queue | 6.95 s | 4.76–5.06 s |
| three "next"s half a second apart | — | 0.01–0.03 s each |
| the longest round of `tick` | 4.71 s | under 5 ms |

**Not done, and why:**

- **The first track after Enter** still waits for the search and its address: there is
  nothing to read ahead. Left: yt-dlp's unpacked builds (`yt-dlp_linux.zip` and the
  others) might cut its ~1.15 s start — not measured; the busy label says "searching for
  the track" for the whole wait, although most of it is resolving.
- **A lifetime the provider states:** the plugin knows it (YouTube's `expire`,
  SoundCloud's policy); the core trusts 8 minutes for every address. Carrying it would
  change the plugin contract and `AudioSource`.
- **Resuming a stream that breaks mid-way:** the continuation that follows a head could
  resume any stream, but that changes every stream's behaviour and was not asked.
- **A fourth quick "next"** waits for the third track planned after the third one.

**Testing:** the read-ahead thread against a provider answering from memory — the plan's
order, a start waiting for the track in flight instead of resolving it twice, a failing
track asked once, a resolution that gave way asked again, an address past its lifetime,
a stale stream resolved first, heads kept and dropped with the plan; heads and the rest
on a local server — the rest only from exactly that byte of exactly that file, no head
from a server that ignores ranges, a stale address replaced, no new address an error and
not a short track; the plugin gate, and a stopped tool on Unix and on Windows; the third
track's guess; the listening profile; a real local queue played with next and previous,
the plan's reasons and the habits checked; `headshell prefetch --json`'s snapshot. The
two fixes were measured not to be empty: with `byte_len` reporting the length, the
stream test waits 3.006 s and fails; without the tool being stopped, the user's call
waits 4.3 s and the gate test fails.

**Verification:** `examples/latency_probe.rs` in the container, before and after, twice
each, and `--breakdown` for one start's parts; the live plugin tests. One run of the live
SoundCloud tests failed at the plugin's client_id discovery and passed when run again:
the service, not this change.

## D-078 — Lyrics: the exact recording's words, the file's first, LRCLIB by length; plugin api 4

**Date:** 2026-09-26 · **Status:** APPLIED (2026-09-26)

**Question:** The user: *"Let's add a lyrics section to now playing. Fetch it as LRC
from a source, matching exactly the music that plays. Give me the options, and I'll
tell you which to do."* The facts were verified first (§0.1: an API shape is not
assumed), four questions were asked with them (§0.2), and a fifth when an answer left
the servers open. The answers are the decisions below; the user also asked for the
interface to be done with /apple-design, as for D-075.

**Decision:**

1. **Sources: LRCLIB and a plugin gate** — the user's choice over LRCLIB alone. LRCLIB
   is free, keyless and open (MIT, a Rust server), and gives synced lyrics as LRC. A
   plugin may give its service's own lyrics (api 4). Musixmatch (a key and a commercial
   agreement for timing; its free route is an unofficial endpoint), NetEase and QQ
   (unofficial APIs) can only come as plugins; Spotify never (K4); Genius has no timing.
2. **The file's own lyrics win, and need no network.** The user: *"If there is a file
   in the added folder, or one built into the local music file, use it; otherwise don't
   fetch from the internet until the lyrics button is pressed. For plugin tracks, fetch
   the moment the song starts — with plugins it's online anyway."* So: a `.lrc` beside
   the file with its name, then its tags — synced or not, they are taken as they are.
3. **A server's own lyrics at once** (the fifth question; the user: *"yes, automatic
   when it plays"*): OpenSubsonic's `getLyricsBySongId`, Jellyfin's
   `/Audio/{id}/Lyrics` — the same file on the server. LRCLIB for a server's track only
   when asked, as for a local file's.
4. **"Asked" is the lyrics view on screen**, not `--online`: opening the lyrics tab or
   the whole-sheet view is the user asking, for the playing track and the ones after
   it while it stays open; closing it stops. `headshell lyrics` is the same request.
   The desktop's `HEADSHELL_ONLINE` does not change it — the user's rule is explicit.
5. **No version of the recording's length: the nearest, marked, and a shift** — the
   user's choice over "the words without timing" and "nothing". The nearest synced
   version is shown with how far it is ("15.0 s longer") and buttons that move the
   lines by 0.5 s; the shift is kept per track, and the next play has it. Only when no
   version is synced are the words alone shown.
6. **The view: both** — *up next | lyrics* tabs beside the record, and a button that
   grows the lyrics to the whole sheet.
7. **Phase 3, §3.12.** No new crate: the LRC and `SYLT` readers are written here, LRCLIB
   goes through the existing HTTP client, and the cache through the covers' store —
   whose atomic write and temporary-name check moved to `store.rs`, shared, instead of
   being copied.

**How "exactly the recording" is held.** Synced lines are timed against one release of
a song; another release's are seconds off. LRCLIB's versions are therefore taken **by
the length of the recording that plays** — the decoder's where it is known, the tags'
otherwise:

- the artist and the title must match, normalised as the identity chain does —
  `(Remastered)` is the same song, `(Live)` is not;
- a synced version within **±2 s** (LRCLIB's own window, read in its source), the
  nearest first; the version on the track's album breaks ties;
- **the album guard:** when the track's album has a synced version at LRCLIB, only the
  album's versions are weighed (see *found while verifying*);
- a version marked instrumental within the window is the answer "no words";
- then the nearest synced version anywhere (`LyricsFit::Closest`), then unsynced words.

The chain, in order: the file's own; the provider's own, synced ones winning; LRCLIB's
synced version; the provider's unsynced words; LRCLIB's. A plugin's lyrics are the
recording's own too — the same item the audio comes from — and are not measured.

**The shape.** `Provider::lyrics(id)` with `Capabilities::LYRICS` (its default says
"cannot", stage `LYRICS_READ`), and `Provider::is_plugin()` — a plugin's tracks go
online at once. The stages `LYRICS_READ`, `LYRICS_LOOKUP` and `LYRICS_STORE`. The report
names the source (`sidecar`, `embedded`, `provider`, `lrclib`), whether it is synced,
how it fits (`own`, `length`, `closest`, `unmeasured`) and by how much; "not requested",
"not found" and "failed" are three statuses (K9). `Lyrics::line_at` is the line being
sung; the interface keeps a second copy in `ui/lyrics.js`, bound to the first by
`fixtures/lyrics/line_at_cases.json`, as the anchor's cases bind the position (D-033).

**The cache** is `<data>/lyrics/`: LRCLIB's answer per track — a synced version of the
length for good; the nearest one, unsynced words, "instrumental" and "none" for 30 days,
then asked again (LRCLIB grows and gets corrected) — and the user's shifts, which
outlive a new answer. The file's and the provider's lyrics are not kept: an edited
`.lrc` shows at once.

**The flow.** One background thread per session (`headshell-lyrics`) takes **the playing
track only** — "the moment it starts" was the user's rule, and LRCLIB's patience is not
spent on a queue nobody reaches. What the links answered is kept for the playing track,
so opening the view later asks only LRCLIB, not the plugin again; a failed link is asked
again by "try again". LRCLIB is asked one request a second, and a busy answer (`503`)
twice more before it is said.

**The CLI and the IPC (D-033).** `headshell lyrics "<query>" [--all] | --file <audio>
[--print] [--shift MS]`, with `--json` and a snapshot. The desktop mirrors it as
`lyrics`; `lyrics_now`, `lyrics_view` and `lyrics_shift` are the playing track's side,
and `TickReport::lyrics_ready` says when its answer changed.

**Plugin api 4.** Like api 3 (D-076), a required declaration: the manifest **must** say
`"lyrics": true` or `false`. With `true` the plugin exports `lyrics(id)` and returns `{
synced?, plain?, instrumental?, attribution? }` or `null`; `synced` is LRC text, read by
the core's own parser. api 3 manifests no longer load. The catalog's `ytmusic` 0.6.0
gives YouTube Music's own lyrics, timed; `soundcloud` 0.4.0 says it has none.

**The interface (with /apple-design).** The line being sung is at full strength and the
others dimmed with `opacity` — never held at a scale, since a transform left on text
blurs it in WebKit; a line lifts slightly as it lights and settles at rest. Following it
is one spring: the scroll position jumps to where the line reads (a third of the way
down), the list is put back where it was seen and springs to rest — a glide that
animates no scroll offset (D-028). A line arriving mid-glide carries the glide on from
where it is seen (`motion.js`'s new `rebase`). The user's wheel or drag takes over at
once — the glide's remainder is folded into the scroll position — every line comes up to
be read, and following resumes four seconds after the last scroll. The whole-sheet view
grows out of the lyrics column and goes back into it, the record fading behind; caught
halfway, it turns around; reduced motion is a fade. `L` opens the lyrics and goes back to
up next; `Esc` closes the whole-sheet view first.

**Checked before writing, as asked:**
- **LRCLIB, in its source:** `/api/get` matches the lower-cased artist and title exactly,
  the album exactly when given, the length within ±2 s when given — and without a
  length not at all; among several it returns the lowest id, not the synced one.
  `/api/search` gives up to twenty versions. Neither takes an ISRC or an MBID. Two of
  four quick requests got `503 ServerOverloaded`.
- **Coverage, measured:** eight well-known Turkish songs all had synced versions — ten
  to twenty each, of lengths from 198 s to 315 s for one of them.
- **symphonia 0.6:** ID3 `USLT`, Vorbis `LYRICS`/`UNSYNCEDLYRICS`, APE `lyrics` and MP4
  `©lyr` arrive as the standard `Lyrics` tag; ID3 `SYLT` as a raw binary tag.
- **OpenSubsonic's specification and Navidrome's source:** `line.start` and `offset` in
  milliseconds, a positive offset brings the lines sooner, `kind` separates the main
  lyrics from translations.
- **Jellyfin's source:** `LyricDto`, a line's `Start` in ticks, `404` for none; its LRC
  parser never fills the metadata's offset.
- **YouTube Music:** the `next` endpoint names the song's lyrics page; `browse` asked as
  the mobile app gives timed lines (from LyricFind or Musixmatch), as the web client the
  words alone. Five songs of five had timed lines; an instrumental had none.

**Found while verifying:**
- **A festival take lent its timing.** Portishead's "Sour Times" (*Dummy*, 251 s): at
  LRCLIB the versions within two seconds were a 2013 festival's (251 s) and a 2011
  one's (253 s), their titles without "(Live)"; the album's own was 254 s. The length
  rule alone took the festival's lines. The album guard (the covers' lesson, D-076)
  keeps them out; a test on that real data fails when the guard is taken out. Online,
  the exact query then found the album's 249 s version the search's twenty had not
  listed — within the window, and taken.
- **`/api/get` would have handed back words without timing** while a synced version of
  the same length existed: the choice is made here, over the search's versions.
- **YouTube Music's mobile answer is not the same on every call** — once a line came
  without its start. The plugin checks the lines and falls back to the words alone.
- **`headshell lyrics --file` keyed a shift by the path as typed:** a relative path did
  not meet the library's absolute one. The key uses the absolute path.
- **No test parsed the page's scripts:** a syntax error in `app.js` leaves the window
  empty, and every other test reads the scripts as text. Now they are declared in
  QuickJS; a broken `app.js` was measured to fail it.
- **In the container:** WebKit's overlay scroll bar stayed lit — following scrolls all
  the time — so the lyrics hide it; the source line was cut off in the narrow column and
  now wraps; "local has none" became "the file has no .lrc beside it and no lyrics in
  its tags".
- **The hanging-plugin test** replaced the echo plugin's script without its new export,
  and failed on the contract instead of the timeout: it exports `lyrics` now.

**Not done, and why:**
- **Word-by-word timing:** LRCLIB gives it as YAML (`lyricsfile`), OpenSubsonic as
  `cueLine`, Jellyfin as `Cues`; a YAML parser is a new crate, not asked.
- **Seeking to a line with a click:** the core has no seek (§3.8).
- **Translations** (OpenSubsonic's `translation` kind): not asked.
- **Asking LRCLIB again for a track whose answer is kept,** before its 30 days: no way
  in the app; deleting `<data>/lyrics/` does it.

**The release order** is D-076's: this change, then the catalog (`soundcloud` 0.4.0,
`ytmusic` 0.6.0, api 4), then a release. Until the catalog publishes them this
repository's live plugin tests are red — they install from the published catalog, and
this core refuses api 3. The catalog cannot go first: its CI builds headshell from this
repository's `master`. The live plugin tests passed against a local mirror of the catalog
carrying the api 4 plugins, run one at a time; run in parallel, two SoundCloud tests
failed at the service (a timeout, the client_id discovery) and passed on their own.

**Verification:** the §3.9 container (Debian 13, the host's WebKitGTK, Xvfb, a
PulseAudio null sink): a made-up song whose `.lrc` was followed line by line through
its end — the pause shown as ♪, a two-line line, Turkish letters; words without timing;
a made-up track LRCLIB has nothing for ("not found", saying what was looked at); "Sour
Times", its album's version by length; "Sil Baştan" at 290 s, which LRCLIB has only 15 s
off — marked, shifted by 1.5 s, the shift kept in the index; the whole-sheet view
growing and going back, `Esc`, `L`, and a hand scroll that brightened every line and
gave the following back four seconds later.

## D-079 — Context menus: every element offers its own actions; the engine's menu never opens

**Date:** 2026-09-30 · **Status:** APPLIED (2026-09-30)

**Question:** The user: *"For everything in the interface I want a screen that opens
properly on a right click: right-clicking a thing shows the controls that belong to it,
and 'Inspect Element' goes away — a custom right-click menu. You can ask questions."*
The code was read first (what each element does today, what the core can do), a
proposal was shown with its map — which element offers what — and four questions were
asked (§0.2). The interface was done with /apple-design, as for D-075 and D-078.

**Decision:**

1. **Only what the interface already does** — the user's choice over adding queue
   editing now. A menu is a second way to an existing command, never a capability of
   its own (K1, D-033): no new IPC command, no new CLI subcommand. "Play next", "add to
   the queue" and "remove from the queue" need the core — the queue has no insert or
   remove, and D-077's read-ahead would have to follow them — so they are their own
   step, with their own decision.
2. **Text boxes get the app's menu too** — the user's choice over leaving the engine's
   there: cut, copy, paste, select all, and what the box's form does (play the first
   match, queue every match; search) once something is typed. A password is never
   copied out of its box.
3. **No way back to the engine's menu, in a debug build either** — the user's choice
   over Shift+right click in debug builds and an "inspect" row.
4. **An empty spot opens nothing** — the user's choice over the section's own actions
   and a general playback menu. An element with nothing to offer (the theme in use, a
   heading) opens nothing either.

**Why the engine's menu had to go** — it was more than "Inspect Element", which only
debug builds show (their inspector is on). WebKitGTK's own menu offered "Back" and
"Reload" on an empty spot — a reload rebuilds the interface and loses the open section,
the search and the scroll — and "Open Image in New Window" on a cover.

**What the elements offer:**

| Element | Its menu |
|---|---|
| The player bar, the record's card, the whole-sheet lyrics' head | pause/play · next · previous · stop · shuffle ✓ · repeat off/all/one · lyrics (or up next) · now playing · the track's rows |
| A queue row | play this track (pause on the playing one) · the track's rows |
| A library result | play · play every track by the artist · the track's rows |
| The statistics' artists, tracks, albums | play (every track, shuffled) · find in the library · copy |
| A year's bar | narrow down to the year / back to all time · the year's sleeve card |
| A sidebar row | open (`Ctrl`+number) · the section's own work: scan, fetch the catalog, update every plugin, copy the last run's report… |
| A plugin's card, a catalog entry | the card's own buttons, from one list — they cannot drift apart · copy its name |
| A provider, a server, a secret, a theme, a music directory | test · copy the address · delete (asks first) · apply · copy the path |
| A notice | copy the text or the error chain · close · close every notice |
| The lyrics | copy the line · copy the lyrics · 0.5 s sooner/later · back to their own timing · the whole sheet |
| The sleeve preview | save as… · refresh the preview · square/story |
| A report or a JSON block, selected text | copy |

"The track's rows" are: find the artist in the library, copy artist and title, resolve
its identity (the diagnostics' form, filled in).

**How it behaves** (`ui/menu.js`, the platform's menus as the model):

- It opens where the button went down, grows out of the pointer (a critically damped
  spring from 0.96, `motion.js`), stays 8 px inside the window — turning to the
  pointer's left or above it — and fades where it is when it closes. Reduced motion:
  only the fade; the theme's duration `0`: none.
- **Press, move onto a row, let go** picks it in one gesture; a plain click leaves the
  menu open. A release that has not moved 5 px never picks: a menu that opens with a
  row under a still pointer must not run it.
- A press outside closes the menu **and does nothing else** — the pointer and the click
  are swallowed; a right press opens the next menu there. The wheel, a resize and the
  window losing focus close it; so does the element it is about leaving the page (its
  list was drawn again, and the rows could point at another track).
- **The keyboard:** the Menu key or Shift+F10 opens the focused element's menu under it
  (above it without room — never over it), with the first row lit and the focus in the
  menu; the arrows, Home/End, Enter, Esc and a letter (type-ahead) work, and every key
  is the menu's while it is open — the space must not pause the music behind it.
- What the menu is about is ringed while it is open. A row that cannot be undone
  (remove, delete) is red and asks first — "really remove?" — like the card's button;
  leaving the row takes the question back, so a single row is ever lit.
- Every row's shortcut is written beside it (`space`, `→`, `L`, `Ctrl+2`).

**The clipboard switch.** WebKitGTK refused the page's clipboard read:
`navigator.clipboard.readText()` threw `NotAllowedError`, and `execCommand("paste")`
did nothing — the menu's paste could only report that (it did, with its stage). Tauri's
`enable_clipboard_access()` turns it on (WebKitGTK's `javascript-can-access-clipboard`,
WebView2's clipboard permission; macOS always allows it). The config has no switch for
it, so the main window is built in `setup` from its config (`create: false`) — no new
crate. Only the app's own code runs in the page (CSP `script-src 'self'`; nothing from
the catalog or a plugin is written as markup, `ui_contract.rs`), and it reads the
clipboard only when "paste" is picked. Cut, copy and paste go through the engine's own
commands first, as the box's keys do.

**Found while verifying:**

- **The paste refusal** above — the first paste in the container was the error notice.
- **A notice went while its menu was open:** a notice re-arms its timer when the
  pointer leaves it, and going to its menu is leaving it; the menu went with it. A
  notice whose menu is open is held, and has four seconds once the menu closes.
- **A focus ring stayed behind:** a right press focused the button under it, and the
  `Esc` that closed the menu lit that button's focus ring — the same ring as the menu's
  mark, which then looked stuck. A right press no longer moves the focus, except into a
  text box, whose edits the menu is for.
- **Two rows were lit at once:** an armed "really remove?" stayed red while the pointer
  lit another row. Leaving the row now disarms it.
- **`Ctrl+Z` undoes nothing in the page** — plain typing either, before this change.
  Not this change's; noted here so it is not taken for the menu's paste.

**Not done, and why:**

- **Queue editing** — decision 1; its own step.
- **An album in its order:** the core keeps no track numbers, so "play every track on
  it" queues the album's tracks in the search's order. The row does not say "play the
  album".
- **macOS's `⌘`:** the rows say `Ctrl`, as the interface does everywhere today (the
  shortcut list, the tooltips).
- **Saving a cover from its menu:** not asked.

**Testing:** `tests/menu_js.rs` evaluates `menu.js` in QuickJS (after `motion.js`, which
it imports): the placement — to the pointer's right and below it, to its left or above
it without room, from the keyboard above the element and not onto it, a menu taller
than the window kept at its top, and no opening point on a grid over the window putting
a menu outside it; the release rule; the type-ahead; the separators. `ui_contract.rs`
reads `menu.js` too — its ids, no markup, and that it parses.

**Verification:** the §3.9 container (Debian 13, the host's WebKitGTK 2.52.6, Xvfb, a
PulseAudio null sink), at 1180×780 and 900×600, driven with `xdotool`: a sidebar row's
menu (the ring, the lit row, "search the library" landing in the search box); a queue
row's "play this track"; the player bar's menu turned above the pointer; press–drag–
release onto "next track"; `Esc` closing the menu and not the sheet; a plain right click
leaving the menu open and a click outside closing it without jumping to the row under
it; the play box's cut, copy, paste and select all, read back with `xclip`; Shift+F10,
the arrows and Enter in the box; the statistics' rows, a year's sleeve card; a library
row's ring on a table row; a notice held past its lifetime while its menu was open; an
empty spot opening nothing; selected text copied; the echo plugin's card — its menu, the
armed row taken back, and removed on the second pick; the icons-only sidebar at 900 px;
a lyric line copied from the lyrics LRCLIB gave.

**Addendum (2026-10-01, D-080).** "Add to queue" — decision 1's "its own step" — landed
in D-080, with the songs' menus now one builder: a library result, a statistics track, a
queue row and a suggestion offer the same rows. The library result lost "play every track
by the artist" on the way; an artist's own menu (the statistics, the suggestions) has it.

## D-080 — Play from a chosen provider; suggestions while typing; a menu for every song

**Date:** 2026-10-01 · **Status:** APPLIED (2026-10-01)

**Question:** The user: *"Add a part for choosing a provider in the providers section, and
in the song search let it suggest while I type — as I type 'let it', first Let It Happen,
then the artist, then the other results if there are any."* The code was read first:
only local files are in the catalog, and servers and plugins were asked only when the
catalog found nothing — with local music there was no way to play from YouTube Music.
Three questions were asked (§0.2); the answers are the decisions below. The interface was
done with /apple-design.

**Decision:**

1. **The chosen provider is searched first; the others only when it finds nothing, and
   their results are named** — the user's own words over "only the chosen one" and "a
   priority order": *"search the one chosen in providers; if nothing comes up, show the
   other provider's results as 'results from <name>:'"*. Nothing chosen is the registry's
   first that can play — local files, today's behaviour.
2. **Suggestions come from the chosen provider**: the catalog at once, a server or a plugin
   after a pause in typing — each pause a question over the network, which the user chose.
3. **A picked suggestion plays at once, and every song gets a menu** — the user: *"play it
   at once; also give every song a right-click 'add to queue' and 'play now', and there can
   be 7–8 more options; let them stay placeholders for now, write ones that could really be
   useful."* The real rows: play now, add to queue, find the artist / the album in the
   library, copy artist and title, resolve its identity. The placeholders, shown as "soon"
   and not pickable: play next, add to a playlist…, start a radio from it, like.

**The order, in the core.** `Session::queue_from_search` — so `play`, the CLI's `play`,
`artwork` and `lyrics` follow it alike — asks the chosen provider first: its rows in the
scanned catalog when it was scanned, its own search when not. Only when it finds nothing are
the others asked, in the registry's order (local files, servers, plugins), their answers one
after another. The chosen one's failure is the answer — "could not reach it" is not a reason
to play something else; among the others, one that fails does not hide what the rest found,
and when none found anything its failure is the answer (K9).

**The choice** is kept in `preferences.json` in the data directory, not the database (D-021's
line: a choice is not library data); a broken file is an error, never a silent "nothing
chosen". `Session::play_from` says where the search starts, whether the user chose it, whether
it answers at once (a scanned catalog), and a choice that cannot be kept — a server removed, a
plugin disabled — is named in `missing`. Only a registered provider that can play can be chosen.

**The ranking** (`suggest.rs`, pure): the tracks whose title begins with the text (four), the
artists — whose name or a word of it begins with it, then the artists of those tracks (three)
— then every other match (five), the same song twice once. Words are compared as the identity
chain compares them, an apostrophe dropped first.

**Not a recorded run.** `suggest` is asked at every pause in typing; recording it would make
`headshell diag`'s last run always "suggest". A provider that could not be asked is in the
report's `failures`, with its stage and chain.

**Typing does not hold the player.** The desktop's commands and the player's rounds share one
thread (D-032). A plugin's search for a suggestion would have held the rounds and the controls
for as long as it took, so `Session::suggest_plan` reads the catalog there, at once, and
`SuggestPlan::finish` — which needs no session — asks the network off it. A test fails to
compile if either stops being `Send`.

**The CLI (D-033).** `headshell suggest "<query>"` (and `--json`), `headshell provider use
<name>`; `provider list` marks the chosen one and says why when it is not a choice. The TUI's
live counterpart of the desktop's suggestions, "play now" and "add to queue" is a search line:
`/` opens it, Enter plays the chosen row, Tab adds it to the queue. "Add to queue" is
`Player::enqueue`: at the end of the play order, shuffled or not; into an empty queue it plays.
The read-ahead (D-077) sees the new track at its next round.

**"Play next" stays a placeholder.** It puts a track between the playing one and the one
already handed to the engine for a gapless start (D-077); that handover has to be taken back
first. Appending changes nothing already handed over.

**The interface (with /apple-design).** The suggestions are a panel under the play box with
the context menu's surface, growing out of the field; the field keeps the focus all along — the
arrows light a row, Enter plays it, Esc closes the panel before anything behind it. A press on
the panel does not take the focus, so a row's menu can add one song after another with the panel
up. The catalog is asked 90 ms after a key, a server or a plugin 300 ms after; an older answer
arriving late is dropped. The providers section's "play from" card lights the chosen row with a
pill that slides to it on a spring, chosen on pointer-down; the play box's placeholder says where
it searches.

**Found while verifying:**
- **"dont" did not begin "Don't".** The identity chain's normalisation turns an apostrophe into
  a space; suggestions drop it first.
- **The provider list's columns were never aligned:** a provider id's `Display` ignores the
  column width. It is written as a string now.
- **In the container, typing lagged while the now-playing sheet was up and the record turned:**
  the container runs WebKit without compositing, so the turning record is painted on the page's
  thread every frame. With playback paused or the sheet down the field answered at once. This
  change adds no per-frame work; on a machine with compositing it is to be watched, not assumed.
- **The container's WebKitGTK is 2.54.0 now** (Debian updated it); this machine's is 2.52.6.
- **A playback test failed once in the workspace run** with an ALSA I/O error from this
  machine's sound device, while its sibling tests used it too; alone it passed three times out
  of three.

**Not done, and why:** play next (above); playlists, radios and likes — the placeholders, no
store behind them yet; covers in the suggestion rows; the TUI's search line was not seen on a
terminal, only tested through its keys and rows.

**Testing:** the ranking (the user's example, artists before their tracks, Turkish letters,
apostrophes, duplicates, sizes); the order and the fallback with fake providers (nothing chosen,
the chosen one first, a choice that cannot be kept, refusals, the chosen one's failure, a failing
fallback); `enqueue` (last in the play order, shuffled too; an empty queue plays); the choice's
file; the TUI's keys, rows and timing; `suggest` (a snapshot) and `provider use` / `provider
list` through the CLI.

**Verification:** the §3.9 container (Debian 13, Xvfb, a PulseAudio null sink) with the echo
plugin approved: "let it" → Let It Happen, then Tame Impala; "gece" → "nothing in Local files",
then "results from Echo (test):"; a suggestion's menu — the real rows and the placeholders;
"add to queue" into an empty queue (it played) and into a full one (last, the panel still up);
↓ and Enter playing exactly the lit track; Echo chosen — the pill sliding, the placeholder, its
suggestions with no heading; Esc closing the panel only; a queue row's menu; the window at
900×600.

**Addendum (2026-10-01): covers in the suggestions.** Left undone above; the user asked
for them on seeing the panel (*"why are there no covers — can't we load them?"*). The
suggestion rows had none because the cover worker (D-076) looked only at the queue. It
takes a second list now, what is on screen (`ArtworkWorker::show`, the desktop's
`artwork_shown`): the cache's covers come back at once, the rest are asked **only where each
track lives** — its file's tags and folder, its provider's own cover — after the playing
track and before the rest of the queue, and a found one arrives with the tick's
`artwork_ready`, as the queue's do. The third parties (MusicBrainz, the Cover Art Archive)
are never asked for a suggestion: the list changes at every pause in typing, and they take
a request a second. For the same reason nothing found is not kept as "not found" — the queue,
online, still asks them for that album later. A new list replaces the old one's pending
lookups. The CLI's counterpart stays `headshell artwork`: the covers of what a query finds,
waited for. Verified in the container: a local album's folder cover on its suggestion rows,
the echo plugin's own cover on its track, and a track whose plugin has none keeping the small
record.

## D-081 — A search's top tier is read whole in one response, or nothing is claimed; a duration narrows the search

**Date:** 2026-10-02 · **Status:** APPLIED (2026-10-02)

**Question:** CI went red on master with the D-080 addendum: `an_ambiguous_query_without_duration_claims_no_authority`
failed on Linux and Windows in the same run — `Radiohead — Creep` without a duration got an MBID (`tied 1`)
where it had always got the local key. The test was on the list of flaky ones. It was measured before
anything was changed, and the user was asked (§0.2): **A** — read on while the outcome can still change, and
a tier that does not fit claims nothing (a public trait signature changes) — or **B** — never read on, and a
cut page always claims nothing (one request, but a popular song's file never gets an MBID either). The user
chose **A**.

D-046 Q3 had rejected *paging* — fetching every match, ~9 s for each ambiguous query — and left "fixing the
set itself" open as a separate decision. This is that decision. The question put to the user did not cite
D-046, and should have; the design that came out of the measurements stays inside D-046's objection: at most
one more request, never a merge of pages.

**Measured** (the live MusicBrainz, 2026-10-02):

1. **The failure is the resolver's, not the test's.** The search scores every exact `Creep` alike — 149 of
   192 hits — and returns 25 of them. Which 25 depends on the index replica that answers: three replicas,
   three first pages, holding 7, 9 and **1** plain recording (no note, a known length). On the third the
   resolver saw a single winner and claimed it: the identity depended on the replica again, the very thing
   D-046 ended.
2. **A page past the first comes from another replica.** Each address is answered from a cache filled by
   whichever replica served it. Reading 0–25, 25–125 and 125–225: in four reads of five, 25 hits came twice
   and 25 never. The first implementation — read on up to 300 across pages, as option A had said — made
   `Creep` **with** a duration flip between an MBID and the local key from run to run (one run in five got the
   MBID). It was replaced before it was kept.
3. **Scores are relative to the whole search, but rounded with each replica's statistics.** A tail page does
   not restart at 100 (`Creep`'s runs 82 → 32), yet the same exact hits scored all 100 on one replica and
   34 × 100 + 66 × 99 on another. The first inexact hits scored 94, 93, 92 and 86 (`One More Time,
   Aerodynamic`, `Roads (live)`, `Şımarık (remix)`, `Archangel (TASO REMIX)`).
4. **`dur:[a TO b]` narrows a search to a length range.** `Creep` ±15 s: 53 hits, 33 exact, the same answer
   on every read; `Bohemian Rhapsody` 49; `Yesterday` 94, three of them exactly 125 000 ms long — a real tie.

**Decision:**

1. **A winner of the part read is not claimed.** `MetadataLookup::search_recordings` returns a `SearchPage {
   candidates, top_continues }`; when the source's top tier goes on past what was read, a winner unique among
   the candidates read is no authority — `LocalKey`, with `Resolution::unread_equals` set.
2. **The top tier is read whole, in one response.** The first 25 as before; if the tier goes on and the
   outcome can still change, the first 100 — MusicBrainz's largest page — **from the start**, in one request.
   One response is one replica's ranking, whole. Never the next page; a tier wider than 100 claims nothing.
3. **The reading stops when nothing unread can change the outcome.** A tie at the ceiling of every sorting
   criterion — the highest score (`similarity` is clamped to 1), no note and a known length, and with a
   duration a gap of 0 — is final: an unread candidate can only join it. That is the usual end of a
   popular song without a duration: one request, as before.
4. **A duration narrows the search** to ±15 s around it (`LENGTH_WINDOW_MS`, the fuzzy scorer's own mismatch
   limit: further away a candidate scores at most 0.7, below every threshold, so no possible winner is lost).
   If the window's tier is read whole and its best is a plain recording at the ceiling, the window decides —
   nothing outside it can equal that. Otherwise every length is read: a recording of unknown length falls
   outside every window and may be the best there is.
5. **MusicBrainz's top tier is a score of 97 or more**: between the exact hits' 99–100 and the inexact ones'
   94 and below, with a point of rounding either way. A hit without a score, or an answer without a count
   that filled its page, reads as "goes on" — "I don't know" must not read as "it ended".
6. **`Resolution::unread_equals`** says the tier was not read whole; `tied_candidates` is then a lower bound.
   The CLI and the desktop say "at least N candidates could not be told apart", or "the source ranks more
   candidates as high than were read". The line "N candidates got the same score; the choice is deterministic
   but arbitrary" — stale since D-046, when a tie stopped choosing — is gone, and so is "the metadata source
   returned no candidates" for a resolution that had candidates below the threshold ("no candidate came
   close enough").

**Cost.** Without a duration: one request, two when the first 25 do not settle (the replica that failed CI).
With a duration: one or two in the window, and the reads of every length only when the window holds no plain
recording at the ceiling. A page of 100 is ~330 KB and ~1 s; the shared limiter's 1.1 s between requests
stays the pace.

**Measured after.** `Creep` without a duration: the same `local:b51521e93103eefa` in five runs out of five
(tied 7–35, each a lower bound). With a duration: `mb:d8b2fac9…` — the 238 026 ms studio take — in ten
resolutions out of ten over five runs. The accuracy set: **72/72 = 100%** (it runs on `StaticLookup`, which
now cuts its answer like a search engine and, not ranking, counts every match as the top). The live suite:
five runs, all green.

**Assumption, written down:** a hit MusicBrainz ranks below the top tier does not equal the winner by the
resolver's measure. MusicBrainz's style keeps "remaster" and the like in the note, not the title, so it holds
in practice; a title with an edition suffix — ranked lower by Lucene, normalised equal by us — would break it.

**Public signatures that changed:** `MetadataLookup::search_recordings` (a length range and a limit; returns
`SearchPage`); `SearchPage`, `LengthRange`, `WHOLE_PAGE`, `LENGTH_WINDOW_MS` (new); `Resolution::unread_equals`
(new, `serde` default `false`). `MusicBrainzLookup::with_search_limit` now sizes only the release search (the
cover chain, D-076); a recording search's page is its caller's. The three implementations are in the core.

**Testing:** a scripted lookup that answers in turn and records what was asked — the replica that failed CI,
a winner of the whole tier, a tier wider than one response, a tie at the ceiling stopping the reading, the
window deciding, the window falling back to every length, a window wider than one response, the window's
bounds. MusicBrainz on canned answers: a head inside the top tier, 99 still in it, the tier ending, an answer
holding every hit, an unknown score or count, the page cap, the `dur` clause. Two deliberate faults — the
first answer always whole, the window never deciding — fail five tests and one; taking out the early stop
fails two. Live: the first 25 of `Creep` say the top goes on; the same query with a duration gives the same
identity twice.

**Also in this round:** CI's actions moved to their Node 24 majors — `checkout` v7, `upload-artifact` v7,
`download-artifact` v8, `action-gh-release` v3 (their notes were read: the runtime, credentials in a separate
file, fork pull requests blocked, by-ID download paths and enforced digests — none touches how they are used
here). The release workflow's run only at the next tag.

## D-082 — The desktop's online choice: kept in the data directory, `headshell online`, a settings section

**Date:** 2026-10-02 · **Status:** APPLIED (2026-10-02)

**Question:** The desktop went online only when it was started with `HEADSHELL_ONLINE=1` (D-076), so a user
who opens the app from its icon could never get a cover from MusicBrainz and the Cover Art Archive. D-076 had
left the in-app switch undone: by D-033 a CLI command first, and the variable stood in for it. Asked (§0.2):
what the saved choice covers, and where the switch sits. The user's answers: **"Yalnız masaüstü"** — the CLI
keeps deciding run by run with `--online`, and the command only shows and writes the desktop's choice — and
**System → a new "settings" row**.

**Decision:**

1. **The choice is kept in `preferences.json`** (`"lookup": "online" | "offline"`, beside D-080's `play_from`).
   Nothing saved is offline, as a fresh install is; a file from before reads as nothing chosen.
2. **`HEADSHELL_ONLINE` decides one launch over it**, `1` or `0`; anything else still stops startup with the
   reason (D-076's rule). `Session::online_choice` gives `OnlineChoice { mode, source, saved, variable }`, and
   the source — `variable`, `saved` or `default` — is always said (K9).
3. **The CLI is not bound by it** — the user's choice. `headshell online` shows the desktop's mode and who
   decided it, the saved choice, the variable as this shell has it, and that the CLI goes online only with
   `--online`; `headshell online on|off` writes the choice. A test proves a CLI run stays offline with the
   desktop's choice on.
4. **The desktop goes by it at once.** The switch saves through the core, and the cover worker starts over in
   the new mode; an offline "not looked up" is never cached, so turning online on reaches the covers it had
   skipped — no restart. Reading the choice applies it too: a choice saved from the CLI while the app runs
   takes effect when the settings are read, so the switch never shows a mode the app is not in. While the
   variable holds the launch, the switch is disabled and the note says by what, and what the saved choice is.
5. **The settings section** is the system group's third row, `Ctrl`+`0` — appended, so no existing shortcut
   moved. One card: what online asks and the switch, then a note — the variable's hold, the cost of an online
   import (one track a second), and that lyrics and plugins go online by their own rules (D-078, D-071). The
   row's menu has "online lookups" as a check item. The texts that sent the user to the variable — the cover
   note under the queue, a cover's tooltip, diagnostics' resolve hint and environment block — now say
   settings.
6. **The switch** (with /apple-design): `role="switch"`, the knob on a spring, so a second press while it
   travels turns it back from where it is; the track's colour cross-fades. Pressed, the knob stretches toward
   where a release sends it. It answers at once and is settled by the core's answer; a failed call says why
   and the switch goes back. Reduced motion drops the stretch.

**No run at startup.** `environment` carries `lookup_held`, so the switch and the row's menu are right before
settings is first opened without a recorded run — diagnostics' last run stays the user's (D-080's care).
Opening settings is a run (`online`), as opening providers is.

**IPC (D-033):** `online` (`headshell online`) and `online_set` (`headshell online on|off`); `Environment.lookup`
is the mode the app goes by now, `lookup_held` is new. `lookup_mode_from_env` gave way to `lookup_mode_variable`,
which tells "unset" from `0`.

**Testing:** who decides, as a table; the choice saved beside `play_from` without touching it; a preferences
file from before D-082; through the CLI — nothing chosen, on, the variable holding a launch, a value it does not
take, and a CLI run staying offline with the desktop online; `headshell online --json` has a snapshot;
`ui_contract.rs` holds the new section, its row and the shortcut list.

**Verification:** a Debian 13 container (WebKitGTK 2.54.0; the host's is 2.52.6), Xvfb and a PulseAudio null
sink, three local tracks named for real recordings. Settings off, then on — `preferences.json` written —
and `Creep` played with the Pablo Honey cover from the Cover Art Archive, no restart; off again, and `Roads`
got the offline note's new text; `headshell online on` from the CLI while the app ran, and opening settings
showed it on and brought `Dummy`'s cover; a restart with `HEADSHELL_ONLINE=0` — the switch disabled, its note
naming the variable and the saved choice; the row's menu held and, after a restart without the variable,
checked before settings was opened; turning it off from the menu; the pressed knob's stretch; 900×600.

**Not done:** nothing that was asked. The section holds one choice today; D-080's "play from" stays in
providers, where it was chosen.

## D-083 — A simpler interface; the developer's details only in the debug mode (`DEBUG=1`, settings)

**Date:** 2026-10-02 · **Status:** APPLIED (2026-10-02)

**Question:** The user: *"Simplify the interface and make it more readable. Visual balance: reduce the
clutter, bring only the critical information forward, and move the details to a second layer so they do
not overwhelm the user — minimalist and usable, without overdoing it. Debug mode: make every detail meant
for developers — technical logs, performance metrics, internal state parameters — visible only with
`DEBUG=1`."* Every section was captured in the §3.9 container first, and a map was proposed: what stays in
front, what goes one level deeper for everyone, what only the debug mode shows. Three questions were asked
(§0.2), done with /apple-design. The answers:

1. **Diagnostics:** hidden — *"but let's put a 'turn on the debug mode' option at the very bottom of the
   settings"* (the user's own words, over a "copy the report" button in the help sheet, which was not added:
   the switch leads to the full report).
2. **A notice's stage:** plain words in front, the stage's code and the full chain folded — K9 kept.
3. **The source label** in now playing and the queue: only for a source other than the local files, by its
   name.

**Decision:**

1. **Three layers.** In front: what the user acts on. One level deeper, for everyone (a folded "details", a
   tooltip): what explains it — K9's summaries among them. Only in the debug mode: raw reports, internal
   identifiers, versions of the contracts, hashes, platforms, paths of the engine's tools, the counts that are
   zero, diagnostics. **`DEBUG=1` brings back what the interface showed before this round**, and nothing is
   lost on the way: every element is still drawn, and one attribute on the root (`data-debug`) with the
   `.debug-only` class — `.plain-only` its counterpart — decides what shows. Turning the mode on or off
   redraws nothing.
2. **The debug mode** is the interface's own choice, kept in `ui.json` beside the theme (§3.3's line: neither
   is a core concept, so neither goes to `preferences.json`, and there is no CLI counterpart — the CLI is the
   developer's surface already, as the themes have none). A switch at the bottom of settings; `DEBUG=1`
   holds a launch on, the switch locked and its note naming the variable and the saved choice. **Only
   exactly `1`:** the name is shared (Node's `DEBUG=*`, a Makefile's `DEBUG=yes`), so another value is not
   refused — the app would not start for a variable meant for another program — but left alone, with a
   warning in the log. While the mode is on at startup the log goes from warnings to `debug` (`RUST_LOG`
   still decides when set) — the "technical logs" of the ask.
3. **Notices** (K9): the stage in plain words is the headline — "The server could not be reached" — every
   one of the core's 25 stages has its words (`STAGE_TEXT`, held complete by `ui_contract.rs`; a stage added
   without words would fall to "Something went wrong", which says no stage). Then the first cause, two lines
   at most. The stage's code, the full chain and "copy" are under "details"; the debug mode puts the code
   beside the words. `.stage` keeps its meaning for themes — the notice's stage — now in words; the code is
   the new `.toast-code`.
4. **Partial success** (K9): plainly and only when not zero for everyone ("9 short plays not counted"),
   every count, zeros included, in the debug mode — statistics and the plugin list. The import report shows
   what changed in the library (new listens, already there, new tracks); how the archive was read (raw
   records, listens, ISRCs, skipped, the identity chain) is under "details"; the raw report is the debug
   mode's.
5. **Per section:** diagnostics' row and `Ctrl`+`9` exist only in the debug mode; a song's "resolve its
   identity" (diagnostics' tool) and the "copy id" rows likewise. Providers lose the ids, the capability
   column and "scan only if changed" (debug); plugins show their version, not their id and api; a tool's
   state is in words ("ready", "damaged — install it again"), its version, platform and hashes in the debug
   mode; the permission note is one sentence without decision numbers. The theme guide is folded; "themes
   that could not be loaded" shows only when there is one. The import's and the providers' introductions are
   shorter. A cover's or the lyrics' failure says so in words; its chain is the debug mode's.
6. **The source label** (the user's choice): the local files' tracks carry none; another source's carry its
   name ("Echo (test)", "YouTube Music"), the id in its tooltip; the debug mode labels the local ones `local`.

**Testing:** `ui_settings.rs` — the theme and the debug mode share `ui.json` and writing one keeps the
other, a file from before reads as off; `debug.rs` — only `1` turns it on, other values are left alone, the
choice is kept and the variable holds a launch; `ui_contract.rs` — every core stage has plain words (a stage
taken out of `STAGE_TEXT` fails it), the debug rule outranks the layout's.

**Verification:** the §3.9 container (WebKitGTK 2.54.0; the host's is 2.52.6) with three local tracks, the
echo plugin and an imported export: every section captured before and after; a mixed queue (the local track
without a label, the plugin's with "Echo (test)"); a failing plugin track's notice, plain and in the debug
mode; an import's report and its details; the debug mode turned on from settings — diagnostics' row, the
ids, the capability column, the zero counts and `api 4` back at once — and off; a launch held by `DEBUG=1`;
`DEBUG=yes` ignored with the warning in the log, and `debug` lines in the log with `DEBUG=1`; 900×600.

**Not done:** nothing that was asked. The CLI is unchanged — its output is the developer's by design.

## D-084 — The interface speaks to the listener; the developer's explanations only in the debug mode

**Date:** 2026-10-02 · **Status:** APPLIED (2026-10-02)

**Question:** After D-083 the user: *"It turned out nice again, but what I meant was this: look at the plugins
section — who wants to learn how plugins work in the backend? Or in settings, who wants to see that the data
is kept separately? If you were using this app and weren't very knowledgeable, would you like these? Let's
redo all of these from scratch to be more end-user focused, but when debug is on these texts can appear. I
understand your aim, but being overly developer-focused would hurt us."* D-083 had hidden the developer's
*data*; the *words* still explained how the app works — "Written in JavaScript and run in the engine inside
the app", "Choices this app keeps in its data directory. The CLI makes its own…". Every visible text was
taken out of the page and the script and read as a listener would. Asked (§0.2): the voice
— the user chose **warm, conversational** over short and plain; the names — **everyday words**; and the
music folder, which led to D-085.

**Decision:**

1. **Every section says what it is for, in a sentence**: *"Every song you've ever played, all in one
   place."* (library), *"Your listening, in numbers. Click a year to zoom in on it."* (stats), *"Bring your
   Spotify history along — every song you've played there joins your stats."* (import), *"Listen from
   YouTube Music, SoundCloud and more — add a service and it's ready to play."* (services), *"Make headshell
   work just the way you like."* (settings). How it works — the engine, the catalog's repository and
   hashes, the data directory, the CLI, decision numbers — is a `debug-only` paragraph under it: nothing was
   deleted, the debug mode shows every explanation there was.
2. **Everyday names:** providers → **sources**, plugins → **services**, the group "sources" → **connect**,
   secrets → **keys**, catalog → **browse**, installed → **added**; approve → **allow**, forget consent →
   **revoke**, enable/disable → **turn on/off**, install the tools → **finish setup**, scan → **check for new
   music**, play from → **search here first**, online lookups → **look things up online**, tracks →
   **songs**, listens → **plays**, queue all → **play all**, play box → **search box**. The panels' ids
   (`providers`, `plugins`), the classes of the theme contract and the CLI do not change: the names are the
   listener's.
3. **Notices** keep their plain stage (D-083, K9) and say what to do instead of the core's cause: *"Couldn't
   reach the server — Check your internet connection and try again."* (`STAGE_HINT`, for the stages where
   there is something to do). The core's cause is in the details for everyone and in front in the debug mode.
4. **The rest in the same voice:** the getting-started card (*"Add your music."*, *"Bring your history."*,
   *"Press play."*), the help sheet, the lyrics' notes (*"These lyrics might run a little early or late —
   nudge them…"*), the covers' tooltips (*"found online"*), the import report (new plays, already here, new
   songs; how the songs were matched, in words), the services' cards (*"connects to"*, *"lets you"*, a tool's
   state only when something is to be done: *"Needs a quick setup — press finish setup."*), the result
   notices (*"Found 2 new songs — 3 songs in your music folders now."*, *"Echo (test) is allowed — enjoy!"*),
   the menus. A count that is zero, an id, an `api`, a path, a hash, a reason string: the debug mode's.

**Not changed:** the card the sleeve draws (it is the core's renderer's, with its own snapshot); the CLI's
output — the developer's surface by design; the casing — lowercase titles stay the interface's look.

**Testing:** `ui_contract.rs` (every id the script looks up exists — the copy moved several; the stage words
are complete); the gates.

**Verification:** the §3.9 container (WebKitGTK 2.54.0; the host's is 2.52.6), every section in the
listener's view and a few in the debug mode's; three layout faults found only on screen and fixed — an empty
note keeping its room, a developer paragraph falling into the header's empty right cell, a key in the help
sheet's text breaking its row.

## D-085 — The music folders are chosen in the app

**Date:** 2026-10-02 · **Status:** APPLIED (2026-10-02)

**Question:** The local files came only from `HEADSHELL_MUSIC_DIRS`, or the system's music folder when it was
unset — the interface could only say "set an environment variable", the most developer-facing sentence it
had (found while rewriting the words, D-084). Asked whether this round should only reword it or add a
folder picker; the user: **add the folder picker** (the core keeps the choice, `headshell music-dirs` is its
CLI counterpart).

**Decision:**

1. **The choice is kept by the core** in `preferences.json` (`music_dirs`). Order: `HEADSHELL_MUSIC_DIRS`
   for a run that sets it → the folders chosen in the app → `XDG_MUSIC_DIR` → the system's music folder.
   `Config::music_folders()` gives `MusicFolders { dirs, source, saved, system }`: who decided is always said
   (K9). `None` is nothing chosen; an **empty list is a choice** — no local folders.
2. **Adding keeps what was there:** with nothing chosen, the system's folder stays on the list beside the new
   one. A folder must exist and be a folder; it is kept absolute. Adding one already there changes nothing
   and says so (`changed: false`); taking off one that is not there is an error that lists the folders.
3. **`Config::music_dirs()` is gone:** it could not fail, and reading the chosen folders can — a broken
   `preferences.json` is an error, not a silent fall back to the system's folder. Its callers — the registry,
   the scan reports, the desktop's environment — use `music_folders()`.
4. **`headshell music-dirs [add <dir> | remove <dir>]`** (with `--json`): the folders, who decided, and when
   the songs change — at the next `headshell provider scan`.
5. **The desktop:** sources → *music folders*: one row per folder — its name first, where it is quieter —
   with a remove button, and *add a folder…* (the dialog's folder picker). A change rebuilds the registry —
   the local provider plays only under its folders — and checks for new music at once: *"Added extra3 — 3
   songs in your music folders now."* While the variable decides the run, the list is locked and says so.

**Testing:** the order as a table (the variable, an empty choice, the system's folder said beside a choice);
adding, adding twice, a folder that is not there, the last one off; through the CLI, end to end — listed,
added, kept in `preferences.json`, read by `provider scan` (a song in), taken off and scanned again (the song
out), with a snapshot.

**Verification:** the §3.9 container with a home whose Music folder held two songs: the default folder
listed; a folder added through the dialog (3 songs after the automatic check), taken off (2 songs), the
choice written each time.
