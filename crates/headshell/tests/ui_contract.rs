//! The interface's internal links and the §3.3 class contract.
//!
//! The webview has no type checking: `$("playQuery")` returns `null` on a
//! typo, `null.addEventListener` blows up at startup and **the window stays
//! empty**. The compiler does not see this, and neither would `cargo test` —
//! this file does.
//!
//! Three links are locked down:
//!
//! 1. Every `id` the scripts (`app.js`, `motion.js`) look up is defined in
//!    `index.html`.
//! 2. Every `data-panel` in the sidebar lands on a `panel-*` section and is in
//!    the same set as the `PANELS` list in `app.js` — so the shortcut
//!    (`Ctrl`+`1`…`9`) does not go to the wrong panel.
//! 3. D-037/2's **class contract**: the class names themes target are still
//!    reachable — as a selector or in the markup. The README says "they are
//!    not renamed without notice"; this test holds the "without notice".
//!
//! Also a dead token check: every `--headshell-*` token declared in `:root`
//! must be used at least once. An unused token makes the theme author a
//! promise that is not kept.
//!
//! D-072 added three more silent breakages: an undefined icon draws an empty
//! square, CSP (`style-src 'self'`) ignores an inline `style` attribute
//! without saying anything, and a description from the catalog (D-071,
//! remote data) written as markup could smuggle a script into a page with
//! IPC access.
//!
//! D-078 added the plainest one: a script that does not parse. The webview
//! drops the whole module — and every module that imports it — and the
//! window stays empty; every other test here reads the scripts as text and
//! would stay green. They are parsed with the embedded QuickJS.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

const APP_JS: &str = include_str!("../ui/app.js");
const MOTION_JS: &str = include_str!("../ui/motion.js");
const ANCHOR_JS: &str = include_str!("../ui/anchor.js");
const LYRICS_JS: &str = include_str!("../ui/lyrics.js");
const MENU_JS: &str = include_str!("../ui/menu.js");
const VIDEO_JS: &str = include_str!("../ui/video.js");
const INDEX_HTML: &str = include_str!("../ui/index.html");
const STYLE_CSS: &str = include_str!("../ui/style.css");
const STARTUP_ERROR_HTML: &str = include_str!("../ui/startup-error.html");
const STARTUP_ERROR_JS: &str = include_str!("../ui/startup-error.js");
/// The core's stages: a notice names its stage in plain words (D-083).
const DIAG_RS: &str = include_str!("../../headshell-core/src/diag/mod.rs");

/// The class names `api: 1` promises.
///
/// The README gave this list as an example: "`.topbar`, `.player`, `.queue`,
/// `.toast`, …"; an ellipsis cannot be tested, this list can. Removing a name
/// from here requires bumping `THEME_API` (see `src/theme.rs`).
const CONTRACT_CLASSES: &[&str] = &[
    "topbar",
    "brand",
    "play-form",
    "busy",
    "sidebar",
    "nav",
    "content",
    "panel",
    "queue",
    "index",
    "current",
    "empty",
    "hint",
    "row",
    "grid",
    "check",
    "table",
    "num",
    "summary",
    "big",
    "label",
    "ranking",
    "two-col",
    "block",
    "themes",
    "theme",
    "name",
    "author",
    "tag",
    "warn",
    "rejected",
    "id",
    "dropzone",
    "over",
    "player",
    "controls",
    "on",
    "now",
    "title",
    "state",
    "bar",
    "fill",
    "time",
    "toasts",
    "toast",
    "stage",
    "info",
];

/// Collects, for every match after `needle` in `haystack`, the piece up to
/// the closing character. A small scanner: not worth adding a dependency for.
fn collect_between(haystack: &str, open: &str, close: char) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = haystack;
    while let Some(start) = rest.find(open) {
        rest = &rest[start + open.len()..];
        match rest.find(close) {
            Some(end) => {
                found.push(rest[..end].to_owned());
                rest = &rest[end..];
            }
            None => break,
        }
    }
    found
}

