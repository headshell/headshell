// The interface's words (D-089).
//
// Every text the window shows comes from the language bundle the core gives
// (`locale_bundle`): the language's own texts, and English whole for every
// key the language leaves out. This file holds no words of its own.
//
// - `t(key, args)`: a text. `{name}` is filled from `args`; the plural form
//   is picked by `args.count` with the webview's own `Intl.PluralRules`,
//   which knows every language's rules.
// - `diag(key, args)`: a message a developer may be sent — the user's
//   language, " ‖ ", English; English once when the two are the same.
// - Static markup names its keys: `data-i18n="key"` for an element's text,
//   `data-i18n-attr="placeholder=key; aria-label=key"` for its attributes.
//   `translatePage(root)` fills them, and does again when the language
//   changes.
//
// A key the bundle doesn't have shows as `⟦key⟧` — visible, as the core's
// translator does, so a gap is seen rather than silently blank.

const SEPARATOR = " ‖ ";

let bundle = { code: "en", name: "English", strings: {}, english: {} };
let rules = pluralRules("en");

function pluralRules(code) {
  try {
    return new Intl.PluralRules(code);
  } catch {
    // A code the webview doesn't know counts like English rather than
    // breaking every plural on the page.
    return new Intl.PluralRules("en");
  }
}

/// Takes the core's bundle as the page's words.
export function useWords(next) {
  if (!next) return;
  bundle = next;
  rules = pluralRules(next.code);
  document.documentElement.lang = next.code;
}

/// The code the page speaks: `en`, `tr`.
export function speaking() {
  return bundle.code;
}

function render(text, code, args) {
  let template = text;
  if (text !== null && typeof text === "object") {
    const count = Number(args.count ?? 0);
    const category = (code === bundle.code ? rules : pluralRules(code)).select(count);
    template = text[category] ?? text.other ?? Object.values(text)[0] ?? "";
  }
  return String(template).replace(/\{([A-Za-z0-9_]+)\}/g, (whole, name) =>
    Object.hasOwn(args, name) ? String(args[name]) : whole,
  );
}

/// Whether the page's language has its own words for `key` — a key it
/// leaves out is said in English.
export function hasOwn(key) {
  return Object.hasOwn(bundle.strings, key);
}

/// The text for `key` in the page's language; English where it has none.
export function t(key, args = {}) {
  return hasOwn(key) ? render(bundle.strings[key], bundle.code, args) : english(key, args);
}

/// The text for `key` in English.
export function english(key, args = {}) {
  const text = bundle.english[key];
  return text === undefined ? `⟦${key}⟧` : render(text, "en", args);
}

/// A diagnostic message: both voices, or English once.
export function diag(key, args = {}) {
  const theirs = t(key, args);
  const ours = english(key, args);
  return theirs === ours ? ours : `${theirs}${SEPARATOR}${ours}`;
}

/// Numbers and percentages the way the page's language writes them.
export function numberFormat(options = {}) {
  try {
    return new Intl.NumberFormat(bundle.code, options);
  } catch {
    return new Intl.NumberFormat("en", options);
  }
}

/// Fills the keyed markup under `root`. An element whose text fell back to
/// English is marked `lang="en"`: the page's language rules (a Turkish
/// uppercase, hyphenation) must not apply to English words.
export function translatePage(root = document) {
  for (const element of root.querySelectorAll("[data-i18n]")) {
    const key = element.dataset.i18n;
    element.textContent = t(key);
    if (hasOwn(key) || bundle.code === "en") element.removeAttribute("lang");
    else element.setAttribute("lang", "en");
  }
  for (const element of root.querySelectorAll("[data-i18n-attr]")) {
    for (const pair of element.dataset.i18nAttr.split(";")) {
      const [attribute, key] = pair.split("=").map((part) => part.trim());
      if (attribute && key) element.setAttribute(attribute, t(key));
    }
  }
}
