//! The plugin engine: embedded QuickJS (D-069).
//!
//! Every plugin runs **on its own thread, in its own QuickJS runtime**. The
//! thread is started on the first call (lazily, like the process in api 1),
//! evaluates the script as an ES module, and then waits for work: the
//! caller sends a function name + arguments over a channel and waits for
//! the answer with a timeout.
//!
//! ## Why a separate thread
//!
//! 1. **Timeouts.** If the JS gets stuck in a loop, QuickJS's interrupt hook
//!    stops it when the time is up — not even `try/catch` can catch that
//!    (measured). But while the JS waits inside an engine call (HTTP, a
//!    tool) the hook cannot run; in that case what saves the caller is
//!    waiting for the answer on the channel **with a time limit**. On the
//!    same thread, a pending HTTP request would make the core wait too.
//! 2. **`Send`.** A QuickJS runtime cannot move between threads; a
//!    `Provider` has to be `Send + Sync`. Creating the runtime on its own
//!    thread and letting it die there removes the need for `rquickjs`'s
//!    `parallel` feature.
//!
//! ## What it holds and what it doesn't
//!
//! It holds: time (interrupt + wait limit), memory (a cap per runtime), the
//! stack (deep recursion turns into an exception), the network and files
//! (the plugin reaches the outside world only through `host`'s gates, see
//! [`super::host`]). Anything the JS throws — including running out of
//! memory — is an exception and does not bring the core down.
//!
//! It does not hold: a crash in QuickJS's **own** C code. In api 1 the
//! plugin was a separate process and the core survived if it died; in api 2
//! the plugin lives in the core's address space. This trade was made on
//! purpose in D-069: in return, plugins need no install, permissions are
//! enforced, and the engine can go to mobile (iOS does not allow spawning
//! subprocesses).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use rquickjs::function::Args;
use rquickjs::{CaughtError, Context, Ctx, Function, Module, Runtime, Value};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result, io_err};
use crate::provider::Capabilities;

use super::ScriptSpec;
use super::host::{self, HostState};
use super::protocol::export;

/// The memory cap of a plugin's runtime.
///
/// InnerTube's search response is ~1-2 MB of JSON, and its parsed form in
/// QuickJS is a few times that. If the cap is exceeded the JS gets an "out
/// of memory" exception — that call fails, not the core.
const MEMORY_LIMIT: usize = 128 * 1024 * 1024;

/// The JS stack cap. It must be smaller than the thread's stack
/// ([`THREAD_STACK`]): QuickJS checks its own limit and throws "Maximum call
/// stack size exceeded" before hitting the operating system's.
const JS_STACK_LIMIT: usize = 1024 * 1024;

/// The plugin thread's stack.
const THREAD_STACK: usize = 8 * 1024 * 1024;

/// How much longer the answer is waited for after the time is up.
///
/// The interrupt hook stops the JS when the time is up, but the answer takes
/// a few milliseconds to arrive on the channel; this margin closes that
/// race. If the margin runs out too, the plugin is stuck in an engine call
/// and is dropped.
const GRACE: Duration = Duration::from_secs(2);

/// How long shutdown waits for the thread to finish — so it has time to
/// delete the secret files.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// The small layer the engine gives the plugin before anything else:
/// `console`, and freezing `host`.
///
/// `console` is a browser object, not an ECMAScript one; QuickJS does not
/// have it. But plugin authors reach for it — it is wired to `host.log`.
const PRELUDE: &str = r#"
"use strict";
(() => {
  const show = (value) => {
    if (typeof value === "string") return value;
    if (value === undefined) return "undefined";
    if (value instanceof Error) return value.stack ? `${value}\n${value.stack}` : String(value);
    try {
      const text = JSON.stringify(value);
      return text === undefined ? String(value) : text;
    } catch (_) {
      return String(value);
    }
  };
  const line = (values) => values.map(show).join(" ");
  globalThis.console = Object.freeze({
    log: (...values) => host.log.info(line(values)),
    info: (...values) => host.log.info(line(values)),
    warn: (...values) => host.log.warn(line(values)),
    error: (...values) => host.log.error(line(values)),
    debug: (...values) => host.log.debug(line(values)),
  });
  for (const key of Object.keys(host)) {
    const value = host[key];
    if (value !== null && typeof value === "object") Object.freeze(value);
  }
  Object.freeze(host);
})();
"#;

