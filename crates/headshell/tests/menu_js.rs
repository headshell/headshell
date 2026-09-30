//! The rules of the context menus (`ui/menu.js`, D-079).
//!
//! A menu placed one pixel off the window is cut off, and one that picks the
//! row under a still pointer runs an action nobody chose — with "remove"
//! among the rows. Neither shows up anywhere but on the screen, so the rules
//! that decide them are kept pure and tested here, in the embedded QuickJS
//! (D-070), the same way as `motion_js.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use rquickjs::{CaughtError, Context, Ctx, Module, Runtime, Value};
use serde_json::{Value as Json, json};

fn source(name: &str) -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ui")
            .join(name),
    )
    .unwrap_or_else(|err| panic!("STEP: MENU_JS — could not read ui/{name}: {err}"))
}

/// Evaluates `menu.js` (after `motion.js`, which it imports) and puts its
/// pure rules on the global object for [`js`].
fn with_menu<R>(check: impl for<'js> FnOnce(&Ctx<'js>) -> R) -> R {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        if let Err(err) = Module::declare(ctx.clone(), "motion.js", source("motion.js")) {
            panic!(
                "STEP: MENU_JS — motion.js does not declare: {}",
                CaughtError::from_error(&ctx, err)
            );
        }
        let evaluated = Module::declare(ctx.clone(), "menu.js", source("menu.js"))
            .and_then(|module| module.eval())
            .and_then(|(module, promise)| promise.finish::<()>().map(|()| module));
        let module = match evaluated {
            Ok(module) => module,
            Err(err) => panic!(
                "STEP: MENU_JS — could not evaluate menu.js (does it touch the DOM at the top level?): {}",
                CaughtError::from_error(&ctx, err)
            ),
        };
        for name in [
            "menuPlacement",
            "releaseOutcome",
            "typeahead",
            "menuRows",
            "PICK_DISTANCE",
            "EDGE_MARGIN",
        ] {
            let value: Value = module
                .get(name)
                .unwrap_or_else(|_| panic!("menu.js does not export `{name}`"));
            ctx.globals().set(name, value).unwrap();
        }
        check(&ctx)
    })
}

/// The value of a JS expression, through JSON.
fn js(ctx: &Ctx<'_>, expression: &str) -> Json {
    let text: String = match ctx.eval(format!("JSON.stringify({expression})")) {
        Ok(text) => text,
        Err(err) => panic!(
            "`{expression}` failed: {}",
            CaughtError::from_error(ctx, err)
        ),
    };
    serde_json::from_str(&text).unwrap()
}

const WINDOW: &str = "{ width: 1180, height: 780 }";
const MENU: &str = "{ width: 220, height: 300 }";

fn place(ctx: &Ctx<'_>, anchor: &str) -> Json {
    js(ctx, &format!("menuPlacement({anchor}, {MENU}, {WINDOW})"))
}

#[test]
fn a_menu_opens_to_the_right_of_the_pointer_and_below_it() {
    with_menu(|ctx| {
        assert_eq!(
            place(ctx, "{ x: 100, y: 120 }"),
            json!({ "left": 100, "top": 120, "originX": 0, "originY": 0 }),
            "with room, the menu's corner is the pointer — it grows from there"
        );
    });
}

#[test]
fn without_room_it_turns_to_the_pointers_left_or_above_it() {
    with_menu(|ctx| {
        // At the right edge its right side is the pointer; at the bottom its
        // bottom is — it grows out of the corner the pointer is on.
        assert_eq!(
            place(ctx, "{ x: 1100, y: 120 }"),
            json!({ "left": 880, "top": 120, "originX": 220, "originY": 0 })
        );
        assert_eq!(
            place(ctx, "{ x: 100, y: 700 }"),
            json!({ "left": 100, "top": 400, "originX": 0, "originY": 300 })
        );
        // A pointer inside the edge's margin still leaves the margin free:
        // the menu stops 8 px short of the edge, and grows from its corner.
        assert_eq!(
            place(ctx, "{ x: 1175, y: 775 }"),
            json!({ "left": 952, "top": 472, "originX": 220, "originY": 300 })
        );
    });
}

#[test]
fn from_the_keyboard_it_flips_above_the_element_not_onto_it() {
    with_menu(|ctx| {
        // An element near the bottom: below it there is no room, so the menu
        // goes above its top edge — not above the bottom edge, which would
        // cover the element the menu is about.
        assert_eq!(
            place(ctx, "{ x: 40, y: 760, flipY: 720 }"),
            json!({ "left": 40, "top": 420, "originX": 0, "originY": 300 })
        );
    });
}

