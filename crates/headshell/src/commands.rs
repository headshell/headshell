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
//! `server_add`, `play`, `artwork`, `lyrics`) hold the queue, and the playback controls wait
//! meanwhile. So this does not stay invisible, they send [`BUSY_EVENT`]: the
//! interface writes which job is running (K9).

use std::future::Future;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use headshell_core::artwork::{ArtworkKey, ArtworkPicture, ArtworkReport, ArtworkVariant};
use headshell_core::diag::Stage;
use headshell_core::ids::ProviderId;
use headshell_core::lyrics::{LyricsItem, LyricsKey, LyricsReport};
use headshell_core::model::PlayRule;
use headshell_core::playback::{
    PlayState, PlaybackAnchor, PrefetchReport, QueueItem, QueueView, RepeatMode,
};
use headshell_core::provider::remote::{self, NewServer, ServerKind};
use headshell_core::session::{
    self, ArtworkRequest, ImportReport, LookupMode, LyricsRequest, MusicFoldersReport,
    OnlineReport, PlayFromReport, PlayOptions, PluginCatalogReport, PluginConsentReport,
    PluginInstallReport, PluginListReport, PluginRemoveReport, PluginUpdateReport,
    ProviderListReport, ProviderRoles, ProviderTestReport, ResolveReport, RolesReport, ScanReport,
    SearchReport, SecretListReport, SecretWriteReport, ServerAddReport, ServerListReport,
    ServerRemoveReport, SleeveResponse, StatsResponse,
};
use headshell_core::sleeve::CardPreset;
use headshell_core::stats::StatsQuery;
use headshell_core::suggest::SuggestReport;
use headshell_core::video::{VideoPlan, VideoSource};

use crate::debug::DebugMode;
use crate::state::{AppState, CommandError, CommandResult};
use crate::theme::{ActiveTheme, ThemeList};
use crate::video::VideoAway;

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

/// Chooses where the play box searches first (D-080) — `headshell provider
/// use`. The answer is where it searches now.
#[tauri::command]
pub async fn provider_use(
    state: State<'_, AppState>,
    name: String,
) -> CommandResult<PlayFromReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live
                    .session()
                    .set_play_from(&core.registry, &ProviderId::new(name))
            })
        })
        .await
}

/// The music folders and who decided them (D-085) — `headshell
/// music-dirs`.
#[tauri::command]
pub async fn music_dirs(state: State<'_, AppState>) -> CommandResult<MusicFoldersReport> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.session().music_folders() }))
        .await
}

/// Adds a music folder — `headshell music-dirs add`. The registry is rebuilt
/// at once, so the scan the interface runs next reads it, and a song from it
/// plays (the local provider plays only under its folders).
#[tauri::command]
pub async fn music_dirs_add(
    state: State<'_, AppState>,
    path: String,
) -> CommandResult<MusicFoldersReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let report = core
                    .live
                    .session()
                    .add_music_folder(std::path::Path::new(&path))?;
                if report.changed {
                    core.refresh_registry()?;
                }
                Ok(report)
            })
        })
        .await
}

/// Takes a music folder off — `headshell music-dirs remove`; its songs leave
/// at the scan the interface runs next.
#[tauri::command]
pub async fn music_dirs_remove(
    state: State<'_, AppState>,
    path: String,
) -> CommandResult<MusicFoldersReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let report = core
                    .live
                    .session()
                    .remove_music_folder(std::path::Path::new(&path))?;
                core.refresh_registry()?;
                Ok(report)
            })
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
                    // The window shows videos unless they are off (D-086):
                    // the playing player knows.
                    shows_video: core.live.player().shows_video(),
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