/// The result of a call, from the thread to the caller.
#[derive(Debug)]
enum Outcome {
    Value(serde_json::Value),
    /// The JS threw an error.
    Threw {
        message: String,
        location: String,
    },
    /// The interrupt hook ran out the time.
    Interrupted,
    /// The plugin broke the contract (no such function, the return value cannot
    /// be turned into JSON, a promise that never resolves).
    Contract(String),
}

struct Job {
    function: &'static str,
    args: Vec<serde_json::Value>,
    timeout: Duration,
    /// Set by the caller's side to stop a call made ahead of time (D-077):
    /// the engine's gates look at it, and a running tool is stopped.
    cancel: Option<Arc<AtomicBool>>,
    reply: mpsc::SyncSender<Outcome>,
}

/// The end that talks to a plugin's thread.
pub(crate) struct ScriptWorker {
    plugin: String,
    jobs: Option<mpsc::Sender<Job>>,
    done: mpsc::Receiver<()>,
}

impl std::fmt::Debug for ScriptWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptWorker")
            .field("plugin", &self.plugin)
            .finish_non_exhaustive()
    }
}

impl ScriptWorker {
    /// Starts the thread, evaluates the script, checks the exports.
    ///
    /// # Errors
    /// If the script cannot be read, evaluation throws, the time runs out, or
    /// the function for a declared capability is not exported — all at
    /// [`Stage::PluginStart`].
    pub(crate) fn start(spec: ScriptSpec, timeout: Duration) -> Result<Self> {
        let plugin = spec.plugin.clone();
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<()>>(1);
        let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);

        std::thread::Builder::new()
            .name(format!("plugin:{plugin}"))
            .stack_size(THREAD_STACK)
            .spawn(move || {
                worker_main(spec, timeout, &job_rx, &ready_tx);
                let _ = done_tx.send(());
            })
            .map_err(|err| crashed(&plugin, format!("could not start the thread: {err}")))?;