#[test]
fn every_id_the_script_looks_up_exists_in_the_page() {
    let wanted: Vec<String> = [APP_JS, MOTION_JS, MENU_JS]
        .iter()
        .flat_map(|script| collect_between(script, "$(\"", '"'))
        .collect();
    assert!(
        wanted.len() > 30,
        "the id scan came up empty: {}",
        wanted.len()
    );

    let mut missing: Vec<String> = wanted
        .into_iter()
        .filter(|id| !INDEX_HTML.contains(&format!("id=\"{id}\"")))
        .collect();
    missing.sort();
    missing.dedup();

    assert!(
        missing.is_empty(),
        "app.js looks up ids that do not exist (the window stays empty at startup): {missing:?}"
    );
}

#[test]
fn the_sidebar_the_sections_and_the_shortcut_list_name_the_same_panels() {
    let mut from_sidebar = collect_between(INDEX_HTML, "data-panel=\"", '"');
    from_sidebar.sort();

    let list = APP_JS
        .split_once("const PANELS = [")
        .expect("no PANELS list in app.js")
        .1
        .split_once(']')
        .expect("the PANELS list does not close")
        .0;
    let mut from_script: Vec<String> = collect_between(list, "\"", '"')
        .into_iter()
        .filter(|item| !item.trim().is_empty() && item != ", ")
        .collect();
    from_script.sort();

    assert_eq!(
        from_sidebar, from_script,
        "the panel lists of the sidebar and the Ctrl+number shortcut have parted ways"
    );

    for panel in &from_sidebar {
        assert!(
            INDEX_HTML.contains(&format!("id=\"panel-{panel}\"")),
            "the `{panel}` tab has no section — the tab leads to empty content"
        );
    }
}

#[test]
fn the_class_names_themes_target_are_still_reachable() {
    // The criterion is **not** "does it have a rule in CSS": `.panel` carries no
    // rule, but it is in the DOM and a theme can target it. The contract is that
    // the class is *reachable* — either as a selector or in the markup.
    let markup_classes = class_attribute_words(INDEX_HTML);
    let mut missing = Vec::new();
    for class in CONTRACT_CLASSES {
        let reachable =
            defines_class(STYLE_CSS, class) || markup_classes.iter().any(|w| w == class);
        if !reachable {
            missing.push(*class);
        }
    }
    assert!(
        missing.is_empty(),
        "a class in the contract is no longer a selector or in the markup — themes target these (D-037/2): {missing:?}"
    );
}