#[test]
fn a_menu_taller_than_the_window_is_kept_at_its_top() {
    with_menu(|ctx| {
        // `style.css` caps the height at the window less the margins; the
        // rest scrolls. It must not go above the window's top.
        assert_eq!(
            js(
                ctx,
                &format!(
                    "menuPlacement({{ x: 100, y: 400 }}, {{ width: 220, height: 764 }}, {WINDOW})"
                )
            ),
            json!({ "left": 100, "top": 8, "originX": 0, "originY": 392 })
        );
    });
}

#[test]
fn no_opening_point_puts_a_menu_outside_the_window() {
    with_menu(|ctx| {
        let margin = js(ctx, "EDGE_MARGIN").as_f64().unwrap();
        for x in (0..=1180).step_by(59) {
            for y in (0..=780).step_by(39) {
                let spot = place(ctx, &format!("{{ x: {x}, y: {y} }}"));
                let left = spot["left"].as_f64().unwrap();
                let top = spot["top"].as_f64().unwrap();
                assert!(
                    left >= margin && left + 220.0 <= 1180.0 - margin,
                    "opened at {x},{y}: the menu leaves the window sideways ({spot})"
                );
                assert!(
                    top >= margin && top + 300.0 <= 780.0 - margin,
                    "opened at {x},{y}: the menu leaves the window up or down ({spot})"
                );
                let origin_x = spot["originX"].as_f64().unwrap();
                let origin_y = spot["originY"].as_f64().unwrap();
                assert!(
                    (0.0..=220.0).contains(&origin_x) && (0.0..=300.0).contains(&origin_y),
                    "opened at {x},{y}: it grows from outside itself ({spot})"
                );
            }
        }
    });
}

#[test]
fn a_release_picks_only_after_the_pointer_moved() {
    with_menu(|ctx| {
        let distance = js(ctx, "PICK_DISTANCE").as_f64().unwrap();
        let outcome = |moved: f64, on_row: bool, inside: bool| {
            js(ctx, &format!("releaseOutcome({moved}, {on_row}, {inside})"))
        };
        // A plain right click: the press opened the menu, the release is the
        // end of the same click — even if a row happens to be under the still
        // pointer, nothing may run on its own.
        assert_eq!(outcome(0.0, true, true), json!("stay"));
        assert_eq!(outcome(distance - 0.1, true, true), json!("stay"));
        // Pressed, moved onto a row, let go: that row, in one gesture.
        assert_eq!(outcome(distance, true, true), json!("pick"));
        // Let go on the title or a separator: the menu waits.
        assert_eq!(outcome(40.0, false, true), json!("stay"));
        // Dragged away and let go outside: the gesture is given up.
        assert_eq!(outcome(40.0, false, false), json!("close"));
    });
}

#[test]
fn a_typed_letter_steps_through_the_rows_it_starts() {
    with_menu(|ctx| {
        let labels = r#"["pause", "next track", "previous track", "stop", null, "shuffle"]"#;
        let go = |current: i32, prefix: &str| {
            js(ctx, &format!("typeahead({labels}, {current}, {prefix:?})"))
        };
        assert_eq!(go(-1, "s"), json!(3), "the first row from the top");
        assert_eq!(
            go(3, "s"),
            json!(5),
            "again: the next one, past what cannot be picked"
        );
        assert_eq!(go(5, "s"), json!(3), "and around again");
        assert_eq!(
            go(3, "st"),
            json!(3),
            "a longer prefix stays on a row that still matches"
        );
        assert_eq!(go(3, "sh"), json!(5));
        assert_eq!(go(-1, "P"), json!(0), "letters are matched without case");
        assert_eq!(go(0, "x"), json!(-1), "nothing matches: nothing moves");
        assert_eq!(
            js(ctx, r#"typeahead([null, "play"], -1, "p")"#),
            json!(1),
            "a row that cannot be picked is never landed on"
        );
    });
}

#[test]
fn separators_are_only_drawn_between_rows() {
    with_menu(|ctx| {
        assert_eq!(
            js(
                ctx,
                r#"menuRows([null, "-", { label: "a" }, false, "-", "-", { label: "b" }, "-"])"#
            ),
            json!([{ "label": "a" }, "-", { "label": "b" }]),
            "the conditions a list carries are left out, and no line doubles or hangs at an end"
        );
        assert_eq!(
            js(ctx, r#"menuRows(["-", null, "-"])"#),
            json!([]),
            "nothing to show: the menu does not open"
        );
        assert_eq!(js(ctx, "menuRows(undefined)"), json!([]));
    });
}
