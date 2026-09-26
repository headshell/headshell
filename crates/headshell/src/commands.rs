//! IPC commands (D-033).
//!
//! **The contract = the core's surface + serde.** No command here makes up a
//! new data shape; each returns an existing core type and crosses with
//! `serde`. The **same** data as the CLI's `--json` output.
//!
//! The command list matches the CLI's subcommands one to one, because both
//! are shells of the same core. The only thing done here: pass the argument
//! to the core, give the result back.
//!
//! Every command body runs on the core thread ([`crate::state`]), which is
//! why all of them are wrapped in `state.run_on_core(...)`. This is not a
//! layer but an address: it says where the work is done.
//!
//! **Long commands** (`import`, `resolve`, `provider_scan`, `provider_test`,
//! `server_add`, `play`, `artwork`) hold the queue, and the playback controls wait
//! meanwhile. So this does not stay invisible, they send [`BUSY_EVENT`]: the
//! interface writes which job is running (K9).

use std::future::Future;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use headshell_core::artwork::{ArtworkKey, ArtworkPicture, ArtworkReport, ArtworkVariant};
use headshell_core::diag::Stage;
use headshell_core::ids::ProviderId;
use headshell_core::model::PlayRule;
use headshell_core::playback::{PlaybackAnchor, PrefetchReport, QueueView, RepeatMode};
use headshell_core::provider::remote::{self, NewServer, ServerKind};
use headshell_core::session::{
    self, ArtworkRequest, ImportReport, LookupMode, PlayOptions, PluginCatalogReport,
    PluginConsentReport, PluginInstallReport, PluginListReport, PluginRemoveReport,
    PluginUpdateReport, ProviderListReport, ProviderTestReport, ResolveReport, ScanReport,
    SearchReport, SecretListReport, SecretWriteReport, ServerAddReport, ServerListReport,
    ServerRemoveReport, SleeveResponse, StatsResponse,
};
use headshell_core::sleeve::CardPreset;
use headshell_core::stats::StatsQuery;

use crate::state::{AppState, CommandError, CommandResult};
use crate::theme::{ActiveTheme, ThemeList};

/// Tells the webview that a long job started/ended.
///
/// The event name is `headshell://busy`, its payload the job's name (`null`
/// at the end). It goes out on a state change, not on a timer — D-033's rule
/// holds here too.
pub const BUSY_EVENT: &str = "headshell://busy";

/// Wraps a long-running core call with the busy event.
async fn busy<T, F>(app: &AppHandle, what: &str, task: F) -> headshell_core::Result<T>
where
    F: Future<Output = headshell_core::Result<T>>,
{
    let _ = app.emit(BUSY_EVENT, Some(what));
    let result = task.await;
    // It must close on the error path too: otherwise the interface would look
    // busy forever.
    let _ = app.emit(BUSY_EVENT, None::<&str>);
    result
}

// ————————————————————————————————————— Library

#[tauri::command]
pub async fn search(
    state: State<'_, AppState>,
    query: String,
    limit: usize,
    min_ms: Option<u64>,
) -> CommandResult<SearchReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let rule = min_ms.map_or_else(PlayRule::default, PlayRule::new);
                core.live.session().search(&query, limit, rule)
            })
        })
        .await
}

#[tauri::command]
pub async fn stats(
    state: State<'_, AppState>,
    year: Option<i16>,
    top: usize,
    min_ms: Option<u64>,
) -> CommandResult<StatsResponse> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let query = StatsQuery {
                    year,
                    top,
                    min_ms_played: min_ms.unwrap_or(headshell_core::stats::DEFAULT_MIN_MS_PLAYED),
                };
                core.live.session().stats(query)
            })
        })
        .await
}

/// The Sleeve card. If `out` is given it is written to a file too (the
/// extension decides the format).
#[tauri::command]
pub async fn sleeve(
    state: State<'_, AppState>,
    year: Option<i16>,
    story: bool,
    out: Option<String>,
) -> CommandResult<SleeveResponse> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let query = StatsQuery {
                    year,
                    top: 10,
                    min_ms_played: headshell_core::stats::DEFAULT_MIN_MS_PLAYED,
                };
                let size = if story {
                    CardPreset::Story.size()
                } else {
                    CardPreset::Square.size()
                };
                let path = out.map(std::path::PathBuf::from);
                core.live.session().sleeve(query, size, path.as_deref())
            })
        })
        .await
}

