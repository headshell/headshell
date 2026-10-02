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
import { contextMenus } from "./menu.js";
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

/// What each element offers on a right click (D-079): a function that
/// builds the menu when it is asked for, so it reads the state of that
/// moment. Written where the element is drawn — `h(…, { menu })` — or, for
/// the page's fixed elements, under "context menus" below.
const menus = new WeakMap();

/// A small element builder. `text` always goes to `textContent`.
function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") el.className = value;
    else if (key === "text") el.textContent = value;
    else if (key === "dataset") Object.assign(el.dataset, value);
    else if (key === "menu") menus.set(el, value);
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
// An error shows up with its stage (K9): "where did it break" is answered at
// first glance — in plain words (D-083), the stage's code and the full chain
// folded under "details", copied with a single click. `diag`'s last run may
// have been replaced by another command in the meantime, but this block does
// not change. In the debug mode the code stands beside the words.

/// The core's stages in plain words, by their wire names (`diag::Stage`;
/// `ui_contract.rs` holds every one of them here).
const STAGE_TEXT = {
  CONFIG_LOAD: "A setting couldn't be read",
  IMPORT_READ: "That file couldn't be opened",
  IMPORT_DETECT: "That doesn't look like a Spotify export",
  IMPORT_PARSE: "That export couldn't be read",
  IDENTITY_RESOLVE: "That song couldn't be matched",
  LIBRARY_OPEN: "Your library couldn't be opened",
  LIBRARY_WRITE: "Your library couldn't be saved",
  LIBRARY_QUERY: "Your library couldn't be searched",
  STATS_COMPUTE: "Your stats couldn't be counted",
  PROVIDER_CALL: "The source didn't answer",
  PLUGIN_LOAD: "A service couldn't be loaded",
  PLUGIN_RUNTIME: "A service stopped working",
  PLUGIN_START: "A service couldn't start",
  PLUGIN_CATALOG: "The list of services couldn't be loaded",
  NETWORK_REQUEST: "Couldn't reach the server",
  SLEEVE_RENDER: "Your card couldn't be drawn",
  PLAYBACK_RESOLVE: "That song couldn't be found",
  PLAYBACK_DECODE: "That song couldn't be played",
  PLAYBACK_OUTPUT: "No sound came out",
  ARTWORK_READ: "The cover couldn't be read",
  ARTWORK_LOOKUP: "The cover couldn't be found",
  ARTWORK_STORE: "The cover couldn't be saved",
  LYRICS_READ: "The lyrics couldn't be read",
  LYRICS_LOOKUP: "The lyrics couldn't be found",
  LYRICS_STORE: "The lyrics couldn't be saved",
};

/// What the listener can do about it (D-084), where there is something to
/// do; the core's own cause is the debug mode's, and the details'.
const STAGE_HINT = {
  CONFIG_LOAD: "One of headshell's files may be damaged — the details say which.",
  IMPORT_READ: "Check that the file is still there, then try again.",
  IMPORT_DETECT: "Make sure it's the zip Spotify sent you.",
  IMPORT_PARSE: "Make sure it's the zip Spotify sent you, just as it came.",
  LIBRARY_OPEN: "Try again — if it keeps happening, the details will help.",
  LIBRARY_WRITE: "Try again — if it keeps happening, the details will help.",
  LIBRARY_QUERY: "Try again — if it keeps happening, the details will help.",
  PROVIDER_CALL: "It may be busy — give it a moment and try again.",
  PLUGIN_LOAD: "Try updating it under services.",
  PLUGIN_RUNTIME: "Try again, or turn it off and on again under services.",
  PLUGIN_START: "Try turning it off and on again under services.",
  PLUGIN_CATALOG: "Check your internet connection and try again.",
  NETWORK_REQUEST: "Check your internet connection and try again.",
  PLAYBACK_RESOLVE: "Try another search, or check for new music under sources.",
  PLAYBACK_DECODE: "The file may be damaged — try another song.",
  PLAYBACK_OUTPUT: "Check that your speakers or headphones are connected.",
};

function stageText(stage) {
  return STAGE_TEXT[stage] ?? "Something went wrong";
}
//
// A notice comes from below (next to the player) and leaves to the right:
// the close button, running out of time and dragging to the right all exit
// the same way.

const TOAST_LIFETIME = { info: 6000, error: 20000 };
const toastTimers = new Map();
// Notices whose menu is open: they stay until it closes, or the menu would
// go with them.
const toastsHeld = new Set();

