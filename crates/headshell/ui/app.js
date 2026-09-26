// headshell — the desktop interface (PLAN §3.2, D-072).
//
// **The Golden Rule holds here too.** This file makes no decisions: it turns
// a key into a command and draws the data the core returns. The answers to
// "pause or resume", "what's next", "which play counts", "can the plugin
// run" are in the core.
//
// The one exception is deliberate: **the position estimate** (`anchor.js`).
// It is known to be a second copy of the formula, and it is locked down by
// the accuracy set (D-033).
//
// The core's text-producing helpers (`status_text`, `describe`) are **not
// repeated** here. The CLI picks priorities among them to fit a single line;
// the interface has no such squeeze and shows them all side by side. A copy
// that makes no choices does not drift (D-057).
//
// Motion lives in `motion.js` (springs, projection, dragging); only what goes
// where is written here. Text always goes through `textContent`: no data from
// the core or the catalog is interpreted as markup (`ui_contract.rs` forbids
// `innerHTML`).
//
// Identifiers and user-facing text are both in English (D-036, D-073).

import { positionAt, clock } from "./anchor.js";
import { lineAt, readingTop } from "./lyrics.js";
import {
  animate,
  currentMotion,
  flip,
  horizontalDrag,
  invalidateMotion,
  place,
  presentation,
  project,
  rebase,
  rubberband,
  verticalDrag,
  watchMotionPreference,
} from "./motion.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

// ————————————————————————————————————— building elements

/// A small element builder. `text` always goes to `textContent`.
function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") el.className = value;
    else if (key === "text") el.textContent = value;
    else if (key === "dataset") Object.assign(el.dataset, value);
    else if (key.startsWith("on")) el.addEventListener(key.slice(2), value);
    else el.setAttribute(key, value === true ? "" : String(value));
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

const SVG = "http://www.w3.org/2000/svg";

/// An icon from the icon set in `index.html`. The name must be written as a
/// string constant: `ui_contract.rs` checks that every name given to `icon`
/// and `iconRef` calls is defined in the set.
function icon(name) {
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("class", "icon");
  svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS(SVG, "use");
  use.setAttribute("href", iconRef(name));
  svg.append(use);
  return svg;
}

function iconRef(name) {
  return `#i-${name}`;
}

// ————————————————————————————————————— formatting

const numbers = new Intl.NumberFormat("en-US");
const percent = new Intl.NumberFormat("en-US", { style: "percent", maximumFractionDigits: 1 });

function fmt(n) {
  return numbers.format(n);
}

/// A count with its noun: `1 track`, `2 tracks` (D-073).
function countOf(n, noun) {
  return `${fmt(n)} ${n === 1 ? noun : `${noun}s`}`;
}

/// The CLI's `duration()` format: `3h 12m` or `12m`.
function durationText(ms) {
  const minutes = Math.floor(ms / 60000);
  const hours = Math.floor(minutes / 60);
  return hours > 0 ? `${fmt(hours)}h ${minutes % 60}m` : `${minutes}m`;
}

function hoursText(ms) {
  return `${new Intl.NumberFormat("en-US", { maximumFractionDigits: 1 }).format(ms / 3600000)} h`;
}

/// The chain's first cause: with the `STEP:` line and the `→` prefix dropped.
function firstLine(chain) {
  if (!chain) return "unknown error";
  const lines = chain
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("STEP:"));
  return (lines[0] ?? chain).replace(/^→\s*/, "");
}

// ————————————————————————————————————— notices
//
// An error shows up with its stage (K9): "where did it break" should be
// answered at first glance. The full chain stays folded and is copied with a
// single click — `diag`'s last run may have been replaced by another command
// in the meantime, but this block does not change.
//
// A notice comes from below (next to the player) and leaves to the right:
// the close button, running out of time and dragging to the right all exit
// the same way.

const TOAST_LIFETIME = { info: 6000, error: 20000 };
const toastTimers = new Map();

function toast(err, info = false) {
  const box = h("div", { class: info ? "toast info" : "toast", role: info ? "status" : "alert" });

  if (info) {
    box.append(h("div", { text: err }));
  } else {
    const stage = err?.stage ?? "UNKNOWN";
    const chain = err?.chain ?? String(err);
    box.append(
      h("span", { class: "stage", text: `STEP: ${stage}` }),
      h("div", { text: firstLine(err?.chain) }),
      h("details", {}, h("summary", { text: "full chain" }), h("pre", { text: chain })),
      h(
        "div",
        { class: "toast-actions" },
        h(
          "button",
          {
            type: "button",
            onclick: () =>
              copyText(chain.startsWith("STEP:") ? chain : `STEP: ${stage}\n  ${chain}`, "error chain copied to the clipboard"),
          },
          icon("copy"),
          "copy",
        ),
      ),
    );
  }

  box.append(
    h(
      "button",
      {
        class: "close",
        type: "button",
        title: "close",
        "aria-label": "close the notice",
        onclick: () => dismissToast(box),
      },
      icon("close"),
    ),
  );

  const stack = $("toasts");
  flip([...stack.children], () => stack.append(box));
  animate(box, { y: 0, opacity: 1 }, { from: { y: 18, opacity: 0 } });

  const lifetime = info ? TOAST_LIFETIME.info : TOAST_LIFETIME.error;
  armToast(box, lifetime);
  // It must not go while being read: the timer stops while the pointer is on
  // it.
  box.addEventListener("pointerenter", () => clearTimeout(toastTimers.get(box)));
  box.addEventListener("pointerleave", () => armToast(box, Math.min(lifetime, 4000)));

  horizontalDrag(box, {
    ignore: "button, summary, pre",
    onStart: () => {
      clearTimeout(toastTimers.get(box));
      return presentation(box, "x");
    },
    // Pulling left does not close it: the resistance grows and it comes back on
    // release.
    onMove: (x) =>
      place(box, {
        x: x < 0 ? rubberband(x, box.offsetWidth) : x,
        opacity: 1 - Math.max(0, x) / (box.offsetWidth * 1.4),
      }),
    // The decision is made from where the velocity carries it, not from the
    // release point: a short flick closes it too.
    onEnd: (velocity) => {
      const x = presentation(box, "x");
      if (x + project(velocity) > box.offsetWidth * 0.5) {
        dismissToast(box, velocity);
      } else {
        animate(box, { x: 0, opacity: 1 }, { velocity: { x: velocity }, dampingRatio: 0.8 });
        armToast(box, 4000);
      }
    },
  });
}

function armToast(box, ms) {
  clearTimeout(toastTimers.get(box));
  toastTimers.set(
    box,
    setTimeout(() => dismissToast(box), ms),
  );
}

function dismissToast(box, velocity = 0) {
  if (!box.isConnected) return;
  clearTimeout(toastTimers.get(box));
  toastTimers.delete(box);
  animate(
    box,
    { x: box.offsetWidth + 48, opacity: 0 },
    {
      velocity: { x: Math.max(velocity, 0) },
      onRest: () => {
        const stack = $("toasts");
        flip(
          [...stack.children].filter((el) => el !== box),
          () => box.remove(),
        );
      },
    },
  );
}

function dismissAllToasts() {
  for (const box of [...$("toasts").children]) dismissToast(box);
}

/// Calls a command; on an error it shows it and returns `undefined`.
///
/// It does not swallow: every failure appears on screen with its stage.
/// Returning `undefined` is so the caller skips drawing — not a silent empty
/// result.
async function call(command, args = {}) {
  try {
    return await invoke(command, args);
  } catch (err) {
    toast(err);
    return undefined;
  }
}

async function copyText(text, done) {
  try {
    await navigator.clipboard.writeText(text);
    toast(done, true);
  } catch (err) {
    // If the clipboard refuses we do not stay silent: the user would think it was
    // copied and send an empty bug report.
    toast({ stage: "CONFIG_LOAD", chain: `STEP: CONFIG_LOAD\n  → could not write to the clipboard: ${err}` });
  }
}

// ————————————————————————————————————— file dialog
//
// The plugin's JS wrapper comes from npm; this interface has no bundler, so
// the command is called directly. The permission in `capabilities/default.json`
// is limited to `open`/`save`: the dialog returns a **path**, and the side that
// reads and writes that path is the core.

async function pickFile(options) {
  return filePath(await call("plugin:dialog|open", { options }));
}

async function pickSavePath(options) {
  return filePath(await call("plugin:dialog|save", { options }));
}

/// On the desktop the dialog returns a plain path; mobile gives the
/// `content://` form as an object. Accepting both removes an assumption.
function filePath(picked) {
  if (!picked) return null;
  if (typeof picked === "string") return picked;
  return picked.path ?? null;
}

// ————————————————————————————————————— sections
//
// The order is the sidebar's order, and the `Ctrl`+number shortcut comes from
// this order (`ui_contract.rs` checks that the two name the same set). A new
// section is added here, to a group in the sidebar and to `ON_OPEN`.

const PANELS = ["now", "library", "stats", "sleeve", "import", "providers", "plugins", "theme", "diag"];

// The work done when a section opens. All of it is local reading: nothing
// here goes online — the catalog is only read with the button (D-071).
const ON_OPEN = {
  stats: () => {
    if (!stats.loaded) runStats(null);
  },
  sleeve: () => {
    sleeveFormat.placeThumb(true);
    if (!sleeve.previewed) previewSleeve();
  },
  providers: refreshProviders,
  plugins: () => {
    pluginTabs.placeThumb(true);
    refreshPlugins();
  },
  theme: refreshThemes,
  diag: refreshDiag,
};

// "Now playing" is not one of the sections in the content column but a sheet
// over them (D-075). So "where am I" has two answers: the section below, and
// whether the sheet is up — the sidebar shows whichever is in front.
let basePanel = null;
let nowOpen = false;
let currentPanel = "now";
const scrollByPanel = new Map();
const content = $("content");
const nowSheet = $("panel-now");

function openPanel(name, { instant = false } = {}) {
  if (!PANELS.includes(name)) return;
  if (name === "now") {
    setNowOpen(true, { instant });
    return;
  }
  // Chosen while the sheet is up: the section is switched under it, out of
  // sight, and the sheet going down reveals it — one thing moves, not two.
  showBase(name, { instant: instant || nowOpen });
  setNowOpen(false, { instant });
  ON_OPEN[name]?.();
}

/// The section in the content column, under the sheet.
function showBase(name, { instant }) {
  const previous = basePanel;
  basePanel = name;
  if (previous === name) return;
  if (previous) scrollByPanel.set(previous, content.scrollTop);
  for (const section of content.querySelectorAll(".panel")) {
    section.hidden = section.id !== `panel-${name}`;
  }
  content.scrollTop = scrollByPanel.get(name) ?? 0;
  updateScrollEdge();
  // A section lower in the list comes from below: the motion should say where
  // we are going.
  if (!instant && previous) {
    const section = document.getElementById(`panel-${name}`);
    const direction = PANELS.indexOf(name) > PANELS.indexOf(previous) ? 1 : -1;
    animate(section, { y: 0, opacity: 1 }, { from: { y: 10 * direction, opacity: 0 }, responseScale: 0.7 });
  }
}

/// The sidebar follows what is in front: the sheet if it is up, the section
/// under it if not.
function markNav(instant) {
  currentPanel = nowOpen ? "now" : basePanel;
  for (const button of document.querySelectorAll(".nav")) {
    button.setAttribute("aria-current", String(button.dataset.panel === currentPanel));
  }
  movePill(instant);
}

/// Moves the selection indicator under the selected tab. On quick successive
/// selections the spring is taken over: the indicator turns around halfway.
function movePill(instant) {
  const pill = $("navPill");
  const active = document.querySelector(`.nav[data-panel="${currentPanel}"]`);
  if (!active) return;
  const y = active.offsetTop;
  if (instant || pill.hidden) {
    place(pill, { y });
    pill.hidden = false;
  } else {
    animate(pill, { y });
  }
}

for (const button of document.querySelectorAll(".nav")) {
  // Selection happens the moment the mouse goes down: waiting for the release
  // delays the response (desktop sidebars behave like this too). `click` is
  // for the keyboard and touch.
  button.addEventListener("pointerdown", (event) => {
    if (event.button === 0 && event.pointerType !== "touch") openPanel(button.dataset.panel);
  });
  button.addEventListener("click", () => openPanel(button.dataset.panel));
}

