//! The plugin's only door to the outside world: the `host` object (D-069).
//!
//! QuickJS itself has no network, files or processes — only the language.
//! Whatever a plugin can do outside is given **here**, and every door does its
//! own check:
//!
//! | JS | What it does | Check |
//! |---|---|---|
//! | `host.http.request/get/post` | HTTP request; `binary: true` returns the body as base64 (api 3) | `permissions.net` on every request and every redirect; time limit; forbidden while loading |
//! | `host.secrets.get(k)` | the plugin's **own** secret | namespace (D-042) |
//! | `host.secrets.file(k)` | writes the secret to a `0600` temporary file, returns its path | only its own secret; deleted when the engine shuts down |
//! | `host.storage.get/set/remove` | persistent key-value store private to the plugin | the plugin's state directory; 1 MB cap |
//! | `host.tools.run(name, args, options)` | runs a tool the engine installed | only an artifact declared in the manifest with a verified hash; time limit |
//! | `host.log.*`, `console.*` | writes to `tracing` | — |
//!
//! All of them are **synchronous**: when the function returns, the work is
//! done. The engine has no event loop and no timers; `async function` can be
//! written, but engine calls do not return promises. An asynchronous API can
//! be added later without breaking `api` (adding does not break), not the
//! other way round.
//!
//! ## Why redirects are followed by hand
//!
//! The permission check looks at the request address. If the HTTP client
//! followed redirects itself, an allowed address could carry the plugin to a
//! forbidden one and the engine would never see it. That is why the client
//! given to plugins does not follow redirects
//! ([`crate::net::plugin_http_client`]); the engine follows every step and
//! asks again at every step.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rquickjs::function::Opt;
use rquickjs::{Coerced, Ctx, Exception, Function, IntoJs, Object, Value};
use serde::Deserialize;

use crate::net::{HttpClient, HttpHeader, HttpMethod, HttpRequest};

use super::ScriptSpec;
use super::artifact::{ArtifactStore, RequirementState};
use super::manifest::{Permissions, Requirement};
use super::protocol::PLUGIN_API;

/// The most redirects a request will follow.
const MAX_REDIRECTS: usize = 5;

/// The total cap of `host.storage` (key + value bytes).
///
/// The store is a cache (like a `client_id`), not a database; if the cap is
/// exceeded, the write is **refused** and old values are not silently
/// dropped.
const STORAGE_LIMIT: usize = 1024 * 1024;

/// What a call made ahead of time is told when it has to give way (D-077).
pub(crate) const GAVE_WAY: &str = "a call made ahead of time gave way to one the user asked for";

/// The most bytes taken from a tool's stdout/stderr.
///
/// The rest is read and discarded (so the tool does not block on a full
/// pipe), and the result says `truncated: true` — truncation is not silent.
const MAX_TOOL_OUTPUT: usize = 8 * 1024 * 1024;

/// A plugin's state on the engine side. Lives on the plugin's thread.
pub(crate) struct HostState {
    plugin: String,
    permissions: Permissions,
    secrets: BTreeMap<String, String>,
    state_dir: PathBuf,
    http: std::result::Result<Arc<dyn HttpClient>, String>,
    store: ArtifactStore,
    requires: Vec<Requirement>,
    /// Verified tool paths. The hash is checked on first use and the path kept
    /// here: re-hashing a 40 MB binary on every call would add seconds to a
    /// play request.
    tools: RefCell<BTreeMap<String, PathBuf>>,
    deadline: Rc<Cell<Option<Instant>>>,
    /// `true` while the module is being evaluated: network and tools are
    /// forbidden.
    loading: Cell<bool>,
    storage: RefCell<Option<BTreeMap<String, String>>>,
    secret_files: RefCell<BTreeMap<String, PathBuf>>,
    /// The running call's stop flag, when it was made ahead of time (D-077).
    cancel: RefCell<Option<Arc<AtomicBool>>>,
}

impl HostState {
    pub(crate) fn new(spec: ScriptSpec, deadline: Rc<Cell<Option<Instant>>>) -> Self {
        Self {
            plugin: spec.plugin,
            permissions: spec.permissions,
            secrets: spec.secrets,
            state_dir: spec.state_dir,
            http: spec.http,
            store: spec.store,
            requires: spec.requires,
            tools: RefCell::new(BTreeMap::new()),
            deadline,
            loading: Cell::new(false),
            storage: RefCell::new(None),
            secret_files: RefCell::new(BTreeMap::new()),
            cancel: RefCell::new(None),
        }
    }