/// The words of every `class="…"` attribute in `index.html`.
fn class_attribute_words(html: &str) -> Vec<String> {
    collect_between(html, "class=\"", '"')
        .iter()
        .flat_map(|value| {
            value
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Is there a `.foo` selector. So `.foobar` does not match by mistake, the
/// character after the class name must not be a name character.
fn defines_class(css: &str, class: &str) -> bool {
    let needle = format!(".{class}");
    let mut rest = css;
    while let Some(at) = rest.find(&needle) {
        let after = &rest[at + needle.len()..];
        let next = after.chars().next();
        if !matches!(next, Some(c) if c.is_alphanumeric() || c == '-' || c == '_') {
            return true;
        }
        rest = after;
    }
    false
}

/// The `hidden` attribute must not lose to a class rule.
///
/// This test was written after a bug: `.sheet { display: flex }` overrode the
/// UA stylesheet's `[hidden] { display: none }` rule and the shortcut sheet
/// came up open on every start. The markup was right, the JavaScript was
/// right, only the specificity was wrong — a behaviour test would not have
/// seen it either.
#[test]
fn the_hidden_attribute_outranks_layout_rules() {
    // A match at the start of a line is looked for: this file's own comment
    // also contains the text `[hidden] { display: none }`, and the first
    // version of the test tripped over exactly that.
    let rule = STYLE_CSS
        .split_once("\n[hidden] {")
        .expect(
            "no `[hidden]` rule: a class that defines `display` leaves a `hidden` element on screen",
        )
        .1
        .split_once('}')
        .expect("the `[hidden]` rule does not close")
        .0;
    assert!(
        rule.contains("display: none") && rule.contains("!important"),
        "there is a `[hidden]` rule but it does not beat the classes: {rule:?}"
    );
}

#[test]
fn no_declared_token_is_dead() {
    let root = STYLE_CSS
        .split_once(":root {")
        .expect("no :root block in style.css")
        .1
        .split_once("\n}")
        .expect("the :root block does not close")
        .0;

    let declared: Vec<&str> = root
        .lines()
        .filter_map(|line| line.trim().strip_prefix("--"))
        .filter_map(|line| line.split(':').next())
        .map(str::trim)
        .collect();
    assert!(
        declared.len() >= 14,
        "the token set has shrunk: {declared:?}"
    );

    for token in declared {
        let uses = STYLE_CSS.matches(&format!("var(--{token})")).count();
        assert!(
            uses > 0,
            "`--{token}` is declared but never used — a promise to the theme author that is not kept"
        );
    }
}

/// The startup error window (D-070) is a page of its own: the element the
/// script writes to must be there, otherwise the user sees an **empty** window
/// again — the exact opposite of why this page exists.
#[test]
fn the_startup_error_page_has_the_element_its_script_writes_to() {
    assert!(
        STARTUP_ERROR_HTML.contains("src=\"startup-error.js\""),
        "the page does not load its script"
    );
    let ids = collect_between(STARTUP_ERROR_JS, "getElementById(\"", '"');
    assert!(!ids.is_empty(), "the script writes to no element");
    for id in ids {
        assert!(
            STARTUP_ERROR_HTML.contains(&format!("id=\"{id}\"")),
            "startup-error.js looks up `{id}`, which is not on the page"
        );
    }
    // The text must not be interpreted as markup: the error chain can carry
    // user data (a file path, a server message).
    let code_uses_inner_html = STARTUP_ERROR_JS
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .any(|line| line.contains("innerHTML"));
    assert!(
        !code_uses_inner_html,
        "the error text is written with `innerHTML`"
    );
}

/// Skips comment lines: a comment can **describe** a prohibition or a pattern
/// ("not `innerHTML`"), and that does not mean the code uses it.
fn code_lines(source: &str) -> impl Iterator<Item = &str> {
    source.lines().filter(|line| {
        let line = line.trim_start();
        !(line.starts_with("//") || line.starts_with('*') || line.starts_with("/*"))
    })
}

/// Every icon used must be defined in the set (D-072).
///
/// `<use href="#i-missing">` gives no error, it draws an empty square: the
/// play button becomes invisible and no test, no console says so.
#[test]
fn every_icon_the_page_draws_is_in_the_sprite() {
    let mut used = collect_between(INDEX_HTML, "href=\"#i-", '"');
    for line in code_lines(APP_JS) {
        used.extend(collect_between(line, "icon(\"", '"'));
        used.extend(collect_between(line, "iconRef(\"", '"'));
    }
    used.sort();
    used.dedup();
    assert!(used.len() > 15, "the icon scan came up empty: {used:?}");

    let missing: Vec<&String> = used
        .iter()
        .filter(|name| !INDEX_HTML.contains(&format!("<symbol id=\"i-{name}\"")))
        .collect();
    assert!(
        missing.is_empty(),
        "icons not in the set are used (an empty square is drawn): {missing:?}"
    );
}

/// No script writes a string as markup; text goes through `textContent`.
///
/// Part of the data the interface shows is not even produced on this machine:
/// the descriptions and plugin names in the catalog come from a remote index
/// (D-071). A description written with `innerHTML` would smuggle markup into a
/// page that has IPC access — that is, to secrets, plugin consent, writing
/// files. The inline `style` attribute is here too: CSP ignores it silently.
#[test]
fn no_script_writes_markup_or_inline_style() {
    const FORBIDDEN: &[&str] = &[
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "setAttribute(\"style\"",
    ];
    for (name, source) in [
        ("app.js", APP_JS),
        ("motion.js", MOTION_JS),
        ("anchor.js", ANCHOR_JS),
        ("lyrics.js", LYRICS_JS),
        ("menu.js", MENU_JS),
        ("video.js", VIDEO_JS),
        ("startup-error.js", STARTUP_ERROR_JS),
    ] {
        for line in code_lines(source) {
            for pattern in FORBIDDEN {
                assert!(
                    !line.contains(pattern),
                    "{name} uses `{pattern}`: {}",
                    line.trim()
                );
            }
        }
    }
}

/// CSP `style-src 'self'` does not apply an inline `style="…"` attribute and
/// tells nobody: the element stays unstyled. Dynamic values (the progress
/// bar, the theme preview) are written with `el.style` — the CSSOM does not
/// trip over the CSP.
#[test]
fn the_pages_carry_no_inline_style_attribute() {
    for (name, html) in [
        ("index.html", INDEX_HTML),
        ("startup-error.html", STARTUP_ERROR_HTML),
    ] {
        assert!(
            !html.contains(" style=\""),
            "{name} carries an inline `style` attribute; CSP ignores it"
        );
    }
}

/// Every module the page loads parses (D-078), so a syntax error is caught
/// here, where the webview would only leave the window empty. Declaring
/// compiles a module and finds what it imports among the modules already
/// declared — so they go first, in the order the page's imports need them;
/// nothing is evaluated.
#[test]
fn every_script_the_page_loads_parses() {
    let runtime = rquickjs::Runtime::new().unwrap();
    let context = rquickjs::Context::full(&runtime).unwrap();
    context.with(|ctx| {
        for (name, source) in [
            ("anchor.js", ANCHOR_JS),
            ("lyrics.js", LYRICS_JS),
            ("motion.js", MOTION_JS),
            ("menu.js", MENU_JS),
            ("video.js", VIDEO_JS),
            ("app.js", APP_JS),
        ] {
            if let Err(err) = rquickjs::Module::declare(ctx.clone(), name, source) {
                panic!(
                    "{name} does not parse: {}",
                    rquickjs::CaughtError::from_error(&ctx, err)
                );
            }
        }
    });
}

/// Every stage the core can fail in has plain words in the interface
/// (D-083): a notice's headline is its stage, and a stage added to the core
/// without words would fall to "Something went wrong" — saying no stage at
/// all, which K9 forbids.
#[test]
fn every_core_stage_has_plain_words_in_the_notices() {
    let names = DIAG_RS
        .split_once("pub const fn as_str(self)")
        .expect("no Stage::as_str in diag/mod.rs")
        .1
        .split_once("\n}\n")
        .expect("Stage::as_str does not close")
        .0;
    let stages: Vec<String> = collect_between(names, "=> \"", '"');
    assert!(
        stages.len() >= 20,
        "the stage names were not found: {stages:?}"
    );

    let words = APP_JS
        .split_once("const STAGE_TEXT = {")
        .expect("no STAGE_TEXT in app.js")
        .1
        .split_once("};")
        .expect("STAGE_TEXT does not close")
        .0;
    let missing: Vec<&String> = stages
        .iter()
        .filter(|stage| !words.contains(&format!("{stage}:")))
        .collect();
    assert!(
        missing.is_empty(),
        "stages without plain words in STAGE_TEXT: {missing:?}"
    );
}

/// What the debug mode hides is hidden by one rule (D-083), and the rule
/// outranks the layout's: a `.debug-only` row with `display: flex` of its own
/// must not show without the mode.
#[test]
fn the_debug_mode_hides_its_details_with_one_rule() {
    let rule = STYLE_CSS
        .split_once(":root:not([data-debug]) .debug-only")
        .expect("no debug-only rule in style.css")
        .1
        .split_once('}')
        .expect("the rule does not close")
        .0;
    assert!(rule.contains("display: none !important"), "{rule}");
    assert!(
        INDEX_HTML.contains("class=\"nav debug-only\" data-panel=\"diag\""),
        "diagnostics' row must be the debug mode's"
    );
}