/// What the play box offers while it is typed in (D-080) — `headshell
/// suggest`.
///
/// The catalog is read on the core thread, at once. A server or a plugin is
/// asked **off** it (`SuggestPlan::finish` needs no session): typing must not
/// hold the player's rounds and controls the way a search for a track to
/// play does. No busy notice either — this is not a job the user waits on.
#[tauri::command]
pub async fn suggest(state: State<'_, AppState>, query: String) -> CommandResult<SuggestReport> {
    let plan = state
        .run_on_core(move |core| {
            Box::pin(async move { core.live.session().suggest_plan(&core.registry, &query) })
        })
        .await?;
    Ok(plan.finish().await)
}

/// Plays exactly these tracks — a picked suggestion, a song's "play now"
/// (D-080). No search in between. The TUI's search line does the same with
/// Enter.
#[tauri::command]
pub async fn play_items(
    app: AppHandle,
    state: State<'_, AppState>,
    items: Vec<QueueItem>,
) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let task = core.live.session().player_from_items(
                    &core.registry,
                    items,
                    core.live.player().shows_video(),
                );
                let player = busy(&app, "starting the track", task).await?;
                // The old player's accumulated listens are **taken**, not thrown away.
                core.live.replace_player(player);
                Ok(core.live.player().queue().view())
            })
        })
        .await
}

/// Adds tracks at the end of the queue (D-080); into an empty queue they
/// play. The TUI's search line does the same with Tab.
#[tauri::command]
pub async fn queue_add(
    state: State<'_, AppState>,
    items: Vec<QueueItem>,
) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().enqueue(items).await?;
                Ok(core.live.player().queue().view())
            })
        })
        .await
}

/// Adds what a text finds to the queue — a history row has a name, not a
/// provider's track: the search the play box does finds it (D-080). `all`
/// adds every match (an artist's tracks), otherwise the first.
#[tauri::command]
pub async fn queue_add_search(
    app: AppHandle,
    state: State<'_, AppState>,
    query: String,
    all: bool,
) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let limit = PlayOptions::new(String::new()).limit;
                let task =
                    core.live
                        .session()
                        .queue_from_search(&core.registry, &query, all, limit);
                let items = busy(&app, "searching for the track", task).await?;
                core.live.player_mut().enqueue(items).await?;
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

// ————————————————————————————————————— Music videos (D-086)

/// Who plays each role — the roles part of `headshell provider list`.
#[tauri::command]
pub async fn provider_roles(state: State<'_, AppState>) -> CommandResult<ProviderRoles> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.session().roles(&core.registry) }))
        .await
}

/// Chooses who plays a role — `headshell provider use <name> --for <role>`;
/// without a name, `headshell provider follow <role>`.
#[tauri::command]
pub async fn provider_role_set(
    state: State<'_, AppState>,
    role: String,
    name: Option<String>,
) -> CommandResult<RolesReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let role = session::Role::parse(&role)?;
                let id = name.map(ProviderId::new);
                core.live
                    .session()
                    .set_role(&core.registry, role, id.as_ref())
            })
        })
        .await
}

/// The videos for the playing track — `headshell video`. It can search a
/// video service, so it says it is busy.
#[tauri::command]
pub async fn video_find(app: AppHandle, state: State<'_, AppState>) -> CommandResult<VideoPlan> {
    state
        .run_on_core(move |core| {
            Box::pin(
                async move { busy(&app, "finding the video", core.live.video_for_current()).await },
            )
        })
        .await
}

/// What the page needs to show a video: the anchor, and for a file, the
/// address it plays from.
#[derive(Debug, Clone, Serialize)]
pub struct VideoShown {
    pub anchor: PlaybackAnchor,
    /// The address of the file being shown; `None` for a player that fetches
    /// its own video (YouTube).
    pub media_url: Option<String>,
}

/// Hands the sound to the page's video player at `at_ms`.
#[tauri::command]
pub async fn video_show(
    state: State<'_, AppState>,
    source: VideoSource,
    provider: String,
    at_ms: u64,
) -> CommandResult<VideoShown> {
    let anchor = state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live
                    .player_mut()
                    .show_video(source, ProviderId::new(provider), at_ms)?;
                Ok(core.live.anchor())
            })
        })
        .await?;
    let media_url = video_media(state).await?;
    Ok(VideoShown { anchor, media_url })
}