        match ready_rx.recv_timeout(timeout + GRACE) {
            Ok(Ok(())) => Ok(Self {
                plugin,
                jobs: Some(job_tx),
                done: done_rx,
            }),
            Ok(Err(err)) => Err(err),
            // Once the channel is dropped, the thread exits as soon as it finishes
            // loading.
            Err(RecvTimeoutError::Timeout) => Err(timed_out(&plugin, "load", timeout)),
            Err(RecvTimeoutError::Disconnected) => Err(crashed(
                &plugin,
                "the thread fell over while loading".to_owned(),
            )),
        }
    }

    /// Calls an exported function; returns its value as JSON.
    ///
    /// # Errors
    /// A timeout ([`ErrorKind::PluginTimeout`]), a JS error
    /// ([`ErrorKind::PluginThrew`]), a contract violation
    /// ([`ErrorKind::PluginContract`]) or a dead thread
    /// ([`ErrorKind::PluginCrashed`]).
    pub(crate) fn call(
        &mut self,
        function: &'static str,
        args: Vec<serde_json::Value>,
        timeout: Duration,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<serde_json::Value> {
        let Some(jobs) = &self.jobs else {
            return Err(crashed(
                &self.plugin,
                "the engine had been shut down".to_owned(),
            ));
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        jobs.send(Job {
            function,
            args,
            timeout,
            cancel,
            reply: reply_tx,
        })
        .map_err(|_| crashed(&self.plugin, "the thread no longer exists".to_owned()))?;

        match reply_rx.recv_timeout(timeout + GRACE) {
            Ok(Outcome::Value(value)) => Ok(value),
            Ok(Outcome::Threw { message, location }) => Err(Error::new(
                Stage::ProviderCall,
                ErrorKind::PluginThrew {
                    plugin: self.plugin.clone(),
                    method: function.to_owned(),
                    message,
                    location,
                },
            )),
            Ok(Outcome::Interrupted) | Err(RecvTimeoutError::Timeout) => {
                Err(timed_out(&self.plugin, function, timeout))
            }
            Ok(Outcome::Contract(detail)) => Err(contract(&self.plugin, function, detail)),
            Err(RecvTimeoutError::Disconnected) => Err(crashed(
                &self.plugin,
                format!("the thread fell over during the {function} call"),
            )),
        }
    }

    /// Stops the thread and waits a short while for it to finish.
    ///
    /// The wait is for cleanup: as it exits, the thread deletes the secret files
    /// given to the plugin ([`super::host`]). A thread stuck in a call is not
    /// waited for; it exits on its own when the call it is stuck in ends.
    pub(crate) fn shutdown(&mut self) {
        if self.jobs.take().is_some() {
            let _ = self.done.recv_timeout(SHUTDOWN_WAIT);
        }
    }
}

impl Drop for ScriptWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn crashed(plugin: &str, detail: String) -> Error {
    Error::new(
        Stage::PluginStart,
        ErrorKind::PluginCrashed {
            plugin: plugin.to_owned(),
            detail,
        },
    )
}

fn timed_out(plugin: &str, method: &str, timeout: Duration) -> Error {
    let stage = if method == "load" {
        Stage::PluginStart
    } else {
        Stage::ProviderCall
    };
    Error::new(
        stage,
        ErrorKind::PluginTimeout {
            plugin: plugin.to_owned(),
            method: method.to_owned(),
            seconds: timeout.as_secs(),
        },
    )
}

fn contract(plugin: &str, method: &str, detail: String) -> Error {
    Error::new(
        Stage::ProviderCall,
        ErrorKind::PluginContract {
            plugin: plugin.to_owned(),
            method: method.to_owned(),
            detail,
        },
    )
}

/// The thread's body: set up, load, report ready, wait for work.
fn worker_main(
    spec: ScriptSpec,
    load_timeout: Duration,
    jobs: &mpsc::Receiver<Job>,
    ready: &mpsc::SyncSender<Result<()>>,
) {
    let plugin = spec.plugin.clone();
    let source = match std::fs::read_to_string(&spec.main) {
        Ok(source) => source,
        Err(err) => {
            let _ = ready.send(Err(io_err(Stage::PluginStart, &spec.main, err)));
            return;
        }
    };
    let module_name = spec.module_name.clone();
    let capabilities = spec.capabilities;

    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = ready.send(Err(crashed(
                &plugin,
                format!("could not set up QuickJS: {err}"),
            )));
            return;
        }
    };
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_max_stack_size(JS_STACK_LIMIT);

    // The interrupt hook and the engine calls see the same deadline: the hook
    // stops the JS, and engine calls stop themselves (before going to HTTP).
    let deadline: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let fired = Rc::new(Cell::new(false));
    {
        let deadline = Rc::clone(&deadline);
        let fired = Rc::clone(&fired);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            let hit = deadline.get().is_some_and(|at| Instant::now() >= at);
            if hit {
                fired.set(true);
            }
            hit
        })));
    }

    let context = match Context::full(&runtime) {
        Ok(context) => context,
        Err(err) => {
            let _ = ready.send(Err(crashed(
                &plugin,
                format!("could not set up the QuickJS context: {err}"),
            )));
            return;
        }
    };
    let host = Rc::new(HostState::new(spec, Rc::clone(&deadline)));

    context.with(|ctx| {
        let module = match load(&ctx, &host, &source, &module_name, load_timeout, &fired) {
            Ok(module) => module,
            Err(err) => {
                let _ = ready.send(Err(err));
                return;
            }
        };
        if let Err(detail) = check_exports(&module, capabilities) {
            let _ = ready.send(Err(Error::new(
                Stage::PluginStart,
                ErrorKind::PluginContract {
                    plugin: plugin.clone(),
                    method: "load".to_owned(),
                    detail,
                },
            )));
            return;
        }
        if ready.send(Ok(())).is_err() {
            // The caller stopped waiting (the load time ran out).
            return;
        }

        // When the channel closes (`shutdown`, or the provider was dropped) the
        // loop ends.
        while let Ok(job) = jobs.recv() {
            fired.set(false);
            deadline.set(Some(Instant::now() + job.timeout));
            host.set_cancel(job.cancel);
            let outcome = invoke(&ctx, &module, job.function, job.args, &fired);
            host.set_cancel(None);
            deadline.set(None);
            // If the caller stopped waiting, the answer has nowhere to go.
            let _ = job.reply.send(outcome);
        }
    });
    // `host` is dropped here: the secret files are deleted.
    drop(host);
}