// The shortcuts on the "getting started" card.
for (const button of document.querySelectorAll(".go")) {
  button.addEventListener("click", () => {
    const target = button.dataset.go;
    if (target === "focus-play") {
      $("playQuery").focus();
    } else if (target === "plugins") {
      openPanel("plugins");
      pluginTabs.select($("tabCatalog"));
    } else {
      openPanel(target);
    }
  });
}

// The scroll edge: a shadow appears when content slides under the top bar —
// not while the sheet covers that content.
function updateScrollEdge() {
  $("mainColumn").classList.toggle("scrolled", !nowOpen && content.scrollTop > 2);
}
content.addEventListener("scroll", updateScrollEdge, { passive: true });

window.addEventListener("resize", () => {
  movePill(true);
  sleeveFormat.placeThumb(true);
  pluginTabs.placeThumb(true);
  nowTabs.placeThumb(true);
});

// ————————————————————————————————————— segmented control

/// A segmented control: the thumb slides under the selected segment on a
/// spring. The same structure for both options (`radiogroup`) and tabs
/// (`tablist`).
function segmented(container, onSelect) {
  const thumb = container.querySelector(".segmented-thumb");
  const segments = [...container.querySelectorAll(".segment")];
  const attribute = container.getAttribute("role") === "tablist" ? "aria-selected" : "aria-checked";
  const selected = () => segments.find((s) => s.getAttribute(attribute) === "true") ?? segments[0];

  function placeThumb(instant = false) {
    const current = selected();
    if (!current.offsetWidth) return; // no size in a hidden section; it settles when opened
    thumb.style.width = `${current.offsetWidth}px`;
    const x = current.offsetLeft - 2;
    if (instant) place(thumb, { x });
    else animate(thumb, { x });
  }

  function select(segment, { focus = false } = {}) {
    if (!segment || segment.getAttribute(attribute) === "true") return;
    for (const s of segments) {
      s.setAttribute(attribute, String(s === segment));
      s.tabIndex = s === segment ? 0 : -1;
    }
    placeThumb();
    if (focus) segment.focus();
    onSelect(segment);
  }

  for (const segment of segments) {
    segment.tabIndex = segment.getAttribute(attribute) === "true" ? 0 : -1;
    segment.addEventListener("pointerdown", (event) => {
      if (event.button === 0 && event.pointerType !== "touch") select(segment);
    });
    segment.addEventListener("click", () => select(segment));
    segment.addEventListener("keydown", (event) => {
      const step = { ArrowRight: 1, ArrowLeft: -1 }[event.key];
      if (!step) return;
      event.preventDefault();
      const index = (segments.indexOf(segment) + step + segments.length) % segments.length;
      select(segments[index], { focus: true });
    });
  }
  return { select, placeThumb, selected };
}

// ————————————————————————————————————— player
//
// **Everything** kept here came from the core. It is not a source of truth of
// its own but a copy of the last answer: the queue, the anchor.

let anchor = null;
let queue = { items: [], position: 0, repeat: "off", shuffle: false };

// The user-facing name of the repeat mode (D-036). The keys are wire values;
// if an unknown mode arrives, the raw value is shown — silently saying "off"
// would show a wrong state as right (K9).
const REPEAT_LABELS = { off: "off", all: "all", one: "one track" };
const STATE_TEXT = {
  playing: "playing",
  paused: "paused",
  buffering: "buffering",
  stopped: "stopped",
};

function renderQueue(view) {
  if (!view) return;
  queue = view;

  $("queue").replaceChildren(...view.items.map((item, index) => queueRow(item, index, index === view.position)));
  const empty = view.items.length === 0;
  // While the queue is empty the "getting started" card takes the sheet: an
  // empty list does not tell the user what to do.
  $("nowView").hidden = empty;
  $("nowEmpty").hidden = !empty;
  $("queueCount").textContent = empty ? "" : String(view.items.length);
  $("tabQueue").title = empty ? "" : countOf(view.items.length, "track");
  // Nothing to show lyrics for: the whole-sheet view goes back.
  if (empty) setLyricsFull(false);

  const shuffle = $("btnShuffle");
  shuffle.classList.toggle("on", view.shuffle);
  shuffle.setAttribute("aria-pressed", String(view.shuffle));
  const repeat = $("btnRepeat");
  repeat.classList.toggle("on", view.repeat !== "off");
  repeat.setAttribute("aria-pressed", String(view.repeat !== "off"));
  $("repeatIcon").setAttribute("href", view.repeat === "one" ? iconRef("repeat-one") : iconRef("repeat"));
  // The wire value (`off`/`all`/`one`) stays as it is — it is a JSON key.
  // The text the user reads is separate (D-036).
  repeat.title = `repeat: ${REPEAT_LABELS[view.repeat] ?? view.repeat}`;

  renderNow();
  revealCurrentRow();
  // What is already known is painted at once; the core is asked for the rest.
  paintCovers();
  refreshCovers();
}

function queueRow(item, index, current) {
  const track = item.track;
  return h(
    "li",
    { class: current ? "current" : null, "aria-current": current ? "true" : null },
    h(
      "button",
      {
        class: "q-row",
        type: "button",
        title: current ? "now playing" : "skip to this track",
        // The decision to jump in the queue is in the core: only the index is passed
        // on here.
        onclick: async () => {
          renderQueue(await call("jump_to", { index }));
          renderAnchor(await call("anchor"));
        },
      },
      // The playing row shows bars instead of its number (they move while
      // playing, `.queue[data-state]`).
      current
        ? h("span", { class: "index eq", "aria-hidden": "true" }, h("i"), h("i"), h("i"), h("i"))
        : h("span", { class: "index", text: String(index + 1) }),
      // The cover arrives later (`paintCovers`); until then, a small record.
      h("span", { class: "q-cover", "aria-hidden": "true" }),
      h(
        "span",
        { class: "q-main" },
        h("span", { class: "q-title", text: track.title }),
        h("span", { class: "q-artist", text: track.artist }),
      ),
      h("span", { class: "chip", text: item.id.provider, title: "provider" }),
      h("span", { class: "q-time", text: track.duration_ms ? clock(track.duration_ms) : "" }),
    ),
  );
}

/// The playing track: the player bar and the "now playing" sheet's card.
/// The track comes from the queue, the state from the anchor.
function renderNow() {
  const current = queue.items[queue.position];
  const track = current?.track;
  const state = anchor?.state ?? "stopped";

  $("nowTitle").textContent = track ? track.title : "—";
  $("nowArtist").textContent = track ? track.artist : "";
  $("queue").dataset.state = state;
  moveArm();
  paintLabel();
  renderLyricsHead();

  const card = $("nowCard");
  card.hidden = !current;
  if (!current) return;
  card.className = `now-card ${state}`;
  $("nowCardState").textContent = STATE_TEXT[state] ?? state;
  $("nowCardTitle").textContent = track.title;
  $("nowCardArtist").textContent = [track.artist, track.album].filter(Boolean).join(" · ");
  $("nowCardSource").textContent = current.id.provider;
  $("nowCardDuration").textContent = track.duration_ms ? clock(track.duration_ms) : "";
}

function renderAnchor(next) {
  if (!next) return;
  anchor = next;
  const state = next.state;
  $("nowState").className = `state ${state}`;
  const running = state === "playing" || state === "buffering";
  $("playPauseIcon").setAttribute("href", running ? iconRef("pause") : iconRef("play"));
  $("btnPlayPause").setAttribute("aria-label", running ? "pause" : "play");
  $("player").classList.toggle("buffering", state === "buffering");
  renderNow();
  startDrawing();
}

// The drawing loop: the position **is not asked of the core**, it is
// estimated from the anchor (D-015). If IPC stalls, the bar keeps moving. The
// loop only runs while playing and only writes to the DOM when something
// visible changes: a paused track must not rewrite the same text every
// frame.
const barFill = $("barFill");
const timeLabel = $("timeLabel");
let drawing = 0;
let drawnRatio = -1;
let drawnLabel = "";

function draw() {
  if (!anchor) return;
  const position = positionAt(anchor, Date.now());
  const duration = anchor.duration_ms ?? 0;
  const ratio = duration > 0 ? Math.min(position / duration, 1) : 0;
  playedRatio = ratio;
  moveArm();
  if (Math.abs(ratio - drawnRatio) > 0.0004) {
    barFill.style.transform = `scaleX(${ratio})`;
    drawnRatio = ratio;
  }
  const label = `${clock(position)} / ${duration > 0 ? clock(duration) : "—:—"}`;
  if (label !== drawnLabel) {
    timeLabel.textContent = label;
    drawnLabel = label;
  }
  lightLyricLine(position);
}

function drawFrame() {
  drawing = 0;
  draw();
  if (anchor?.state === "playing") drawing = requestAnimationFrame(drawFrame);
}

function startDrawing() {
  if (!drawing) drawing = requestAnimationFrame(drawFrame);
}

// The tonearm on the sheet's turntable reads the record the way the progress
// bar reads the anchor: from the outer groove to the inner one, and back to
// its rest when stopped. The angles (degrees, clockwise) come from the
// drawing (`index.html`, `style.css`): the arm turns on its pivot at 84/16,
// and its stylus — the tip of a headshell turned towards the spindle — is at
// 80.9/79.5 at rest. The record's centre is 44/52; the stylus meets the
// lead-in groove (radius 35) at 11.8° and the run-out (radius 19) at 29.2°.
const ARM_REST = 0;
const ARM_LEAD_IN = 11.8;
const ARM_RUN_OUT = 29.2;
const nowArm = $("nowArm");
let playedRatio = 0;
let armAngle = null;
let armSwinging = false;

function moveArm() {
  const onRecord = (anchor?.state ?? "stopped") !== "stopped" && queue.items.length > 0;
  const angle = onRecord ? ARM_LEAD_IN + (ARM_RUN_OUT - ARM_LEAD_IN) * playedRatio : ARM_REST;
  if (armAngle !== null && Math.abs(angle - armAngle) < 0.05) return;
  const jump = armAngle === null ? 0 : Math.abs(angle - armAngle);
  armAngle = angle;
  // Out of sight there is nothing to watch; the groove-by-groove creep while
  // playing is written straight. A swing — a new track, a start, a stop —
  // goes on a spring, and a swing already under way is taken over, not cut.
  if (nowSheet.hidden || (!armSwinging && jump < 1)) {
    armSwinging = false;
    place(nowArm, { rotate: angle });
  } else {
    armSwinging = true;
    animate(nowArm, { rotate: angle }, { responseScale: 1.6, onRest: () => (armSwinging = false) });
  }
}

async function togglePause() {
  // With an empty queue there is nothing to play: the button shows what to do.
  if (queue.items.length === 0) {
    $("playQuery").focus();
    return;
  }
  renderAnchor(await call("toggle_pause"));
}

async function playNext() {
  renderQueue(await call("next"));
  renderAnchor(await call("anchor"));
}

async function playPrevious() {
  renderQueue(await call("previous"));
  renderAnchor(await call("anchor"));
}

$("btnPlayPause").addEventListener("click", togglePause);
$("btnNext").addEventListener("click", playNext);
$("btnPrev").addEventListener("click", playPrevious);
$("btnStop").addEventListener("click", async () => {
  renderAnchor(await call("stop"));
  renderQueue(await call("queue"));
});
$("btnShuffle").addEventListener("click", async () =>
  renderQueue(await call("set_shuffle", { on: !queue.shuffle })),
);
$("btnRepeat").addEventListener("click", async () => {
  const nextMode = { off: "all", all: "one", one: "off" }[queue.repeat] ?? "off";
  renderQueue(await call("set_repeat", { mode: nextMode }));
});

// ————————————————————————————————————— covers (D-076)
//
// The core's cover worker looks the queue up in the background, the playing
// track first; playback never waits for it. The tick names the covers whose
// answer arrived, the report says for every row what was found, where, or
// why not, and the images come as `data:` URIs — the only images the page's
// CSP allows. Nothing is decided here: the rows and the label only show the
// report.
//
// A cover fades in the first time it appears; after that, a row drawn again
// shows it at once.

let coverReport = null;
const coverThumbs = new Map();
const coverLabels = new Map();
const coversSeen = new Set();
let coversBusy = false;
let coversAgain = false;

const COVER_SOURCES = {
  embedded: "from the file's tags",
  folder: "from an image in the file's folder",
  provider: "from the provider",
  cover_art_archive: "from the Cover Art Archive",
};

