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

mod commands;
mod core_thread;
mod env;
mod state;
mod theme;

use std::process::ExitCode;

use headshell_core::config::Config;
use tokio::sync::mpsc;

use crate::state::{AppState, Core};
use crate::theme::ThemeStore;

fn main() -> ExitCode {
    // First thing: fixing up the environment. GDK reads `GDK_BACKEND` during
    // `gtk_init`, WebKit reads `WEBKIT_DISABLE_DMABUF_RENDERER` when the web
    // process is born — both after Tauri's setup, so anything after here is
    // too late.
    env::fixup();
    init_tracing();

    // The context is generated once: it embeds the interface files in the
    // binary, and both paths (the app or the error window) use the same one.
    let context = tauri::generate_context!();

    // The library is opened **before** the window: if the data directory
    // is missing or the database is corrupt, the user should look at an
    // error that says its stage, not at an empty window.
    let (core, themes) = match open_core() {
        Ok(opened) => opened,
        Err(text) => {
            report(&text);
            show_startup_error(context, &text);
            return ExitCode::FAILURE;
        }
    };

    match run(core, themes, context) {
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

fn open_core() -> Result<(Core, ThemeStore), String> {
    let config = Config::discover().map_err(|err| err.chain_text())?;
    // The theme store does not go to the core; it only knows the data directory
    // (§3.3).
    let themes = ThemeStore::new(config.data_dir());
    let lookup = headshell_core::session::lookup_mode_from_env().map_err(|err| err.chain_text())?;
    let core = Core::open(config, lookup).map_err(|err| err.chain_text())?;
    Ok((core, themes))
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

fn run(core: Core, themes: ThemeStore, context: tauri::Context) -> Result<(), String> {
    let (jobs_tx, jobs_rx) = mpsc::unbounded_channel();

    tauri::Builder::default()
        // It only lets the user pick a path. The side that reads/writes the
        // file is the core — which is why `capabilities/default.json` grants no
        // `fs` permission.
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new(jobs_tx, themes))
        .setup(move |app| {
            // The main window is built here rather than from the config alone
            // (`create: false`): the config has no switch for the clipboard. The
            // context menus' "paste" reads it (D-079); without the switch
            // WebKitGTK refuses the read, and the menu could only report that.
            // Only the app's own code runs in the page (CSP `script-src 'self'`,
            // nothing is written as markup — `ui_contract.rs`), and it reads the
            // clipboard only when "paste" is picked.
            let main = app
                .config()
                .app
                .windows
                .first()
                .ok_or("STEP: CONFIG_LOAD\n  tauri.conf.json declares no window")?;
            tauri::WebviewWindowBuilder::from_config(app.handle(), main)?
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
            commands::provider_test,
            commands::provider_scan,
            commands::servers_list,
            commands::server_add,
            commands::server_remove,
            // Playback
            commands::play,
            commands::toggle_pause,
            commands::stop,
            commands::next,
            commands::previous,
            commands::jump_to,
            commands::set_shuffle,
            commands::set_repeat,
            // Covers (D-076)
            commands::artwork,
            commands::artwork_queue,
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
        ])
        .run(context)
        .map_err(|err| format!("STEP: PLAYBACK_OUTPUT\n  could not open the window: {err}"))

    // When `run` returns, `AppState` is dropped, the channel closes, and the
    // core thread leaves its loop and writes the remaining listens (the
    // `shutdown` at the end of `core_thread::run`).
}

/// Logging goes to `stderr`; `println!` was only for user-facing output, and
/// a GUI has no such output.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("headshell_core=warn,headshell=warn")
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