/// `host` + `console` first, then the script. Evaluation is time-limited and
/// cannot go online.
fn load<'js>(
    ctx: &Ctx<'js>,
    host: &Rc<HostState>,
    source: &str,
    module_name: &str,
    timeout: Duration,
    fired: &Rc<Cell<bool>>,
) -> Result<Module<'js, rquickjs::module::Evaluated>> {
    let plugin = host.plugin().to_owned();
    let start_err = |detail: String| {
        Error::new(
            Stage::PluginStart,
            ErrorKind::PluginCrashed {
                plugin: plugin.clone(),
                detail,
            },
        )
    };

    host::install(ctx, Rc::clone(host))
        .map_err(|err| start_err(format!("could not set up the engine API: {err}")))?;
    let prelude: std::result::Result<(), _> = ctx.eval(PRELUDE);
    if let Err(err) = prelude {
        return Err(start_err(format!(
            "could not evaluate the prelude: {}",
            caught_text(ctx, err)
        )));
    }

    host.set_loading(true);
    host.set_deadline(Some(Instant::now() + timeout));
    let evaluated = (|| {
        let declared = Module::declare(ctx.clone(), module_name, source)?;
        let (module, promise) = declared.eval()?;
        promise.finish::<()>()?;
        Ok(module)
    })();
    host.set_deadline(None);
    host.set_loading(false);

    evaluated.map_err(|err: rquickjs::Error| {
        if fired.get() {
            return timed_out(&plugin, "load", timeout);
        }
        let (message, location) = describe_caught(CaughtError::from_error(ctx, err));
        Error::new(
            Stage::PluginStart,
            ErrorKind::PluginThrew {
                plugin: plugin.clone(),
                method: "load".to_owned(),
                message,
                location,
            },
        )
    })
}

/// Is the function for every declared capability exported.
fn check_exports(
    module: &Module<'_, rquickjs::module::Evaluated>,
    capabilities: Capabilities,
) -> std::result::Result<(), String> {
    let mut wanted = vec![(export::HEALTH, "every plugin")];
    if capabilities.contains(Capabilities::SEARCH) {
        wanted.push((export::SEARCH, "the `search` capability"));
    }
    if capabilities.contains(Capabilities::STREAM) {
        wanted.push((export::RESOLVE_SOURCE, "the `stream` capability"));
    }
    if capabilities.contains(Capabilities::ARTWORK) {
        wanted.push((export::ARTWORK, "`\"artwork\": true` in the manifest"));
    }
    if capabilities.contains(Capabilities::LYRICS) {
        wanted.push((export::LYRICS, "`\"lyrics\": true` in the manifest"));
    }
    let missing: Vec<String> = wanted
        .into_iter()
        .filter(|(name, _)| {
            !module
                .get::<_, Value>(*name)
                .is_ok_and(|value| value.is_function())
        })
        .map(|(name, why)| format!("`{name}` (required for {why})"))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the script does not export these functions: {} — provide them with `export function …`",
            missing.join(", ")
        ))
    }
}