    pub(crate) fn plugin(&self) -> &str {
        &self.plugin
    }

    pub(crate) fn set_loading(&self, loading: bool) {
        self.loading.set(loading);
    }

    pub(crate) fn set_deadline(&self, deadline: Option<Instant>) {
        self.deadline.set(deadline);
    }

    /// The running call's stop flag (D-077); `None` for a call made on the
    /// user's behalf, which nothing stops.
    pub(crate) fn set_cancel(&self, cancel: Option<Arc<AtomicBool>>) {
        *self.cancel.borrow_mut() = cancel;
    }

    /// Was the running call — one made ahead of time — asked to give way.
    fn stop_asked(&self) -> std::result::Result<(), String> {
        let asked = self
            .cancel
            .borrow()
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire));
        if asked {
            return Err(GAVE_WAY.to_owned());
        }
        Ok(())
    }

    /// The call's remaining time; if it ran out, why.
    fn remaining(&self) -> std::result::Result<Duration, String> {
        match self.deadline.get() {
            Some(at) => at
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .ok_or_else(|| "the call's time ran out".to_owned()),
            // There is no call without a time limit; still, we don't hand out an
            // unlimited budget.
            None => Ok(Duration::from_secs(20)),
        }
    }

    fn forbid_while_loading(&self, what: &str) -> std::result::Result<(), String> {
        if self.loading.get() {
            return Err(format!(
                "{what} is not possible while the module is loading — leave this to the first call (health, search…)"
            ));
        }
        Ok(())
    }

    fn log(&self, level: &str, message: &str) {
        match level {
            "debug" => tracing::debug!(plugin = %self.plugin, "{message}"),
            "warn" => tracing::warn!(plugin = %self.plugin, "{message}"),
            "error" => tracing::error!(plugin = %self.plugin, "{message}"),
            _ => tracing::info!(plugin = %self.plugin, "{message}"),
        }
    }

    // --- http ---------------------------------------------------------------

    fn http_request(&self, options: HttpOptions) -> std::result::Result<serde_json::Value, String> {
        self.forbid_while_loading("going online")?;
        let http = self.http.as_ref().map_err(Clone::clone)?;

        let mut method = match options.method.as_deref().map(str::to_ascii_uppercase) {
            None => HttpMethod::Get,
            Some(name) if name == "GET" => HttpMethod::Get,
            Some(name) if name == "POST" => HttpMethod::Post,
            Some(name) => {
                return Err(format!(
                    "`{name}` is not supported — the engine only sends GET and POST"
                ));
            }
        };
        let mut headers: Vec<HttpHeader> = options
            .headers
            .into_iter()
            .map(|(name, value)| HttpHeader::new(name, value))
            .collect();
        let mut body = options.body.map(String::into_bytes);
        let mut url = options.url;

        for hop in 0..=MAX_REDIRECTS {
            if let Err(reason) = self.permissions.check_url(&url) {
                tracing::warn!(plugin = %self.plugin, %url, "the plugin's request was refused: {reason}");
                return Err(reason);
            }
            self.remaining()?;
            self.stop_asked()?;

            let request = HttpRequest {
                method,
                url: url.clone(),
                headers: headers.clone(),
                body: body.clone(),
            };
            let response = super::block_on(http.send(&request))
                .map_err(|err| err.chain_text().replace('\n', " "))?;

            if matches!(response.status, 301 | 302 | 303 | 307 | 308)
                && let Some(location) = response.header("location")
            {
                if hop == MAX_REDIRECTS {
                    return Err(format!(
                        "gave up after {MAX_REDIRECTS} redirects (last address: {url})"
                    ));
                }
                url = resolve_location(&url, location)?;
                // What browsers do: 303 always, and 301/302 on POST, switch to GET.
                if response.status == 303
                    || (matches!(response.status, 301 | 302) && method == HttpMethod::Post)
                {
                    method = HttpMethod::Get;
                    body = None;
                    headers.retain(|header| !header.name.eq_ignore_ascii_case("content-type"));
                }
                continue;
            }

            let mut response_headers = serde_json::Map::new();
            for header in &response.headers {
                let name = header.name.to_ascii_lowercase();
                let value = match response_headers.get(&name).and_then(|v| v.as_str()) {
                    Some(previous) => format!("{previous}, {}", header.value),
                    None => header.value.clone(),
                };
                response_headers.insert(name, serde_json::Value::String(value));
            }
            // Binary (api 3, D-076): an image read as text is destroyed — every
            // byte that is not UTF-8 becomes U+FFFD. The body goes as base64,
            // and the response says which it is.
            let (body, encoding) = if options.binary {
                (crate::encoding::base64_standard(&response.body), "base64")
            } else {
                (response.text_lossy(), "text")
            };
            return Ok(serde_json::json!({
                "status": response.status,
                "ok": response.is_success(),
                "url": url,
                "headers": response_headers,
                "body": body,
                "encoding": encoding,
            }));
        }
        Err("redirect loop".to_owned())
    }

    // --- secrets ------------------------------------------------------------

    fn secret(&self, key: &str) -> Option<String> {
        self.secrets.get(key).cloned()
    }

    /// Writes the secret to a `0600` temporary file and returns its path.
    ///
    /// Its only user today is yt-dlp's cookie file (D-061): yt-dlp reads cookies
    /// only **from a file**. The door is kept narrow — the plugin cannot have the
    /// file written with content of its choosing, only **its own secret**; it
    /// cannot read the file either, it can only hand the path to a tool.
    fn secret_file(&self, key: &str) -> std::result::Result<Option<String>, String> {
        let Some(value) = self.secrets.get(key) else {
            return Ok(None);
        };
        if let Some(path) = self.secret_files.borrow().get(key) {
            return Ok(Some(path.display().to_string()));
        }
        let path = std::env::temp_dir().join(format!(
            "headshell-{}-{}-{}-{}",
            file_safe(&self.plugin),
            file_safe(key),
            std::process::id(),
            jiff::Timestamp::now().as_nanosecond()
        ));
        write_private(&path, value.as_bytes()).map_err(|err| {
            format!(
                "could not write the secret file ({}): {err}",
                path.display()
            )
        })?;
        self.secret_files
            .borrow_mut()
            .insert(key.to_owned(), path.clone());
        Ok(Some(path.display().to_string()))
    }

    // --- storage ------------------------------------------------------------

    fn storage_path(&self) -> PathBuf {
        self.state_dir.join("storage.json")
    }

    /// Hands the store (read from disk if needed) to `apply`.
    ///
    /// A corrupt store **does not count as empty**: resetting it silently would
    /// swallow the plugin's data. The error is thrown to the plugin, and by the
    /// plugin to the user.
    fn with_storage<T>(
        &self,
        apply: impl FnOnce(&mut BTreeMap<String, String>) -> T,
    ) -> std::result::Result<T, String> {
        let mut slot = self.storage.borrow_mut();
        if slot.is_none() {
            let path = self.storage_path();
            let loaded = match std::fs::read_to_string(&path) {
                Ok(text) => serde_json::from_str(&text).map_err(|err| {
                    format!(
                        "store is corrupt ({}): {err} — deleting the file resets it",
                        path.display()
                    )
                })?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(err) => {
                    return Err(format!(
                        "could not read the store ({}): {err}",
                        path.display()
                    ));
                }
            };
            *slot = Some(loaded);
        }
        match slot.as_mut() {
            Some(map) => Ok(apply(map)),
            None => Err("could not load the store".to_owned()),
        }
    }

    fn storage_get(&self, key: &str) -> std::result::Result<Option<String>, String> {
        self.with_storage(|map| map.get(key).cloned())
    }

    fn storage_write(&self, key: &str, value: Option<String>) -> std::result::Result<(), String> {
        let snapshot = self.with_storage(|map| {
            let mut next = map.clone();
            match &value {
                Some(value) => {
                    next.insert(key.to_owned(), value.clone());
                }
                None => {
                    next.remove(key);
                }
            }
            next
        })?;
        let size: usize = snapshot.iter().map(|(k, v)| k.len() + v.len()).sum();
        if size > STORAGE_LIMIT {
            return Err(format!(
                "the store cap would be exceeded ({size} bytes > {STORAGE_LIMIT}); not written"
            ));
        }
        let text = serde_json::to_string_pretty(&snapshot).map_err(|err| err.to_string())?;
        let path = self.storage_path();
        let temp = path.with_extension(format!("json.{}.writing", std::process::id()));
        std::fs::create_dir_all(&self.state_dir)
            .and_then(|()| std::fs::write(&temp, text))
            .and_then(|()| std::fs::rename(&temp, &path))
            .map_err(|err| {
                let _ = std::fs::remove_file(&temp);
                format!("could not write the store ({}): {err}", path.display())
            })?;
        *self.storage.borrow_mut() = Some(snapshot);
        Ok(())
    }

    // --- tools --------------------------------------------------------------

    /// The path of a tool that is declared, installed and whose hash matches.
    fn tool_path(&self, name: &str) -> std::result::Result<PathBuf, String> {
        if let Some(path) = self.tools.borrow().get(name) {
            return Ok(path.clone());
        }
        let Some(requirement) = self.requires.iter().find(|r| r.name == name) else {
            return Err(format!(
                "`{name}` is not an artifact declared in this plugin's manifest — the engine \
                 only runs the tools in `requires`"
            ));
        };
        match self.store.state_of(requirement) {
            Ok(RequirementState::Installed { path }) => {
                self.tools
                    .borrow_mut()
                    .insert(name.to_owned(), path.clone());
                Ok(path)
            }
            Ok(state) => Err(format!(
                "{name} is not ready: {} — the engine installs it, not the plugin: \
                 `headshell plugin install {}`",
                state.describe(),
                self.plugin
            )),
            Err(err) => Err(err.chain_text().replace('\n', " ")),
        }
    }

    fn run_tool(
        &self,
        name: &str,
        args: Vec<String>,
        options: ToolOptions,
    ) -> std::result::Result<serde_json::Value, String> {
        self.forbid_while_loading("running tools")?;
        self.stop_asked()?;
        let path = self.tool_path(name)?;
        let remaining = self.remaining()?;
        let budget = options
            .timeout_ms
            .map_or(remaining, |ms| Duration::from_millis(ms).min(remaining));

        let mut command = Command::new(&path);
        command
            .args(&args)
            .current_dir(&self.state_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            // A console program started from a desktop app flashes a console
            // window on every call.
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|err| format!("could not start {name} ({}): {err}", path.display()))?;

        let stdout = child.stdout.take().map(read_capped_in_background);
        let stderr = child.stderr.take().map(read_capped_in_background);

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                // A call made ahead of time gives way: the tool is stopped the
                // same way a timed-out one is (D-077).
                Ok(None) if self.stop_asked().is_err() => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{name} was stopped: {GAVE_WAY}"));
                }
                Ok(None) if started.elapsed() >= budget => {
                    let _ = child.kill();
                    let _ = child.wait();
                    // The readers are not waited for: the tool's own child (PyInstaller
                    // binaries start themselves in a subprocess) can keep the pipe
                    // open, and then we would be stuck here.
                    return Err(format!(
                        "{name} did not finish within {:.1} s and was stopped",
                        budget.as_secs_f64()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(err) => return Err(format!("error while waiting for {name}: {err}")),
            }
        };

        // If a reader died the output does not count as empty: with missing
        // output it would look as if "the tool said nothing" (K9).
        let collect =
            |reader: Option<std::thread::JoinHandle<(Vec<u8>, bool)>>, which: &str| match reader
                .map(std::thread::JoinHandle::join)
            {
                Some(Ok(output)) => Ok(output),
                Some(Err(_)) => Err(format!("could not read {name}'s output ({which})")),
                None => Err(format!(
                    "could not open a pipe to {name}'s output ({which})"
                )),
            };
        let (out, out_cut) = collect(stdout, "stdout")?;
        let (err, err_cut) = collect(stderr, "stderr")?;
        Ok(serde_json::json!({
            "code": status.code(),
            "stdout": String::from_utf8_lossy(&out),
            "stderr": String::from_utf8_lossy(&err),
            "truncated": out_cut || err_cut,
        }))
    }
}

