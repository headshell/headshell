//! `headshell` — the core's desktop shell (PLAN Phase 3).
//!
//! **The Golden Rule:** no business logic here. This package only opens the
//! window, forwards IPC commands to the core, drives the tick loop and passes
//! events to the webview. Delete a feature from here and the core still
//! offers it — the CLI's `--json` output proves it: it gets **the same** data
//! as the GUI.
//!
//! **D-030:** the dependency goes one way. This package never sees
//! `headshell-cli`; if one wants something from the other, that thing belongs
//! in the core.

// No console window on Windows; it stays in debug builds so the diagnostic
// lines are visible.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod command_names;
mod commands;
mod core_thread;
mod debug;
mod env;
mod localhost;
mod state;
mod theme;
mod ui_settings;
mod video;
mod video_server;

use std::process::ExitCode;

use headshell_core::config::Config;
use tauri::Manager as _;
use tokio::sync::mpsc;

use crate::debug::{DebugSwitch, Variable};
use crate::state::{AppState, Core};
use crate::theme::ThemeStore;
use crate::video::VideoDesk;

fn main() -> ExitCode {
    // First thing: fixing up the environment. GDK reads `GDK_BACKEND` during
    // `gtk_init`, WebKit reads `WEBKIT_DISABLE_DMABUF_RENDERER` when the web
    // process is born — both after Tauri's setup, so anything after here is
    // too late.
    env::fixup();
    // The debug mode is known before the log starts: while it is on, the log
    // is the developer's too (D-083). Where the data directory is comes first;
    // if it cannot be found, `open_core` says so below.
    let config = Config::discover();
    let variable = Variable::read();
    let held = variable == Variable::On;
    let saved = config.as_ref().is_ok_and(|config| {
        DebugSwitch::new(config.data_dir(), false)
            .mode()
            .is_ok_and(|mode| mode.saved)
    });
    init_tracing(held || saved);
    if let Variable::Ignored(value) = &variable {
        tracing::warn!(
            "{}={value:?} is not 1 — the debug mode follows the setting",
            debug::DEBUG_ENV
        );
    }

    // The context is generated once: it embeds the interface files in the
    // binary, and both paths (the app or the error window) use the same one.
    let context = tauri::generate_context!();

    // The library is opened **before** the window: if the data directory
    // is missing or the database is corrupt, the user should look at an
    // error that says its stage, not at an empty window.
    let (core, themes, debug, video) = match open_core(config, held) {
        Ok(opened) => opened,
        Err(text) => {
            report(&text);
            show_startup_error(context, &text);
            return ExitCode::FAILURE;
        }
    };

    match run(core, themes, debug, video, context) {
        Ok(()) => ExitCode::SUCCESS,
        Err(text) => {
            // If the window could not be opened at all there is no surface to
            // show; the error goes to `stderr` with its stage (K9).
            report(&text);
            ExitCode::FAILURE
        }
    }
}

fn report(text: &str) {
    eprintln!("{text}");
    eprintln!("\nfor details: headshell diag");
}

fn open_core(
    config: headshell_core::Result<Config>,
    debug_held: bool,
) -> Result<(Core, ThemeStore, DebugSwitch, VideoDesk), String> {
    let config = config.map_err(|err| err.chain_text())?;
    // The theme store, the debug switch and the video desk do not go to the
    // core; they only know the data directory (§3.3, D-083, D-086).
    let themes = ThemeStore::new(config.data_dir());
    let debug = DebugSwitch::new(config.data_dir(), debug_held);
    let video = VideoDesk::new(config.data_dir());
    // Videos turned off in settings: the first player shows none (D-086).
    // An unreadable `ui.json` is said in the log, and videos stay on — the
    // debug switch and the theme report the same file their own way.
    let shows_video = video.enabled().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "the video setting could not be read; videos stay on");
        true
    });
    let core = Core::open(config, shows_video).map_err(|err| err.chain_text())?;
    Ok((core, themes, debug, video))
}