/// A row's cover as the user reads it. "Not looked up" and "not found" are
/// different answers (K9); an unknown status is shown raw, not guessed.
function coverText(item) {
  switch (item.status) {
    case "found":
      return `cover ${COVER_SOURCES[item.source] ?? item.source}`;
    case "not_found":
      return `no cover found — ${item.detail}`;
    case "not_checked_offline":
      return "cover not looked up — offline (HEADSHELL_ONLINE=1 asks MusicBrainz)";
    case "failed":
      return `the cover failed — ${firstLine(item.chain)}`;
    case "pending":
      return "looking for the cover…";
    default:
      return `cover: ${item.status}`;
  }
}

/// The report's row for a queue index — only if it is still the same track:
/// a report from before the last queue change is not painted on new rows.
function coverItemAt(index) {
  const item = coverReport?.items[index];
  const queued = queue.items[index];
  if (!item || !queued) return null;
  const same =
    item.provider === queued.id.provider && item.title === queued.track.title && item.artist === queued.track.artist;
  return same ? item : null;
}

/// Asks the core for the queue's covers and fetches the images that are new.
/// One request at a time: a call that comes while one is under way asks
/// again when it ends, so the last answer is always the newest.
async function refreshCovers() {
  if (coversBusy) {
    coversAgain = true;
    return;
  }
  coversBusy = true;
  try {
    do {
      coversAgain = false;
      const report = await call("artwork_queue");
      if (!report) return;
      coverReport = report;
      const found = new Set(report.items.filter((item) => item.status === "found").map((item) => item.key));
      // Only the queue's covers are kept: a long session must not hold every
      // album it ever played.
      for (const cache of [coverThumbs, coverLabels]) {
        for (const key of cache.keys()) if (!found.has(key)) cache.delete(key);
      }
      await fetchCovers([...found].filter((key) => !coverThumbs.has(key)), "thumb", coverThumbs);
      const current = coverItemAt(queue.position);
      if (current?.status === "found" && !coverLabels.has(current.key)) {
        await fetchCovers([current.key], "label", coverLabels);
      }
      paintCovers();
    } while (coversAgain);
  } finally {
    coversBusy = false;
  }
}

async function fetchCovers(keys, variant, into) {
  if (keys.length === 0) return;
  for (const picture of (await call("artwork_images", { keys, variant })) ?? []) {
    into.set(picture.key, picture.uri);
  }
}

function paintCovers() {
  const rows = $("queue").children;
  const appeared = [];
  for (let index = 0; index < rows.length; index += 1) {
    const slot = rows[index].querySelector(".q-cover");
    if (!slot) continue;
    const item = coverItemAt(index);
    slot.title = item ? coverText(item) : "";
    const uri = item?.status === "found" ? coverThumbs.get(item.key) : undefined;
    const shown = slot.querySelector("img");
    if (!uri) {
      shown?.remove();
      continue;
    }
    if (shown?.dataset.key === item.key) continue;
    const img = h("img", { alt: "", src: uri, dataset: { key: item.key } });
    if (shown) shown.replaceWith(img);
    else slot.append(img);
    if (!coversSeen.has(item.key)) {
      animate(img, { opacity: 1 }, { from: { opacity: 0 } });
      appeared.push(item.key);
    }
  }
  for (const key of appeared) coversSeen.add(key);
  paintLabel();
  paintCoverNote();
}

// The playing track's cover on the record's label. Without one the plain
// label stays — the look the turntable had before covers.
const nowCover = $("nowCover");
const nowCoverImage = $("nowCoverImage");
let labelKey = null;

function paintLabel() {
  const item = coverItemAt(queue.position);
  const uri = item?.status === "found" ? coverLabels.get(item.key) : undefined;
  const key = uri ? item.key : null;
  if (key === labelKey) return;
  labelKey = key;
  // `hidden` is an HTML property; on an SVG element only the attribute works.
  nowCover.toggleAttribute("hidden", !uri);
  if (!uri) return;
  nowCoverImage.setAttribute("href", uri);
  animate(nowCover, { opacity: 1 }, { from: { opacity: 0 } });
  renderLyricsHead();
}

/// What the covers could not do, under the queue's title — only when there
/// is something the user can act on. The full count is in its tooltip.
function paintCoverNote() {
  const note = $("coverNote");
  const s = coverReport?.summary;
  const parts = [];
  if (s?.not_checked_offline > 0) {
    parts.push(
      `Offline — no cover looked up for ${countOf(s.not_checked_offline, "track")}. ` +
        "HEADSHELL_ONLINE=1 at startup asks MusicBrainz and the Cover Art Archive.",
    );
  }
  if (s?.failed > 0) {
    parts.push(`The cover of ${countOf(s.failed, "track")} failed — the row's tooltip says why.`);
  }
  note.hidden = parts.length === 0;
  note.textContent = parts.join(" ");
  note.title = s ? coverSummaryText(s) : "";
}

function coverSummaryText(s) {
  const parts = [
    [s.embedded, "embedded"],
    [s.folder, "folder"],
    [s.provider, "provider"],
    [s.cover_art_archive, "Cover Art Archive"],
    [s.not_found, "not found"],
    [s.not_checked_offline, "not looked up (offline)"],
    [s.failed, "failed"],
    [s.pending, "pending"],
  ]
    .filter(([n]) => n > 0)
    .map(([n, label]) => `${fmt(n)} ${label}`);
  return `covers of ${countOf(s.tracks, "track")}: ${parts.join(" · ") || "none"}`;
}

// ————————————————————————————————————— lyrics (D-078)
//
// The core looks the playing track's lyrics up in the background: where the
// track lives at once; LRCLIB, for a local file's or a server's track, only
// while a lyrics view is on screen — the user's rule, and this file keeps its
// half of it: the view being shown is the request (`lyrics_view`). A plugin's
// track is looked up at once; it plays from the internet anyway. The status,
// the source, how the timing fits and the shift are the core's; this only
// draws them.
//
// The line being sung is estimated every frame from the anchor, as the
// progress bar is (`lyrics.js`, bound to the core's copy by a shared set of
// cases).
//
// Following it is one spring on the list's transform. The scroll position
// jumps to where the line reads, the list is put back where it was seen and
// springs to rest: a glide that animates no scroll offset (D-028). A line that
// comes mid-glide carries the glide on from where the list is seen
// (`rebase`). The user's own scroll takes over at once — the glide's
// remainder is folded into the scroll position — and every line comes up to
// be read; following resumes a few seconds after the last scroll.

const LYRIC_DIM = 0.34;
const LYRIC_BROWSING = 0.8;
const FOLLOW_PAUSE_MS = 4000;
const SHIFT_STEP_MS = 500;

const lyricsScroll = $("lyricsScroll");
const lyricsList = $("lyricsLines");
const lyricsFullView = $("lyricsFull");
const nowView = $("nowView");

let lyricsItem = null;
let lyricsShown = false;
let lyricsFull = false;
let lyricsDrawn = "";
let litLine = null;
let litElement = null;
let lightStale = true;
let followInstant = true;
let browsing = false;
let browseTimer = 0;

const LYRICS_FROM = {
  sidecar: "from the .lrc beside the file",
  embedded: "from the file's tags",
  lrclib: "from LRCLIB",
};

const nowTabs = segmented($("nowTabs"), (tab) => {
  const lyrics = tab.id === "tabLyrics";
  $("queuePanel").hidden = lyrics;
  $("lyricsHome").hidden = !lyrics;
  $("btnLyricsExpand").hidden = !lyrics;
  followInstant = true;
  lightStale = true;
  updateLyricsView();
  if (lyrics) draw();
  else revealCurrentRow();
});

function lyricsTabSelected() {
  return nowTabs.selected().id === "tabLyrics";
}

/// Is a lyrics view on screen? That is the user asking for lyrics.
function lyricsOnScreen() {
  return nowOpen && (lyricsFull || lyricsTabSelected());
}

/// Tells the core when a lyrics view comes on screen or goes: only while one
/// is there are a local file's and a server's lyrics looked up online.
async function updateLyricsView() {
  const shown = lyricsOnScreen();
  if (shown === lyricsShown) return;
  lyricsShown = shown;
  if (shown) {
    followInstant = true;
    lightStale = true;
  }
  const item = await call("lyrics_view", { open: shown });
  if (item !== undefined) showLyrics(item);
}

async function refreshLyrics() {
  const item = await call("lyrics_now");
  if (item !== undefined) showLyrics(item);
}

/// The core's answer for the playing track — `null` when nothing plays.
function showLyrics(item) {
  lyricsItem = item;
  const lyrics = item?.lyrics ?? null;
  const drawn = item ? `${item.key}\u0001${item.status}\u0001${JSON.stringify(lyrics)}` : "";
  if (drawn !== lyricsDrawn) {
    lyricsDrawn = drawn;
    drawLyricLines(lyrics);
  }
  paintLyricsNote(item);
  paintLyricsSource(item);
  paintLyricsShift(item);
  // The line lit now, at once: a new track, a new shift.
  lightStale = true;
  draw();
}

function drawLyricLines(lyrics) {
  litLine = null;
  litElement = null;
  followInstant = true;
  place(lyricsList, { y: 0 });
  const synced = Boolean(lyrics?.synced);
  lyricsList.classList.toggle("unsynced", Boolean(lyrics) && !synced);
  lyricsList.replaceChildren(
    ...(lyrics?.lines ?? []).map((line) => {
      const gap = line.text.trim() === "";
      // A pause in synced lyrics shows as a note; a blank line in unsynced
      // words parts the verses.
      const el = h("li", { class: gap ? "lyric gap" : "lyric" }, gap && synced ? "♪" : line.text);
      if (synced) place(el, { opacity: browsing ? LYRIC_BROWSING : LYRIC_DIM });
      return el;
    }),
  );
  lyricsScroll.scrollTop = 0;
}

/// Lights the line sung at `position` and follows it — every frame, from
/// `draw`; it writes to the DOM only when the line changes.
function lightLyricLine(position) {
  const lyrics = lyricsItem?.lyrics;
  if (!lyricsShown || !lyrics?.synced) return;
  const index = lineAt(lyrics, position, lyricsItem.offset_ms ?? 0);
  if (index === litLine && !lightStale) return;
  lightStale = false;
  litLine = index;
  const current = index === null ? null : lyricsList.children[index] ?? null;
  if (litElement && litElement !== current) {
    animate(litElement, { opacity: browsing ? LYRIC_BROWSING : LYRIC_DIM });
  }
  if (current && current !== litElement) {
    animate(current, { opacity: 1 });
    // A small lift as it lights, settling at rest: a transform left on text
    // would blur it.
    animate(current, { scale: 1 }, { from: { scale: 0.985 } });
  }
  litElement = current;
  followLine();
}

/// Brings the lit line — or the first, before it — to the reading position.
function followLine() {
  const instant = followInstant;
  followInstant = false;
  if (!lyricsShown || browsing || !lyricsItem?.lyrics?.synced) return;
  const target = litElement ?? lyricsList.firstElementChild;
  if (!target || !lyricsScroll.clientHeight) return;
  const before = lyricsScroll.scrollTop;
  lyricsScroll.scrollTop = readingTop(
    target.offsetTop,
    target.offsetHeight,
    lyricsScroll.clientHeight,
    lyricsScroll.scrollHeight,
  );
  const jump = lyricsScroll.scrollTop - before;
  if (Math.abs(jump) < 0.5) return;
  if (instant || !currentMotion().spatial) {
    place(lyricsList, { y: 0 });
    return;
  }
  // Put back where it was seen, and let it glide to rest.
  rebase(lyricsList, "y", jump);
  animate(lyricsList, { y: 0 }, { responseScale: 1.5 });
}

/// The user scrolls the lyrics: their hand wins at once.
function startBrowsing() {
  const shift = presentation(lyricsList, "y");
  if (shift !== 0) {
    // What the user grabs is where the lines are seen.
    lyricsScroll.scrollTop -= shift;
    place(lyricsList, { y: 0 });
  }
  if (!browsing && lyricsItem?.lyrics?.synced) {
    for (const line of lyricsList.children) {
      if (line !== litElement) animate(line, { opacity: LYRIC_BROWSING });
    }
  }
  browsing = true;
  clearTimeout(browseTimer);
  browseTimer = setTimeout(stopBrowsing, FOLLOW_PAUSE_MS);
}