impl Drop for HostState {
    fn drop(&mut self) {
        // The secret's copy on disk goes with the engine (D-061's rule: no
        // persistent copy of an account session is left behind).
        for path in self.secret_files.borrow().values() {
            if let Err(err) = std::fs::remove_file(path)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(
                    plugin = %self.plugin,
                    path = %path.display(),
                    "could not delete a secret file: {err}"
                );
            }
        }
    }
}

/// The options of `host.http.request`.
#[derive(Debug, Deserialize)]
struct HttpOptions {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    /// The response body as base64 instead of text (api 3, D-076).
    #[serde(default)]
    binary: bool,
}

/// The options of `host.tools.run`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolOptions {
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// Turns a redirect header into an absolute address.
fn resolve_location(base: &str, location: &str) -> std::result::Result<String, String> {
    let location = location.trim();
    if location.contains("://") {
        return Ok(location.to_owned());
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return Err(format!(
            "could not resolve the redirect: {base} → {location}"
        ));
    };
    if let Some(network_path) = location.strip_prefix("//") {
        return Ok(format!("{scheme}://{network_path}"));
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..authority_end]);
    if location.starts_with('/') {
        return Ok(format!("{origin}{location}"));
    }
    let path = &rest[authority_end..];
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    let directory = path.rfind('/').map_or("/", |at| &path[..=at]);
    Ok(format!("{origin}{directory}{location}"))
}