/// Shows the error in a window if startup failed (D-070).
///
/// The Windows release build has no console (`windows_subsystem`, at the top
/// of the file): an error written to `stderr` goes **nowhere** there, and a
/// user double-clicking the app saw nothing at all. K9 forbids that — every
/// failure says which stage it is in. The window opens on every platform: one
/// text, one behaviour.
///
/// The text reaches the page through the `#` part of the address, not over
/// IPC: there is no core, no commands, and the page's CSP (`script-src
/// 'self'`) does not allow inline scripts.
fn show_startup_error(mut context: tauri::Context, text: &str) {
    // The main window (`index.html`) must not open: there is no core behind
    // it, and the interface would stay empty and frozen.
    context.config_mut().app.windows.clear();
    let url = format!("startup-error.html#{}", encode_fragment(text));

    let built = tauri::Builder::default()
        .setup(move |app| {
            tauri::WebviewWindowBuilder::new(
                app,
                "startup-error",
                tauri::WebviewUrl::App(url.into()),
            )
            .title("headshell could not start")
            .inner_size(760.0, 460.0)
            .build()?;
            Ok(())
        })
        .build(context);
    match built {
        // Returns when the window is closed; the process still exits with a
        // failure code.
        Ok(app) => {
            let _ = app.run_return(|_, _| {});
        }
        Err(err) => {
            eprintln!("STEP: STARTUP_ERROR — the error window could not be opened either: {err}")
        }
    }
}

/// Makes text writable into the fragment (`#…`) part of an address.
///
/// Every byte outside the unreserved characters (RFC 3986) becomes `%XX`.
/// Written by hand, because URL parsers silently drop line breaks — a
/// multi-line error chain would collapse into one line. The page turns it
/// back with `decodeURIComponent`.
fn encode_fragment(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    out
}

