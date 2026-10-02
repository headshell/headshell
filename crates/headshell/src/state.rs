//! The thread that owns the core, and the way to send it work.
//!
//! **The Golden Rule:** no business logic here. This file is only a channel,
//! a constructor and an error envelope.
//!
//! ## Why its own thread and not a lock
//!
//! The measured constraint: `Session` is **`Send` but not `Sync`** — the
//! SQLite connection carries a `RefCell` — and the future `import_archive`
//! returns is not `Send` (`Box<dyn ExportArchive>` does not cross threads).
//! Tauri, on the other hand, wants every async command's future to be
//! `Send`.
//!
//! `Mutex<Core>` does not solve this: carrying the lock across an `.await`
//! needs `Core: Sync`, and `Core` is not `Sync`. The solution is to put the
//! core on a single thread: **no core type crosses the thread boundary**, only
//! job closures and serialisable results do.
//!
//! A side benefit: the tick loop lives on the same thread too, so there is
//! no lock race between commands and `tick()` — the channel puts them in
//! order.
//!
//! **The price is visible:** while a long `import` runs, the playback
//! controls wait in line too. So it does not stay silent, long commands send
//! a `headshell://busy` event (K9).

use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use headshell_core::config::Config;
use headshell_core::diag::Stage;
use headshell_core::playback::{LiveSession, Player};
use headshell_core::provider::ProviderRegistry;
use headshell_core::session::{LookupMode, OnlineReport, Session};

use crate::debug::DebugSwitch;
use crate::theme::ThemeStore;

/// Everything the core thread owns.
pub struct Core {
    pub live: LiveSession,
    /// The provider registry. It is refreshed with [`Core::refresh_registry`]
    /// when a server is added or removed: otherwise a new server would stay
    /// invisible until the app was closed.
    pub registry: ProviderRegistry,
    /// The desktop's `--online` (D-082): `HEADSHELL_ONLINE` for this launch
    /// when it is set, otherwise the saved choice, otherwise offline. Every
    /// command that goes online in the CLI with the flag goes online here
    /// with it; [`Core::set_online`] changes it while the app runs.
    pub lookup: LookupMode,
}

impl Core {
    /// Opens the library, sets up the providers, starts the cover and lyrics
    /// workers and starts with an **empty** player.
    ///
    /// The empty player is not a placeholder: a `LiveSession` should always
    /// exist so commands do not have to ask "is a session open". With nothing
    /// playing, `anchor` returns `Stopped` anyway.
    ///
    /// # Errors
    /// If the data directory cannot be opened, the database cannot be set up,
    /// the registered server file or `preferences.json` is corrupt,
    /// `HEADSHELL_ONLINE` holds anything but `1` or `0`, or the cover or
    /// lyrics cache cannot be opened.
    pub fn open(config: Config) -> headshell_core::Result<Self> {
        let session = Session::open(config)?;
        let lookup = session.online_choice()?.mode;
        let registry = headshell_core::provider::default_registry(session.config())?;
        let player = Player::new(registry.clone());
        let mut live = LiveSession::new(session, player);
        live.start_artwork(lookup)?;
        live.start_lyrics()?;
        Ok(Self {
            live,
            registry,
            lookup,
        })
    }

    /// Rebuilds the registry after the server list changed.
    ///
    /// # Errors
    /// If the registered server file cannot be read.
    pub fn refresh_registry(&mut self) -> headshell_core::Result<()> {
        self.registry = headshell_core::provider::default_registry(self.live.session().config())?;
        Ok(())
    }

    /// Saves whether the desktop goes online, and goes by it at once
    /// (D-082): the cover worker starts over in the new mode and looks the
    /// queue up again — an offline "not looked up" is not cached, so turning
    /// online on reaches the covers it skipped. While `HEADSHELL_ONLINE`
    /// holds this launch, the choice is saved for the next one and nothing
    /// changes now; the report says who decided.
    ///
    /// # Errors
    /// If `preferences.json` cannot be read or written, or the cover worker
    /// cannot start in the new mode.
    pub fn set_online(&mut self, mode: LookupMode) -> headshell_core::Result<OnlineReport> {
        let report = self.live.session().set_online(mode)?;
        self.go_by(report.online.mode)?;
        Ok(report)
    }

