// headshell — context menus (D-079).
//
// The engine's own menu never opens, in any build. It was not the app's
// menu: on an empty spot it offered "Back" and "Reload" — a reload rebuilt
// the interface and lost the open section, the search and the scroll — on a
// cover "Open Image in New Window", and in a debug build "Inspect Element".
// In its place the element under the pointer offers its own actions; an
// element with none offers nothing, and nothing opens.
//
// This file only shows a menu, places it and takes the choice. What is in a
// menu comes from where its element is drawn (`app.js`): a menu is a second
// way to reach what the interface already does, never a capability of its
// own (K1, D-033).
//
// It behaves the way the platform's menus do (GTK, macOS):
//
// - It opens where the button went down, grows out of the pointer and stays
//   inside the window — to the pointer's left, or above it, when there is no
//   room.
// - Pressing, moving onto a row and letting go picks the row in one gesture;
//   a plain click leaves the menu open for a second click.
// - The Menu key or Shift+F10 opens the focused element's menu under it.
//   The arrows move, Enter picks, Esc closes, a letter goes to the row that
//   starts with it. While a menu is open every key is the menu's: the space
//   must not pause the music behind it.
// - A press outside closes the menu and does nothing else; a right press
//   opens the next menu where it went down.
// - A row that cannot be undone asks first: the first pick turns it into
//   "really …?", the second does it.
//
// The top level is pure, as in `motion.js`: `tests/menu_js.rs` evaluates the
// placement, the release and the type-ahead rules in QuickJS.

import { animate, place } from "./motion.js";

/// How far the pointer must travel after the press that opened a menu for
/// the release to pick the row under it (px). Below this the release ends a
/// plain click, and the menu stays open.
export const PICK_DISTANCE = 5;

/// The free space kept between a menu and the window's edges (px).
export const EDGE_MARGIN = 8;

/// The menu's springs, as a share of the theme's response: quicker than a
/// sheet's — a menu is small and asked for often.
const OPEN_RESPONSE = 0.5;
const CLOSE_RESPONSE = 0.4;

/// The size a menu grows from: barely smaller than its own. Not a sheet
/// arriving, only a hint of where it came from.
const GROW_FROM = 0.96;

/// How long typed letters add up to one prefix (ms).
const TYPEAHEAD_MS = 700;

// ————————————————————————————————————— pure rules

/// Where a menu of `size` goes in the `viewport`, opened at `anchor`: to the
/// right of `x` and below `y`; to the left of `flipX`, or above `flipY`, when
/// there is no room. The pointer gives one point for both. An element opened
/// from the keyboard gives its bottom edge, and its top to flip to, so the
/// menu does not cover what it is about. A menu taller than the window is
/// kept at its top and scrolls (`style.css`).
///
/// `originX`/`originY` is where the anchor falls in the menu: the corner the
/// menu grows out of.
export function menuPlacement(anchor, size, viewport, margin = EDGE_MARGIN) {
  const x = span(anchor.x, anchor.flipX ?? anchor.x, size.width, viewport.width, margin);
  const y = span(anchor.y, anchor.flipY ?? anchor.y, size.height, viewport.height, margin);
  return { left: x.start, top: y.start, originX: x.origin, originY: y.origin };
}

function span(at, flipAt, extent, room, margin) {
  const fits = at + extent <= room - margin;
  const from = fits ? at : flipAt;
  const wanted = fits ? at : flipAt - extent;
  const start = Math.max(margin, Math.min(wanted, room - margin - extent));
  return { start, origin: Math.min(Math.max(from - start, 0), extent) };
}

/// What letting go of the button that opened the menu does. `moved`: how far
/// the pointer went since the press; `onRow`: the release is over a row that
/// can be picked; `inside`: over the menu at all.
///
/// Without moving it is the end of a plain click, wherever it lands: a menu
/// that opens with a row under a still pointer must not pick that row on its
/// own.
export function releaseOutcome(moved, onRow, inside) {
  if (moved < PICK_DISTANCE) return "stay";
  if (onRow) return "pick";
  return inside ? "stay" : "close";
}