/// A **preview** of the Sleeve card — writes no file.
///
/// It runs the same calculation as `sleeve` and returns the core's own
/// renderer (`sleeve::render_svg`). Drawing the card here would break K1: a
/// second drawing of the same card would live in JS and drift silently.
#[tauri::command]
pub async fn sleeve_svg(
    app: AppHandle,
    state: State<'_, AppState>,
    year: Option<i16>,
    story: bool,
) -> CommandResult<String> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let query = StatsQuery {
                    year,
                    top: 10,
                    min_ms_played: headshell_core::stats::DEFAULT_MIN_MS_PLAYED,
                };
                let size = if story {
                    CardPreset::Story.size()
                } else {
                    CardPreset::Square.size()
                };
                busy(&app, "preparing the card", async {
                    let response = core.live.session().sleeve(query, size, None)?;
                    Ok(headshell_core::sleeve::render_svg(
                        &response.data,
                        response.size,
                    ))
                })
                .await
            })
        })
        .await
}

// ————————————————————————————————————— Import and identity

#[tauri::command]
pub async fn import(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> CommandResult<ImportReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let path = std::path::PathBuf::from(path);
                let lookup = session::lookup_for(core.lookup)?;
                busy(
                    &app,
                    "importing",
                    core.live.session_mut().import_archive(&path, lookup),
                )
                .await
            })
        })
        .await
}

#[tauri::command]
pub async fn resolve(
    app: AppHandle,
    state: State<'_, AppState>,
    query: String,
) -> CommandResult<ResolveReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let lookup = session::lookup_for(core.lookup)?;
                busy(
                    &app,
                    "resolving the identity",
                    core.live.session_mut().resolve_track(&query, lookup),
                )
                .await
            })
        })
        .await
}

// ————————————————————————————————————— Providers

#[tauri::command]
pub async fn providers(state: State<'_, AppState>) -> CommandResult<ProviderListReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move { core.live.session().providers(&core.registry) })
        })
        .await
}

#[tauri::command]
pub async fn provider_test(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<ProviderTestReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let id = ProviderId::new(name);
                busy(
                    &app,
                    "testing the provider",
                    core.live.session().test_provider(&core.registry, &id),
                )
                .await
            })
        })
        .await
}

#[tauri::command]
pub async fn provider_scan(
    app: AppHandle,
    state: State<'_, AppState>,
    if_stale: bool,
) -> CommandResult<ScanReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                // The registry carries an `Arc`; the clone is cheap, and more readable
                // than splitting `core` in two (one side `&mut Session`, the other
                // `&ProviderRegistry`).
                let registry = core.registry.clone();
                let session = core.live.session_mut();
                let task = async {
                    if if_stale {
                        session.scan_providers_if_stale(&registry).await
                    } else {
                        session.scan_providers(&registry).await
                    }
                };
                busy(&app, "scanning the library", task).await
            })
        })
        .await
}

#[tauri::command]
pub async fn servers_list(state: State<'_, AppState>) -> CommandResult<ServerListReport> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.session().list_servers() }))
        .await
}

/// Adds a remote server.
///
/// The password comes over IPC — as in the CLI it is not written on a
/// command line, and does not end up in `ps` output or the shell history.
/// Persistent storage is still the core's job (`servers.json` with `0600`
/// permissions); the keyring debt is in Phase 2 (D-021).
#[tauri::command]
#[expect(
    clippy::too_many_arguments,
    reason = "one to one with the CLI's `provider add` arguments; making up a 'request' type would be the translating layer D-033 rejected"
)]
pub async fn server_add(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: String,
    url: String,
    user: String,
    password: Option<String>,
    api_key: Option<String>,
    name: Option<String>,
    verify: bool,
) -> CommandResult<ServerAddReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let kind = ServerKind::parse(&kind)?;
                // The name suggestion comes from the core: the CLI shows the same one.
                let id = name.map_or_else(|| remote::suggest_id(&url, kind), ProviderId::new);
                let spec = NewServer {
                    id,
                    kind,
                    url,
                    username: user,
                    password: if api_key.is_some() { None } else { password },
                    api_key,
                    verify,
                };
                let task = async {
                    let http = headshell_core::net::default_http_client()?;
                    core.live.session_mut().add_server(spec, http).await
                };
                let report = busy(&app, "verifying the server", task).await?;
                // The new server should be playable right away: without refreshing
                // the registry it would stay invisible until the app closed.
                core.refresh_registry()?;
                Ok(report)
            })
        })
        .await
}

#[tauri::command]
pub async fn server_remove(
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<ServerRemoveReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let report = core.live.session().remove_server(&ProviderId::new(name))?;
                core.refresh_registry()?;
                Ok(report)
            })
        })
        .await
}