function stopBrowsing() {
  browsing = false;
  if (lyricsItem?.lyrics?.synced) {
    for (const line of lyricsList.children) {
      if (line !== litElement) animate(line, { opacity: LYRIC_DIM });
    }
  }
  followLine();
}

for (const type of ["wheel", "touchstart", "pointerdown"]) {
  lyricsScroll.addEventListener(type, startBrowsing, { passive: true });
}
lyricsScroll.addEventListener("keydown", (event) => {
  if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End"].includes(event.key)) startBrowsing();
});

/// `1.2 s` — to a tenth.
function seconds(ms) {
  const magnitude = Math.abs(ms);
  return `${Math.floor(magnitude / 1000)}.${Math.floor((magnitude % 1000) / 100)} s`;
}

function lengthDifference(deltaMs) {
  if (!deltaMs) return "of this recording's length";
  return `${seconds(deltaMs)} ${deltaMs > 0 ? "longer" : "shorter"}`;
}

/// Why there are no lyrics, or what to know about these. "Not looked up",
/// "not found" and "failed" are different answers (K9); an unknown status is
/// shown raw, not guessed.
function paintLyricsNote(item) {
  const note = $("lyricsNote");
  let text = null;
  let warn = false;
  let retry = false;
  if (!item) {
    text = "Nothing is playing.";
  } else {
    switch (item.status) {
      case "pending":
        text = "Looking for the lyrics…";
        break;
      case "not_requested":
        text = "The lyrics are looked up while this view is open.";
        break;
      case "not_found":
        text = `No lyrics for this recording — ${item.detail}.`;
        break;
      case "instrumental":
        text = "Instrumental — no words.";
        break;
      case "failed":
        text = `The lyrics could not be looked up — ${firstLine(item.chain)}`;
        retry = true;
        break;
      case "found":
        if (item.synced && item.fit === "closest") {
          warn = true;
          text =
            "The timing may be off: LRCLIB has no version of this recording's length, and its nearest " +
            `is ${lengthDifference(item.delta_ms)}. Shift the lines until they meet the singing.`;
        } else if (!item.synced) {
          text = "Words without their timing — nothing to follow.";
        } else if (item.fit === "unmeasured") {
          text = "The recording's length is not known, so the timing could not be checked.";
        }
        break;
      default:
        text = `lyrics: ${item.status}`;
    }
  }
  note.hidden = !text;
  note.classList.toggle("warn", warn);
  note.replaceChildren(...(text ? [h("span", { text })] : []));
  if (retry) {
    note.append(h("br"), h("button", { type: "button", onclick: retryLyrics }, "try again"));
  }
}

async function retryLyrics() {
  const item = await call("lyrics_view", { open: true });
  if (item !== undefined) showLyrics(item);
}

/// Where they came from — the source, how the version fits, and the
/// service's credit when it gives one.
function paintLyricsSource(item) {
  const el = $("lyricsSource");
  if (!item || !["found", "instrumental"].includes(item.status)) {
    el.textContent = "";
    el.title = "";
    return;
  }
  let from = item.source === "provider" ? `from ${item.provider}` : (LYRICS_FROM[item.source] ?? `from ${item.source}`);
  if (item.source === "lrclib" && item.fit === "length") {
    from += ` · its version ${lengthDifference(item.delta_ms)}`;
  }
  const credit = item.lyrics?.attribution;
  el.textContent = credit ? `${from} · ${credit}` : from;
  el.title = el.textContent;
}

function paintLyricsShift(item) {
  const box = $("lyricsShift");
  const synced = item?.status === "found" && item.synced;
  box.hidden = !synced;
  if (!synced) return;
  // The nearest version only: the shift is what the user needs now.
  box.classList.toggle("urge", item.fit === "closest");
  const offset = item.offset_ms ?? 0;
  const value = $("btnShiftReset");
  value.textContent = offset === 0 ? "on time" : `${seconds(offset)} ${offset > 0 ? "sooner" : "later"}`;
  value.classList.toggle("shifted", offset !== 0);
  value.disabled = offset === 0;
}

/// The shift is kept by the core, per track (`lyrics_shift`): the next time
/// the track plays it is already right.
async function shiftLyrics(stepMs) {
  if (!lyricsItem) return;
  const offsetMs = stepMs === null ? 0 : (lyricsItem.offset_ms ?? 0) + stepMs;
  const item = await call("lyrics_shift", { key: lyricsItem.key, offsetMs });
  if (item !== undefined) showLyrics(item);
}

$("btnShiftSooner").addEventListener("click", () => shiftLyrics(SHIFT_STEP_MS));
$("btnShiftLater").addEventListener("click", () => shiftLyrics(-SHIFT_STEP_MS));
$("btnShiftReset").addEventListener("click", () => shiftLyrics(null));

// The whole-sheet view: it grows out of the lyrics column and goes back into
// it (spatial continuity), the record and the queue fading behind it. Caught
// halfway, it turns around from where it is. Reduced motion: a fade.

function renderLyricsHead() {
  const track = queue.items[queue.position]?.track;
  $("lyricsFullTitle").textContent = track?.title ?? "";
  $("lyricsFullArtist").textContent = track ? [track.artist, track.album].filter(Boolean).join(" · ") : "";
  lyricsFullView.classList.toggle("playing", anchor?.state === "playing");
  const uri = labelKey ? coverLabels.get(labelKey) : undefined;
  const cover = $("lyricsFullCover");
  cover.hidden = !uri;
  if (uri && cover.getAttribute("src") !== uri) cover.src = uri;
}

function setLyricsFull(full) {
  if (full === lyricsFull) return;
  lyricsFull = full;
  if (full) {
    if (lyricsFullView.hidden) {
      // The origin is the lyrics column's centre, where the view grows from.
      const from = $("lyricsHome").getBoundingClientRect();
      lyricsFullView.hidden = false;
      const box = lyricsFullView.getBoundingClientRect();
      lyricsFullView.style.transformOrigin = `${from.left + from.width / 2 - box.left}px ${from.top + from.height / 2 - box.top}px`;
      place(lyricsFullView, { opacity: 0, scale: 0.94 });
    }
    $("lyricsFullBody").append($("lyricsPanel"));
    nowView.inert = true;
    animate(lyricsFullView, { opacity: 1, scale: 1 });
    animate(nowView, { opacity: 0 });
    renderLyricsHead();
    $("btnLyricsCollapse").focus();
  } else {
    nowView.inert = false;
    animate(nowView, { opacity: 1 });
    animate(
      lyricsFullView,
      { opacity: 0, scale: 0.94 },
      {
        onRest: () => {
          lyricsFullView.hidden = true;
          $("lyricsHome").append($("lyricsPanel"));
          followInstant = true;
          lightStale = true;
          draw();
        },
      },
    );
    if (lyricsFullView.contains(document.activeElement)) $("btnLyricsExpand").focus();
  }
  followInstant = true;
  lightStale = true;
  updateLyricsView();
  draw();
}

$("btnLyricsExpand").addEventListener("click", () => setLyricsFull(true));
$("btnLyricsCollapse").addEventListener("click", () => setLyricsFull(false));

// ————————————————————————————————————— now playing sheet (D-075)
//
// It rises out of the player bar and goes back into it (spatial continuity).
// It opens from a click anywhere on the bar outside its buttons, the chevron,
// the sidebar row, Ctrl+1, starting a track — and from a drag: upwards on the
// bar, downwards on the sheet. A drag follows the pointer 1:1; on release the
// momentum decides where it goes, not the release point, and the velocity is
// handed to the spring. Caught halfway, it carries on from where it is.
//
// The sheet, the scrim under it and the chevron on the bar move as one: all
// three are written from the sheet's position, and on a spring they get the
// same parameters and proportional velocities. A spring is linear, so they
// stay in step without a per-frame hook (`motion_js.rs` locks that down).

const player = $("player");
const nowScrim = $("nowScrim");
const nowToggle = $("btnNowToggle");
const nowToggleIcon = $("nowToggleIcon");

/// How far the sheet travels: the height of the row it covers.
function sheetTravel() {
  return content.offsetHeight || window.innerHeight;
}

/// The two that follow the sheet's position `y` (0 open, `travel` closed):
/// the scrim darkens as the sheet rises, the chevron turns over. Each with its
/// change per pixel, for handing over a velocity.
function followers(y, travel) {
  return [
    [nowScrim, "opacity", 1 - y / travel, -1 / travel],
    [nowToggleIcon, "scaleY", (2 * y) / travel - 1, 2 / travel],
  ];
}

function placeSheet(y) {
  place(nowSheet, { y });
  for (const [el, prop, value] of followers(y, sheetTravel())) place(el, { [prop]: value });
}

/// Without a `velocity` the springs keep the one they have: a sheet turned
/// around by a click carries on smoothly instead of stopping dead.
function springSheet(target, velocity, onRest) {
  const handed = velocity !== undefined;
  animate(nowSheet, { y: target }, { velocity: handed ? { y: velocity } : undefined, onRest });
  for (const [el, prop, value, perPixel] of followers(target, sheetTravel())) {
    animate(el, { [prop]: value }, { velocity: handed ? { [prop]: velocity * perPixel } : undefined });
  }
}

/// Puts the sheet into the layout where it hides: below its row — or in its
/// place but transparent, when motion is reduced and it is going to fade in.
function unfoldSheet({ fade = false } = {}) {
  if (!nowSheet.hidden) return;
  nowSheet.hidden = false;
  nowScrim.hidden = false;
  content.classList.add("covered");
  if (fade) {
    place(nowSheet, { y: 0, opacity: 0 });
    place(nowScrim, { opacity: 0 });
  } else {
    place(nowSheet, { opacity: 1 });
    placeSheet(sheetTravel());
  }
}

function setNowOpen(open, { instant = false, velocity } = {}) {
  nowOpen = open;
  markNav(instant);
  updateScrollEdge();
  nowToggle.setAttribute("aria-expanded", String(open));
  nowToggle.title = open ? "close now playing (Esc)" : "now playing (Ctrl+1)";
  // What the sheet covers is out of reach while it is up, for the pointer and
  // the keyboard alike; it is back the moment the sheet starts down.
  content.inert = open;

  if (!open && nowSheet.hidden) return;
  // A focused element leaving with the sheet would take the focus with it.
  if (!open && nowSheet.contains(document.activeElement)) nowToggle.focus();

  const target = open ? 0 : sheetTravel();
  const done = open
    ? undefined
    : () => {
        nowSheet.hidden = true;
        nowScrim.hidden = true;
        content.classList.remove("covered");
      };
  // Reduced motion: no slide but a fade in place — unless a drag has already
  // moved the sheet out of place; then it just lands.
  const spatial = currentMotion().spatial;
  const fade = !instant && !spatial && (nowSheet.hidden || presentation(nowSheet, "y") === 0);
  unfoldSheet({ fade });
  if (fade) {
    place(nowToggleIcon, { scaleY: open ? -1 : 1 });
    animate(nowSheet, { opacity: open ? 1 : 0 }, { onRest: done });
    animate(nowScrim, { opacity: open ? 1 : 0 });
  } else if (instant || !spatial) {
    placeSheet(target);
    done?.();
  } else {
    springSheet(target, velocity, done);
  }
  if (open) {
    nowTabs.placeThumb(true);
    revealCurrentRow();
  }
  updateLyricsView();
}

/// Keeps the playing row in sight in the queue. Not `scrollIntoView`: it would
/// also scroll the clipped column the sheet lives in.
function revealCurrentRow() {
  if (nowSheet.hidden) return;
  const list = $("queue");
  const row = list.querySelector("li.current");
  if (!row) return;
  const top = row.offsetTop;
  const bottom = top + row.offsetHeight;
  if (top < list.scrollTop) list.scrollTop = top - 8;
  else if (bottom > list.scrollTop + list.clientHeight) list.scrollTop = bottom - list.clientHeight + 8;
}

/// Where the sheet goes when let go: where its momentum carries it, not where
/// it was released — a short flick is enough.
function releaseSheet(velocity) {
  const landing = presentation(nowSheet, "y") + project(velocity);
  setNowOpen(landing < sheetTravel() / 2, { velocity });
}

/// Past the open position the sheet resists — there is nothing more above;
/// past the closed one it resists the same way, out of sight.
function dragSheetTo(y) {
  const travel = sheetTravel();
  if (y < 0) placeSheet(rubberband(y, travel));
  else if (y > travel) placeSheet(travel + rubberband(y - travel, travel));
  else placeSheet(y);
}

