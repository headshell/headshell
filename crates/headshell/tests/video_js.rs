//! The video layer's pure parts (D-086), evaluated in the embedded QuickJS
//! (the same route as `anchor_parity_js.rs`, D-070): the address YouTube's
//! player is asked for, and how its state numbers and refusals are read.
//!
//! The address carries the rules the window keeps: the player's own controls
//! off (the app's are used), no keyboard of its own, no full screen, no notes
//! over the picture — and the start second, which is how the video picks up
//! where the music was.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rquickjs::{CaughtError, Context, Function, Module, Runtime};

const VIDEO_JS: &str = include_str!("../ui/video.js");

fn with_module<T>(
    check: impl FnOnce(&rquickjs::Module<'_, rquickjs::module::Evaluated>) -> T,
) -> T {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        let module = Module::declare(ctx.clone(), "video.js", VIDEO_JS)
            .and_then(|module| module.eval())
            .and_then(|(module, promise)| promise.finish::<()>().map(|()| module));
        match module {
            Ok(module) => check(&module),
            Err(err) => panic!(
                "video.js could not be evaluated: {}",
                CaughtError::from_error(&ctx, err)
            ),
        }
    })
}

#[test]
fn the_player_is_asked_for_from_the_second_the_music_was_at() {
    let url: String = with_module(|module| {
        let embed: Function = module.get("embedUrl").unwrap();
        embed
            .call(("CCHdMIEGaaM", 61_900.0, "http://localhost:4242"))
            .unwrap()
    });
    assert!(
        url.starts_with("https://www.youtube-nocookie.com/embed/CCHdMIEGaaM?"),
        "{url}"
    );
    for part in [
        "enablejsapi=1",
        "controls=0",
        "disablekb=1",
        "fs=0",
        "iv_load_policy=3",
        "rel=0",
        // Whole seconds, rounded down: the player starts at a second.
        "start=61",
        "origin=http%3A%2F%2Flocalhost%3A4242",
    ] {
        assert!(url.contains(part), "{part} is not in {url}");
    }
}

#[test]
fn the_players_numbers_are_read_as_the_cores_words() {
    let (states, refusals): (Vec<String>, Vec<bool>) = with_module(|module| {
        let state: Function = module.get("youtubeState").unwrap();
        let refuses: Function = module.get("youtubeRefuses").unwrap();
        let states = [-1, 1, 2, 3, 5]
            .into_iter()
            .map(|code| state.call::<_, String>((code,)).unwrap())
            .collect();
        let refusals = [150, 101, 100, 2, 1]
            .into_iter()
            .map(|code| refuses.call::<_, bool>((code,)).unwrap())
            .collect();
        (states, refusals)
    });
    assert_eq!(
        states,
        ["buffering", "playing", "paused", "buffering", "paused"]
    );
    assert_eq!(refusals, [true, true, true, true, false]);
}