// ————————————————————————————————————— Playback

/// Searches, queues and starts playing. **Does not wait.**
///
/// `Session::play` is the blocking route (the CLI's `headshell play`); the GUI
/// uses `player_from_search` instead, and the core thread drives the loop.
#[tauri::command]
pub async fn play(
    app: AppHandle,
    state: State<'_, AppState>,
    query: String,
    all: bool,
    shuffle: bool,
) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let options = PlayOptions {
                    all,
                    shuffle,
                    ..PlayOptions::new(query)
                };
                let task = core
                    .live
                    .session()
                    .player_from_search(&core.registry, options);
                let player = busy(&app, "searching for the track", task).await?;
                // The old player's accumulated listens are **taken**, not thrown away.
                core.live.replace_player(player);
                Ok(core.live.player().queue().view())
            })
        })
        .await
}

#[tauri::command]
pub async fn toggle_pause(state: State<'_, AppState>) -> CommandResult<PlaybackAnchor> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                // The "pause or resume" decision is inside `Player::toggle_pause`.
                core.live.player().toggle_pause();
                Ok(core.live.anchor())
            })
        })
        .await
}

#[tauri::command]
pub async fn stop(state: State<'_, AppState>) -> CommandResult<PlaybackAnchor> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().stop();
                Ok(core.live.anchor())
            })
        })
        .await
}

#[tauri::command]
pub async fn next(state: State<'_, AppState>) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().next().await?;
                Ok(core.live.player().queue().view())
            })
        })
        .await
}

#[tauri::command]
pub async fn previous(state: State<'_, AppState>) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().previous().await?;
                Ok(core.live.player().queue().view())
            })
        })
        .await
}

#[tauri::command]
pub async fn jump_to(state: State<'_, AppState>, index: usize) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().jump_to(index).await?;
                Ok(core.live.player().queue().view())
            })
        })
        .await
}

#[tauri::command]
pub async fn set_shuffle(state: State<'_, AppState>, on: bool) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().queue_mut().set_shuffle(on);
                Ok(core.live.player().queue().view())
            })
        })
        .await
}

/// Sets the repeat mode. The mode name comes in the core's own spelling
/// (`off` | `all` | `one`) — no second mapping table is kept in the shell.
#[tauri::command]
pub async fn set_repeat(state: State<'_, AppState>, mode: RepeatMode) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().queue_mut().set_repeat(mode);
                Ok(core.live.player().queue().view())
            })
        })
        .await
}

// ————————————————————————————————————— Covers (D-076)
//
// `artwork` is `headshell artwork`, waiting for every cover. The other two are
// the playing queue's side: the core's cover worker looks the queue up in the
// background, the tick carries the keys that arrived, and the interface asks
// for their images here — `data:` URIs, since the page's CSP allows nothing
// else.

#[tauri::command]
pub async fn artwork(
    app: AppHandle,
    state: State<'_, AppState>,
    query: Option<String>,
    all: bool,
    file: Option<String>,
    out: Option<String>,
) -> CommandResult<ArtworkReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let request = ArtworkRequest {
                    query,
                    all,
                    file: file.map(std::path::PathBuf::from),
                    out: out.map(std::path::PathBuf::from),
                };
                let lookup = core.lookup;
                let task = core
                    .live
                    .session_mut()
                    .artwork(&core.registry, request, lookup);
                busy(&app, "looking up covers", task).await
            })
        })
        .await
}

/// The playing queue's covers as they stand: which are found, which are not,
/// and why.
#[tauri::command]
pub async fn artwork_queue(state: State<'_, AppState>) -> CommandResult<ArtworkReport> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.artwork_report() }))
        .await
}

/// One size — the label or the thumbnail — of the given cover keys. A key
/// the core never gave is the interface's mistake, and it is said, not
/// skipped (K9).
#[tauri::command]
pub async fn artwork_images(
    state: State<'_, AppState>,
    keys: Vec<String>,
    variant: ArtworkVariant,
) -> CommandResult<Vec<ArtworkPicture>> {
    let keys = keys
        .iter()
        .map(|raw| {
            ArtworkKey::parse(raw).ok_or_else(|| {
                CommandError::new(Stage::ArtworkStore, &format!("not a cover key: `{raw}`"))
            })
        })
        .collect::<CommandResult<Vec<_>>>()?;
    state
        .run_on_core(move |core| Box::pin(async move { core.live.artwork_images(&keys, variant) }))
        .await
}

// ————————————————————————————————————— Reading ahead (D-077)
//
// `prefetch` is `headshell prefetch`: the first tracks a query finds, made
// ready one after another. `prefetch_queue` is the playing queue's side: what
// the core's read-ahead thread has made ready, and how its session went.