fn run(
    core: Core,
    themes: ThemeStore,
    debug: DebugSwitch,
    video: VideoDesk,
    context: tauri::Context,
) -> Result<(), String> {
    let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();
    let port = localhost::free_port()?;

    tauri::Builder::default()
        // It only lets the user pick a path. The side that reads/writes the
        // file is the core — which is why `capabilities/default.json` grants no
        // `fs` permission.
        .plugin(tauri_plugin_dialog::init())
        // D-086: the interface is served from `http://127.0.0.1:<port>/`, not
        // from `tauri://localhost`. YouTube's player refuses to play in a page
        // that sends no `Referer` (its "Error 153"), and a page from a custom
        // scheme never sends one. Only the loopback address listens.
        .plugin(
            tauri_plugin_localhost::Builder::new(port)
                .host(localhost::HOST)
                .build(),
        )
        .manage(AppState::new(jobs_tx, themes, debug, video))
        .setup(move |app| {
            // The main window is built here rather than from the config alone
            // (`create: false`): the config has no switch for the clipboard. The
            // context menus' "paste" reads it (D-079); without the switch
            // WebKitGTK refuses the read, and the menu could only report that.
            // Only the app's own code runs in the page (CSP `script-src 'self'`,
            // nothing is written as markup — `ui_contract.rs`), and it reads the
            // clipboard only when "paste" is picked.
            let mut main = app
                .config()
                .app
                .windows
                .first()
                .ok_or("STEP: CONFIG_LOAD\n  tauri.conf.json declares no window")?
                .clone();
            let page = localhost::page_url(port)?;
            // The page is not the app's own scheme, so Tauri lets it call
            // nothing until it is granted: the core's events and the file
            // dialog, and the app's own commands — for this address only.
            let mut grant = tauri::ipc::CapabilityBuilder::new("page")
                .remote(page.to_string())
                .window(main.label.clone())
                .permission("core:default")
                .permission("dialog:allow-open")
                .permission("dialog:allow-save");
            for command in command_names::COMMANDS {
                grant = grant.permission(format!("allow-{}", command.replace('_', "-")));
            }
            app.add_capability(grant)?;
            main.url = tauri::WebviewUrl::External(page);
            localhost::wait_until_listening(port)?;
            tauri::WebviewWindowBuilder::from_config(app.handle(), &main)?
                .enable_clipboard_access()
                .build()?;
            // The core is moved onto its own thread here: the `AppHandle` only
            // exists during setup, and events go out from there.
            core_thread::spawn(core, jobs_rx, app.handle().clone())?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Library
            commands::search,
            commands::stats,
            commands::sleeve,
            commands::sleeve_svg,
            // Import and identity
            commands::import,
            commands::resolve,
            // Providers
            commands::providers,
            commands::provider_use,
            commands::provider_test,
            commands::provider_scan,
            commands::music_dirs,
            commands::music_dirs_add,
            commands::music_dirs_remove,
            commands::servers_list,
            commands::server_add,
            commands::server_remove,
            // Playback
            commands::play,
            // Suggestions, exact tracks and adding to the queue (D-080)
            commands::suggest,
            // Recommendations (D-087)
            commands::recommend,
            commands::recommend_dismiss,
            commands::recommend_reset,
            commands::play_items,
            commands::queue_add,
            commands::queue_add_search,
            commands::toggle_pause,
            commands::stop,
            commands::next,
            commands::previous,
            commands::jump_to,
            commands::set_shuffle,
            commands::set_repeat,
            // Music videos (D-086)
            commands::provider_roles,
            commands::provider_role_set,
            commands::video_find,
            commands::video_show,
            commands::video_media,
            commands::video_report,
            commands::video_ended,
            commands::video_audio,
            commands::video_away,
            commands::video_away_set,
            commands::videos_enabled,
            commands::videos_set,
            // Covers (D-076)
            commands::artwork,
            commands::artwork_queue,
            commands::artwork_shown,
            commands::artwork_images,
            commands::lyrics,
            commands::lyrics_now,
            commands::lyrics_view,
            commands::lyrics_shift,
            // Reading ahead (D-077)
            commands::prefetch,
            commands::prefetch_queue,
            // Plugins and secrets (the Phase 2 surface)
            commands::plugins,
            commands::plugin_approve,
            commands::plugin_disable,
            commands::plugin_enable,
            commands::plugin_forget,
            commands::plugin_install,
            commands::plugin_catalog,
            commands::plugin_update,
            commands::plugin_remove,
            commands::secrets,
            commands::secret_set,
            commands::secret_remove,
            // Themes
            commands::themes_list,
            commands::theme_active,
            commands::theme_select,
            // State and diagnostics
            commands::anchor,
            commands::queue,
            commands::diag,
            commands::diag_text,
            commands::environment,
            commands::online,
            commands::online_set,
            commands::debug_mode,
            commands::debug_set,
        ])
        .run(context)
        .map_err(|err| format!("STEP: PLAYBACK_OUTPUT\n  could not open the window: {err}"))

    // When `run` returns, `AppState` is dropped, the channel closes, and the
    // core thread leaves its loop and writes the remaining listens (the
    // `shutdown` at the end of `core_thread::run`).
}

/// Logging goes to `stderr`; `println!` was only for user-facing output, and
/// a GUI has no such output. `RUST_LOG` decides when it is set; otherwise the
/// debug mode raises the level from warnings to the developer's (D-083).
fn init_tracing(debug: bool) {
    let level = if debug { "debug" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!("headshell_core={level},headshell={level}"))
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::encode_fragment;

    /// The encoding must come back through the page's `decodeURIComponent` —
    /// line breaks, Turkish letters and `#`/`%` included. The decoding the page
    /// does is done here in a real JS engine (QuickJS, D-070).
    #[test]
    fn the_error_text_survives_the_trip_through_the_url_fragment() {
        let text = "STEP: CONFIG_LOAD\n  → data directory — no %LOCALAPPDATA%; #1 çğıöşü İ";
        let encoded = encode_fragment(text);
        assert!(!encoded.contains('\n') && !encoded.contains('#') && !encoded.contains(' '));

        let runtime = rquickjs::Runtime::new().unwrap();
        let context = rquickjs::Context::full(&runtime).unwrap();
        let decoded: String = context.with(|ctx| {
            let decode: rquickjs::Function = ctx.globals().get("decodeURIComponent").unwrap();
            decode.call((encoded.as_str(),)).unwrap()
        });
        assert_eq!(decoded, text);
    }
}