/// Makes text that goes into a file name harmless.
fn file_safe(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Writes a new file only its owner can read.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

/// Reads a pipe in the background, up to the cap; reads and discards the
/// rest.
///
/// The second item of the return value is "output incomplete": `true` if the
/// cap was exceeded **or** the read was cut short by an error. Both reach the
/// plugin as `truncated`; incomplete output must not look complete.
fn read_capped_in_background<R: Read + Send + 'static>(
    mut reader: R,
) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let room = MAX_TOOL_OUTPUT.saturating_sub(kept.len());
                    if read > room {
                        truncated = true;
                    }
                    kept.extend_from_slice(&buffer[..read.min(room)]);
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    truncated = true;
                    break;
                }
            }
        }
        (kept, truncated)
    })
}

// --- JS side ----------------------------------------------------------------

fn throw(ctx: &Ctx<'_>, message: &str) -> rquickjs::Error {
    Exception::throw_message(ctx, message)
}

/// Turns a JS value (an object) into a serde type — through JSON.
fn from_js<'js, T: serde::de::DeserializeOwned>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
    what: &str,
) -> rquickjs::Result<T> {
    let text = ctx
        .json_stringify(value)?
        .map(|text| text.to_string())
        .transpose()?
        .unwrap_or_else(|| "null".to_owned());
    serde_json::from_str(&text).map_err(|err| {
        Exception::throw_type(ctx, &format!("{what} is not in the expected form: {err}"))
    })
}