#[tauri::command]
pub async fn prefetch(
    app: AppHandle,
    state: State<'_, AppState>,
    query: String,
    all: bool,
) -> CommandResult<PrefetchReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let task = core
                    .live
                    .session_mut()
                    .prefetch(&core.registry, &query, all);
                busy(&app, "making tracks ready", task).await
            })
        })
        .await
}

/// The playing queue's read-ahead: the tracks planned, where each stands,
/// and the session's counts.
#[tauri::command]
pub async fn prefetch_queue(state: State<'_, AppState>) -> CommandResult<PrefetchReport> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.prefetch_report() }))
        .await
}

// ————————————————————————————————————— Themes (§3.3)
//
// These three commands **do not enter** the core thread: a theme is a CSS
// mechanism, not a concept the core carries (see `crate::theme`). That is why
// they answer even while a long `import` runs.
//
// The stage is `CONFIG_LOAD`: this is reading a configuration from the data
// directory — adding a `THEME_LOAD` stage to the core that only the GUI needs
// would leak a shell concept into the core's diagnostic vocabulary.

fn theme_error(text: String) -> CommandError {
    CommandError::new(headshell_core::diag::Stage::ConfigLoad, &text)
}

/// The loadable themes + **the rejected ones and their reasons** (K9).
#[tauri::command]
pub async fn themes_list(state: State<'_, AppState>) -> CommandResult<ThemeList> {
    state.themes().list().map_err(theme_error)
}

/// Loads the saved choice. Called once at startup.
#[tauri::command]
pub async fn theme_active(state: State<'_, AppState>) -> CommandResult<ActiveTheme> {
    state.themes().active().map_err(theme_error)
}

/// Picks a theme and writes it persistently. Without an `id` it goes back to
/// the default.
#[tauri::command]
pub async fn theme_select(
    state: State<'_, AppState>,
    id: Option<String>,
) -> CommandResult<ActiveTheme> {
    state.themes().select(id).map_err(theme_error)
}

// ————————————————————————————————————— Plugins (Phase 2)
//
// The plugin surface was opened to the GUI **after** Phase 3: when the
// interface was written Phase 2 had been postponed (D-027), and the shell
// knew nothing of its capabilities. The commands make exactly the same core
// calls as the CLI's `headshell plugin ...` subcommands — both are shells of
// the same core.
//
// How much of the permissions is enforced is written in every list with the
// `permissions_enforced` field (D-040 → D-069). The interface does not hide
// it: no trust is given to a protection that does not exist, and the limit
// of the one that does is said.
//
// The catalog (D-071) is only read when the user asks: not when the panel
// opens, but on "fetch the catalog". Every command that goes online sends
// the busy event.
//
// Every command that changes a plugin's state **refreshes** the provider
// registry. The registry is built at startup and only the server commands
// used to do this: an approved plugin could not be played until the app was
// reopened, and a removed plugin kept showing up in search (found in D-071).
// The playing track is not affected — the player keeps its own copy.

#[tauri::command]
pub async fn plugins(state: State<'_, AppState>) -> CommandResult<PluginListReport> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.session().plugins() }))
        .await
}

/// Approves the permissions a plugin declares (D-040).
#[tauri::command]
pub async fn plugin_approve(
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<PluginConsentReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let report = core.live.session().approve_plugin(&name)?;
                core.refresh_registry()?;
                Ok(report)
            })
        })
        .await
}

#[tauri::command]
pub async fn plugin_disable(
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<PluginConsentReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let report = core.live.session().disable_plugin(&name)?;
                core.refresh_registry()?;
                Ok(report)
            })
        })
        .await
}

#[tauri::command]
pub async fn plugin_enable(
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<PluginConsentReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let report = core.live.session().enable_plugin(&name)?;
                core.refresh_registry()?;
                Ok(report)
            })
        })
        .await
}

#[tauri::command]
pub async fn plugin_forget(
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<PluginConsentReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let report = core.live.session().forget_plugin(&name)?;
                core.refresh_registry()?;
                Ok(report)
            })
        })
        .await
}

/// Reads the catalog and shows it against this machine (D-071). **Goes
/// online.**
#[tauri::command]
pub async fn plugin_catalog(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<PluginCatalogReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let http = headshell_core::net::default_http_client()?;
                busy(
                    &app,
                    "reading the catalog",
                    core.live.session().plugin_catalog(http),
                )
                .await
            })
        })
        .await
}