/// Calls a function and turns the result into JSON.
fn invoke<'js>(
    ctx: &Ctx<'js>,
    module: &Module<'js, rquickjs::module::Evaluated>,
    function: &'static str,
    args: Vec<serde_json::Value>,
    fired: &Rc<Cell<bool>>,
) -> Outcome {
    let Ok(callee) = module.get::<_, Function>(function) else {
        return Outcome::Contract(format!("`{function}` is not exported"));
    };

    let mut list = Args::new(ctx.clone(), args.len());
    for arg in args {
        let parsed = serde_json::to_string(&arg)
            .map_err(|err| err.to_string())
            .and_then(|text| ctx.json_parse(text).map_err(|err| err.to_string()));
        match parsed.map(|value| list.push_arg(value)) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                return Outcome::Contract(format!("could not pass the arguments: {err}"));
            }
            Err(err) => return Outcome::Contract(format!("could not pass the arguments: {err}")),
        }
    }

    let returned: rquickjs::Result<Value> = callee.call_arg(list);
    let value = match returned {
        Ok(value) => value,
        Err(err) => return failure(ctx, err, fired),
    };
    // An `async function` returns a promise; the job queue runs until it
    // resolves.
    let value = match value.as_promise() {
        Some(promise) => match promise.finish::<Value>() {
            Ok(value) => value,
            Err(rquickjs::Error::WouldBlock) => {
                return Outcome::Contract(format!(
                    "`{function}` returned a promise and the promise never resolved — the engine has \
                     no timers, and there is no work left to wait for"
                ));
            }
            Err(err) => return failure(ctx, err, fired),
        },
        None => value,
    };

    match ctx.json_stringify(value) {
        Ok(Some(text)) => match text.to_string() {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(json) => Outcome::Value(json),
                Err(err) => {
                    Outcome::Contract(format!("could not turn the return value into JSON: {err}"))
                }
            },
            Err(err) => Outcome::Contract(format!("could not read the return value: {err}")),
        },
        // `undefined` and functions do not exist in JSON; they count as "no value".
        Ok(None) => Outcome::Value(serde_json::Value::Null),
        Err(err) => match failure(ctx, err, fired) {
            Outcome::Threw { message, .. } => Outcome::Contract(format!(
                "could not turn the return value into JSON: {message}"
            )),
            other => other,
        },
    }
}

fn failure(ctx: &Ctx<'_>, err: rquickjs::Error, fired: &Rc<Cell<bool>>) -> Outcome {
    if fired.get() {
        return Outcome::Interrupted;
    }
    let (message, location) = describe_caught(CaughtError::from_error(ctx, err));
    Outcome::Threw { message, location }
}

/// Turns a caught error into a `(message, location)` pair.
///
/// The location is the **first** line of the stack: `main.js:42:7`. Not the
/// whole stack — the diagnostics report must be one line, and what the
/// plugin author needs is where the error came from.
fn describe_caught(caught: CaughtError<'_>) -> (String, String) {
    match caught {
        CaughtError::Exception(exception) => {
            let message = exception
                .message()
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| "(error without a message)".to_owned());
            let location = exception
                .stack()
                .as_deref()
                .and_then(first_frame)
                .map(|frame| format!(" ({frame})"))
                .unwrap_or_default();
            (message, location)
        }
        CaughtError::Value(value) => {
            let text = value
                .as_string()
                .and_then(|text| text.to_string().ok())
                .unwrap_or_else(|| format!("{value:?}"));
            (
                format!("threw a value that is not an error object: {text}"),
                String::new(),
            )
        }
        CaughtError::Error(err) => (format!("engine error: {err}"), String::new()),
    }
}

/// `"    at search (main.js:42:7)\n..."` → `main.js:42:7`.
fn first_frame(stack: &str) -> Option<String> {
    let line = stack.lines().map(str::trim).find(|line| !line.is_empty())?;
    let inner = line
        .rsplit_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap_or_else(|| line.trim_start_matches("at ").trim());
    (!inner.is_empty()).then(|| inner.to_owned())
}

fn caught_text(ctx: &Ctx<'_>, err: rquickjs::Error) -> String {
    let (message, location) = describe_caught(CaughtError::from_error(ctx, err));
    format!("{message}{location}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_stack_frame_is_the_location() {
        assert_eq!(
            first_frame("    at search (main.js:42:7)\n    at <eval> (main.js:1:1)\n").as_deref(),
            Some("main.js:42:7")
        );
        assert_eq!(
            first_frame("    at main.js:3:1\n").as_deref(),
            Some("main.js:3:1")
        );
        assert_eq!(first_frame(""), None);
    }
}