/// `null` if missing, a string if present — not `undefined`.
///
/// `rquickjs` turns Rust's `None` into `undefined`; the contract says "`null`
/// if missing" (like `localStorage.getItem`). The return value is pinned
/// here so a plugin checking with `=== null` is not fooled.
fn nullable<'js>(ctx: &Ctx<'js>, value: Option<String>) -> rquickjs::Result<Value<'js>> {
    match value {
        Some(text) => text.into_js(ctx),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

/// Turns a `serde_json` value into a JS value.
fn to_js<'js>(ctx: &Ctx<'js>, value: &serde_json::Value) -> rquickjs::Result<Value<'js>> {
    let text = serde_json::to_string(value).map_err(|err| throw(ctx, &err.to_string()))?;
    ctx.json_parse(text)
}

/// Sets up the `host` object and puts it in the global scope.
pub(crate) fn install<'js>(ctx: &Ctx<'js>, state: Rc<HostState>) -> rquickjs::Result<()> {
    let host = Object::new(ctx.clone())?;
    host.set("api", PLUGIN_API)?;
    host.set("name", "headshell")?;
    host.set("version", env!("CARGO_PKG_VERSION"))?;
    host.set("platform", state.store.platform().to_owned())?;
    host.set("plugin", state.plugin.clone())?;

    let log = Object::new(ctx.clone())?;
    for level in ["debug", "info", "warn", "error"] {
        let state = Rc::clone(&state);
        log.set(
            level,
            Function::new(ctx.clone(), move |message: Coerced<String>| {
                state.log(level, &message.0);
            })?,
        )?;
    }
    host.set("log", log)?;

    let secrets = Object::new(ctx.clone())?;
    {
        let state = Rc::clone(&state);
        secrets.set(
            "get",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, key: String| -> rquickjs::Result<Value<'js>> {
                    nullable(&ctx, state.secret(&key))
                },
            )?,
        )?;
    }
    {
        let state = Rc::clone(&state);
        secrets.set(
            "file",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, key: String| -> rquickjs::Result<Value<'js>> {
                    let path = state.secret_file(&key).map_err(|err| throw(&ctx, &err))?;
                    nullable(&ctx, path)
                },
            )?,
        )?;
    }
    host.set("secrets", secrets)?;

    let storage = Object::new(ctx.clone())?;
    {
        let state = Rc::clone(&state);
        storage.set(
            "get",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, key: String| -> rquickjs::Result<Value<'js>> {
                    let value = state.storage_get(&key).map_err(|err| throw(&ctx, &err))?;
                    nullable(&ctx, value)
                },
            )?,
        )?;
    }
    {
        let state = Rc::clone(&state);
        storage.set(
            "set",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, key: String, value: Coerced<String>| -> rquickjs::Result<()> {
                    state
                        .storage_write(&key, Some(value.0))
                        .map_err(|err| throw(&ctx, &err))
                },
            )?,
        )?;
    }
    {
        let state = Rc::clone(&state);
        storage.set(
            "remove",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, key: String| -> rquickjs::Result<()> {
                    state
                        .storage_write(&key, None)
                        .map_err(|err| throw(&ctx, &err))
                },
            )?,
        )?;
    }
    host.set("storage", storage)?;

    let http = Object::new(ctx.clone())?;
    {
        let state = Rc::clone(&state);
        http.set(
            "request",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>, options: Value<'js>| -> rquickjs::Result<Value<'js>> {
                    let options: HttpOptions = from_js(&ctx, options, "request options")?;
                    let response = state
                        .http_request(options)
                        .map_err(|err| throw(&ctx, &err))?;
                    to_js(&ctx, &response)
                },
            )?,
        )?;
    }
    {
        let state = Rc::clone(&state);
        http.set(
            "get",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      url: String,
                      headers: Opt<Value<'js>>|
                      -> rquickjs::Result<Value<'js>> {
                    let headers = match headers.0 {
                        Some(value) if !value.is_undefined() && !value.is_null() => {
                            from_js(&ctx, value, "headers")?
                        }
                        _ => BTreeMap::new(),
                    };
                    let response = state
                        .http_request(HttpOptions {
                            url,
                            method: None,
                            headers,
                            body: None,
                            binary: false,
                        })
                        .map_err(|err| throw(&ctx, &err))?;
                    to_js(&ctx, &response)
                },
            )?,
        )?;
    }
    {
        let state = Rc::clone(&state);
        http.set(
            "post",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      url: String,
                      body: Coerced<String>,
                      headers: Opt<Value<'js>>|
                      -> rquickjs::Result<Value<'js>> {
                    let headers = match headers.0 {
                        Some(value) if !value.is_undefined() && !value.is_null() => {
                            from_js(&ctx, value, "headers")?
                        }
                        _ => BTreeMap::new(),
                    };
                    let response = state
                        .http_request(HttpOptions {
                            url,
                            method: Some("POST".to_owned()),
                            headers,
                            body: Some(body.0),
                            binary: false,
                        })
                        .map_err(|err| throw(&ctx, &err))?;
                    to_js(&ctx, &response)
                },
            )?,
        )?;
    }
    host.set("http", http)?;

    let tools = Object::new(ctx.clone())?;
    {
        let state = Rc::clone(&state);
        tools.set(
            "run",
            Function::new(
                ctx.clone(),
                move |ctx: Ctx<'js>,
                      name: String,
                      args: Opt<Vec<Coerced<String>>>,
                      options: Opt<Value<'js>>|
                      -> rquickjs::Result<Value<'js>> {
                    let args = args
                        .0
                        .unwrap_or_default()
                        .into_iter()
                        .map(|arg| arg.0)
                        .collect();
                    let options = match options.0 {
                        Some(value) if !value.is_undefined() && !value.is_null() => {
                            from_js(&ctx, value, "tool options")?
                        }
                        _ => ToolOptions::default(),
                    };
                    let outcome = state
                        .run_tool(&name, args, options)
                        .map_err(|err| throw(&ctx, &err))?;
                    to_js(&ctx, &outcome)
                },
            )?,
        )?;
    }
    host.set("tools", tools)?;

    ctx.globals().set("host", host)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_resolve_like_a_browser_would() {
        let base = "https://a.example.com/dir/page?q=1";
        assert_eq!(
            resolve_location(base, "https://b.example.com/x").unwrap(),
            "https://b.example.com/x"
        );
        assert_eq!(
            resolve_location(base, "//c.example.com/y").unwrap(),
            "https://c.example.com/y"
        );
        assert_eq!(
            resolve_location(base, "/root").unwrap(),
            "https://a.example.com/root"
        );
        assert_eq!(
            resolve_location(base, "sibling").unwrap(),
            "https://a.example.com/dir/sibling"
        );
        assert_eq!(
            resolve_location("https://a.example.com", "path").unwrap(),
            "https://a.example.com/path"
        );
    }

    #[test]
    fn file_names_derived_from_plugin_input_cannot_escape() {
        assert_eq!(file_safe("../../etc"), "______etc");
    }
}