verticalDrag(nowSheet, {
  // The handle, the turntable and the empty space pull the sheet; the text,
  // the queue and the other buttons keep their own gestures.
  ignore: "button:not(.grabber), a, input, select, summary, .now-meta, .now-side, .lyrics-panel, .onboarding",
  onStart: () => presentation(nowSheet, "y"),
  onMove: dragSheetTo,
  onEnd: releaseSheet,
});

verticalDrag(player, {
  ignore: "button",
  onStart: () => {
    player.classList.remove("pressed");
    unfoldSheet();
    return presentation(nowSheet, "y");
  },
  onMove: dragSheetTo,
  onEnd: releaseSheet,
});

// The tap rule: the press shows on the way down, the sheet moves on the way
// up. A drag that follows swallows the click (`motion.js`).
player.addEventListener("pointerdown", (event) => {
  if (event.button === 0 && !event.target.closest("button")) player.classList.add("pressed");
});
// A drag up from the bar crosses the sheet's text. The bar's own text cannot
// be selected, but the press would still begin a selection that the gesture
// carries into the page — it was seen selecting the whole sheet.
player.addEventListener("mousedown", (event) => {
  if (!event.target.closest("button")) event.preventDefault();
});
for (const type of ["pointerup", "pointercancel", "pointerleave"]) {
  player.addEventListener(type, () => player.classList.remove("pressed"));
}
player.addEventListener("click", (event) => {
  if (!event.target.closest("button")) setNowOpen(!nowOpen);
});
nowToggle.addEventListener("click", () => setNowOpen(!nowOpen));
$("btnNowGrabber").addEventListener("click", () => setNowOpen(false));

// ————————————————————————————————————— play

function playQuery() {
  return $("playQuery").value.trim();
}

async function startPlay(query, all) {
  const view = await call("play", { query, all, shuffle: false });
  if (!view) return;
  renderQueue(view);
  renderAnchor(await call("anchor"));
  openPanel("now");
}

$("playForm").addEventListener("submit", (event) => {
  event.preventDefault();
  const query = playQuery();
  if (query) startPlay(query, false);
});

// Shift+Enter: queue every matching track. The **core** builds the queue;
// the results are not collected and sent from here.
$("playQuery").addEventListener("keydown", (event) => {
  if (event.key !== "Enter" || !event.shiftKey) return;
  event.preventDefault();
  const query = playQuery();
  if (query) startPlay(query, true);
});

$("btnPlayAll").addEventListener("click", () => {
  const query = playQuery();
  if (query) startPlay(query, true);
  else $("playQuery").focus();
});

// ————————————————————————————————————— keyboard
//
// Shortcuts do not work while in a text box: the space key must type a space
// in the search box, not stop playback.