/// The address the page plays the shown video from, when it is a file —
/// for a video the core started on its own (a track that is a video only).
/// Only the file the core shows is ever served.
#[tauri::command]
pub async fn video_media(state: State<'_, AppState>) -> CommandResult<Option<String>> {
    let shown = state
        .run_on_core(move |core| {
            Box::pin(
                async move { Ok(core.live.player().video_now().map(|now| now.source.clone())) },
            )
        })
        .await?;
    match shown {
        Some(VideoSource::LocalFile { path }) => state
            .video()
            .serve(&path)
            .map(Some)
            .map_err(|text| CommandError::new(Stage::VideoPlayback, &text)),
        _ => {
            state.video().stop_serving();
            Ok(None)
        }
    }
}

/// The page's video player reports its clock: about once a second, and at
/// every change. A late one — the track moved on — is refused, and the page
/// drops it.
#[tauri::command]
pub async fn video_report(
    state: State<'_, AppState>,
    position_ms: u64,
    playing: PlayState,
    duration_ms: Option<u64>,
) -> CommandResult<()> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live
                    .player_mut()
                    .video_report(position_ms, playing, duration_ms)
            })
        })
        .await
}

/// The video ran to its end: the queue moves on.
#[tauri::command]
pub async fn video_ended(state: State<'_, AppState>) -> CommandResult<QueueView> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().video_ended().await?;
                Ok(core.live.player().queue().view())
            })
        })
        .await
}

/// The music takes the sound back at `at_ms` — where the video was.
#[tauri::command]
pub async fn video_audio(state: State<'_, AppState>, at_ms: u64) -> CommandResult<PlaybackAnchor> {
    let anchor = state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().show_audio(at_ms)?;
                Ok(core.live.anchor())
            })
        })
        .await?;
    state.video().stop_serving();
    Ok(anchor)
}

/// What a video does when the player is left.
#[tauri::command]
pub async fn video_away(state: State<'_, AppState>) -> CommandResult<VideoAway> {
    state
        .video()
        .away()
        .map_err(|text| CommandError::new(Stage::ConfigLoad, &text))
}

/// Are music videos on? A window-only choice, as the debug mode is.
#[tauri::command]
pub async fn videos_enabled(state: State<'_, AppState>) -> CommandResult<bool> {
    state
        .video()
        .enabled()
        .map_err(|text| CommandError::new(Stage::ConfigLoad, &text))
}

/// Turns music videos on or off: saved, and the playing player told at once
/// — off, a track that is a video only is passed by, as the CLI passes it.
/// The page hands a video being shown back to the music before this.
#[tauri::command]
pub async fn videos_set(state: State<'_, AppState>, on: bool) -> CommandResult<bool> {
    let on = state
        .video()
        .set_enabled(on)
        .map_err(|text| CommandError::new(Stage::ConfigLoad, &text))?;
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                core.live.player_mut().set_shows_video(on);
                Ok(())
            })
        })
        .await?;
    Ok(on)
}

/// Saves what a video does when the player is left.
#[tauri::command]
pub async fn video_away_set(
    state: State<'_, AppState>,
    away: VideoAway,
) -> CommandResult<VideoAway> {
    state
        .video()
        .set_away(away)
        .map_err(|text| CommandError::new(Stage::ConfigLoad, &text))
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

/// Covers for what is on screen besides the queue — the play box's
/// suggestions (D-080). Only where each track lives is asked; what the cache
/// knows comes back at once, a cover found later arrives with the tick's
/// `artwork_ready`. The CLI's counterpart is `headshell artwork`: the covers of
/// what a query finds, waited for.
#[tauri::command]
pub async fn artwork_shown(
    state: State<'_, AppState>,
    items: Vec<QueueItem>,
) -> CommandResult<ArtworkReport> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.artwork_shown(&items) }))
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