/// The row a typed prefix goes to: the first one from the starting row on,
/// wrapping around, whose label starts with it — or -1. `labels` has `null`
/// for what cannot be picked. A single letter starts after the `current`
/// row, so pressing it again steps through the rows it starts; a longer
/// prefix starts at the current row, which may still match.
export function typeahead(labels, current, prefix) {
  const wanted = prefix.toLowerCase();
  const count = labels.length;
  const first = prefix.length > 1 ? Math.max(current, 0) : current + 1;
  for (let step = 0; step < count; step += 1) {
    const index = (first + step) % count;
    const label = labels[index];
    if (label !== null && label.toLowerCase().startsWith(wanted)) return index;
  }
  return -1;
}

/// The rows as they are drawn: `null` and `false` left out (so a list can
/// carry its conditions), no separator at either end or twice in a row.
/// Empty when there is nothing to show.
export function menuRows(items) {
  const rows = [];
  for (const item of items ?? []) {
    if (!item) continue;
    if (item === "-") {
      if (rows.length > 0 && rows[rows.length - 1] !== "-") rows.push("-");
    } else {
      rows.push(item);
    }
  }
  while (rows[rows.length - 1] === "-") rows.pop();
  return rows;
}

// ————————————————————————————————————— the menu

/// Sets up the context menus.
///
/// `root` is the page's single menu element, empty and hidden. `find(target)`
/// answers what the element under the pointer offers: `{ owner, title,
/// items, mark }`, or `null` for nothing — then nothing opens. `owner` is the
/// element the menu is about: the menu closes when it leaves the page (its
/// list was drawn again, so the rows may point at something else). `mark` is
/// where the ring goes while the menu is open — `owner` if not given, `false`
/// for none. `onClose` is called when the menu goes, however it goes.
/// `checkMark()` draws the tick of a checked row.
///
/// An item: `{ label, run, key, kind: "check" | "radio", checked, danger,
/// disabled, confirm }`. `key` is the shortcut written beside it; `confirm`
/// is the text of the second step before a row that cannot be undone. `"-"`
/// is a separator.
export function contextMenus({ root, find, checkMark }) {
  let menu = null;
  let lastPress = null;
  let keyboardAt = -Infinity;
  let swallowing = null;
  let typed = "";
  let typedAt = -Infinity;

  // The list the menu is about was drawn again: its rows may now point at
  // something else, so the menu goes.
  const watcher = new MutationObserver(() => {
    if (menu?.owner && !menu.owner.isConnected) close();
  });

  function open(target, pointer) {
    // The old menu goes before the new one is asked for: building a menu may
    // hold what it is about (a notice), and the old one's closing must not
    // let go of it afterwards.
    if (menu) close();
    const found = target ? find(target) : null;
    const rows = menuRows(found?.items);
    if (rows.length === 0) return;

    const entries = [];
    const nodes = [];
    const title = found.title ?? null;
    if (title) nodes.push(element("p", "menu-title", title, { "aria-hidden": "true" }));
    const ticks = rows.some((row) => row !== "-" && row.kind);
    for (const item of rows) {
      if (item === "-") {
        nodes.push(element("div", "menu-separator", null, { role: "separator" }));
        continue;
      }
      const row = element("div", item.danger ? "menu-item danger" : "menu-item", null, {
        id: `menu-row-${entries.length}`,
        role: item.kind === "check" ? "menuitemcheckbox" : item.kind === "radio" ? "menuitemradio" : "menuitem",
      });
      if (item.kind) row.setAttribute("aria-checked", String(Boolean(item.checked)));
      if (item.disabled) row.setAttribute("aria-disabled", "true");
      if (ticks) {
        const tick = element("span", "menu-check");
        if (item.checked) tick.append(checkMark());
        row.append(tick);
      }
      const label = element("span", "menu-label", item.label);
      row.append(label);
      if (item.key) row.append(element("span", "menu-key", item.key));
      entries.push({ item, row, label, armed: false });
      nodes.push(row);
    }
    root.replaceChildren(...nodes);
    root.setAttribute("aria-label", title ?? "actions");
    root.removeAttribute("aria-activedescendant");

    // Measured at its full size, at the window's corner — a menu left at the
    // right edge would measure narrower than it is.
    root.classList.remove("closing");
    root.hidden = false;
    root.style.left = "0px";
    root.style.top = "0px";
    root.scrollTop = 0;
    place(root, { opacity: 0, scale: 1 });
    const box = root.getBoundingClientRect();
    const spot = menuPlacement(
      pointer ?? keyboardAnchor(target),
      { width: box.width, height: box.height },
      { width: window.innerWidth, height: window.innerHeight },
    );
    root.style.left = `${spot.left}px`;
    root.style.top = `${spot.top}px`;
    root.style.transformOrigin = `${spot.originX}px ${spot.originY}px`;
    animate(root, { opacity: 1, scale: 1 }, { from: { opacity: 0, scale: GROW_FROM }, responseScale: OPEN_RESPONSE });

    const mark = found.mark === undefined ? found.owner : found.mark;
    menu = {
      entries,
      owner: found.owner ?? null,
      mark: mark || null,
      active: null,
      returnFocus: document.activeElement,
      held: pointer?.held ?? null,
      press: null,
      onClose: found.onClose ?? null,
    };
    menu.mark?.classList.add("menu-target");
    if (menu.owner) watcher.observe(document.body, { childList: true, subtree: true });
    typed = "";
    // From the keyboard the focus comes into the menu, onto its first row;
    // from the pointer it stays where it was — a text box keeps its
    // selection in sight, and nothing is lit before the pointer moves.
    if (!pointer) {
      root.focus({ preventScroll: true });
      move(1);
    }
  }

  function close() {
    if (!menu) return;
    const closing = menu;
    menu = null;
    watcher.disconnect();
    closing.mark?.classList.remove("menu-target");
    closing.onClose?.();
    root.classList.add("closing");
    root.removeAttribute("aria-activedescendant");
    if (root.contains(document.activeElement)) {
      const back = closing.returnFocus;
      if (back && back !== document.body && back.isConnected) back.focus({ preventScroll: true });
      else root.blur();
    }
    animate(
      root,
      { opacity: 0 },
      {
        responseScale: CLOSE_RESPONSE,
        onRest: () => {
          root.hidden = true;
          root.classList.remove("closing");
          root.replaceChildren();
        },
      },
    );
  }

  /// Runs a row. The menu goes first: the focus is back where the action
  /// expects it (a text box to paste into), and what the action opens is not
  /// covered.
  function pick(entry) {
    if (!menu || !entry || entry.item.disabled) return;
    if (entry.item.confirm && !entry.armed) {
      entry.armed = true;
      entry.label.textContent = entry.item.confirm;
      entry.row.classList.add("armed");
      light(entry);
      return;
    }
    close();
    entry.item.run?.();
  }

  function light(entry, { reveal = false } = {}) {
    if (!menu || menu.active === entry) return;
    const left = menu.active;
    left?.row.classList.remove("active");
    // Leaving a row that asks "really …?" takes the question back: the
    // second step belongs to the row the hand is still on, and a single row
    // is lit at a time.
    if (left?.armed) {
      left.armed = false;
      left.label.textContent = left.item.label;
      left.row.classList.remove("armed");
    }
    menu.active = entry;
    if (!entry) {
      root.removeAttribute("aria-activedescendant");
      return;
    }
    entry.row.classList.add("active");
    root.setAttribute("aria-activedescendant", entry.row.id);
    if (reveal) entry.row.scrollIntoView({ block: "nearest" });
  }

  function pickable() {
    return menu.entries.filter((entry) => !entry.item.disabled);
  }

  function move(delta) {
    const rows = pickable();
    if (rows.length === 0) return;
    const at = rows.indexOf(menu.active);
    const next = at === -1 ? (delta > 0 ? 0 : rows.length - 1) : (at + delta + rows.length) % rows.length;
    light(rows[next], { reveal: true });
  }

  function moveToEnd(last) {
    const rows = pickable();
    if (rows.length > 0) light(rows[last ? rows.length - 1 : 0], { reveal: true });
  }

  function typeTo(letter, time) {
    typed = time - typedAt < TYPEAHEAD_MS ? typed + letter : letter;
    typedAt = time;
    const labels = menu.entries.map((entry) => (entry.item.disabled ? null : entry.label.textContent));
    const index = typeahead(labels, menu.entries.indexOf(menu.active), typed);
    if (index !== -1) light(menu.entries[index], { reveal: true });
  }

  function entryAt(target) {
    const row = target?.closest?.(".menu-item");
    return (row && menu?.entries.find((entry) => entry.row === row)) || null;
  }

  // ———————————————————————————— the engine's menu, the keys

  // The engine's menu never opens (D-079). A right click on the menu itself
  // leaves it as it is.
  document.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    if (root.contains(event.target)) return;
    // The key already opened this one (below); the engine's own event for the
    // same key may come after it.
    if (event.timeStamp - keyboardAt < 500) return;
    // A press just before it, at the same spot, is the pointer's. Otherwise
    // the engine sent it for a key: the menu goes under the focused element.
    const press = lastPress;
    const fromPointer =
      press !== null &&
      event.timeStamp - press.time < 1000 &&
      Math.abs(press.x - event.clientX) <= 2 &&
      Math.abs(press.y - event.clientY) <= 2;
    open(
      fromPointer ? event.target : (document.activeElement ?? event.target),
      fromPointer ? { x: event.clientX, y: event.clientY, held: press.down ? press : null } : null,
    );
  });

  window.addEventListener(
    "keydown",
    (event) => {
      if (menu) {
        onMenuKey(event);
      } else if (event.key === "ContextMenu" || (event.key === "F10" && event.shiftKey)) {
        event.preventDefault();
        event.stopPropagation();
        keyboardAt = event.timeStamp;
        open(document.activeElement, null);
      }
    },
    { capture: true },
  );

  function onMenuKey(event) {
    // Every key is the menu's while it is open: the space must not pause the
    // music behind it, a letter must not land in the text box under it.
    event.preventDefault();
    event.stopPropagation();
    const key = event.key;
    if (key === "Escape" || key === "Tab" || key === "ContextMenu" || (key === "F10" && event.shiftKey)) {
      close();
    } else if (key === "ArrowDown") {
      move(1);
    } else if (key === "ArrowUp") {
      move(-1);
    } else if (key === "Home" || key === "PageUp") {
      moveToEnd(false);
    } else if (key === "End" || key === "PageDown") {
      moveToEnd(true);
    } else if (key === "Enter" || key === " ") {
      pick(menu.active);
    } else if (key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      typeTo(key, event.timeStamp);
    }
  }

  // ———————————————————————————— the pointer

  window.addEventListener(
    "pointerdown",
    (event) => {
      lastPress = { x: event.clientX, y: event.clientY, time: event.timeStamp, button: event.button, down: true };
      // A new press is a new gesture: a trap left by one whose release never
      // came (let go outside the window) must not eat it.
      swallowing = null;
      if (!menu) return;
      if (root.contains(event.target)) {
        // A press on the menu picks where it is let go.
        menu.press = event.button;
        return;
      }
      // A press outside closes the menu and does nothing else — except the
      // right one, which opens the next menu where it went down.
      close();
      if (event.button === 2) return;
      swallowing = event.pointerId;
      event.preventDefault();
      event.stopPropagation();
    },
    { capture: true },
  );

  // A right press asks what an element offers; it does not give the element
  // the focus. A ring left on a button once its menu has gone — the focus
  // ring, which shows as soon as a key is pressed — reads as the menu's own
  // ring stuck there. A text box is the exception: its menu's edits are for
  // it, so it takes the focus as it always did.
  window.addEventListener(
    "mousedown",
    (event) => {
      if (event.button === 2 && !event.target.closest?.("input, textarea")) event.preventDefault();
    },
    { capture: true },
  );

  // The engine's mouse events that follow the swallowed press.
  for (const type of ["mousedown", "mouseup"]) {
    window.addEventListener(
      type,
      (event) => {
        if (swallowing === null) return;
        event.preventDefault();
        event.stopPropagation();
      },
      { capture: true },
    );
  }

  window.addEventListener(
    "pointerup",
    (event) => {
      if (lastPress?.button === event.button) lastPress.down = false;
      if (swallowing === event.pointerId) {
        event.preventDefault();
        event.stopPropagation();
        swallowNextClick();
        // The engine's `mouseup` comes after this event: it is swallowed too,
        // and the trap goes when this input event is over.
        setTimeout(() => (swallowing = null), 0);
        return;
      }
      if (!menu) return;
      const entry = entryAt(event.target);
      if (menu.press !== null) {
        menu.press = null;
        pick(entry);
        return;
      }
      const held = menu.held;
      if (held && event.button === held.button) {
        menu.held = null;
        const moved = Math.hypot(event.clientX - held.x, event.clientY - held.y);
        const outcome = releaseOutcome(moved, Boolean(entry && !entry.item.disabled), root.contains(event.target));
        if (outcome === "pick") pick(entry);
        else if (outcome === "close") close();
      }
    },
    { capture: true },
  );

  window.addEventListener(
    "pointercancel",
    (event) => {
      if (lastPress?.button === event.button) lastPress.down = false;
      if (swallowing === event.pointerId) swallowing = null;
      if (menu) {
        menu.press = null;
        menu.held = null;
      }
    },
    { capture: true },
  );

  // Nothing is lit before the pointer moves over a row; off the rows,
  // nothing is.
  root.addEventListener("pointermove", (event) => {
    if (!menu) return;
    const entry = entryAt(event.target);
    light(entry && !entry.item.disabled ? entry : null);
  });
  root.addEventListener("pointerleave", () => light(null));
  // A press on the menu neither moves the focus nor begins a selection: a
  // text box under it keeps both.
  root.addEventListener("mousedown", (event) => event.preventDefault());

  // The world under the menu moved: it goes.
  window.addEventListener("blur", close);
  window.addEventListener("resize", close);
  window.addEventListener(
    "wheel",
    (event) => {
      if (menu && !root.contains(event.target)) close();
    },
    { capture: true, passive: true },
  );

  return { close };
}

/// Under the focused element, at its start; above it when there is no room.
function keyboardAnchor(target) {
  const el = target instanceof Element ? target : document.body;
  const rect = el.getBoundingClientRect();
  return { x: rect.left + Math.min(16, rect.width / 2), y: rect.bottom + 2, flipY: rect.top - 2 };
}

/// A small element builder: text only ever goes to `textContent`.
function element(tag, className, text, attributes = {}) {
  const el = document.createElement(tag);
  el.className = className;
  if (text !== null && text !== undefined) el.textContent = text;
  for (const [name, value] of Object.entries(attributes)) el.setAttribute(name, value);
  return el;
}

/// The click that ends a press outside the menu is not a click: that press
/// only closed the menu. The trap lives until the current input event is
/// over, so it cannot eat a later, real click (the same as `motion.js`).
function swallowNextClick() {
  const swallow = (event) => {
    event.stopPropagation();
    event.preventDefault();
  };
  globalThis.addEventListener("click", swallow, { capture: true, once: true });
  setTimeout(() => globalThis.removeEventListener("click", swallow, { capture: true }), 0);
}