function isTyping(target) {
  if (!target) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

document.addEventListener("keydown", (event) => {
  if (event.defaultPrevented) return;
  if (helpOpen) {
    if (event.key === "Escape") closeHelp();
    // While the sheet is open, focus stays inside it; the shortcuts behind it go
    // quiet.
    if (event.key === "Tab") {
      event.preventDefault();
      $("btnHelpClose").focus();
    }
    return;
  }
  if (event.key === "Escape") {
    // The topmost goes first: the notices lie over the sheet, the
    // whole-sheet lyrics over the record.
    if (toastTimers.size > 0) dismissAllToasts();
    else if (nowOpen && lyricsFull) setLyricsFull(false);
    else if (nowOpen) setNowOpen(false);
    return;
  }
  // Ctrl+1…9: sections. It works in a text box too — pressing a number key
  // with Ctrl is not typing.
  if ((event.ctrlKey || event.metaKey) && !event.altKey) {
    const slot = Number(event.key);
    if (Number.isInteger(slot) && slot >= 1 && slot <= PANELS.length) {
      event.preventDefault();
      openPanel(PANELS[slot - 1]);
    }
    return;
  }
  if (isTyping(event.target) || event.ctrlKey || event.metaKey || event.altKey) return;

  if (event.key === " ") {
    event.preventDefault();
    togglePause();
  } else if (event.key === "ArrowRight") {
    event.preventDefault();
    playNext();
  } else if (event.key === "ArrowLeft") {
    event.preventDefault();
    playPrevious();
  } else if (event.key === "/") {
    event.preventDefault();
    $("playQuery").focus();
  } else if (event.key === "l" || event.key === "L") {
    // The lyrics; pressed again, back to up next.
    event.preventDefault();
    if (!nowOpen) {
      nowTabs.select($("tabLyrics"));
      setNowOpen(true);
    } else if (lyricsFull) {
      setLyricsFull(false);
    } else {
      nowTabs.select(lyricsTabSelected() ? $("tabQueue") : $("tabLyrics"));
    }
  } else if (event.key === "?") {
    event.preventDefault();
    openHelp();
  }
});

// ————————————————————————————————————— shortcut sheet
//
// It grows out of the button that opens it and goes back to it when closing
// (spatial continuity). If reopened while closing, it turns around halfway;
// it does not wait to finish.

let helpOpen = false;
let helpReturnFocus = null;

function openHelp() {
  const sheet = $("helpSheet");
  const box = $("helpBox");
  if (!helpOpen) helpReturnFocus = document.activeElement;
  helpOpen = true;
  const fresh = sheet.hidden;
  sheet.hidden = false;

  const origin = $("btnHelp").getBoundingClientRect();
  const rect = box.getBoundingClientRect();
  box.style.transformOrigin = `${origin.left + origin.width / 2 - rect.left}px ${origin.top + origin.height / 2 - rect.top}px`;

  animate(sheet, { opacity: 1 }, fresh ? { from: { opacity: 0 } } : {});
  animate(box, { scale: 1, opacity: 1 }, fresh ? { from: { scale: 0.92, opacity: 0 } } : {});
  $("btnHelpClose").focus();
}

function closeHelp() {
  if (!helpOpen) return;
  helpOpen = false;
  const sheet = $("helpSheet");
  const box = $("helpBox");
  animate(sheet, { opacity: 0 });
  animate(box, { scale: 0.92, opacity: 0 }, { onRest: () => (sheet.hidden = true) });
  helpReturnFocus?.focus?.();
}

$("btnHelp").addEventListener("click", openHelp);
$("btnHelpClose").addEventListener("click", closeHelp);
$("helpSheet").addEventListener("pointerdown", (event) => {
  // Clicking outside the box closes it; clicking inside does not.
  if (event.target === $("helpSheet")) closeHelp();
});

// ————————————————————————————————————— library search

const SEARCH_LIMIT = 100;
let lastSearchQuery = null;

$("searchForm").addEventListener("submit", async (event) => {
  event.preventDefault();
  const query = $("searchQuery").value.trim();
  if (!query) return;
  const report = await call("search", { query, limit: SEARCH_LIMIT, minMs: null });
  if (!report) return;
  // A copy of the last query — so "queue them all" does not make the user type
  // the query again.
  lastSearchQuery = report.query;

  const hits = report.hits;
  $("searchResult")
    .querySelector("tbody")
    .replaceChildren(
      ...hits.map((hit) =>
        h(
          "tr",
          {},
          h("td", { text: hit.title }),
          h("td", { text: hit.artist }),
          h("td", { class: "dim", text: hit.album ?? "" }),
          h("td", { class: "num", text: fmt(hit.play_count) }),
          h("td", { class: "num dim", text: durationText(hit.ms_played) }),
          h(
            "td",
            {},
            h(
              "button",
              { type: "button", onclick: () => startPlay(`${hit.artist} ${hit.title}`, false) },
              icon("play"),
              "play",
            ),
          ),
        ),
      ),
    );
  const found = hits.length > 0;
  $("searchIdle").hidden = true;
  $("searchResult").hidden = !found;
  $("btnSearchPlayAll").hidden = !found;
  $("searchCount").hidden = !found;
  $("searchCount").textContent =
    hits.length >= SEARCH_LIMIT ? `first ${fmt(SEARCH_LIMIT)} results` : `${countOf(hits.length, "result")}`;
  $("searchEmpty").hidden = found;
  $("searchEmpty").textContent = `no records found for "${report.query}".`;
});

// The **core** builds the queue: the result rows are not collected and sent
// from here; the same query is given again with the `all` flag. A second
// copy of the matching must not live in the interface.
$("btnSearchPlayAll").addEventListener("click", () => {
  if (lastSearchQuery) startPlay(lastSearchQuery, true);
});

// ————————————————————————————————————— statistics
//
// The year bars are both a chart and a selector: clicking a bar narrows to
// that year, clicking the selected bar again goes back to all time. The bars
// themselves come from the all-time report's `by_year`; they do not change
// when a year is selected, only which one is selected does.

const stats = { loaded: false, year: null, years: [] };

async function runStats(year) {
  const response = await call("stats", {
    year,
    top: Number($("statsTop").value) || 10,
    minMs: null,
  });
  if (!response) return;
  const report = response.report;
  stats.loaded = true;
  stats.year = year;
  if (year === null) {
    stats.years = report.by_year;
    renderYears();
  }
  renderStats(report);
  updateYearSelection();
}

function figure(big, label, muted = false) {
  return h(
    "div",
    { class: muted ? "muted" : null },
    h("span", { class: "big", text: big }),
    h("span", { class: "label", text: label }),
  );
}

function renderStats(r) {
  const scope = r.query.year ?? "all time";
  $("statsScope").textContent = `${scope} · ${countOf(r.listens_in_scope, "record")} in scope`;
  $("statsSummary").replaceChildren(
    figure(fmt(r.plays), "counted listens"),
    figure(hoursText(r.total_ms_played), "total time"),
    figure(fmt(r.unique_artists), "artists"),
    figure(fmt(r.unique_tracks), "tracks"),
  );
  // Partial success is **always** reported: how many records fell below the
  // threshold, how many have no canonical identity, how many are outside the
  // year. Nothing is swallowed silently (K9).
  const notes = [
    `${countOf(r.skipped_short, "short play")} below the threshold`,
    `${countOf(r.without_canonical_id, "record")} without a canonical identity`,
  ];
  if (r.query.year !== null) notes.push(`${countOf(r.out_of_scope, "record")} outside the year`);
  $("statsNotes").textContent = notes.join(" · ");
  // Zero listens is not an error, but an empty table is not an answer either:
  // it says what to do.
  $("statsEmpty").hidden = r.plays > 0;
  $("statsLists").hidden = r.plays === 0;

  renderRanking($("statsArtists"), r.top_artists, (a) => [a.artist, `${countOf(a.unique_tracks, "track")}`, a.plays]);
  renderRanking($("statsTracks"), r.top_tracks, (t) => [t.title, t.artist, t.plays]);
  renderRanking($("statsAlbums"), r.top_albums, (a) => [a.album, a.artist, a.plays]);
}

function renderRanking(list, entries, format) {
  const max = Math.max(1, ...entries.map((entry) => format(entry)[2]));
  const bars = [];
  list.replaceChildren(
    ...entries.map((entry, index) => {
      const [name, sub, plays] = format(entry);
      const bar = h("span", { class: "rank-bar", "aria-hidden": "true" });
      bars.push([bar, plays / max]);
      return h(
        "li",
        {},
        bar,
        h("span", { class: "rank-no", text: String(index + 1) }),
        h("span", { class: "rank-name", title: `${name} — ${sub}` }, name, h("span", { class: "rank-sub", text: ` · ${sub}` })),
        h("span", { class: "num", text: fmt(plays) }),
      );
    }),
  );
  for (const [bar, ratio] of bars) animate(bar, { scaleX: ratio }, { from: { scaleX: 0 } });
}

function renderYears() {
  const years = stats.years;
  $("statsYearsBlock").hidden = years.length < 2;
  const max = Math.max(1, ...years.map((year) => year.plays));
  const chart = $("statsYears");
  const fills = [];
  chart.replaceChildren(
    ...years.map((entry) => {
      const fill = h("span", { class: "year-fill" });
      fills.push([fill, entry.plays / max]);
      return h(
        "button",
        {
          class: "year",
          type: "button",
          dataset: { year: String(entry.year) },
          title: `${entry.year}: ${countOf(entry.plays, "listen")} · ${durationText(entry.ms_played)}`,
          "aria-pressed": "false",
          onclick: () => runStats(stats.year === entry.year ? null : entry.year),
        },
        h("span", { class: "year-track" }, fill),
        h("span", { class: "year-label", text: String(entry.year) }),
      );
    }),
  );
  fills.forEach(([fill, ratio], index) =>
    animate(fill, { scaleY: ratio }, { from: { scaleY: 0 }, responseScale: 1 + index * 0.04 }),
  );
}

function updateYearSelection() {
  for (const bar of $("statsYears").children) {
    bar.setAttribute("aria-pressed", String(Number(bar.dataset.year) === stats.year));
  }
}

$("btnStatsRefresh").addEventListener("click", () => runStats(stats.year));
$("statsTop").addEventListener("change", () => runStats(stats.year));

// ————————————————————————————————————— sleeve card (Phase 0.5)
//
// The **core** draws the card (`sleeve::render_svg`); the only thing done here
// is showing the SVG that arrives. If a second renderer lived in the
// interface, the saved file and the card on screen would drift apart over
// time.

const sleeve = { previewed: false, format: "square" };

const sleeveFormat = segmented($("sleeveForm").querySelector(".segmented"), (segment) => {
  sleeve.format = segment.dataset.format;
  if (sleeve.previewed) previewSleeve();
});

function sleeveArgs() {
  const yearText = $("sleeveYear").value.trim();
  return {
    year: yearText ? Number(yearText) : null,
    story: sleeve.format === "story",
  };
}

async function previewSleeve() {
  const svg = await call("sleeve_svg", sleeveArgs());
  if (svg === undefined) return;
  sleeve.previewed = true;
  const image = h("img", { alt: "sleeve card preview" });
  // A `data:` URI — CSP `img-src 'self' data:` allows it. The SVG is loaded
  // as an image; it does not mix into the document.
  image.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
  $("sleevePreview").replaceChildren(image);
  $("sleevePreview").hidden = false;
  $("sleeveEmpty").hidden = true;
  $("btnSleeveSave").disabled = false;
  animate(image, { scale: 1, opacity: 1 }, { from: { scale: 0.97, opacity: 0 } });
}

$("sleeveForm").addEventListener("submit", (event) => {
  event.preventDefault();
  previewSleeve();
});

$("btnSleeveSave").addEventListener("click", async () => {
  const args = sleeveArgs();
  const path = await pickSavePath({
    title: "save the sleeve card",
    defaultPath: `headshell-sleeve-${args.year ?? "all-time"}${args.story ? "-story" : ""}.png`,
    filters: [
      { name: "PNG", extensions: ["png"] },
      { name: "SVG", extensions: ["svg"] },
    ],
  });
  if (!path) return;
  const response = await call("sleeve", { ...args, out: path });
  if (!response) return;
  const written = response.written;
  toast(
    written
      ? `card written: ${written.path} · ${countOf(written.bytes, "byte")}`
      : "the card was computed but no file was written",
    true,
  );
});

// ————————————————————————————————————— import

// Wire values (D-036); an unrecognised format is shown by its raw name.
const EXPORT_LABELS = {
  spotify_extended: "Spotify extended streaming history",
  spotify_account: "Spotify account data",
  apple_music: "Apple Music",
  google_takeout: "Google Takeout",
};

// Skip reasons arrive as wire values (`not_music`, `missing_title`…). If a
// reason we do not know arrives, we show the raw value — silently swallowing
// a new reason would make the skipped record invisible (K9).
const SKIP_LABELS = {
  not_music: "not music",
  missing_title: "no track title",
  missing_artist: "no artist name",
  bad_timestamp: "timestamp not read",
  bad_duration: "duration not read",
};

// The links of the identity chain, in K6's order.
const CHAIN = [
  ["by_isrc", "ISRC", "m-isrc"],
  ["by_mbid", "MBID", "m-mbid"],
  ["by_fuzzy", "fuzzy", "m-fuzzy"],
  ["by_fingerprint", "fingerprint", "m-fingerprint"],
  ["by_local_key", "local key", "m-local"],
];

$("importForm").addEventListener("submit", (event) => {
  event.preventDefault();
  const path = $("importPath").value.trim();
  if (path) runImport(path);
});

$("btnImportPick").addEventListener("click", async () => {
  const path = await pickFile({
    title: "choose an export archive",
    multiple: false,
    filters: [{ name: "Export archive", extensions: ["zip"] }],
  });
  if (path) runImport(path);
});

async function runImport(path) {
  $("importPath").value = path;
  const report = await call("import", { path });
  if (!report) return;

  // Operations with partial success, like matching, **always** return a
  // summary: how many records came, how many with ISRC, how many fuzzy, how
  // many did not match.
  const i = report.import;
  const d = report.identity;
  const w = report.write;
  $("importSource").textContent =
    `${i.source} · ${EXPORT_LABELS[i.export] ?? i.export} · ${countOf(i.files_matched, "file")}`;
  $("importSummary").replaceChildren(
    figure(fmt(i.records_total), "raw records"),
    figure(fmt(i.listens), "became listens"),
    figure(fmt(w.inserted), "newly written"),
    figure(fmt(w.duplicates), "already there", true),
    figure(fmt(w.new_tracks), "new tracks"),
    figure(fmt(i.with_isrc), "carrying an ISRC", true),
  );
  renderChain(d);

  // The skipped ones with their reasons: "0 records came" and "they came but
  // were filtered out" are separate diagnoses (K9).
  const skipped = Object.entries(i.skipped ?? {});
  const skippedLine = $("importSkipped");
  skippedLine.hidden = skipped.length === 0;
  skippedLine.textContent = `skipped: ${skipped.map(([why, n]) => `${SKIP_LABELS[why] ?? why} ${fmt(n)}`).join(" · ")}`;

  $("importResult").textContent = JSON.stringify({ import: i, identity: d, write: w }, null, 2);
  $("importReport").hidden = false;

  // The history changed: the statistics and the card are recomputed the next
  // time they open.
  stats.loaded = false;
  sleeve.previewed = false;
  toast(`imported · ${countOf(w.inserted, "new listen")}`, true);
}

/// How many records each link resolved: the ratios are the part's share of the
/// whole, not a threshold. Choosing what counts as "authoritative" is the
/// core's job (`ResolveSummary`).
function renderChain(summary) {
  const total = summary.total;
  const bar = h("div", { class: "chain-bar", role: "img", "aria-label": "identity chain breakdown" });
  const legend = h("ul", { class: "chain-legend" });
  for (const [key, label, className] of CHAIN) {
    const count = summary[key] ?? 0;
    if (count > 0) {
      const segment = h("span", { class: `chain-seg ${className}`, title: `${label}: ${fmt(count)}` });
      segment.style.flexGrow = String(count);
      bar.append(segment);
    }
    legend.append(
      h(
        "li",
        {},
        h("span", { class: `swatch-dot ${className}`, "aria-hidden": "true" }),
        `${label} `,
        h("b", { text: fmt(count) }),
      ),
    );
  }
  legend.append(h("li", {}, `total `, h("b", { text: fmt(total) })));
  $("importChain").replaceChildren(bar, legend);
}

// Drag and drop: dropping the archive on the window instead of typing a
// path. The moment the window is entered, the import section opens so the
// place to drop it is visible.
const dropzone = $("dropzone");
listen("tauri://drag-enter", () => {
  openPanel("import");
  dropzone.classList.add("over");
});
listen("tauri://drag-leave", () => dropzone.classList.remove("over"));
listen("tauri://drag-drop", (event) => {
  dropzone.classList.remove("over");
  const path = event.payload?.paths?.[0];
  if (!path) return;
  openPanel("import");
  runImport(path);
});

// ————————————————————————————————————— providers

// The capability flags arrive as the `Capabilities` bit mask (the same order
// as the constants in the core).
const CAPABILITIES = [
  [1 << 0, "search"],
  [1 << 1, "browse"],
  [1 << 2, "play"],
  [1 << 3, "control"],
];

// A catalog entry's capabilities arrive as text.
const CAPABILITY_NAMES = { search: "search", browse: "browse", stream: "play", control: "control" };

let environment = null;

async function refreshProviders() {
  renderMusicDirs();

  const list = await call("providers");
  if (list) {
    $("providerTable")
      .querySelector("tbody")
      .replaceChildren(...list.providers.map(providerRow));
    $("providerTable").hidden = list.providers.length === 0;
    $("providerEmpty").hidden = list.providers.length > 0;
  }

  const servers = await call("servers_list");
  if (!servers) return;
  $("serverTable")
    .querySelector("tbody")
    .replaceChildren(
      ...servers.servers.map((server) =>
        h(
          "tr",
          {},
          h("td", { text: server.id }),
          h("td", { class: "dim", text: server.kind }),
          h("td", { class: "mono", text: server.url }),
          h("td", { text: `${server.username} (${server.auth})` }),
          h(
            "td",
            {},
            armedButton("delete", async () => {
              const report = await call("server_remove", { name: server.id });
              if (report) {
                toast(`${report.id} removed · left: ${fmt(report.remaining)}`, true);
                refreshProviders();
              }
            }),
          ),
        ),
      ),
    );
  $("serverTable").hidden = servers.servers.length === 0;
  $("serverEmpty").hidden = servers.servers.length > 0;
}

function renderMusicDirs() {
  const dirs = environment?.music_dirs ?? [];
  $("musicDirs").replaceChildren(
    ...(dirs.length > 0
      ? dirs.map((dir) => h("li", { text: dir }))
      : [h("li", { class: "empty", text: "not set" })]),
  );
}

function providerRow(info) {
  const status = h("span", { class: "health", text: "—" });
  const caps = CAPABILITIES.filter(([bit]) => (info.capabilities & bit) !== 0).map(([, name]) =>
    h("span", { class: "tag", text: name }),
  );
  return h(
    "tr",
    {},
    h("td", {}, h("div", { text: info.display_name }), h("div", { class: "dim mono", text: info.id })),
    h("td", {}, h("div", { class: "caps" }, caps.length > 0 ? caps : h("span", { class: "dim", text: "none" }))),
    h("td", {}, status),
    h(
      "td",
      {},
      h(
        "button",
        {
          type: "button",
          onclick: async () => {
            const report = await call("provider_test", { name: info.id });
            // "I did not look" and "I could not reach it" are separate: the error is in
            // the notice, the health answer is here.
            if (!report) {
              status.className = "health down";
              status.replaceChildren(h("span", { class: "dot" }), " error (see the notice)");
              return;
            }
            const health = report.health;
            const count = health.track_count;
            status.className = health.reachable ? "health up" : "health down";
            status.replaceChildren(
              h("span", { class: "dot" }),
              health.reachable
                ? ` up${count === null ? "" : ` · ${countOf(count, "track")}`}`
                : ` unreachable`,
              health.reachable
                ? null
                : h("span", { class: "health-detail", text: ` — ${health.detail ?? "no reason given"}` }),
            );
          },
        },
        "test",
      ),
    ),
  );
}

$("btnScan").addEventListener("click", () => scan(false));
$("btnScanStale").addEventListener("click", () => scan(true));

async function scan(onlyStale) {
  const report = await call("provider_scan", { ifStale: onlyStale });
  if (!report) return;
  // "Unchanged" and "I could not look" are different things: the reason the
  // core gives is shown as it is (K9).
  toast(
    report.scanned
      ? `scanned · ${countOf(report.summary.indexed, "track")} (${fmt(report.summary.failed)} unreadable) · added ${fmt(report.write.inserted)}, updated ${fmt(report.write.updated)}, dropped ${fmt(report.write.removed)} — ${report.reason}`
      : `scan skipped — ${report.reason}`,
    true,
  );
  refreshProviders();
}

$("serverForm").addEventListener("submit", async (event) => {
  event.preventDefault();
  const apiKey = $("serverApiKey").value.trim();
  const name = $("serverName").value.trim();
  const report = await call("server_add", {
    kind: $("serverKind").value,
    url: $("serverUrl").value.trim(),
    user: $("serverUser").value.trim(),
    password: $("serverPassword").value || null,
    apiKey: apiKey || null,
    name: name || null,
    verify: $("serverVerify").checked,
  });
  // The password must not stay in the form — after a failed attempt either.
  $("serverPassword").value = "";
  $("serverApiKey").value = "";
  if (!report) return;
  const notes = report.notes.length > 0 ? ` · ${report.notes.join(" · ")}` : "";
  toast(`${report.server.id} added${report.verified ? " (verified)" : " (not verified)"}${notes}`, true);
  refreshProviders();
});

/// The button of an irreversible action: the first click only asks, the
/// second does it. The button itself instead of the browser's `confirm` —
/// webviews do not show that consistently, and the dialog permission is only
/// open to file picking.
function armedButton(label, action) {
  const button = h("button", { type: "button", class: "danger", text: label });
  let armed = null;
  button.addEventListener("click", async () => {
    if (!armed) {
      button.textContent = `really ${label}?`;
      armed = setTimeout(() => {
        armed = null;
        button.textContent = label;
      }, 4000);
      return;
    }
    clearTimeout(armed);
    armed = null;
    button.textContent = label;
    await action();
  });
  return button;
}

// ————————————————————————————————————— plugins (the Phase 2 surface)
//
// Three things are shown **separately**, and none of them suppresses the
// others: a manifest problem, the artifacts the engine must install (D-055)
// and the consent state (D-040). Which button stands out is a presentation
// decision; whether the plugin can run is decided by the core
// (`PluginEntry::is_loadable`).

const CONSENT = {
  approved: ["approved", "chip chip-ok"],
  not_asked: ["awaiting consent", "chip chip-warn"],
  needs_approval: ["asks for new permissions", "chip chip-warn"],
  disabled: ["disabled", "chip"],
};

const pluginTabs = segmented(document.querySelector("#panel-plugins .segmented"), (tab) => {
  for (const id of ["pluginsInstalled", "pluginsCatalog", "pluginsSecrets"]) {
    $(id).hidden = tab.getAttribute("aria-controls") !== id;
  }
  if (tab.id === "tabSecrets") refreshSecrets();
});

async function refreshPlugins() {
  const list = await call("plugins");
  if (list) {
    $("pluginList").replaceChildren(...list.plugins.map(pluginCard));
    $("pluginEmpty").hidden = list.plugins.length > 0;

    const s = list.summary;
    $("pluginSummary").textContent =
      `${countOf(s.discovered, "plugin")} · ${fmt(s.ready)} ready · ${fmt(s.needs_install)} awaiting install · ` +
      `${fmt(s.awaiting_approval)} awaiting consent · ${fmt(s.disabled)} disabled · ` +
      `${fmt(s.incompatible)} incompatible · ${fmt(s.broken)} broken`;
    // No trust is given to a protection that does not exist, and the limit of
    // the one that does is said: two notes sit with the list and one of them is
    // always visible.
    $("pluginEnforce").hidden = list.permissions_enforced;
    $("pluginEnforced").hidden = !list.permissions_enforced;
    updatePluginBadge(s);
  }
  if (pluginTabs.selected().id === "tabSecrets") refreshSecrets();
}

/// In the sidebar, the number of plugins waiting for something from the
/// user: consent or installing. The numbers come from the core's summary.
function updatePluginBadge(summary) {
  const waiting = summary.awaiting_approval + summary.needs_install;
  const badge = $("navPluginBadge");
  badge.hidden = waiting === 0;
  badge.textContent = String(waiting);
  badge.title = waiting === 1 ? "1 plugin is waiting for you" : `${fmt(waiting)} plugins are waiting for you`;
}

function pluginCard(entry) {
  const consent = entry.consent?.state;
  const [consentText, consentClass] = CONSENT[consent] ?? [consent, "chip"];
  const head = h(
    "div",
    { class: "plugin-head" },
    h("span", { class: "name", text: entry.display_name ?? entry.name }),
    h("span", {
      class: "author",
      text: [entry.name, entry.version, entry.api === null || entry.api === undefined ? null : `api ${entry.api}`]
        .filter(Boolean)
        .join(" · "),
    }),
    consent ? h("span", { class: consentClass, text: consentText }) : null,
  );

  const card = h("li", { class: "plugin" }, head);

  // If the manifest could not be read, the reason stays here — so the reason
  // the buttons do not work does not stay out of sight (K9).
  if (entry.problem) card.append(h("p", { class: "plugin-problem", text: entry.problem }));

  if (consent === "needs_approval") {
    card.append(
      h("p", { class: "perms" }, h("b", { text: "newly requested network: " }), (entry.consent.extra?.net ?? []).join(", ") || "—"),
    );
  }
  const net = entry.permissions?.net ?? [];
  card.append(
    h("p", { class: "perms" }, h("b", { text: "network: " }), net.length > 0 ? net.join(", ") : "none requested"),
  );

  // The artifacts the engine will download go on a separate line (D-055): the
  // engine does the downloading, not the plugin, so they do not mix with the
  // plugin's permission list.
  for (const requirement of entry.requires ?? []) {
    card.append(
      h(
        "p",
        { class: "plugin-need" },
        h("b", { text: "engine: " }),
        `${requirement.name} ${requirement.version} (${requirement.platform}) — ${requirementText(requirement.state)}`,
      ),
    );
  }

  const actions = h("div", { class: "actions" });
  // Install only if something is missing that installing will fix: for an
  // artifact whose platform is not supported, "install" would be a pointless
  // button.
  const installable = (entry.requires ?? []).some(
    (requirement) => requirement.state === "Missing" || requirement.state?.Corrupt,
  );
  if (consent === "not_asked" || consent === "needs_approval") {
    actions.append(pluginButton("approve", "plugin_approve", entry.name, true));
  }
  if (installable) actions.append(pluginButton("install the tools", "plugin_install", entry.name, true));
  if (consent === "disabled") actions.append(pluginButton("enable", "plugin_enable", entry.name, true));
  if (consent === "approved") actions.append(pluginButton("disable", "plugin_disable", entry.name));
  actions.append(h("span", { class: "spacer" }));
  if (consent && consent !== "not_asked") {
    actions.append(pluginButton("forget consent", "plugin_forget", entry.name));
  }
  actions.append(
    // Removing cannot be undone (the directory goes along with `state/`).
    armedButton("remove", async () => {
      const report = await call("plugin_remove", { name: entry.name });
      if (!report) return;
      const kept = report.kept_secrets.length ? ` · secrets left: ${report.kept_secrets.join(", ")}` : "";
      toast(
        `${report.name} removed · consent ${report.consent_forgotten ? "forgotten" : "had no record"}${kept}`,
        true,
      );
      refreshPlugins();
    }),
  );
  card.append(actions);
  return card;
}

/// The artifact state arrives externally tagged: `"Missing"` or
/// `{ Installed: { path } }` / `{ Corrupt: { expected, found } }` /
/// `{ Unsupported: { platform, available } }`.
function requirementText(state) {
  if (state === "Missing") return "not installed";
  if (typeof state === "object" && state !== null) {
    if (state.Installed) return `installed · ${state.Installed.path}`;
    if (state.Corrupt) {
      return `hash mismatch (expected ${short(state.Corrupt.expected)}, found ${short(state.Corrupt.found)})`;
    }
    // Installing does not fix this; that is why the "install" button is not
    // shown.
    if (state.Unsupported) {
      return `no release for this platform (${state.Unsupported.platform}) · declared: ${state.Unsupported.available.join(", ")}`;
    }
  }
  // An unknown state is not silently counted as "fine".
  return JSON.stringify(state);
}

function short(hash) {
  return typeof hash === "string" ? hash.slice(0, 12) : String(hash);
}

function pluginButton(label, command, name, primary = false) {
  return h(
    "button",
    {
      type: "button",
      class: primary ? "primary" : null,
      onclick: async () => {
        const report = await call(command, { name });
        if (!report) return;
        // The install report's shape differs from the consent report's: each is
        // summarised with its own fields; no common "result" type is made up.
        toast(report.action ? consentText(report) : installText(name, report), true);
        refreshPlugins();
      },
    },
    label,
  );
}

// The name of the consent command is a wire value (D-036); an unrecognised
// name is shown raw.
const ACTION_LABELS = {
  approve: "approved",
  disable: "disabled",
  enable: "enabled",
  forget: "consent forgotten",
};

function consentText(report) {
  const state = report.status?.state;
  return `${report.name}: ${ACTION_LABELS[report.action] ?? report.action} · ${CONSENT[state]?.[0] ?? state ?? "—"}`;
}

function installText(name, report) {
  const origin = report.fetched ? `downloaded ${report.fetched.version} from the catalog` : "tools";
  const tools = report.ready ? "ready" : "still incomplete";
  const consent = CONSENT[report.consent?.state]?.[0] ?? report.consent?.state ?? "—";
  return `${name}: ${origin} · ${countOf(report.declared, "artifact")}, ${tools} · ${consent}`;
}

$("btnPluginReload").addEventListener("click", refreshPlugins);

// ————————————————————————————————————— catalog (D-071)
//
// The catalog is read **only with the button**: opening the panel does not go
// online (the interface's version of "importing an export never silently
// connects anyone to the network"). The state and the reason come from the
// core; they are only written out here.

const INSTALL = {
  not_installed: [null, null],
  current: ["installed · up to date", "chip chip-ok"],
  update_available: ["update available", "chip chip-info"],
  manual: ["installed by hand", "chip"],
  modified: ["changed locally", "chip chip-warn"],
  unreadable: ["origin record unreadable", "chip chip-err"],
};

async function refreshCatalog() {
  const report = await call("plugin_catalog");
  if (!report) return;
  $("catalogList").replaceChildren(...report.plugins.map((plugin) => catalogCard(plugin, report.platform)));
  const s = report.summary;
  $("catalogSummary").textContent =
    `${countOf(s.listed, "plugin")} · ${fmt(s.installable)} installable · ${fmt(s.installed)} installed · ` +
    `${countOf(s.updates, "update")} · ${fmt(s.problems)} cannot be installed`;
  const delisted = $("catalogDelisted");
  delisted.hidden = report.delisted.length === 0;
  delisted.textContent = report.delisted.length
    ? `Pulled from the catalog but installed on this machine: ${report.delisted.join(", ")}. ` +
      "To remove them, use \"remove\" on the installed tab."
    : "";
}

function catalogCard(plugin, platform) {
  const state = plugin.installed?.state;
  const [stateText, stateClass] = plugin.problem ? ["cannot be installed", "chip chip-err"] : (INSTALL[state] ?? [state, "chip"]);
  const card = h(
    "li",
    { class: "plugin" },
    h(
      "div",
      { class: "plugin-head" },
      h("span", { class: "name", text: plugin.display_name ?? plugin.name }),
      h("span", { class: "author", text: [plugin.name, plugin.version].filter(Boolean).join(" · ") }),
      stateText ? h("span", { class: stateClass, text: stateText }) : null,
    ),
  );

  if (plugin.description) card.append(h("p", { class: "plugin-desc", text: plugin.description }));
  // An entry that cannot be installed is not hidden: the user should see why
  // they cannot install what they were looking for (K9).
  if (plugin.problem) card.append(h("p", { class: "plugin-problem", text: plugin.problem }));
  const detail = installDetail(plugin.installed);
  if (detail) card.append(h("p", { class: "plugin-need", text: detail }));

  if (!plugin.problem) {
    const caps = (plugin.capabilities ?? []).map((cap) => CAPABILITY_NAMES[cap] ?? cap);
    if (caps.length > 0) card.append(h("p", { class: "perms" }, h("b", { text: "can do: " }), caps.join(" · ")));
    const net = plugin.permissions?.net ?? [];
    card.append(h("p", { class: "perms" }, h("b", { text: "network: " }), net.length > 0 ? net.join(", ") : "none requested"));
    for (const requirement of plugin.requires ?? []) {
      const here = requirement.assets?.[platform];
      card.append(
        h(
          "p",
          { class: "plugin-need" },
          h("b", { text: "engine: " }),
          here
            ? `${requirement.name} ${requirement.version} (${platform})`
            : `${requirement.name} ${requirement.version} — no release for this platform (${platform})`,
        ),
      );
    }
  }

  const actions = h("div", { class: "actions" });
  if (!plugin.problem && state === "not_installed") {
    actions.append(catalogButton("install", "plugin_install", { name: plugin.name }));
  }
  if (!plugin.problem && state === "update_available") {
    actions.append(catalogButton("update", "plugin_update", { name: plugin.name }));
  }
  if (actions.childElementCount) card.append(actions);
  return card;
}

function installDetail(installed) {
  switch (installed?.state) {
    case "update_available":
      return `installed ${installed.installed} → ${installed.available} in the catalog`;
    case "modified":
      return `installed ${installed.version}; files changed by hand: ${installed.files.join(", ")} — an update does not overwrite them`;
    case "manual":
      return "installed by hand (no origin record) — the catalog does not touch it";
    case "unreadable":
      return `origin record unreadable: ${installed.detail}`;
    default:
      return null;
  }
}

function catalogButton(label, command, args) {
  return h(
    "button",
    {
      type: "button",
      class: "primary",
      onclick: async () => {
        const report = await call(command, args);
        if (!report) return;
        toast(command === "plugin_update" ? updateText(report) : installText(args.name, report), true);
        refreshCatalog();
        // An installed plugin awaits consent (D-040): switch to where the result
        // lives.
        if (command === "plugin_install") pluginTabs.select($("tabInstalled"));
        refreshPlugins();
      },
    },
    label,
  );
}

function updateText(report) {
  const s = report.summary;
  const lines = report.plugins.map((plugin) => {
    const outcome = plugin.outcome;
    let text;
    switch (outcome.state) {
      case "updated":
        text = `updated ${outcome.from} → ${outcome.to}`;
        // A tool change does not ask for consent (D-071), but it is said.
        for (const change of outcome.tools_changed) {
          text += ` · tool changed: ${change.name} ${change.from ?? "—"} → ${change.to ?? "—"}`;
        }
        if (outcome.permissions_added.net.length) {
          text += ` · asks for new permissions: ${outcome.permissions_added.net.join(", ")}`;
        }
        break;
      case "current":
        text = `up to date (${outcome.version})`;
        break;
      case "skipped":
        text = `skipped — ${outcome.reason}`;
        break;
      default:
        text = `COULD NOT UPDATE — ${outcome.error ?? JSON.stringify(outcome)}`;
    }
    return `${plugin.name}: ${text}`;
  });
  const total =
    `${countOf(s.checked, "plugin")}: ${fmt(s.updated)} updated, ${fmt(s.current)} up to date, ` +
    `${fmt(s.skipped)} skipped, ${fmt(s.failed)} failed`;
  return lines.length ? `${lines.join(" · ")} (${total})` : "no plugins installed from the catalog";
}

$("btnCatalogLoad").addEventListener("click", refreshCatalog);
$("btnCatalogUpdateAll").addEventListener("click", async () => {
  const report = await call("plugin_update", { name: null });
  if (!report) return;
  const text = updateText(report);
  if (report.summary.failed === 0) {
    toast(text, true);
  } else {
    // A failed update stays an error; the stage is read from the failed one's
    // own chain (K9), not made up.
    const failed = report.plugins.find((plugin) => plugin.outcome.state === "failed");
    const stage = failed?.outcome.error?.match(/STEP: (\S+)/)?.[1];
    toast({ stage, chain: text });
  }
  refreshPlugins();
  refreshCatalog();
});

// ————————————————————————————————————— secrets (D-042)

async function refreshSecrets() {
  const list = await call("secrets");
  if (!list) return;
  const rows = [];
  for (const [namespace, keys] of Object.entries(list.namespaces)) {
    for (const key of keys) {
      rows.push(
        h(
          "tr",
          {},
          h("td", { class: "mono", text: namespace }),
          h("td", { class: "mono", text: key }),
          h(
            "td",
            {},
            armedButton("delete", async () => {
              const report = await call("secret_remove", { namespace, key });
              if (!report) return;
              toast(`${report.namespace}/${report.key} ${report.changed ? "removed" : "was not there"}`, true);
              refreshSecrets();
            }),
          ),
        ),
      );
    }
  }
  $("secretTable").querySelector("tbody").replaceChildren(...rows);
  $("secretTable").hidden = rows.length === 0;
  $("secretEmpty").hidden = rows.length > 0;
}

$("secretForm").addEventListener("submit", async (event) => {
  event.preventDefault();
  const report = await call("secret_set", {
    namespace: $("secretNamespace").value.trim(),
    key: $("secretKey").value.trim(),
    value: $("secretValue").value,
  });
  // The value must not stay in the form — after a failed attempt either.
  $("secretValue").value = "";
  if (!report) return;
  toast(`${report.namespace}/${report.key} saved`, true);
  refreshSecrets();
});

// ————————————————————————————————————— theme (§3.3)
//
// Applying is one line: writing the CSS the theme store gives into the
// `<style>` tag. Validation, the `api` version check, the "is it extended"
// decision and the preview colours are on the Rust side (`src/theme.rs`);
// they are only shown here.

function applyTheme(theme) {
  if (!theme) return;
  // `textContent` — not `innerHTML`: the theme CSS goes in as text, and a
  // `</style>` inside it is not interpreted as a tag.
  $("themeCss").textContent = theme.css ?? "";
  // The duration token may have changed: the springs should read it from the
  // new theme.
  invalidateMotion();
  if (theme.problem) toast({ stage: "CONFIG_LOAD", chain: theme.problem });
}

async function refreshThemes() {
  const list = await call("themes_list");
  if (!list) return;

  $("themeDir").textContent = [`theme directory : ${list.dir}`, `contract version: api ${list.api}`].join("\n");

  // The default is an option too: the way back from a theme must be visible.
  $("themeList").replaceChildren(
    themeCard({ id: null, name: "default", author: "headshell", builtin: true, preview: list.default_preview }, list.active),
    ...list.themes.map((theme) => themeCard(theme, list.active)),
  );

  $("themeRejected").replaceChildren(
    ...list.rejected.map((entry) => h("li", {}, h("span", { class: "id", text: entry.id }), ` — ${entry.reason}`)),
  );
  $("themeRejectedEmpty").hidden = list.rejected.length > 0;
}

function themeCard(theme, activeId) {
  const current = (theme.id ?? null) === (activeId ?? null);
  const card = h("li", { class: current ? "theme current" : "theme" });
  card.append(swatch(theme.preview));

  const name = h("span", { class: "name", text: theme.name });
  if (theme.author) name.append(h("span", { class: "author", text: ` · ${theme.author}` }));
  card.append(name);

  const foot = h("div", { class: "theme-foot" });
  if (theme.builtin && theme.id) foot.append(h("span", { class: "tag", text: "built in" }));
  // D-038: we do not reject, we flag. The label tells the user "this theme goes
  // outside the contract, there is no guarantee".
  if (theme.extended) foot.append(h("span", { class: "tag warn", text: "extended · no guarantee" }));
  foot.append(
    current
      ? h("button", { type: "button", disabled: true }, icon("check"), "in use")
      : h(
          "button",
          {
            type: "button",
            onclick: async () => {
              const active = await call("theme_select", { id: theme.id ?? null });
              if (!active) return;
              applyTheme(active);
              refreshThemes();
            },
          },
          "apply",
        ),
  );
  card.append(foot);
  return card;
}

/// A small window drawn with the theme's own tokens. The values are raw CSS
/// text; an invalid value leaves the swatch empty, it does not break the
/// card.
function swatch(preview) {
  const box = h("div", { class: "swatch", "aria-hidden": "true" });
  const parts = [
    ["sw-side", "surface"],
    ["sw-line one", "text"],
    ["sw-line two", "text"],
    ["sw-accent", "accent"],
  ];
  if (preview?.bg) box.style.backgroundColor = preview.bg;
  for (const [className, token] of parts) {
    const part = h("span", { class: className });
    if (preview?.[token]) part.style.backgroundColor = preview[token];
    box.append(part);
  }
  return box;
}

$("btnThemeReload").addEventListener("click", refreshThemes);

// ————————————————————————————————————— diagnostics

async function refreshDiag() {
  // The text is the core's own `render()` — the same thing `headshell diag`
  // prints (`diag_text`). The JSON form stays folded.
  const text = await call("diag_text");
  const block = $("diagResult");
  block.hidden = text === undefined;
  $("btnDiagCopy").hidden = !text;
  block.textContent = text ?? "no command has been run yet";
  const report = await call("diag");
  $("diagRawFold").hidden = !report;
  $("diagRaw").textContent = report ? JSON.stringify(report, null, 2) : "";
}

$("btnDiag").addEventListener("click", refreshDiag);

// The diagnostics report exists to be copied; making the user select it by
// hand made it unusable.
$("btnDiagCopy").addEventListener("click", () => {
  const text = $("diagResult").textContent;
  if (text) copyText(text, "diagnostics report copied to the clipboard");
});

// The links of the identity chain — wire values (D-036).
const METHOD_LABELS = {
  isrc: "ISRC",
  mbid: "MBID (metadata lookup)",
  fuzzy: "fuzzy match",
  fingerprint: "audio fingerprint",
  local_key: "local key — no authority",
};

$("resolveForm").addEventListener("submit", async (event) => {
  event.preventDefault();
  const query = $("resolveQuery").value.trim();
  if (!query) return;
  const report = await call("resolve", { query });
  if (!report) return;
  const res = report.resolution;
  const facts = [
    ["query", `${report.artist} - ${report.title}`],
    ["identity", res.canonical_id],
    ["method", `${METHOD_LABELS[res.method] ?? res.method} · confidence ${percent.format(res.confidence)}`],
    [
      "match",
      res.matched
        ? `${res.matched.artist} - ${res.matched.title} [${res.matched.mbid}]`
        : "none (the metadata source returned no candidates)",
    ],
  ];
  if (res.matched?.disambiguation) facts.push(["not", res.matched.disambiguation]);
  // A tie must not stay silent: if the choice was made among equals, the user
  // must see it, otherwise they take an arbitrary choice for a definite answer.
  if (res.tied_candidates > 1) {
    facts.push(["tied", `${countOf(res.tied_candidates, "candidate")} got the same score; the choice is deterministic but arbitrary`]);
  }
  $("resolveFacts").replaceChildren(
    ...facts.flatMap(([label, value]) => [h("dt", { text: label }), h("dd", { text: value })]),
  );
  $("resolveResult").textContent = JSON.stringify(res, null, 2);
  $("resolveCard").hidden = false;
});

// ————————————————————————————————————— events
//
// Events only arrive **when something changes** (D-033). In the silence in
// between, the position is estimated from the anchor; that is why there is no
// "still playing" message.

listen("headshell://tick", async (event) => {
  const report = event.payload;
  renderAnchor(report.anchor);
  if (report.track_changed || report.finished) {
    renderQueue(await call("queue"));
  } else if (report.artwork_ready?.length > 0) {
    // A cover's answer arrived: the rows and the label are painted again.
    refreshCovers();
  }
  if (report.lyrics_ready) {
    // The playing track's lyrics changed: a new track, an answer, a shift.
    refreshLyrics();
  }
  if (report.listens_recorded > 0) {
    // The history grew: the statistics and the card are recomputed the next time
    // they open. The numbers on screen must not go stale and ignore the new
    // listen.
    stats.loaded = false;
    sleeve.previewed = false;
  }
  if (report.store_error) {
    // The records were not thrown away; they were held back and will be retried —
    // but the user should know (K9).
    toast({
      stage: "LIBRARY_WRITE",
      chain: `STEP: LIBRARY_WRITE\n  → ${report.store_error}\n  → ${countOf(report.listens_pending, "listen")} held back, to be retried on the next round`,
    });
  }
});

listen("headshell://error", (event) => toast(event.payload));

listen("headshell://busy", (event) => {
  const label = event.payload;
  const field = $("busy");
  field.textContent = label ?? "";
  field.hidden = !label;
});

// ————————————————————————————————————— startup

(async function boot() {
  // The theme **first**: drawing a frame with the default colours and then
  // jumping to the theme would flash on every start.
  applyTheme(await call("theme_active"));
  watchMotionPreference();
  showBase("library", { instant: true });
  openPanel("now", { instant: true });

  environment = (await call("environment")) ?? null;
  if (environment) {
    $("appVersion").textContent = `headshell ${environment.version}`;
    $("envBlock").textContent = [
      `version     : ${environment.version}`,
      `data dir    : ${environment.data_dir}`,
      `database    : ${environment.database}`,
      `music       : ${environment.music_dirs.length > 0 ? environment.music_dirs.join(", ") : "not set (HEADSHELL_MUSIC_DIRS)"}`,
      `online      : ${environment.lookup === "online" ? "yes (HEADSHELL_ONLINE=1)" : "no — HEADSHELL_ONLINE=1 turns it on"}`,
    ].join("\n");
  }
  renderQueue(await call("queue"));
  renderAnchor(await call("anchor"));
  refreshLyrics();

  // The plugin badge in the sidebar: read from disk, nothing goes online.
  const plugins = await call("plugins");
  if (plugins) updatePluginBadge(plugins.summary);
})();