// ————————————————————————————————————— Lyrics (D-078)
//
// `lyrics` is `headshell lyrics`, waiting for every track. The other three
// are the playing track's side: the core's lyrics worker looks it up in the
// background, the tick says when its answer changed, and the interface asks
// here. Opening the lyrics view is the user asking: `lyrics_view(true)` lets
// LRCLIB be asked for the playing track and every next one while it is open.

#[tauri::command]
pub async fn lyrics(
    app: AppHandle,
    state: State<'_, AppState>,
    query: Option<String>,
    all: bool,
    file: Option<String>,
    print: bool,
    shift: Option<i64>,
) -> CommandResult<LyricsReport> {
    state
        .run_on_core(move |core| {
            Box::pin(async move {
                let request = LyricsRequest {
                    query,
                    all,
                    file: file.map(std::path::PathBuf::from),
                    print,
                    shift_ms: shift,
                };
                let task = core.live.session_mut().lyrics(&core.registry, request);
                busy(&app, "looking up lyrics", task).await
            })
        })
        .await
}

/// The playing track's lyrics as they stand — with the lines, the shift and
/// why there are none. `null` when nothing plays.
#[tauri::command]
pub async fn lyrics_now(state: State<'_, AppState>) -> CommandResult<Option<LyricsItem>> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.lyrics_now() }))
        .await
}

/// The lyrics view opened or closed.
#[tauri::command]
pub async fn lyrics_view(
    state: State<'_, AppState>,
    open: bool,
) -> CommandResult<Option<LyricsItem>> {
    state
        .run_on_core(move |core| Box::pin(async move { core.live.set_lyrics_wanted(open) }))
        .await
}

/// Keeps the user's shift of a track's lyrics, in ms (positive: sooner). A
/// key the core never gave is the interface's mistake, and it is said (K9).
#[tauri::command]
pub async fn lyrics_shift(
    state: State<'_, AppState>,
    key: String,
    offset_ms: i64,
) -> CommandResult<Option<LyricsItem>> {
    let key = LyricsKey::parse(&key).ok_or_else(|| {
        CommandError::new(Stage::LyricsStore, &format!("not a lyrics key: `{key}`"))
    })?;
    state
        .run_on_core(move |core| Box::pin(async move { core.live.shift_lyrics(&key, offset_ms) }))
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

/// Whether the developer's details are shown (D-083). Like the theme, the
/// interface's own choice: no core call, no CLI counterpart.
#[tauri::command]
pub async fn debug_mode(state: State<'_, AppState>) -> CommandResult<DebugMode> {
    state.debug().mode().map_err(theme_error)
}

/// Saves the debug mode; `DEBUG=1` still holds a launch that sets it on.
#[tauri::command]
pub async fn debug_set(state: State<'_, AppState>, on: bool) -> CommandResult<DebugMode> {
    state.debug().set(on).map_err(theme_error)
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
    /// The mode the app goes by now (D-082); the `online` command says who
    /// decided it.
    pub lookup: LookupMode,
    /// `HEADSHELL_ONLINE` holds `lookup` for this launch (D-082): the
    /// settings switch cannot change it until the app starts without it.
    pub lookup_held: bool,
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
                    music_dirs: config.music_folders()?.dirs,
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    lookup: core.lookup,
                    lookup_held: session::lookup_mode_variable()?.is_some(),
                })
            })
        })
        .await
}

/// Whether the desktop goes online, and who decided it (D-082) —
/// `headshell online`.
#[tauri::command]
pub async fn online(state: State<'_, AppState>) -> CommandResult<OnlineReport> {
    state
        .run_on_core(move |core| Box::pin(async move { core.online() }))
        .await
}

/// Saves whether the desktop goes online and goes by it at once (D-082) —
/// `headshell online on|off`.
#[tauri::command]
pub async fn online_set(
    state: State<'_, AppState>,
    mode: LookupMode,
) -> CommandResult<OnlineReport> {
    state
        .run_on_core(move |core| Box::pin(async move { core.set_online(mode) }))
        .await
}