    /// Whether the desktop goes online, and who decided it (D-082). Reading
    /// it applies it: a choice saved from the CLI while the app runs takes
    /// effect here when it is read, so the switch never shows a mode the
    /// app is not in.
    ///
    /// # Errors
    /// As [`Self::set_online`].
    pub fn online(&mut self) -> headshell_core::Result<OnlineReport> {
        let report = self.live.session().online()?;
        self.go_by(report.online.mode)?;
        Ok(report)
    }

    /// Goes by `mode` from now on: the cover worker starts over in it.
    fn go_by(&mut self, mode: LookupMode) -> headshell_core::Result<()> {
        if mode != self.lookup {
            self.live.start_artwork(mode)?;
            self.lookup = mode;
        }
        Ok(())
    }
}

/// A job sent to the core thread.
///
/// The closure borrows `&mut Core` and writes its own result to its own
/// `oneshot`. So there is no need to write an enum variant per command — a
/// message type with 23 variants would be another guise of the translating
/// layer D-033 rejected.
pub type Job = Box<
    dyn for<'a> FnOnce(&'a mut Core) -> Pin<Box<dyn Future<Output = ()> + 'a>> + Send + 'static,
>;

/// The state Tauri manages: the end of the channel going to the core.
pub struct AppState {
    jobs: mpsc::UnboundedSender<Job>,
    /// The theme store (§3.3). It **does not enter** the core thread: a theme is
    /// not a core concept (see [`crate::theme`]), and there is no reason the
    /// interface's theme should not be changeable while a long `import` runs.
    themes: ThemeStore,
    /// The debug mode (D-083) — the interface's own choice, beside the theme.
    debug: DebugSwitch,
}

impl AppState {
    #[must_use]
    pub const fn new(
        jobs: mpsc::UnboundedSender<Job>,
        themes: ThemeStore,
        debug: DebugSwitch,
    ) -> Self {
        Self {
            jobs,
            themes,
            debug,
        }
    }

    #[must_use]
    pub const fn themes(&self) -> &ThemeStore {
        &self.themes
    }

    #[must_use]
    pub const fn debug(&self) -> &DebugSwitch {
        &self.debug
    }

    /// Runs a job on the core thread and waits for its result.
    ///
    /// # Errors
    /// If the core returns an error, the thread has died or the job finished
    /// without answering.
    pub async fn run_on_core<T, F>(&self, task: F) -> CommandResult<T>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(
                &'a mut Core,
            )
                -> Pin<Box<dyn Future<Output = headshell_core::Result<T>> + 'a>>
            + Send
            + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |core| {
            Box::pin(async move {
                let result = task(core).await;
                // If the receiver is gone the command was cancelled; the job was
                // done anyway and the core's state is consistent.
                let _ = tx.send(result);
            })
        });
        self.jobs.send(job).map_err(|_| core_thread_gone())?;
        rx.await
            .map_err(|_| core_thread_gone())?
            .map_err(CommandError::from)
    }
}

/// If the core thread is gone. We do not silently return an empty result
/// (K9).
fn core_thread_gone() -> CommandError {
    CommandError::new(Stage::ConfigLoad, "the core thread is not responding")
}

/// The error that goes to the webview.
///
/// D-033 forbade a separate IPC type for **data**; this is not a data type
/// but an error envelope. `headshell_core::Error` cannot be serialised (its
/// source chain carries `dyn Error`), but nothing is lost: the stage and the
/// full chain text go across — the same thing the CLI prints to `stderr`.
#[derive(Debug, Clone, Serialize)]
pub struct CommandError {
    /// A fixed stage name like `IDENTITY_RESOLVE`. The interface routes by it.
    pub stage: Stage,
    /// The full chain, starting with `STEP: ...`, that can be copied and pasted.
    pub chain: String,
}

impl CommandError {
    /// Builds an envelope for an error that did not come from the core — from a
    /// single place, so the format is the same as the core's (`STEP: <stage>` +
    /// the indented reason).
    #[must_use]
    pub fn new(stage: Stage, text: &str) -> Self {
        Self {
            stage,
            chain: format!("STEP: {stage}\n  {text}"),
        }
    }
}

impl From<headshell_core::Error> for CommandError {
    fn from(err: headshell_core::Error) -> Self {
        Self {
            stage: err.stage(),
            chain: err.chain_text(),
        }
    }
}

/// The return type of the commands.
pub type CommandResult<T> = std::result::Result<T, CommandError>;