function toast(err, info = false) {
  const stage = err?.stage ?? "UNKNOWN";
  const chain = err?.chain ?? String(err);
  // An error is copied whole: its stage and its full chain.
  const whole = chain.startsWith("STEP:") ? chain : `STEP: ${stage}\n  ${chain}`;
  const copyOut = info
    ? () => copyText(String(err), "the notice copied to the clipboard")
    : () => copyText(whole, "the details copied to the clipboard");
  const box = h("div", {
    class: info ? "toast info" : "toast",
    role: info ? "status" : "alert",
    menu: () => {
      clearTimeout(toastTimers.get(box));
      toastsHeld.add(box);
      return {
        mark: false,
        onClose: () => {
          toastsHeld.delete(box);
          // A notice already gone keeps no timer: a stale one would make `Esc`
          // think a notice is still up.
          if (box.isConnected) armToast(box, 4000);
        },
        items: [
          { label: info ? "copy the text" : "copy the details", run: copyOut },
          "-",
          { label: "close", run: () => dismissToast(box) },
          $("toasts").childElementCount > 1 && { label: "close every notice", key: "Esc", run: dismissAllToasts },
        ],
      };
    },
  });

  if (info) {
    box.append(h("div", { text: err }));
  } else {
    box.append(
      h(
        "div",
        { class: "toast-head" },
        h("span", { class: "stage", text: stageText(stage) }),
        h("code", { class: "toast-code debug-only", text: stage }),
      ),
      // For the listener what to do about it; the core's own cause is the
      // debug mode's — and the details' for everyone (D-084).
      ...(STAGE_HINT[stage] ? [h("div", { class: "toast-cause plain-only", text: STAGE_HINT[stage] })] : []),
      h("div", { class: "toast-cause debug-only", text: firstLine(err?.chain) }),
      h(
        "details",
        {},
        h("summary", { text: "details" }),
        h("pre", { text: chain }),
        h("div", { class: "toast-actions" }, h("button", { type: "button", onclick: copyOut }, icon("copy"), "copy")),
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
  box.addEventListener("pointerleave", () => {
    if (!toastsHeld.has(box)) armToast(box, Math.min(lifetime, 4000));
  });

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
    clipboardRefused("write to", err);
  }
}

/// If the clipboard refuses we do not stay silent: the user would think it
/// was copied (or pasted) and send an empty bug report.
function clipboardRefused(what, err) {
  toast({ stage: "CONFIG_LOAD", chain: `STEP: CONFIG_LOAD\n  → could not ${what} the clipboard: ${err}` });
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

const PANELS = ["now", "library", "stats", "sleeve", "import", "providers", "plugins", "theme", "diag", "settings"];

/// A section's shortcut: `Ctrl`+its place in the sidebar, the tenth on `0`
/// — the key after `9` on the number row.
function panelShortcut(panel) {
  const slot = PANELS.indexOf(panel) + 1;
  return `Ctrl+${slot % 10}`;
}

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
  settings: () => {
    onlineSwitch.placeThumb(true);
    debugSwitch.placeThumb(true);
    refreshOnline();
  },
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
  // Diagnostics is the developer's: there only in the debug mode (D-083).
  if (name === "diag" && !debugMode) return;
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
  onlineSwitch.placeThumb(true);
  debugSwitch.placeThumb(true);
  placePlayFromPill(true);
  // The field changed width; the panel under it follows.
  if (suggestUp) openSuggest();
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

// ————————————————————————————————————— switch

/// An on/off switch (`role="switch"`): the knob slides on a spring, so a
/// second press while it travels turns it back from where it is. Pressed, the
/// knob stretches toward where a release sends it (style.css). It answers at
/// once and calls `onChange(on)`; the caller settles it with `set` when the
/// core has answered.
function toggleSwitch(button, onChange) {
  const thumb = button.querySelector(".switch-thumb");
  const on = () => button.getAttribute("aria-checked") === "true";

  function placeThumb(instant = false) {
    if (!button.offsetWidth) return; // no size in a hidden section; it settles when opened
    const travel = button.clientWidth - thumb.offsetWidth - 2 * thumb.offsetLeft;
    const x = on() ? travel : 0;
    if (instant) place(thumb, { x });
    else animate(thumb, { x });
  }

  function set(value, { instant = false } = {}) {
    if (on() !== value) {
      button.setAttribute("aria-checked", String(value));
      placeThumb(instant);
    } else if (instant) {
      placeThumb(true);
    }
  }

  function toggle() {
    if (button.disabled) return;
    set(!on());
    onChange(on());
  }

  button.addEventListener("click", toggle);
  return { set, toggle, on, placeThumb, held: () => button.disabled };
}

// ————————————————————————————————————— online lookups (D-082)
//
// Whether the app asks MusicBrainz and the Cover Art Archive: a choice kept
// by the core, which `HEADSHELL_ONLINE` overrides for one launch. The switch
// shows what the app goes by **now**, and the note says who decided it.

let online = null;

const onlineSwitch = toggleSwitch($("onlineSwitch"), async (on) => {
  const report = await call("online_set", { mode: on ? "online" : "offline" });
  // A failed call has said why in a notice; the switch goes back to what
  // holds.
  paintOnline(report?.online ?? online);
});

async function refreshOnline() {
  const report = await call("online");
  if (report) paintOnline(report.online);
}

function paintOnline(choice) {
  if (!choice) return;
  online = choice;
  const on = choice.mode === "online";
  const held = choice.source === "variable";
  $("onlineSwitch").disabled = held;
  onlineSwitch.set(on, { instant: !$("onlineSwitch").offsetWidth });
  // For the listener what changes for them; who set it and the other
  // services' own rules are the debug mode's (D-084).
  const plain = [];
  const full = [];
  if (held) {
    const saved = choice.saved === "online" ? "on" : "off";
    plain.push(`It's ${on ? "on" : "off"} for now — this choice takes over the next time you open headshell.`);
    full.push(
      `HEADSHELL_ONLINE=${on ? 1 : 0} holds this launch ${on ? "on" : "off"}; ` +
        `the saved choice — ${saved} — applies when the app starts without it.`,
    );
  } else if (on) {
    plain.push("Importing a big history takes longer while this is on.");
    full.push("Importing asks MusicBrainz about every track, one a second: a large export takes hours.");
  }
  full.push("Lyrics and plugins go online by their own rules, not by this one.");
  const onlineNote = $("onlineNote");
  onlineNote.replaceChildren(
    h("span", { class: "plain-only", text: plain.join(" ") }),
    h("span", { class: "debug-only", text: full.join(" ") }),
  );
  // Nothing to say to the listener: the note takes no room outside the debug
  // mode.
  onlineNote.classList.toggle("debug-only", plain.length === 0);
  if (environment) {
    environment.lookup = choice.mode;
    environment.lookup_held = held;
    paintEnvironment();
  }
}

/// The environment block in diagnostics: constants asked once, and the
/// online mode as it stands.
function paintEnvironment() {
  const lookup =
    environment.lookup === "online" ? "yes — settings: online lookups" : "no — settings: online lookups";
  $("envBlock").textContent = [
    `version     : ${environment.version}`,
    `data dir    : ${environment.data_dir}`,
    `database    : ${environment.database}`,
    `music       : ${environment.music_dirs.length > 0 ? environment.music_dirs.join(", ") : "not set (HEADSHELL_MUSIC_DIRS)"}`,
    `online      : ${lookup}`,
  ].join("\n");
}

// ————————————————————————————————————— debug mode (D-083)
//
// The developer's details — diagnostics, raw reports, internal identifiers,
// the counts that are zero — are drawn always and shown only while the debug
// mode is on: one attribute on the root (`data-debug`) and the `.debug-only`
// class (its opposite, `.plain-only`) do it, so turning it on or off redraws
// nothing. Menus and tooltips, made when they are needed, ask `debugMode`.
// The choice is the interface's own, beside the theme; `DEBUG=1` holds a
// launch on.

let debugMode = false;

const debugSwitch = toggleSwitch($("debugSwitch"), async (on) => {
  const mode = await call("debug_set", { on });
  // A failed call has said why in a notice; the switch goes back to what
  // holds.
  paintDebug(mode ?? { on: debugMode, held: $("debugSwitch").disabled, saved: debugMode });
});

function paintDebug(mode) {
  if (!mode) return;
  debugMode = mode.on;
  document.documentElement.toggleAttribute("data-debug", mode.on);
  $("debugSwitch").disabled = mode.held;
  debugSwitch.set(mode.on, { instant: !$("debugSwitch").offsetWidth });
  const note = $("debugNote");
  note.hidden = !mode.held;
  note.textContent = mode.held
    ? `DEBUG=1 keeps it on for now; your choice here — ${mode.saved ? "on" : "off"} — takes over without it.`
    : "";
  // The diagnostics row came or went: the sidebar's pill follows its row, and
  // a section that is no longer there is left.
  if (!mode.on && basePanel === "diag") showBase("settings", { instant: true });
  markNav(true);
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
  buffering: "loading",
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

/// The local provider's id: its tracks need no label of their source.
const LOCAL_PROVIDER = "local";

/// Where a track plays from (D-083): a label only for a source other than
/// the local files, by its name; the id is the debug mode's.
function sourceChip(provider) {
  if (provider === LOCAL_PROVIDER) return h("span", { class: "chip debug-only", text: provider, title: "provider" });
  return h("span", { class: "chip", text: providerName(provider), title: provider });
}

function queueRow(item, index, current) {
  const track = item.track;
  return h(
    "li",
    {
      class: current ? "current" : null,
      "aria-current": current ? "true" : null,
      menu: () => songMenu(track, { item, queueIndex: index, current }),
    },
    h(
      "button",
      {
        class: "q-row",
        type: "button",
        title: current ? "now playing" : "skip to this track",
        onclick: () => jumpTo(index),
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
      sourceChip(item.id.provider),
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
  const source = $("nowCardSource");
  const local = current.id.provider === LOCAL_PROVIDER;
  source.textContent = local ? current.id.provider : providerName(current.id.provider);
  source.title = current.id.provider;
  source.classList.toggle("debug-only", local);
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

/// The decision to jump in the queue is in the core: only the index is
/// passed on here.
async function jumpTo(index) {
  renderQueue(await call("jump_to", { index }));
  renderAnchor(await call("anchor"));
}

async function stopPlayback() {
  renderAnchor(await call("stop"));
  renderQueue(await call("queue"));
}

async function setShuffle(on) {
  renderQueue(await call("set_shuffle", { on }));
}

async function setRepeat(mode) {
  renderQueue(await call("set_repeat", { mode }));
}

$("btnPlayPause").addEventListener("click", togglePause);
$("btnNext").addEventListener("click", playNext);
$("btnPrev").addEventListener("click", playPrevious);
$("btnStop").addEventListener("click", stopPlayback);
$("btnShuffle").addEventListener("click", () => setShuffle(!queue.shuffle));
$("btnRepeat").addEventListener("click", () => {
  setRepeat({ off: "all", all: "one", one: "off" }[queue.repeat] ?? "off");
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
  embedded: "from the song file",
  folder: "from a picture in the song's folder",
  provider: "from where the song plays",
  cover_art_archive: "found online",
};

/// A row's cover as the user reads it. "Not looked up" and "not found" are
/// different answers (K9); an unknown status is shown raw, not guessed.
function coverText(item) {
  switch (item.status) {
    case "found":
      return `cover ${COVER_SOURCES[item.source] ?? item.source}`;
    case "not_found":
      return debugMode ? `no cover found — ${item.detail}` : "no cover found";
    case "not_checked_offline":
      return "no cover yet — “look things up online” in settings can find it";
    case "failed":
      return debugMode ? `the cover failed — ${firstLine(item.chain)}` : "the cover could not be fetched";
    case "pending":
      return "finding the cover…";
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
      `Missing covers? Turn on “look things up online” in settings and we'll find them.`,
    );
  }
  if (s?.failed > 0) {
    parts.push(`${countOf(s.failed, "cover")} couldn't be loaded.`);
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
  sidecar: "from a lyrics file next to the song",
  embedded: "from the song file",
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

/// `L`: the lyrics; again, back to up next — and from the whole sheet, back
/// beside the record.
function toggleLyrics() {
  if (!nowOpen) {
    nowTabs.select($("tabLyrics"));
    setNowOpen(true);
  } else if (lyricsFull) {
    setLyricsFull(false);
  } else {
    nowTabs.select(lyricsTabSelected() ? $("tabQueue") : $("tabLyrics"));
  }
}

/// What `L` does now, as a menu row says it.
function lyricsToggleLabel() {
  if (!nowOpen) return "lyrics";
  if (lyricsFull) return "lyrics beside the record";
  return lyricsTabSelected() ? "up next" : "lyrics";
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
    text = "Play something and its lyrics show up here.";
  } else {
    switch (item.status) {
      case "pending":
        text = "Finding the lyrics…";
        break;
      case "not_requested":
        text = "We'll find the lyrics while you're looking at them.";
        break;
      case "not_found":
        text = debugMode ? `No lyrics for this recording — ${item.detail}.` : "No lyrics for this one, sorry.";
        break;
      case "instrumental":
        text = "It's an instrumental — no words to sing along to.";
        break;
      case "failed":
        text = debugMode
          ? `The lyrics could not be looked up — ${firstLine(item.chain)}`
          : "Couldn't get the lyrics just now.";
        retry = true;
        break;
      case "found":
        if (item.synced && item.fit === "closest") {
          warn = true;
          text = debugMode
            ? "The timing may be off: LRCLIB has no version of this recording's length, and its nearest " +
              `is ${lengthDifference(item.delta_ms)}. Shift the lines until they meet the singing.`
            : "These lyrics might run a little early or late — nudge them with sooner or later until they match.";
        } else if (!item.synced) {
          text = "Just the words this time — they won't scroll along with the song.";
        } else if (item.fit === "unmeasured") {
          text = debugMode
            ? "The recording's length is not known, so the timing could not be checked."
            : "These lyrics might not line up perfectly with the song.";
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

async function startPlay(query, all, shuffle = false) {
  closeSuggest();
  const view = await call("play", { query, all, shuffle });
  if (!view) return;
  renderQueue(view);
  renderAnchor(await call("anchor"));
  openPanel("now");
}

/// Exactly these tracks, with no search in between (D-080): a picked
/// suggestion, a song's "play now".
async function playItems(items) {
  closeSuggest();
  const view = await call("play_items", { items });
  if (!view) return;
  renderQueue(view);
  renderAnchor(await call("anchor"));
  openPanel("now");
}

/// Adds at the end of the queue (D-080); into an empty queue it plays. The
/// sheet does not open: adding is not leaving what the user is doing.
async function addItems(items, what) {
  queueAdded(await call("queue_add", { items }), what);
}

/// A history row has a name, not a provider's track: the play box's search
/// finds it, in the core (D-080).
async function addSearch(query, all, what) {
  queueAdded(await call("queue_add_search", { query, all }), what);
}

async function queueAdded(view, what) {
  if (!view) return;
  renderQueue(view);
  renderAnchor(await call("anchor"));
  toast(`Added ${what} to your queue.`, true);
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

// ————————————————————————————————————— suggestions (D-080)
//
// What the play box offers while it is typed in. The core ranks the matches
// and decides who is asked (`suggest`): the provider chosen to play from,
// the others only when it has nothing. This asks at the right moment, draws
// the answer and takes the pick.
//
// - A scanned catalog answers at once: it is asked 90 ms after a key, so a
//   fast typist's keys do not each cost a question — and when the catalog
//   has nothing and the others go over the network, they are not asked at
//   every key either. A server or a plugin chosen to play from is asked
//   300 ms after the last key.
// - Only the newest question's answer is drawn; an older one arriving late
//   is dropped.
// - The panel grows out of the field and goes back into it. The field keeps
//   the focus all along: the arrows light a row, Enter plays it, Esc closes.
//   A press on the panel does not take the focus away, so a row's menu can
//   add one song after another with the panel still up.

const SUGGEST_AT_ONCE_MS = 90;
const SUGGEST_PAUSE_MS = 300;
const SUGGEST_SLOW_MS = 250;

const playInput = $("playQuery");
const suggestPanel = $("suggest");
let suggestTimer = 0;
let suggestAsked = 0;
let suggestRows = [];
let suggestLit = -1;
let suggestUp = false;

function scheduleSuggest() {
  clearTimeout(suggestTimer);
  const query = playInput.value;
  if (!query.trim()) {
    // An answer still on its way is for a text that is gone.
    suggestAsked += 1;
    closeSuggest();
    return;
  }
  const wait = playFrom?.instant === false ? SUGGEST_PAUSE_MS : SUGGEST_AT_ONCE_MS;
  suggestTimer = setTimeout(() => askSuggest(query), wait);
}

async function askSuggest(query) {
  const asked = ++suggestAsked;
  // "Searching" is said only when the answer takes long enough to notice.
  const slow = setTimeout(() => {
    if (asked === suggestAsked) showSearching();
  }, SUGGEST_SLOW_MS);
  const report = await call("suggest", { query });
  clearTimeout(slow);
  if (asked !== suggestAsked || playInput.value !== query || document.activeElement !== playInput) return;
  if (report) drawSuggest(report, asked);
}

function showSearching() {
  if (suggestRows.length > 0) {
    // What is up stays readable, dimmed: it answers the text before.
    suggestPanel.classList.add("stale");
    return;
  }
  suggestPanel.replaceChildren(
    h("p", { class: "suggest-note searching", text: `Looking in ${providerName(playFrom?.provider)}…` }),
  );
  openSuggest();
}

function drawSuggest(report, asked) {
  suggestRows = [];
  suggestLit = -1;
  const nodes = [];
  if (report.sections[0]?.fallback) {
    nodes.push(h("p", { class: "suggest-note", text: `Nothing in ${providerName(report.play_from.provider)}` }));
  }
  for (const section of report.sections) {
    if (section.fallback) {
      nodes.push(h("p", { class: "suggest-heading", text: `From ${section.display_name}:` }));
    }
    for (const item of section.tracks) nodes.push(suggestTrack(item));
    for (const artist of section.artists) nodes.push(suggestArtist(artist.artist));
    for (const item of section.more) nodes.push(suggestTrack(item));
  }
  if (report.sections.length === 0) {
    nodes.push(h("p", { class: "suggest-note", text: `Couldn't find “${report.query.trim()}” anywhere` }));
  }
  // "Not found" and "could not ask" are different answers (K9).
  for (const failure of report.failures) {
    nodes.push(
      h("p", {
        class: "suggest-failure",
        text: debugMode
          ? `could not ask ${failure.display_name} — STEP: ${failure.stage} — ${firstLine(failure.chain)}`
          : `could not ask ${failure.display_name} — ${stageText(failure.stage).toLowerCase()}`,
        title: failure.chain,
      }),
    );
  }
  suggestPanel.replaceChildren(...nodes);
  suggestPanel.classList.remove("stale");
  playInput.removeAttribute("aria-activedescendant");
  openSuggest();
  coverSuggestions(asked);
}

// The suggestions' covers (D-080): the cache's at once, the rest as the
// cover worker finds them where the tracks live — its file, its provider —
// announced by the tick, as the queue's are. Only the rows on screen keep
// theirs. A cover fades in the first time it appears; a list drawn again
// shows the ones already seen at once.
const suggestThumbs = new Map();

async function coverSuggestions(asked) {
  const tracks = suggestRows.filter((row) => row.item);
  if (tracks.length === 0) return;
  const report = await call("artwork_shown", { items: tracks.map((row) => row.item) });
  if (!report || asked !== suggestAsked) return;
  report.items.forEach((entry, index) => {
    tracks[index].key = entry.key;
  });
  const onScreen = new Set(tracks.map((row) => row.key));
  for (const key of suggestThumbs.keys()) if (!onScreen.has(key)) suggestThumbs.delete(key);
  await paintSuggestCovers(report.items.filter((entry) => entry.status === "found").map((entry) => entry.key));
}

async function paintSuggestCovers(keys) {
  const wanted = [...new Set(keys)].filter(
    (key) => !suggestThumbs.has(key) && suggestRows.some((row) => row.key === key),
  );
  if (wanted.length > 0) {
    for (const picture of (await call("artwork_images", { keys: wanted, variant: "thumb" })) ?? []) {
      suggestThumbs.set(picture.key, picture.uri);
    }
  }
  for (const row of suggestRows) {
    const uri = row.key ? suggestThumbs.get(row.key) : undefined;
    const slot = row.el.querySelector(".suggest-art");
    if (!uri || slot.querySelector("img")) continue;
    const img = h("img", { alt: "", src: uri });
    slot.append(img);
    if (!coversSeen.has(row.key)) {
      animate(img, { opacity: 1 }, { from: { opacity: 0 } });
      coversSeen.add(row.key);
    }
  }
}

function suggestTrack(item) {
  const track = item.track;
  const row = h(
    "div",
    {
      class: "suggest-row",
      role: "option",
      id: `suggest-row-${suggestRows.length}`,
      "aria-selected": "false",
      menu: () => songMenu(track, { item }),
    },
    h("span", { class: "suggest-art", "aria-hidden": "true" }),
    h(
      "span",
      { class: "suggest-main" },
      h("span", { class: "suggest-title", text: track.title }),
      h("span", { class: "suggest-sub", text: [track.artist, track.album].filter(Boolean).join(" · ") }),
    ),
    h("span", { class: "suggest-time", text: track.duration_ms ? clock(track.duration_ms) : "" }),
  );
  suggestRows.push({ el: row, item, key: null, pick: () => playItems([item]) });
  return row;
}

function suggestArtist(artist) {
  const row = h(
    "div",
    {
      class: "suggest-row artist",
      role: "option",
      id: `suggest-row-${suggestRows.length}`,
      "aria-selected": "false",
      menu: () => artistMenu(artist),
    },
    // The name's first letter stands for the artist: there is no picture.
    h("span", { class: "suggest-art", "aria-hidden": "true", text: [...artist.trim()][0] ?? "" }),
    h(
      "span",
      { class: "suggest-main" },
      h("span", { class: "suggest-title", text: artist }),
      h("span", { class: "suggest-sub", text: "artist · play all their songs" }),
    ),
  );
  suggestRows.push({ el: row, pick: () => startPlay(artist, true) });
  return row;
}

function lightSuggest(index) {
  const before = suggestRows[suggestLit];
  before?.el.classList.remove("lit");
  before?.el.setAttribute("aria-selected", "false");
  suggestLit = index;
  const row = suggestRows[index];
  if (!row) {
    playInput.removeAttribute("aria-activedescendant");
    return;
  }
  row.el.classList.add("lit");
  row.el.setAttribute("aria-selected", "true");
  playInput.setAttribute("aria-activedescendant", row.el.id);
  row.el.scrollIntoView({ block: "nearest" });
}

/// It grows out of the field; caught closing, it turns around from where it
/// is seen.
function openSuggest() {
  suggestPanel.style.width = `${playInput.closest(".field").offsetWidth}px`;
  playInput.setAttribute("aria-expanded", "true");
  if (suggestUp) return;
  suggestUp = true;
  const fresh = suggestPanel.hidden;
  suggestPanel.hidden = false;
  animate(
    suggestPanel,
    { opacity: 1, scale: 1 },
    fresh ? { from: { opacity: 0, scale: 0.98 }, responseScale: 0.5 } : { responseScale: 0.5 },
  );
}

function closeSuggest() {
  clearTimeout(suggestTimer);
  playInput.setAttribute("aria-expanded", "false");
  playInput.removeAttribute("aria-activedescendant");
  if (!suggestUp) return;
  suggestUp = false;
  animate(
    suggestPanel,
    { opacity: 0, scale: 0.98 },
    {
      responseScale: 0.4,
      onRest: () => {
        suggestPanel.hidden = true;
        suggestPanel.replaceChildren();
        suggestRows = [];
        suggestLit = -1;
      },
    },
  );
}

function suggestRowAt(target) {
  const el = target.closest?.(".suggest-row");
  return suggestRows.findIndex((row) => row.el === el);
}

playInput.addEventListener("input", scheduleSuggest);
playInput.addEventListener("focus", () => {
  if (playInput.value.trim()) scheduleSuggest();
});
playInput.addEventListener("blur", () => {
  suggestAsked += 1;
  closeSuggest();
});
playInput.addEventListener("keydown", (event) => {
  if (!suggestUp) {
    // The arrow brings the panel back for what is typed.
    if (event.key === "ArrowDown" && playInput.value.trim()) {
      event.preventDefault();
      askSuggest(playInput.value);
    }
    return;
  }
  if (event.key === "ArrowDown") {
    event.preventDefault();
    lightSuggest(Math.min(suggestLit + 1, suggestRows.length - 1));
  } else if (event.key === "ArrowUp") {
    // Above the first row is the field again.
    event.preventDefault();
    lightSuggest(Math.max(suggestLit - 1, -1));
  } else if (event.key === "Enter" && !event.shiftKey && suggestRows[suggestLit]) {
    event.preventDefault();
    suggestRows[suggestLit].pick();
  } else if (event.key === "Escape") {
    // The panel first; the sheet behind it is the next Esc's.
    event.preventDefault();
    event.stopPropagation();
    suggestAsked += 1;
    closeSuggest();
  }
});

// A press on the panel keeps the field's focus; a row lights on the way down
// and plays on the way up.
suggestPanel.addEventListener("mousedown", (event) => event.preventDefault());
suggestPanel.addEventListener("pointerdown", (event) => {
  const index = suggestRowAt(event.target);
  if (index !== -1 && event.button === 0) lightSuggest(index);
});
suggestPanel.addEventListener("pointermove", (event) => {
  const index = suggestRowAt(event.target);
  if (index !== -1 && index !== suggestLit) lightSuggest(index);
});
suggestPanel.addEventListener("click", (event) => {
  const index = suggestRowAt(event.target);
  if (index !== -1) suggestRows[index].pick();
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
  // Ctrl+1…9, 0: sections. It works in a text box too — pressing a number
  // key with Ctrl is not typing.
  if ((event.ctrlKey || event.metaKey) && !event.altKey) {
    const slot = event.key === "0" ? 10 : Number(event.key);
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
    event.preventDefault();
    toggleLyrics();
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
          {
            menu: () => songMenu(hit),
          },
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
    hits.length >= SEARCH_LIMIT ? `the first ${fmt(SEARCH_LIMIT)} matches` : `${countOf(hits.length, "match")}`;
  $("searchEmpty").hidden = found;
  $("searchEmpty").textContent = `Nothing in your history for “${report.query}” yet.`;
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
  $("statsScope").replaceChildren(
    String(scope),
    h("span", { class: "debug-only", text: ` · ${countOf(r.listens_in_scope, "record")} in scope` }),
  );
  $("statsSummary").replaceChildren(
    figure(fmt(r.plays), "plays"),
    figure(hoursText(r.total_ms_played), "of listening"),
    figure(fmt(r.unique_artists), "artists"),
    figure(fmt(r.unique_tracks), "songs"),
  );
  // Partial success is **always** reported: how many records fell below the
  // threshold, how many have no canonical identity, how many are outside the
  // year. Nothing is swallowed silently (K9).
  //
  // Plainly and only when not zero for everyone (D-083); every count, zeros
  // included, in the debug mode.
  const full = [
    `${countOf(r.skipped_short, "short play")} below the threshold`,
    `${countOf(r.without_canonical_id, "record")} without a canonical identity`,
  ];
  if (r.query.year !== null) full.push(`${countOf(r.out_of_scope, "record")} outside the year`);
  const plain = [];
  if (r.skipped_short > 0) plain.push(`${countOf(r.skipped_short, "quick skip")} left out`);
  if (r.without_canonical_id > 0) plain.push(`${countOf(r.without_canonical_id, "play")} couldn't be matched to a song`);
  if (r.query.year !== null && r.out_of_scope > 0) plain.push(`${countOf(r.out_of_scope, "play")} from other years`);
  $("statsNotes").replaceChildren(
    h("span", { class: "plain-only", text: plain.join(" · ") }),
    h("span", { class: "debug-only", text: full.join(" · ") }),
  );
  // Zero listens is not an error, but an empty table is not an answer either:
  // it says what to do.
  $("statsEmpty").hidden = r.plays > 0;
  $("statsLists").hidden = r.plays === 0;

  renderRanking(
    $("statsArtists"),
    r.top_artists,
    (a) => [a.artist, `${countOf(a.unique_tracks, "song")}`, a.plays],
    (a) => artistMenu(a.artist),
  );
  renderRanking(
    $("statsTracks"),
    r.top_tracks,
    (t) => [t.title, t.artist, t.plays],
    (t) => songMenu(t),
  );
  renderRanking(
    $("statsAlbums"),
    r.top_albums,
    (a) => [a.album, a.artist, a.plays],
    (a) => albumMenu(a.album, a.artist),
  );
}

function renderRanking(list, entries, format, menuOf) {
  const max = Math.max(1, ...entries.map((entry) => format(entry)[2]));
  const bars = [];
  list.replaceChildren(
    ...entries.map((entry, index) => {
      const [name, sub, plays] = format(entry);
      const bar = h("span", { class: "rank-bar", "aria-hidden": "true" });
      bars.push([bar, plays / max]);
      return h(
        "li",
        { menu: () => menuOf(entry) },
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
          title: `${entry.year}: ${countOf(entry.plays, "play")} · ${durationText(entry.ms_played)}`,
          "aria-pressed": "false",
          onclick: () => runStats(stats.year === entry.year ? null : entry.year),
          menu: () => ({
            title: `${entry.year} · ${countOf(entry.plays, "play")}`,
            items: [
              stats.year === entry.year
                ? { label: "back to all time", run: () => runStats(null) }
                : { label: `just ${entry.year}`, run: () => runStats(entry.year) },
              { label: `your ${entry.year} sleeve`, run: () => sleeveFor(entry.year) },
            ],
          }),
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

/// A year's card, from its bar in the statistics: the preview is drawn for
/// that year when the section opens.
function sleeveFor(year) {
  $("sleeveYear").value = String(year);
  sleeve.previewed = false;
  openPanel("sleeve");
}

$("btnSleeveSave").addEventListener("click", saveSleeve);

async function saveSleeve() {
  const args = sleeveArgs();
  const path = await pickSavePath({
    title: "save your sleeve",
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
      ? `Saved your card to ${written.path}.`
      : "Your card is ready, but it wasn't saved — try again.",
    true,
  );
}

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
  not_music: "not music (podcasts and the like)",
  missing_title: "no song title",
  missing_artist: "no artist name",
  bad_timestamp: "unreadable date",
  bad_duration: "unreadable length",
};

// The links of the identity chain, in K6's order: how the listener reads
// them, and what the debug mode calls them (D-084).
const CHAIN = [
  ["by_isrc", "by their code", "m-isrc", "ISRC"],
  ["by_mbid", "by name, exactly", "m-mbid", "MBID"],
  ["by_fuzzy", "by a close name", "m-fuzzy", "fuzzy"],
  ["by_fingerprint", "by their sound", "m-fingerprint", "fingerprint"],
  ["by_local_key", "kept as they are", "m-local", "local key"],
];

$("importForm").addEventListener("submit", (event) => {
  event.preventDefault();
  const path = $("importPath").value.trim();
  if (path) runImport(path);
});

$("btnImportPick").addEventListener("click", pickImport);

async function pickImport() {
  const path = await pickFile({
    title: "choose your Spotify data",
    multiple: false,
    filters: [{ name: "Export archive", extensions: ["zip"] }],
  });
  if (path) runImport(path);
}

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
  // What changed in the library first; how the archive was read under
  // "details" (D-083).
  $("importSummary").replaceChildren(
    figure(fmt(w.inserted), "new plays"),
    figure(fmt(w.duplicates), "already here", true),
    figure(fmt(w.new_tracks), "new songs"),
  );
  const isrc = figure(fmt(i.with_isrc), "carrying an ISRC", true);
  isrc.classList.add("debug-only");
  $("importMore").replaceChildren(
    figure(fmt(i.records_total), "entries in the file"),
    figure(fmt(i.listens), "plays found"),
    isrc,
  );
  renderChain(d);

  // The skipped ones with their reasons: "0 records came" and "they came but
  // were filtered out" are separate diagnoses (K9).
  const skipped = Object.entries(i.skipped ?? {});
  const skippedLine = $("importSkipped");
  skippedLine.hidden = skipped.length === 0;
  skippedLine.textContent = `left out: ${skipped.map(([why, n]) => `${SKIP_LABELS[why] ?? why} ${fmt(n)}`).join(" · ")}`;

  $("importResult").textContent = JSON.stringify({ import: i, identity: d, write: w }, null, 2);
  $("importReport").hidden = false;

  // The history changed: the statistics and the card are recomputed the next
  // time they open.
  stats.loaded = false;
  sleeve.previewed = false;
  toast(w.inserted > 0 ? `All done — ${countOf(w.inserted, "new play")} imported!` : "All done — nothing new this time.", true);
}

/// How many records each link resolved: the ratios are the part's share of the
/// whole, not a threshold. Choosing what counts as "authoritative" is the
/// core's job (`ResolveSummary`).
function renderChain(summary) {
  const total = summary.total;
  const bar = h("div", { class: "chain-bar", role: "img", "aria-label": "identity chain breakdown" });
  const legend = h("ul", { class: "chain-legend" });
  for (const [key, label, className, technical] of CHAIN) {
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
        h("span", { class: "debug-only", text: `(${technical}) ` }),
        h("b", { text: fmt(count) }),
      ),
    );
  }
  legend.append(h("li", {}, `in all `, h("b", { text: fmt(total) })));
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
const CAPABILITY_NAMES = {
  search: "search",
  browse: "browse",
  stream: "play",
  control: "control",
  artwork: "show covers",
  lyrics: "show lyrics",
};

// ————————————————————————————————————— play from (D-080)
//
// Which provider the play box and its suggestions search first; the others
// are asked only when it finds nothing. The choice is the core's to keep
// (`provider_use`, the CLI's `provider use`); this shows it and makes it.
// The choice shows the moment the row is pressed — the core is told after,
// and if it refuses, what the core says is drawn again.

const STREAM = 1 << 2;
let playFrom = null;
const providerNames = new Map();

/// The providers list's answer: the names, for the play box and the
/// suggestions, and where it searches first.
function takeProviders(list) {
  providerNames.clear();
  for (const info of list.providers) providerNames.set(info.id, info.display_name);
  playFrom = list.play_from;
  updatePlayPlaceholder();
}

function providerName(id) {
  return (id && providerNames.get(id)) ?? id ?? "nowhere";
}

function updatePlayPlaceholder() {
  playInput.placeholder = playFrom?.provider
    ? `what do you want to hear? (from ${providerName(playFrom.provider)})`
    : "what do you want to hear?";
}

function renderPlayFrom(list) {
  const rows = list.providers.filter((info) => (info.capabilities & STREAM) !== 0).map(playFromRow);
  $("playFromList").replaceChildren($("playFromPill"), ...rows);
  const missing = $("playFromMissing");
  missing.hidden = !playFrom.missing;
  missing.textContent = playFrom.missing
    ? `${providerName(playFrom.missing)} isn't available right now, so we'll look in ` +
      `${providerName(playFrom.provider)} first.`
    : "";
  placePlayFromPill(true);
}

function playFromRow(info) {
  const chosen = info.id === playFrom?.provider;
  const row = h(
    "button",
    {
      type: "button",
      role: "radio",
      class: "play-from-row",
      "aria-checked": String(chosen),
      tabindex: chosen ? "0" : "-1",
      dataset: { provider: info.id },
    },
    h("span", { class: "play-from-name", text: info.display_name }),
    h("span", { class: "play-from-id debug-only", text: info.id }),
  );
  // Chosen the moment the button goes down, as the sidebar's rows are;
  // `click` is for the keyboard and touch.
  row.addEventListener("pointerdown", (event) => {
    if (event.button === 0 && event.pointerType !== "touch") choosePlayFrom(info.id);
  });
  row.addEventListener("click", () => choosePlayFrom(info.id));
  row.addEventListener("keydown", (event) => {
    const step = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 }[event.key];
    if (!step) return;
    event.preventDefault();
    const rows = [...$("playFromList").querySelectorAll(".play-from-row")];
    const next = rows[(rows.indexOf(row) + step + rows.length) % rows.length];
    choosePlayFrom(next.dataset.provider);
    next.focus();
  });
  return row;
}

function markPlayFrom(id) {
  for (const row of $("playFromList").querySelectorAll(".play-from-row")) {
    const chosen = row.dataset.provider === id;
    row.setAttribute("aria-checked", String(chosen));
    row.tabIndex = chosen ? 0 : -1;
  }
  placePlayFromPill(false);
}

/// The light under the chosen row slides to it; quick choices in a row turn
/// it around halfway (the sidebar's pill).
function placePlayFromPill(instant) {
  const pill = $("playFromPill");
  const row = [...$("playFromList").querySelectorAll(".play-from-row")].find(
    (each) => each.getAttribute("aria-checked") === "true",
  );
  if (!row?.offsetHeight) {
    pill.hidden = true;
    return;
  }
  pill.style.height = `${row.offsetHeight}px`;
  if (instant || pill.hidden) {
    place(pill, { y: row.offsetTop });
    pill.hidden = false;
  } else {
    animate(pill, { y: row.offsetTop });
  }
}

async function choosePlayFrom(id) {
  if (playFrom?.provider === id && playFrom.chosen) return;
  markPlayFrom(id);
  const report = await call("provider_use", { name: id });
  if (!report) {
    refreshProviders();
    return;
  }
  playFrom = report.play_from;
  updatePlayPlaceholder();
  $("playFromMissing").hidden = true;
}

let environment = null;

async function refreshProviders() {
  refreshMusicDirs();

  const list = await call("providers");
  if (list) {
    takeProviders(list);
    renderPlayFrom(list);
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
          {
            menu: () => ({
              title: server.id,
              items: [
                { label: "copy the address", run: () => copyText(server.url, "address copied to the clipboard") },
                "-",
                { label: "remove", danger: true, confirm: "remove it?", run: () => removeServer(server) },
              ],
            }),
          },
          h("td", { text: server.id }),
          h("td", { class: "dim", text: server.kind }),
          h("td", { class: "mono", text: server.url }),
          h("td", { text: `${server.username} (${server.auth})` }),
          h("td", {}, armedButton("delete", () => removeServer(server))),
        ),
      ),
    );
  $("serverTable").hidden = servers.servers.length === 0;
  $("serverEmpty").hidden = servers.servers.length > 0;
}

async function removeServer(server) {
  const report = await call("server_remove", { name: server.id });
  if (report) {
    toast(debugMode ? `${report.id} removed · left: ${fmt(report.remaining)}` : `Removed ${report.id}.`, true);
    refreshProviders();
  }
}

// ————————————————————————————————————— music folders (D-085)
//
// Chosen here and kept by the core (`headshell music-dirs`). Adding or taking
// one off checks for new music at once: the songs come and go with their
// folder. While `HEADSHELL_MUSIC_DIRS` decides the run, the list is the
// variable's and cannot be changed here.

let musicFolders = null;

async function refreshMusicDirs() {
  const report = await call("music_dirs");
  if (report) paintMusicDirs(report.folders);
}

function paintMusicDirs(folders) {
  musicFolders = folders;
  const held = folders.source === "variable";
  $("musicDirs").replaceChildren(
    ...(folders.dirs.length > 0
      ? folders.dirs.map((dir) =>
          h(
            "li",
            {
              class: "folder",
              menu: () => [
                { label: "copy the path", run: () => copyText(dir, "path copied to the clipboard") },
                !held && { label: "remove this folder", danger: true, run: () => removeFolder(dir) },
              ],
            },
            // The folder's name first — what the listener recognises — and
            // where it is, quieter; the whole path in the tooltip.
            h("span", { class: "folder-name", text: folderName(dir), title: dir }),
            h("span", { class: "folder-path", text: folderParent(dir), title: dir }),
            held
              ? null
              : h(
                  "button",
                  {
                    type: "button",
                    class: "icon-button subtle",
                    title: "remove this folder",
                    "aria-label": `remove ${dir}`,
                    onclick: () => removeFolder(dir),
                  },
                  icon("close"),
                ),
          ),
        )
      : [h("li", { class: "empty", text: "No folders yet — add one and its songs show up here." })]),
  );
  $("btnAddFolder").disabled = held;
  const note = $("musicDirsNote");
  note.hidden = !held;
  note.replaceChildren(
    h("span", { class: "plain-only", text: "These folders are set outside headshell for now, so they can't be changed here." }),
    h("span", { class: "debug-only", text: "HEADSHELL_MUSIC_DIRS decides the folders for this launch; the ones chosen here apply without it." }),
  );
}

$("btnAddFolder").addEventListener("click", async () => {
  const path = await pickFile({ directory: true, title: "choose a music folder" });
  if (!path) return;
  const report = await call("music_dirs_add", { path });
  if (!report) return;
  paintMusicDirs(report.folders);
  if (!report.changed) {
    toast("That folder is already on the list.", true);
    return;
  }
  await scanAfterFolders(`Added ${folderName(path)}`);
});

async function removeFolder(dir) {
  const report = await call("music_dirs_remove", { path: dir });
  if (!report) return;
  paintMusicDirs(report.folders);
  await scanAfterFolders(`Removed ${folderName(dir)}`);
}

/// A folder changed: its songs come or go with it (D-085).
async function scanAfterFolders(done) {
  const report = await call("provider_scan", { ifStale: false });
  if (report) toast(`${done} — ${countOf(report.summary.indexed, "song")} in your music folders now.`, true);
  refreshProviders();
}

function folderName(path) {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/// Where a folder is: its path without its own name.
function folderParent(path) {
  const name = folderName(path);
  return path.endsWith(name) ? path.slice(0, path.length - name.length).replace(/[\\/]$/, "") || path : path;
}

function providerRow(info) {
  const status = h("span", { class: "health", text: "—" });
  const caps = CAPABILITIES.filter(([bit]) => (info.capabilities & bit) !== 0).map(([, name]) =>
    h("span", { class: "tag", text: name }),
  );
  return h(
    "tr",
    {
      menu: () => ({
        title: info.display_name,
        items: [
          { label: "check the connection", run: () => testProvider(info, status) },
          "-",
          debugMode && { label: `copy “${info.id}”`, run: () => copyText(info.id, "name copied to the clipboard") },
        ],
      }),
    },
    h("td", {}, h("div", { text: info.display_name }), h("div", { class: "dim mono debug-only", text: info.id })),
    h(
      "td",
      { class: "debug-only" },
      h("div", { class: "caps" }, caps.length > 0 ? caps : h("span", { class: "dim", text: "none" })),
    ),
    h("td", {}, status),
    h("td", {}, h("button", { type: "button", onclick: () => testProvider(info, status) }, "check")),
  );
}

async function testProvider(info, status) {
  const report = await call("provider_test", { name: info.id });
  // "I did not look" and "I could not reach it" are separate: the error is in
  // the notice, the health answer is here.
  if (!report) {
    status.className = "health down";
    status.replaceChildren(h("span", { class: "dot" }), " didn't work (see the message)");
    return;
  }
  const health = report.health;
  const count = health.track_count;
  status.className = health.reachable ? "health up" : "health down";
  status.replaceChildren(
    h("span", { class: "dot" }),
    health.reachable ? ` working${count === null ? "" : ` · ${countOf(count, "song")}`}` : ` can't connect`,
    health.reachable
      ? null
      : h("span", { class: "health-detail debug-only", text: ` — ${health.detail ?? "no reason given"}` }),
  );
}

$("btnScan").addEventListener("click", () => scan(false));
$("btnScanStale").addEventListener("click", () => scan(true));

async function scan(onlyStale) {
  const report = await call("provider_scan", { ifStale: onlyStale });
  if (!report) return;
  // "Unchanged" and "I could not look" are different things: the reason the
  // core gives is shown as it is (K9).
  // In plain words what changed; every count and the reason in the debug
  // mode (D-084).
  const technical = report.scanned
    ? `scanned · ${countOf(report.summary.indexed, "track")} (${fmt(report.summary.failed)} unreadable) · added ${fmt(report.write.inserted)}, updated ${fmt(report.write.updated)}, dropped ${fmt(report.write.removed)} — ${report.reason}`
    : `scan skipped — ${report.reason}`;
  const added = report.write.inserted;
  const plain = !report.scanned
    ? "All caught up — nothing new since last time."
    : added > 0
      ? `Found ${countOf(added, "new song")} — ${countOf(report.summary.indexed, "song")} in your music folders now.`
      : `All caught up — ${countOf(report.summary.indexed, "song")} in your music folders.`;
  toast(debugMode ? technical : plain, true);
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
  toast(
    debugMode
      ? `${report.server.id} added${report.verified ? " (verified)" : " (not verified)"}${notes}`
      : report.verified
        ? `Connected to ${report.server.id}!`
        : `Saved ${report.server.id} — we didn't check the connection.`,
    true,
  );
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
  approved: ["allowed", "chip chip-ok"],
  not_asked: ["needs your OK", "chip chip-warn"],
  needs_approval: ["wants more access", "chip chip-warn"],
  disabled: ["turned off", "chip"],
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
    // What needs the user first, and only what is not zero (D-083); every
    // count in the debug mode.
    const plainCounts = [
      [s.ready, "ready to play"],
      [s.needs_install, "need a quick setup"],
      [s.awaiting_approval, "need your OK"],
      [s.disabled, "turned off"],
      [s.incompatible, "won't work here"],
      [s.broken, "broken"],
    ];
    const counts = [
      [s.ready, "ready"],
      [s.needs_install, "awaiting install"],
      [s.awaiting_approval, "awaiting consent"],
      [s.disabled, "disabled"],
      [s.incompatible, "incompatible"],
      [s.broken, "broken"],
    ];
    $("pluginSummary").replaceChildren(
      h("span", {
        class: "plain-only",
        text: [countOf(s.discovered, "service"), ...plainCounts.filter(([n]) => n > 0).map(([n, label]) => `${fmt(n)} ${label}`)].join(" · "),
      }),
      h("span", {
        class: "debug-only",
        text: [countOf(s.discovered, "plugin"), ...counts.map(([n, label]) => `${fmt(n)} ${label}`)].join(" · "),
      }),
    );
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
    h(
      "span",
      { class: "author" },
      entry.version ?? "",
      h("span", {
        class: "debug-only",
        text: [entry.name, entry.api === null || entry.api === undefined ? null : `api ${entry.api}`]
          .filter(Boolean)
          .map((part) => ` · ${part}`)
          .join(""),
      }),
    ),
    consent ? h("span", { class: consentClass, text: consentText }) : null,
  );

  const { lead, tail } = pluginActions(entry);
  const card = h(
    "li",
    {
      class: "plugin",
      menu: () => ({
        title: entry.display_name ?? entry.name,
        items: [
          ...lead,
          "-",
          ...tail,
          "-",
          debugMode && { label: `copy “${entry.name}”`, run: () => copyText(entry.name, "name copied to the clipboard") },
        ],
      }),
    },
    head,
  );

  // If the manifest could not be read, the reason stays here — so the reason
  // the buttons do not work does not stay out of sight (K9).
  if (entry.problem) card.append(h("p", { class: "plugin-problem", text: entry.problem }));

  if (consent === "needs_approval") {
    card.append(
      h("p", { class: "perms" }, h("b", { text: "now also wants to reach: " }), (entry.consent.extra?.net ?? []).join(", ") || "—"),
    );
  }
  const net = entry.permissions?.net ?? [];
  card.append(
    h("p", { class: "perms" }, h("b", { text: "connects to: " }), net.length > 0 ? net.join(", ") : "nothing"),
  );

  // The artifacts the engine will download go on a separate line (D-055): the
  // engine does the downloading, not the plugin, so they do not mix with the
  // plugin's permission list.
  // A tool the service needs is the listener's business only when it is
  // not ready; which tool, its version and its path are the debug mode's.
  for (const requirement of entry.requires ?? []) {
    const { plain, detail, ready } = requirementText(requirement.state);
    card.append(
      h(
        "p",
        { class: ready ? "plugin-need debug-only" : "plugin-need" },
        h("span", { class: "plain-only", text: plain }),
        h("span", {
          class: "debug-only",
          text: `engine: ${requirement.name} ${requirement.version} (${requirement.platform}) — ${detail ?? plain}`,
        }),
      ),
    );
  }

  card.append(
    h("div", { class: "actions" }, lead.map(actionButton), h("span", { class: "spacer" }), tail.map(actionButton)),
  );
  return card;
}

/// What a plugin offers: the buttons on its card, and the same rows in its
/// menu — one list, so the two cannot drift apart.
function pluginActions(entry) {
  const consent = entry.consent?.state;
  // Install only if something is missing that installing will fix: for an
  // artifact whose platform is not supported, "install" would be a pointless
  // button.
  const installable = (entry.requires ?? []).some(
    (requirement) => requirement.state === "Missing" || requirement.state?.Corrupt,
  );
  const command = (label, name, primary = false) => ({ label, primary, run: () => pluginCommand(name, entry.name) });
  const lead = [];
  if (consent === "not_asked" || consent === "needs_approval") lead.push(command("allow", "plugin_approve", true));
  if (installable) lead.push(command("finish setup", "plugin_install", true));
  if (consent === "disabled") lead.push(command("turn on", "plugin_enable", true));
  if (consent === "approved") lead.push(command("turn off", "plugin_disable"));
  const tail = [];
  if (consent && consent !== "not_asked") tail.push(command("revoke", "plugin_forget"));
  // Removing cannot be undone (the directory goes along with `state/`).
  tail.push({ label: "remove", danger: true, confirm: "remove it?", run: () => removePlugin(entry.name) });
  return { lead, tail };
}

/// An action's button: one that cannot be undone asks first.
function actionButton(action) {
  if (action.confirm) return armedButton(action.label, action.run);
  return h("button", { type: "button", class: action.primary ? "primary" : null, onclick: action.run }, action.label);
}

async function removePlugin(name) {
  const report = await call("plugin_remove", { name });
  if (!report) return;
  const kept = report.kept_secrets.length ? ` · secrets left: ${report.kept_secrets.join(", ")}` : "";
  toast(
    debugMode
      ? `${report.name} removed · consent ${report.consent_forgotten ? "forgotten" : "had no record"}${kept}`
      : `Removed ${report.name}.`,
    true,
  );
  refreshPlugins();
}

/// The artifact state arrives externally tagged: `"Missing"` or
/// `{ Installed: { path } }` / `{ Corrupt: { expected, found } }` /
/// `{ Unsupported: { platform, available } }`. In plain words for everyone,
/// the path, the hashes and the platforms in the debug mode (D-083).
function requirementText(state) {
  if (state === "Missing") {
    return { plain: "Needs a quick setup — press finish setup.", detail: "not installed", ready: false };
  }
  if (typeof state === "object" && state !== null) {
    if (state.Installed) return { plain: "Ready.", detail: `installed · ${state.Installed.path}`, ready: true };
    if (state.Corrupt) {
      return {
        plain: "Something went wrong with its setup — press finish setup to fix it.",
        detail: `hash mismatch (expected ${short(state.Corrupt.expected)}, found ${short(state.Corrupt.found)})`,
        ready: false,
      };
    }
    // Installing does not fix this; that is why the "install" button is not
    // shown.
    if (state.Unsupported) {
      return {
        plain: "Doesn't work on this computer yet.",
        detail: `no release for ${state.Unsupported.platform} · declared: ${state.Unsupported.available.join(", ")}`,
        ready: false,
      };
    }
  }
  // An unknown state is not silently counted as "fine".
  return { plain: "Isn't ready yet.", detail: JSON.stringify(state), ready: false };
}

function short(hash) {
  return typeof hash === "string" ? hash.slice(0, 12) : String(hash);
}

async function pluginCommand(command, name) {
  const report = await call(command, { name });
  if (!report) return;
  // The install report's shape differs from the consent report's: each is
  // summarised with its own fields; no common "result" type is made up.
  toast(report.action ? consentText(report) : installText(name, report), true);
  refreshPlugins();
}

// The name of the consent command is a wire value (D-036); an unrecognised
// name is shown raw.
const ACTION_LABELS = {
  approve: "approved",
  disable: "disabled",
  enable: "enabled",
  forget: "consent forgotten",
};

/// What allowing, turning off or revoking did — in a sentence; the states
/// in the debug mode (D-084).
const ACTION_DONE = {
  approve: (name) => `${name} is allowed — enjoy!`,
  disable: (name) => `${name} is turned off.`,
  enable: (name) => `${name} is back on.`,
  forget: (name) => `${name} will ask for your OK again.`,
};

function consentText(report) {
  const state = report.status?.state;
  if (!debugMode) return ACTION_DONE[report.action]?.(report.name) ?? `${report.name}: done.`;
  return `${report.name}: ${ACTION_LABELS[report.action] ?? report.action} · ${CONSENT[state]?.[0] ?? state ?? "—"}`;
}

function installText(name, report) {
  if (!debugMode) {
    if (!report.ready) return `${name} is added, but its setup isn't finished — try finish setup.`;
    return report.consent?.state === "approved" ? `${name} is ready to play!` : `${name} is added — allow it to start listening.`;
  }
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
  current: ["added", "chip chip-ok"],
  update_available: ["update ready", "chip chip-info"],
  manual: ["added by hand", "chip"],
  modified: ["changed by hand", "chip chip-warn"],
  unreadable: ["can't tell", "chip chip-err"],
};

async function refreshCatalog() {
  const report = await call("plugin_catalog");
  if (!report) return;
  $("catalogList").replaceChildren(...report.plugins.map((plugin) => catalogCard(plugin, report.platform)));
  const s = report.summary;
  const plainParts = [countOf(s.listed, "service")];
  if (s.updates > 0) plainParts.push(`${countOf(s.updates, "update")} ready`);
  $("catalogSummary").replaceChildren(
    h("span", { class: "plain-only", text: plainParts.join(" · ") }),
    h("span", {
      class: "debug-only",
      text:
        `${countOf(s.listed, "plugin")} · ${fmt(s.installable)} installable · ${fmt(s.installed)} installed · ` +
        `${countOf(s.updates, "update")} · ${fmt(s.problems)} cannot be installed`,
    }),
  );
  const delisted = $("catalogDelisted");
  delisted.hidden = report.delisted.length === 0;
  delisted.textContent = report.delisted.length
    ? `No longer offered, but still added here: ${report.delisted.join(", ")}. ` +
      "You can remove them under added."
    : "";
}

function catalogCard(plugin, platform) {
  const state = plugin.installed?.state;
  const [stateText, stateClass] = plugin.problem ? ["can't be added", "chip chip-err"] : (INSTALL[state] ?? [state, "chip"]);
  const offered = catalogActions(plugin, state);
  const card = h(
    "li",
    {
      class: "plugin",
      menu: () => ({
        title: plugin.display_name ?? plugin.name,
        items: [
          ...offered,
          "-",
          debugMode && { label: `copy “${plugin.name}”`, run: () => copyText(plugin.name, "name copied to the clipboard") },
        ],
      }),
    },
    h(
      "div",
      { class: "plugin-head" },
      h("span", { class: "name", text: plugin.display_name ?? plugin.name }),
      h(
        "span",
        { class: "author" },
        plugin.version ?? "",
        h("span", { class: "debug-only", text: plugin.name ? ` · ${plugin.name}` : "" }),
      ),
      stateText ? h("span", { class: stateClass, text: stateText }) : null,
    ),
  );

  if (plugin.description) card.append(h("p", { class: "plugin-desc", text: plugin.description }));
  // An entry that cannot be installed is not hidden: the user should see why
  // they cannot install what they were looking for (K9).
  if (plugin.problem) {
    card.append(h("p", { class: "plugin-problem plain-only", text: "This one can't be added on this computer." }));
    card.append(h("p", { class: "plugin-problem debug-only", text: plugin.problem }));
  }
  const detail = installDetail(plugin.installed);
  if (detail) card.append(h("p", { class: "plugin-need", text: detail }));

  if (!plugin.problem) {
    const caps = (plugin.capabilities ?? []).map((cap) => CAPABILITY_NAMES[cap] ?? cap);
    if (caps.length > 0) card.append(h("p", { class: "perms" }, h("b", { text: "lets you: " }), caps.join(" · ")));
    const net = plugin.permissions?.net ?? [];
    card.append(h("p", { class: "perms" }, h("b", { text: "connects to: " }), net.length > 0 ? net.join(", ") : "nothing"));
    for (const requirement of plugin.requires ?? []) {
      const here = requirement.assets?.[platform];
      card.append(
        h(
          "p",
          { class: here ? "plugin-need debug-only" : "plugin-need" },
          h("span", { class: "plain-only", text: "Doesn't work on this computer yet." }),
          h("span", {
            class: "debug-only",
            text: `engine: ${requirement.name} ${requirement.version} (${platform})${here ? "" : " — no release for this platform"}`,
          }),
        ),
      );
    }
  }

  if (offered.length > 0) card.append(h("div", { class: "actions" }, offered.map(actionButton)));
  return card;
}

/// What a catalog entry offers, on its card and in its menu alike.
function catalogActions(plugin, state) {
  if (plugin.problem) return [];
  if (state === "not_installed") {
    return [{ label: "add", primary: true, run: () => catalogCommand("plugin_install", plugin.name) }];
  }
  if (state === "update_available") {
    return [{ label: "update", primary: true, run: () => catalogCommand("plugin_update", plugin.name) }];
  }
  return [];
}

function installDetail(installed) {
  switch (installed?.state) {
    case "update_available":
      return debugMode
        ? `installed ${installed.installed} → ${installed.available} in the catalog`
        : `A newer version is ready (${installed.available}).`;
    case "modified":
      return debugMode
        ? `installed ${installed.version}; files changed by hand: ${installed.files.join(", ")} — an update does not overwrite them`
        : "Changed by hand, so updates leave it alone.";
    case "manual":
      return debugMode
        ? "installed by hand (no origin record) — the catalog does not touch it"
        : "Added by hand, so updates leave it alone.";
    case "unreadable":
      return debugMode ? `origin record unreadable: ${installed.detail}` : null;
    default:
      return null;
  }
}

async function catalogCommand(command, name) {
  const report = await call(command, { name });
  if (!report) return;
  toast(command === "plugin_update" ? updateText(report) : installText(name, report), true);
  refreshCatalog();
  // An installed plugin awaits consent (D-040): switch to where the result
  // lives.
  if (command === "plugin_install") pluginTabs.select($("tabInstalled"));
  refreshPlugins();
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
  if (!debugMode) {
    if (s.failed > 0) return `${countOf(s.failed, "service")} couldn't be updated — the details say why.`;
    return s.updated > 0 ? `Updated ${countOf(s.updated, "service")}.` : "Everything is up to date.";
  }
  return lines.length ? `${lines.join(" · ")} (${total})` : "no plugins installed from the catalog";
}

$("btnCatalogLoad").addEventListener("click", refreshCatalog);
$("btnCatalogUpdateAll").addEventListener("click", updateAllPlugins);

async function updateAllPlugins() {
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
}

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
          {
            menu: () => ({
              title: `${namespace}/${key}`,
              items: [
                { label: "remove", danger: true, confirm: "remove it?", run: () => removeSecret(namespace, key) },
              ],
            }),
          },
          h("td", { class: "mono", text: namespace }),
          h("td", { class: "mono", text: key }),
          h("td", {}, armedButton("delete", () => removeSecret(namespace, key))),
        ),
      );
    }
  }
  $("secretTable").querySelector("tbody").replaceChildren(...rows);
  $("secretTable").hidden = rows.length === 0;
  $("secretEmpty").hidden = rows.length > 0;
}

async function removeSecret(namespace, key) {
  const report = await call("secret_remove", { namespace, key });
  if (!report) return;
  toast(report.changed ? "Removed the key." : "That key wasn't there.", true);
  refreshSecrets();
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
  toast("Saved — it stays private.", true);
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

  $("themeDir").replaceChildren(
    `theme directory : ${list.dir}`,
    h("span", { class: "debug-only", text: `\ncontract version: api ${list.api}` }),
  );

  // The default is an option too: the way back from a theme must be visible.
  $("themeList").replaceChildren(
    themeCard({ id: null, name: "default", author: "headshell", builtin: true, preview: list.default_preview }, list.active),
    ...list.themes.map((theme) => themeCard(theme, list.active)),
  );

  $("themeRejected").replaceChildren(
    ...list.rejected.map((entry) => h("li", {}, h("span", { class: "id", text: entry.id }), ` — ${entry.reason}`)),
  );
  // Only when there is one: an empty heading is noise (D-083).
  $("themeRejectedBlock").hidden = list.rejected.length === 0;
}

function themeCard(theme, activeId) {
  const current = (theme.id ?? null) === (activeId ?? null);
  // The theme in use has nothing to offer: its menu does not open.
  const card = h("li", {
    class: current ? "theme current" : "theme",
    menu: () =>
      current ? null : { title: theme.name, items: [{ label: "use this look", run: () => selectTheme(theme.id ?? null) }] },
  });
  card.append(swatch(theme.preview));

  const name = h("span", { class: "name", text: theme.name });
  if (theme.author) name.append(h("span", { class: "author", text: ` · ${theme.author}` }));
  card.append(name);

  const foot = h("div", { class: "theme-foot" });
  if (theme.builtin && theme.id) foot.append(h("span", { class: "tag", text: "built in" }));
  // D-038: we do not reject, we flag. The label tells the user "this theme goes
  // outside the contract, there is no guarantee".
  if (theme.extended) foot.append(h("span", { class: "tag warn debug-only", text: "extended · no guarantee" }));
  foot.append(
    current
      ? h("button", { type: "button", disabled: true }, icon("check"), "in use")
      : h("button", { type: "button", onclick: () => selectTheme(theme.id ?? null) }, "use"),
  );
  card.append(foot);
  return card;
}

async function selectTheme(id) {
  const active = await call("theme_select", { id });
  if (!active) return;
  applyTheme(active);
  refreshThemes();
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

/// Why a resolution has no match — the CLI's words (K9): no candidate came
/// close, candidates could not be told apart (D-046), or the source ranks
/// more as high than were read (D-081). None of them is a choice.
function noMatchReason(res) {
  const atLeast = res.unread_equals ? "at least " : "";
  if (res.tied_candidates > 1) {
    return `${atLeast}${countOf(res.tied_candidates, "candidate")} could not be told apart; no authority claimed`;
  }
  if (res.unread_equals) {
    return "the source ranks more candidates as high than were read; none is claimed";
  }
  return "no candidate came close enough";
}

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
        : `none — ${noMatchReason(res)}`,
    ],
  ];
  if (res.matched?.disambiguation) facts.push(["not", res.matched.disambiguation]);
  $("resolveFacts").replaceChildren(
    ...facts.flatMap(([label, value]) => [h("dt", { text: label }), h("dd", { text: value })]),
  );
  $("resolveResult").textContent = JSON.stringify(res, null, 2);
  $("resolveCard").hidden = false;
});

// ————————————————————————————————————— context menus (D-079)
//
// The engine's own menu never opens (`menu.js`). In its place every element
// offers what can be done with it, and one with nothing to offer offers
// nothing. Each row calls a function this file already has: a menu is a
// second way to what the interface does, not a capability of its own (K1,
// D-033). Rows drawn from data carry their menu where they are drawn
// (`h(…, { menu })`); the page's fixed elements get theirs here.

/// `Title · Artist` — the head of a track's menu.
function trackTitle(track) {
  return `${track.title} · ${track.artist}`;
}

/// What any track offers besides being played.
function trackItems(track) {
  return [
    { label: "find this artist in your library", run: () => searchLibrary(track.artist) },
    track.album && { label: "find this album in your library", run: () => searchLibrary(track.album) },
    {
      label: "copy artist and title",
      run: () => copyText(`${track.artist} — ${track.title}`, "artist and title copied to the clipboard"),
    },
    // Diagnostics' tool: there in the debug mode only (D-083).
    debugMode && { label: "resolve its identity", run: () => resolveIdentity(track) },
  ];
}

/// Every song's menu (D-080), wherever the song is drawn: play it now, add
/// it to the queue — exactly that track when it is a provider's (`item`: a
/// suggestion, a queue row), found by the play box's search when it is a
/// history row (library, statistics) — then what else it offers. A queue
/// row's "play now" jumps to it; the playing row's pauses.
///
/// The rows marked "soon" have nothing behind them yet: they are shown so
/// the menu keeps its shape when they come, and they cannot be picked.
/// "Play next" needs the queue to take a track between the playing one and
/// the one already handed to the engine for a gapless start (D-077).
function songMenu(track, { item = null, queueIndex = null, current = false } = {}) {
  const query = `${track.artist} ${track.title}`;
  const name = trackTitle(track);
  let playNow;
  if (current) playNow = pauseItem();
  else if (queueIndex !== null) playNow = { label: "play now", run: () => jumpTo(queueIndex) };
  else if (item) playNow = { label: "play now", run: () => playItems([item]) };
  else playNow = { label: "play now", run: () => startPlay(query, false) };
  return {
    title: name,
    items: [
      playNow,
      item
        ? { label: "add to queue", run: () => addItems([item], name) }
        : { label: "add to queue", run: () => addSearch(query, false, name) },
      { label: "play next", disabled: true, key: "soon" },
      "-",
      { label: "add to a playlist…", disabled: true, key: "soon" },
      { label: "start a radio from it", disabled: true, key: "soon" },
      { label: "like", disabled: true, key: "soon" },
      "-",
      ...trackItems(track),
    ],
  };
}

/// The space bar's row, named for what it does now.
function pauseItem() {
  const running = anchor?.state === "playing" || anchor?.state === "buffering";
  return { label: running ? "pause" : "play", key: "space", run: togglePause };
}

/// The library's answer for a query, as if it had been typed there.
function searchLibrary(query) {
  openPanel("library");
  $("searchQuery").value = query;
  $("searchForm").requestSubmit();
}

/// Which link gives a track its identity: the diagnostics' form, filled in.
function resolveIdentity(track) {
  openPanel("diag");
  $("resolveQuery").value = `${track.artist} - ${track.title}`;
  $("resolveForm").requestSubmit();
}

function artistMenu(artist) {
  return {
    title: artist,
    items: [
      { label: "play all their songs", run: () => startPlay(artist, true) },
      { label: "shuffle their songs", run: () => startPlay(artist, true, true) },
      {
        label: "add all their songs to your queue",
        run: () => addSearch(artist, true, `every track by ${artist}`),
      },
      "-",
      { label: "find them in your library", run: () => searchLibrary(artist) },
      { label: "copy the name", run: () => copyText(artist, "name copied to the clipboard") },
    ],
  };
}

/// The row does not promise the album played through: the core keeps no
/// track numbers, so the tracks come in the search's order.
function albumMenu(album, artist) {
  return {
    title: `${album} · ${artist}`,
    items: [
      { label: "play the album", run: () => startPlay(`${artist} ${album}`, true) },
      {
        label: "add the album to your queue",
        run: () => addSearch(`${artist} ${album}`, true, `every track on ${album}`),
      },
      "-",
      { label: "find it in your library", run: () => searchLibrary(album) },
      { label: "copy the name", run: () => copyText(`${artist} — ${album}`, "name copied to the clipboard") },
    ],
  };
}

/// The player: the bar, the record's card and the head of the whole-sheet
/// lyrics all offer the same.
function playerMenu() {
  const current = queue.items[queue.position];
  const sheet = nowOpen
    ? { label: "close now playing", key: "Esc", run: () => setNowOpen(false) }
    : { label: "now playing", key: "Ctrl+1", run: () => setNowOpen(true) };
  if (!current) {
    // Nothing to control yet: the way to begin.
    return { mark: false, items: [{ label: "play something…", key: "/", run: () => $("playQuery").focus() }, sheet] };
  }
  return {
    title: trackTitle(current.track),
    mark: false,
    items: [
      pauseItem(),
      { label: "next song", key: "→", run: playNext },
      { label: "previous song", key: "←", run: playPrevious },
      { label: "stop", run: stopPlayback },
      "-",
      { label: "shuffle", kind: "check", checked: queue.shuffle, run: () => setShuffle(!queue.shuffle) },
      ...Object.entries(REPEAT_LABELS).map(([mode, label]) => ({
        label: `repeat: ${label}`,
        kind: "radio",
        checked: queue.repeat === mode,
        run: () => setRepeat(mode),
      })),
      "-",
      { label: lyricsToggleLabel(), key: "L", run: toggleLyrics },
      sheet,
      "-",
      ...trackItems(current.track),
    ],
  };
}

/// The lyrics: the line under the pointer, all of them, and their timing.
function lyricsMenu(target) {
  const lines = lyricsItem?.lyrics?.lines ?? [];
  const line = target.closest?.(".lyric");
  const synced = lyricsItem?.status === "found" && lyricsItem.synced;
  return {
    mark: false,
    items: [
      line &&
        !line.classList.contains("gap") && {
          label: "copy the line",
          run: () => copyText(line.textContent, "the line copied to the clipboard"),
        },
      lines.length > 0 && {
        label: "copy the lyrics",
        run: () => copyText(lines.map((each) => each.text).join("\n"), "the lyrics copied to the clipboard"),
      },
      "-",
      synced && { label: "show the lines a little earlier", run: () => shiftLyrics(SHIFT_STEP_MS) },
      synced && { label: "show the lines a little later", run: () => shiftLyrics(-SHIFT_STEP_MS) },
      synced && {
        label: "back to the original timing",
        disabled: (lyricsItem.offset_ms ?? 0) === 0,
        run: () => shiftLyrics(null),
      },
      lyricsItem?.status === "failed" && { label: "try again", run: retryLyrics },
      "-",
      lyricsFull
        ? { label: "back to the record", key: "Esc", run: () => setLyricsFull(false) }
        : { label: "lyrics, full screen", run: () => setLyricsFull(true) },
    ],
  };
}

function sleeveMenu() {
  return {
    mark: false,
    items: [
      { label: "save as…", disabled: !sleeve.previewed, run: saveSleeve },
      { label: "refresh the preview", run: previewSleeve },
      "-",
      ...[...$("sleeveForm").querySelectorAll(".segment")].map((segment) => ({
        label: segment.textContent,
        kind: "radio",
        checked: sleeve.format === segment.dataset.format,
        run: () => sleeveFormat.select(segment),
      })),
    ],
  };
}

async function copyDiagReport() {
  const text = await call("diag_text");
  if (text === undefined) return;
  if (text === null) toast("no command has been run yet — there is no report to copy", true);
  else copyText(text, "diagnostics report copied to the clipboard");
}

/// A section's own work, on its row in the sidebar — without opening it
/// first. What shows its result in the section opens the section.
const SECTION_ITEMS = {
  now: () => [{ label: lyricsToggleLabel(), key: "L", run: toggleLyrics }],
  library: () => [
    {
      label: "search your library",
      run: () => {
        openPanel("library");
        $("searchQuery").focus();
      },
    },
  ],
  stats: () => [
    {
      label: "count again",
      run: () => {
        // Opening a section never computed computes it already.
        const loaded = stats.loaded;
        openPanel("stats");
        if (loaded) runStats(stats.year);
      },
    },
  ],
  sleeve: () => [{ label: "save your card…", run: saveSleeve }],
  import: () => [
    {
      label: "choose your Spotify data…",
      run: () => {
        openPanel("import");
        pickImport();
      },
    },
  ],
  providers: () => [
    { label: "check for new music", run: () => scan(false) },
    debugMode && { label: "scan only if changed", run: () => scan(true) },
    { label: "add a music folder…", run: () => $("btnAddFolder").click() },
  ],
  plugins: () => [
    {
      label: "show available services",
      run: () => {
        openPanel("plugins");
        pluginTabs.select($("tabCatalog"));
        refreshCatalog();
      },
    },
    { label: "update every service", run: updateAllPlugins },
    {
      label: "keys",
      run: () => {
        openPanel("plugins");
        pluginTabs.select($("tabSecrets"));
      },
    },
  ],
  diag: () => [{ label: "copy the last run's report", run: copyDiagReport }],
  settings: () => [
    {
      label: "look things up online",
      kind: "check",
      checked: onlineSwitch.on(),
      disabled: onlineSwitch.held(),
      run: () => onlineSwitch.toggle(),
    },
  ],
};

for (const button of document.querySelectorAll(".nav")) {
  const panel = button.dataset.panel;
  const shortcut = panelShortcut(panel);
  menus.set(button, () => ({
    title: button.querySelector(".nav-label").textContent,
    items: [
      panel === "now" && nowOpen
        ? { label: "close", key: "Esc", run: () => setNowOpen(false) }
        : { label: "open", key: shortcut, disabled: currentPanel === panel, run: () => openPanel(panel) },
      "-",
      ...(SECTION_ITEMS[panel]?.() ?? []),
    ],
  }));
}

menus.set($("player"), playerMenu);
menus.set($("nowCard"), playerMenu);
menus.set(lyricsFullView.querySelector(".lyrics-full-head"), playerMenu);
menus.set($("lyricsPanel"), lyricsMenu);
menus.set($("sleeveStage"), sleeveMenu);

/// A block of text meant to be taken elsewhere: copied whole.
function copyBlock(block, label) {
  menus.set(block, () =>
    block.textContent ? [{ label, run: () => copyText(block.textContent, "copied to the clipboard") }] : null,
  );
}

copyBlock($("diagResult"), "copy the report");
copyBlock($("diagRaw"), "copy the JSON");
copyBlock($("envBlock"), "copy the environment");
copyBlock($("importResult"), "copy the JSON");
copyBlock($("resolveResult"), "copy the JSON");
copyBlock($("themeDir"), "copy");

const TEXT_TYPES = new Set(["text", "search", "url", "password", "number", "email", "tel"]);

function isTextField(el) {
  return el.tagName === "TEXTAREA" || (el.tagName === "INPUT" && TEXT_TYPES.has(el.type));
}

/// What a text box's form does, offered beside the edits once there is
/// something typed.
const FIELD_ACTIONS = {
  playQuery: () => [
    { label: "play the first match", key: "Enter", run: () => $("playForm").requestSubmit() },
    { label: "queue every match", key: "Shift+Enter", run: () => startPlay(playQuery(), true) },
  ],
  searchQuery: () => [{ label: "search", key: "Enter", run: () => $("searchForm").requestSubmit() }],
};

/// A text box: the edits every text box has. A password is never copied
/// out of its box.
function fieldMenu(field) {
  // `null` where the box's type has no selection to read (a number).
  const start = field.selectionStart;
  const end = field.selectionEnd;
  const selected = start !== null && end !== null && end > start;
  const secret = field.type === "password";
  const locked = field.readOnly || field.disabled;
  return {
    owner: field,
    mark: false,
    items: [
      { label: "cut", key: "Ctrl+X", disabled: !selected || secret || locked, run: () => editField(field, "cut") },
      { label: "copy", key: "Ctrl+C", disabled: !selected || secret, run: () => editField(field, "copy") },
      { label: "paste", key: "Ctrl+V", disabled: locked, run: () => editField(field, "paste") },
      "-",
      {
        label: "select all",
        key: "Ctrl+A",
        disabled: !field.value,
        run: () => {
          field.focus();
          field.select();
        },
      },
      "-",
      ...(field.value.trim() ? (FIELD_ACTIONS[field.id]?.() ?? []) : []),
    ],
  };
}

/// Cut, copy and paste the way the box's own keys do them: the engine's
/// commands keep the box's undo history. Where the engine refuses one (it
/// guards the clipboard), the clipboard API does the same; a refusal there
/// is said, not swallowed.
async function editField(field, command) {
  field.focus({ preventScroll: true });
  if (document.execCommand(command)) return;
  try {
    if (command === "paste") {
      document.execCommand("insertText", false, await navigator.clipboard.readText());
    } else {
      await navigator.clipboard.writeText(field.value.slice(field.selectionStart, field.selectionEnd));
      if (command === "cut") document.execCommand("delete");
    }
  } catch (err) {
    clipboardRefused(command === "paste" ? "read" : "write to", err);
  }
}

/// The page's selected text — if the element under the pointer is part of
/// it.
function selectionAt(target) {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || !selection.containsNode(target, true)) return "";
  return selection.toString();
}

/// What the element under the pointer offers: a text box its edits; an
/// element the nearest menu of its own or its parents'; selected text a copy
/// on top. Nothing, where there is none of these.
function findMenu(target) {
  const field = target.closest?.("input, textarea");
  if (field && isTextField(field)) return fieldMenu(field);
  const selected = selectionAt(target);
  const copy = selected && {
    label: "copy",
    key: "Ctrl+C",
    run: () => document.execCommand("copy") || copyText(selected, "copied to the clipboard"),
  };
  for (let el = target; el && el !== document.body; el = el.parentElement) {
    const build = menus.get(el);
    if (!build) continue;
    const built = build(target);
    if (!built) break;
    const menu = Array.isArray(built) ? { items: built } : built;
    return { owner: el, ...menu, items: copy ? [copy, "-", ...menu.items] : menu.items };
  }
  return copy ? { mark: false, items: [copy] } : null;
}

contextMenus({ root: $("contextMenu"), find: findMenu, checkMark: () => icon("check") });

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
  // A suggestion's cover found while the list is up (D-080).
  if (report.artwork_ready?.length > 0 && suggestUp) paintSuggestCovers(report.artwork_ready);
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
  // The debug mode before anything is drawn: its details stay hidden until
  // it says so (D-083).
  paintDebug(await call("debug_mode"));
  watchMotionPreference();
  showBase("library", { instant: true });
  openPanel("now", { instant: true });

  environment = (await call("environment")) ?? null;
  if (environment) {
    $("appVersion").textContent = `headshell ${environment.version}`;
    paintEnvironment();
    // The switch from what the app goes by — read without a run of its own,
    // so the last run in diagnostics stays the user's. Its note comes when
    // settings open.
    $("onlineSwitch").disabled = environment.lookup_held;
    onlineSwitch.set(environment.lookup === "online", { instant: true });
  }
  renderQueue(await call("queue"));
  renderAnchor(await call("anchor"));
  refreshLyrics();

  // Where the play box searches first, and the providers' names — for its
  // placeholder and its suggestions (D-080). Read from disk; nothing goes
  // online. Before the plugins: `diag`'s last run stays "plugin list", as it
  // was before this was asked here.
  const providers = await call("providers");
  if (providers) takeProviders(providers);

  // The plugin badge in the sidebar: read from disk, nothing goes online.
  const plugins = await call("plugins");
  if (plugins) updatePluginBadge(plugins.summary);
})();