/// Installs a plugin: downloads it from the catalog if it is not on disk,
/// then its tools (D-055, D-071). **Goes online** — which is why it sends the
/// busy event.
#[tauri::command]
pub async fn plugin_install(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<PluginInstallReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let http = headshell_core::net::default_http_client()?;
                let report = busy(
                    &app,
                    format!("installing {name}").as_str(),
                    core.live.session().install_plugin(&name, http),
                )
                .await?;
                core.refresh_registry()?;
                Ok(report)
            })
        })
        .await
}

/// Updates the plugins installed from the catalog; all of them without a
/// `name` (D-071).
#[tauri::command]
pub async fn plugin_update(
    app: AppHandle,
    state: State<'_, AppState>,
    name: Option<String>,
) -> CommandResult<PluginUpdateReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let http = headshell_core::net::default_http_client()?;
                let what = match &name {
                    Some(name) => format!("updating {name}"),
                    None => "updating the plugins".to_owned(),
                };
                let report = busy(
                    &app,
                    &what,
                    core.live.session().update_plugins(name.as_deref(), http),
                )
                .await?;
                core.refresh_registry()?;
                Ok(report)
            })
        })
        .await
}

/// Removes a plugin and forgets its consent (D-071).
#[tauri::command]
pub async fn plugin_remove(
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<PluginRemoveReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let report = core.live.session().remove_plugin(&name)?;
                core.refresh_registry()?;
                Ok(report)
            })
        })
        .await
}

// ————————————————————————————————————— Secrets (D-042)
//
// The list carries **key names**, not values. The only side that reads a
// secret is the plugin that uses it; the interface only writes and deletes.

#[tauri::command]
pub async fn secrets(state: State<'_, AppState>) -> CommandResult<SecretListReport> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.session().secrets() }))
        .await
}

#[tauri::command]
pub async fn secret_set(
    state: State<'_, AppState>,
    namespace: String,
    key: String,
    value: String,
) -> CommandResult<SecretWriteReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move { core.live.session().set_secret(&namespace, &key, &value) })
        })
        .await
}

#[tauri::command]
pub async fn secret_remove(
    state: State<'_, AppState>,
    namespace: String,
    key: String,
) -> CommandResult<SecretWriteReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move { core.live.session().remove_secret(&namespace, &key) })
        })
        .await
}

// ————————————————————————————————————— State and diagnostics

/// The current anchor. The webview **estimates** the position from this; it
/// does not ask (D-015).
#[tauri::command]
pub async fn anchor(state: State<'_, AppState>) -> CommandResult<PlaybackAnchor> {
    state
        .run_on_core(move |core| Box::pin(async move { Ok(core.live.anchor()) }))
        .await
}

#[tauri::command]
pub async fn queue(state: State<'_, AppState>) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| Box::pin(async move { Ok(core.live.player().queue().view()) }))
        .await
}

#[tauri::command]
pub async fn diag(
    state: State<'_, AppState>,
) -> CommandResult<Option<headshell_core::diag::DiagReport>> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.session().last_diag() }))
        .await
}

/// The readable form of `diag`: the core's own `DiagReport::render()` text —
/// the same as `headshell diag`'s output without `--json` (D-072).
///
/// The interface once printed the report as raw JSON, while `render()`'s
/// documentation says "the GUI will show the same text". Had the formatting
/// been done here or in JS, the block pasted when reporting a bug would have
/// drifted from the CLI's — the same reasoning as `sleeve_svg` (K1).
#[tauri::command]
pub async fn diag_text(state: State<'_, AppState>) -> CommandResult<Option<String>> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                Ok(core
                    .live
                    .session()
                    .last_diag()?
                    .map(|report| report.render()))
            })
        })
        .await
}

/// Constants asked once when the window opens.
///
/// Not a new "state" type but diagnostic information: so "which library did I
/// look at" does not go unanswered next to an error message (K9).
#[derive(Debug, Clone, Serialize)]
pub struct Environment {
    pub data_dir: std::path::PathBuf,
    pub database: std::path::PathBuf,
    pub music_dirs: Vec<std::path::PathBuf>,
    pub version: String,
    /// `HEADSHELL_ONLINE`, as read at startup (D-076).
    pub lookup: LookupMode,
}

#[tauri::command]
pub async fn environment(state: State<'_, AppState>) -> CommandResult<Environment> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let config = core.live.session().config();
                Ok(Environment {
                    data_dir: config.data_dir().to_path_buf(),
                    database: config.database_path(),
                    music_dirs: config.music_dirs(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    lookup: core.lookup,
                })
            })
        })
        .await
}
