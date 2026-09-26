# Writing plugins (contract api 3)

`headshell` plugins are written in **JavaScript** and run in the **QuickJS**
engine embedded in `headshell` (D-069). No Python, Node or other runtime is
needed on the user's machine: putting the plugin in a directory is enough.

A plugin can reach the outside world only through the `host` object the engine
gives it — HTTP, secrets, a small persistent store, tools the engine installs,
and logging. It has no direct access to the file system, to processes or to
sockets. That's why the permissions it declares **are enforced**.

Plugins don't live in this repository: they live in the
[`headshell/plugins`](https://github.com/headshell/plugins) catalog, and users
install them from there (D-071, §9).

Three working examples:

- [`soundcloud/main.js`](https://github.com/headshell/plugins/blob/main/soundcloud/main.js)
  — **a real plugin.** It connects to a live service, discovers its key and
  caches it in `host.storage`. This is the example closest to what you'll
  write.
- [`ytmusic/main.js`](https://github.com/headshell/plugins/blob/main/ytmusic/main.js)
  — a plugin that takes its metadata from a service and its audio from **a
  tool the engine installs** (yt-dlp).
- [`fixtures/plugins/echo/main.js`](../fixtures/plugins/echo/main.js) — a test
  plugin with a fixed catalog and a cover (~70 lines). For seeing the contract
  bare.

> **If you're coming from api 2:** api 3 (D-076) adds covers. The manifest
> **must** say whether the plugin gives them — `"artwork": true` or
> `"artwork": false`; an api 2 manifest doesn't, so it no longer loads. With
> `false`, raising `"api"` to `3` and adding that one line is the whole port.
> With `true`, export `artwork(id, size)` (§3) and fetch the image with
> `binary: true` (§4).
>
> **If you're coming from api 1:** api 1 was a subprocess + JSON-RPC protocol,
> and plugins were written in Python. api 1 plugins **no longer load**;
> `headshell plugin list` shows them as "protocol version mismatch: plugin
> api 1". Porting is short: `main` instead of `exec`, exported functions
> instead of methods, `host.http` instead of `urllib`, `host.storage` instead
> of files. The SoundCloud and YouTube Music plugins followed this path.

---

## 1. What a plugin is

A directory:

```
<data-directory>/plugins/soundcloud/
├── plugin.json      # manifest
└── main.js          # script
```

The data directory differs from system to system (D-070); `headshell diag`
prints the one in use:

| System | Data directory |
|---|---|
| Linux, BSD | `~/.local/share/headshell` (under `$XDG_DATA_HOME` if it is set) |
| macOS | `~/Library/Application Support/headshell` |
| Windows | `%LOCALAPPDATA%\headshell` |

The `HEADSHELL_DATA_DIR` environment variable or the CLI's `--data-dir` flag
comes first on every system. The commands below use the Linux path; on the
other systems only the directory changes.

**The directory name is the ID.** The `name` in `plugin.json` has to be the
same as the directory name; if they don't match, the plugin is rejected.

### `plugin.json`

```json
{
  "name": "soundcloud",
  "display_name": "SoundCloud",
  "version": "0.3.0",
  "api": 3,
  "artwork": true,
  "main": "main.js",
  "capabilities": ["search", "stream"],
  "permissions": {
    "net": ["soundcloud.com", "api-v2.soundcloud.com", "*.sndcdn.com"]
  },
  "requires": [],
  "description": "One sentence."
}
```

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | The same as the directory name. The provider ID. |
| `display_name` | yes | The name shown to the user. |
| `version` | yes in the catalog | The plugin's own version. Every plugin in the catalog is versioned: that is what catches an update, and the file addresses are pinned to it (§9). |
| `api` | yes | The contract version it speaks: `3` today. |
| `artwork` | yes | Does the plugin give covers: `true` — it exports `artwork(id, size)` (§3) — or `false`. There is no default: a missing field is rejected, so "it gives none" is always a statement, never a guess. |
| `main` | yes | The script's path relative to the plugin directory. It must be `.js` and can't leave the directory (`..` and absolute paths are rejected). |
| `capabilities` | no | `search`, `stream`. The engine checks that the function of every declared capability is exported. |
| `permissions.net` | no | The hosts to connect to. See §2. |
| `requires` | no | Tools the engine will install. See §5. |
| `description` | no | One sentence. |

Two api 1 fields **are rejected**, not ignored: `exec` (a plugin is a script,
not a command) and `permissions.fs` (a plugin can't access the file system).

---

## 2. Permissions — enforced

On every `host.http` request, on **every redirect** followed and on the stream
address `resolve_source` returns, the engine checks the host against
`permissions.net`. A request to an undeclared address never reaches the
network; the plugin gets a thrown `not allowed: <host> …` error.

- **An exact name:** `api.soundcloud.com` covers only that name, not its
  subdomains.
- **A wildcard:** `*.sndcdn.com` covers every subdomain
  (`cf-media.sndcdn.com`) but **not** `sndcdn.com` itself. It can only be
  leftmost. A bare `*` and a single-label wildcard like `*.com` are rejected —
  a plugin that says "I go everywhere" has to write them out one by one, or the
  user should reject it.
- **No scheme, port or path is written.** Only `http` and `https` can be
  reached; any port is allowed.
- An IP address can be written (`127.0.0.1`), but IPv6 addresses in square
  brackets aren't supported.

**Consent:** on first install the user approves the permissions with
`headshell plugin approve <name>`. The approved set is stored in
`<data-directory>/plugins.json`. If your plugin is updated and asks for
**more** permissions, it doesn't load until it is approved again; if it asks
for **fewer**, that's fine. Wildcards are taken into account: an approved
`*.x.com` covers an `a.x.com` asked for later.

**The only thing outside the boundary is the tools the engine installs.** A
program run with `host.tools.run` (yt-dlp) is a separate process and isn't
jailed; its own network traffic isn't limited by this list.
`headshell plugin list` writes this every time.

---

## 3. The contract: exported functions

The script is an **ES module**. The core evaluates it on the first call and
calls the functions it exports:

```js
export function health() {
  return { reachable: true, detail: "all is well" };
}

export function search(query, limit) {
  return [{ id: "42", artist: "Ezhel", title: "Geceler", duration_ms: 213000 }];
}

export function resolve_source(id) {
  return { kind: "http_stream", url: `https://cdn.example.com/${id}.mp3`, headers: [] };
}
```

| Function | Required | Returns |
|---|---|---|
| `health()` | yes | `{ reachable, detail?, track_count? }` |
| `search(query, limit)` | if it has the `search` capability | an array of tracks |
| `resolve_source(id)` | if it has the `stream` capability | a source, or `null` |
| `artwork(id, size)` | if the manifest says `"artwork": true` | a cover, or `null` |

The names are deliberately the same as api 1's method names and the core's
`Provider` trait (`resolve_source`, not `resolveSource`). You can write an
`async function` too; the engine resolves the promise.

### `health()`

Being unreachable is **an answer**, not an error: return
`{ reachable: false, detail: "…" }` and write the reason. If `track_count` is
unknown, leave it `null` — giving a wrong number is worse than "I don't know".
If you throw, the core shows it as "unreachable" too, with the message you
threw.

### `search(query, limit)`

```js
[
  {
    id: "42",               // required — a bare ID
    artist: "Ezhel",         // required
    title: "Geceler",        // required
    album: "Müptezhel",      // optional
    duration_ms: 213000,     // optional, but important for fuzzy matching
    isrc: "TRA111700001"     // optional; if the format doesn't fit, it is dropped and counted
  }
]
```

`id` is a **bare** string. The core adds the provider name — you can't make up
an ID in another provider's namespace. If there are no results, return `[]`.

### `resolve_source(id)`

```js
{ kind: "http_stream", url: "https://…", headers: [{ name: "Range", value: "bytes=0-" }] }
```

`null` means "this track can't be played" and isn't an error. The address must
be within your permissions (§2); if it's outside them, the core rejects the
source. `local_file` can't be returned — the plugin has no access to the file
system.

**Audio is never relayed (K3):** the core fetches the address you return
itself.

### `artwork(id, size)`

The cover of one of your tracks, for the "now playing" record and the queue
(D-076). `size` is the edge the app wants, in pixels (`500` today); give the
nearest size the service has.

```js
{ mime: "image/jpeg", data: "/9j/4AAQSkZJRg…" }   // data: the image's bytes, base64
```

**You fetch the image, not the core.** The core never downloads an address a
plugin gives it (K5): fetch it yourself with
`host.http.request({ url, binary: true })` — within your permissions (§2) — and
return the body as it comes. `mime` is optional; the core looks at the bytes
anyway. JPEG, PNG, GIF and WebP are accepted, up to 16 MB and 8000 px; the core
resizes it for the label and the queue.

`null` means "this track has no cover" — not "a portrait of the uploader" or a
video frame: return `null` rather than something that isn't a cover. Either
way, `null` or an error, the app goes on to its own chain: a local file's tags
and folder, and — when the user runs it online — MusicBrainz and the Cover Art
Archive. An error is shown with its reason, as it is for the other calls.

### Raising an error

Throw an ordinary JS error:

```js
throw new Error("SoundCloud's quota is used up (429); wait a while");
```

The core reads this as a **refusal** and doesn't restart the plugin. The user
sees your message and the line the error came from:
`the soundcloud plugin failed on the search call: … (main.js:42:7)`.
"I couldn't find it" and "I couldn't look" are different things (K9): if there
are no results, return `[]`/`null`; if something broke, throw.

A return that doesn't fit the contract (an object instead of an array, a source
whose `kind` isn't recognised) is reported separately as **a contract
violation**: the user can't fix it by waiting; only you can.

---

## 4. `host` — the gate to the outside world

All of it is **synchronous**: when the function returns, the work is done. The
engine has no event loop and no timers (no `setTimeout`).

| API | What it does |
|---|---|
| `host.http.get(url, headers?)` | GET. The headers are a plain object: `{ "User-Agent": "…" }`. |
| `host.http.post(url, body, headers?)` | POST; the body is a string. |
| `host.http.request({ url, method?, headers?, body?, binary? })` | The general form; `method` is `GET` or `POST`. `binary: true` returns the body as base64 (api 3). |
| `host.secrets.get(key)` | A secret in your own namespace; `null` if there is none. |
| `host.secrets.file(key)` | Writes the secret to a `0600` temporary file and returns its path (`null` if there is none). Deleted when the engine shuts down. For handing it to a tool as a file path. |
| `host.storage.get(key)` / `.set(key, value)` / `.remove(key)` | A persistent key-value store private to the plugin; the values are strings. 1 MB in total. |
| `host.tools.run(name, args, { timeoutMs? })` | Runs a tool declared in `requires`, installed and with its hash verified. |
| `host.log.debug/info/warn/error(message)` | Writes to `tracing`; visible with `headshell -v`. |
| `console.log/info/warn/error/debug` | Tied to `host.log`. |
| `host.api`, `host.version`, `host.platform`, `host.plugin` | The contract version, the core version, the platform key (`linux-x86_64`), the plugin's name. |

**The HTTP response:**

```js
{ status: 200, ok: true, url: "the final address", headers: { "content-type": "…" }, body: "…", encoding: "text" }
```

Status codes outside 2xx **aren't thrown**; they come back in `status` —
telling a 404 from a 500 is your job. It throws if the network can't be
reached, if there's no permission, or if the call's time ran out. The engine
follows redirects (at most 5) and asks for permission again at every step. The
body arrives as text (UTF-8, broken bytes replaced) — unless the request said
`binary: true`: then it arrives as base64 and `encoding` says `"base64"`. An
image read as text is destroyed; that is what the binary mode is for.

**Tool output:**

```js
{ code: 0, stdout: "…", stderr: "…", truncated: false }
```

The tool's time can't exceed the call's remaining time; `timeoutMs` sets a
shorter limit. If the time runs out, the tool is stopped and an error is
thrown.

### While the module loads

The script's top level (outside the functions) runs once while loading and
**can't reach the network or run tools** — those jobs belong to the first call.
Loading is limited to 5 seconds.

### Limits

| | |
|---|---|
| Call time | 20 s. Code stuck in a loop is cut off (`try/catch` can't catch the interruption). |
| Load time | 5 s |
| Memory | 128 MB; beyond that an "out of memory" exception — that call fails, not the core |
| A single HTTP request | 15 s |
| Module | A single file; no other file can be loaded with `import` |
| Web APIs | None: `fetch`, `URL`, `TextEncoder`, `Intl`, `setTimeout`. The language and its standard library (JSON, RegExp, Map, Date…) are there. |

---

## 5. Tools: `requires` (D-049, D-055, D-069)

**No plugin may ask the user for root privileges or a system-wide install**
(D-049). You don't make the user install your tool; you declare it in the
manifest, and **the engine** installs it:

```json
"requires": [
  {
    "name": "yt-dlp",
    "version": "2026.08.19",
    "assets": {
      "linux-x86_64":   { "url": "https://…/yt-dlp_linux",   "sha256": "58162f9b…" },
      "macos-aarch64":  { "url": "https://…/yt-dlp_macos",   "sha256": "0f192b7e…" },
      "windows-x86_64": { "url": "https://…/yt-dlp.exe",     "sha256": "66674953…" }
    }
  }
]
```

- The artifact is declared **per platform**. The key is
  `<operating system>-<architecture>`, with `-musl` appended for musl Linux.
  The recognised keys: `linux-x86_64`, `linux-aarch64`, `linux-x86`,
  `linux-arm`, `linux-x86_64-musl`, `linux-aarch64-musl`, `macos-x86_64`,
  `macos-aarch64`, `windows-x86_64`, `windows-aarch64`, `windows-x86`. An
  unknown key makes the manifest invalid (so a typo doesn't turn into "not on
  this platform").
- Every release must be a **self-contained** binary — one that needs no other
  interpreter (yt-dlp's zipapp needs Python; the PyInstaller binaries don't).
- `url` must be `https://`, and `sha256` 64 digits. If the hash doesn't match,
  the file isn't put in place.
- If the user's platform isn't in the map, the plugin doesn't load and the
  status line says so; no install command is suggested, because installing
  won't fix it.
- The artifact lands under `<data-directory>/runtime/<name>-<version>-<platform>`
  (with `.exe` on Windows). Nothing is written to the system.

The plugin calls the tool by name; it doesn't need to know its path:

```js
const run = host.tools.run("yt-dlp", ["--version"], { timeoutMs: 5000 });
```

The engine verifies the tool again by its hash on first use: a file changed
after installation isn't run.

---

## 6. Lifecycle and resilience

- The engine opens for your plugin **on the first call**, not on installation.
  Every plugin runs on its own thread, in its own QuickJS runtime; plugins
  can't see each other's state.
- An error you throw doesn't bring the engine down: the module state (the
  variables you cached) is still there on the next call.
- If the time runs out, the engine is dropped and the next call loads the
  script **from the start** — it doesn't carry on with the state a half-done
  call left behind.
- After three starts it gives up; endless restarts would hide a crash loop. On
  a contract violation (a missing export) it isn't retried at all.
- **A call made ahead of time can give way (D-077).** The core reads the
  queue ahead and looks covers up in the background; meanwhile a call the
  user makes goes first. A tool your `resolve_source` or `artwork` is running
  for a call made ahead of time is then stopped: `host.tools.run` throws, and
  so does a `host.http` request started after that. Let the error through —
  the core counts it as "gave way", not as a failure, and asks again later.
  Catching it and returning `null` would read as "this track has no source".
- **A known limit:** a crash in QuickJS's own C code brings the core down too —
  in api 1 the plugin was a separate process; since api 2 it isn't (D-069's
  trade-off). Nothing JS can do (an infinite loop, a memory overflow, deep
  recursion) falls into this class.

---

## 7. The versioning rule

`api` is a single integer, and its rule is the same as the theme contract's:

> **Adding doesn't raise the version; removing something or changing its
> meaning does.**

Adding a new function to `host`, a new field to the manifest or a new platform
to `PLATFORMS` doesn't raise `api`. The move from api 1 → 2 was of the second
kind: where the plugin runs changed. So was api 2 → 3 (D-076): the manifest
gained a **required** field (`artwork`), and an api 2 manifest doesn't have
it — an optional field would have left "gives no covers" and "was written
before covers" looking the same.

---

## 8. Installing and testing while developing

Put the plugin you're developing in place by hand, as a directory. The catalog
**doesn't touch a plugin placed by hand** — an update doesn't write over it —
so you can also put your working copy in place through a link;
`headshell plugin remove` removes only the link and doesn't touch your copy.

```bash
# Put the plugin in place (Linux; for the macOS and Windows path, the table in §1)
mkdir -p ~/.local/share/headshell/plugins/mine
cp plugin.json main.js ~/.local/share/headshell/plugins/mine/

# Does it show up, and what does it ask for?
headshell plugin list

# Approve its permissions
headshell plugin approve mine

# If it wants a tool
headshell plugin install mine

# If it needs a secret (the value isn't typed on the command line)
headshell secret set plugin:mine client_id

# Is it up?
headshell provider test mine

# If something goes wrong: it says at which stage it broke
headshell diag
```

The placing step on Windows (PowerShell):

```powershell
$target = "$env:LOCALAPPDATA\headshell\plugins\mine"
New-Item -ItemType Directory -Force -Path $target | Out-Null
Copy-Item plugin.json, main.js $target
```

`headshell plugin disable <name>` turns it off (the consent is kept), `enable`
turns it back on, `forget` forgets the consent entirely, and `remove` removes
the plugin and forgets its consent.

Error messages carry the stage: `PLUGIN_LOAD` (manifest/consent),
`PLUGIN_CATALOG` (the catalog: index, download, hash, update, removal),
`PLUGIN_RUNTIME` (tool installation), `PLUGIN_START` (loading the script,
checking the exports), `PROVIDER_CALL` (the call).

---

## 9. The catalog: `headshell/plugins` (D-071)

Users install plugins from the
[`headshell/plugins`](https://github.com/headshell/plugins) catalog:

```bash
headshell plugin catalog              # what's available, what's installed, what can be updated
headshell plugin install soundcloud   # downloads it and verifies every file's sha256
headshell plugin approve soundcloud   # an installed plugin waits for your approval
headshell plugin update               # updates the ones installed from the catalog
```

The `index.json` at the root of the catalog repository carries every plugin's
manifest and the address + sha256 of its files. The client verifies every file
by its hash and compares the downloaded `plugin.json` with what the index
shows; if either doesn't match, nothing is written to disk. The index isn't
written by hand; it is generated with the core's own verification:

```bash
headshell plugin index <catalog-repo>           # write index.json
headshell plugin index <catalog-repo> --check   # is it up to date (CI)
```

**Publishing:** a PR to the catalog repository — `<name>/plugin.json`, the
script and a regenerated `index.json`. The name is lowercase ASCII (letters,
digits, `-`, `_`, `.`), and `version` is required. The file addresses are
pinned to the `<name>-<version>` tag; a version's files don't change after it
is published — a fix is a new version. The details and the publishing steps are
in the catalog repository's README.

The catalog is read only by these commands; the app doesn't go to the network
at startup or in the background. For another catalog,
`HEADSHELL_PLUGIN_INDEX=<address>`: the address must be `https://` (plain
`http` only for `127.0.0.1`/`localhost`).

Usage notes per plugin — SoundCloud's `client_id` discovery, YouTube Music's
cookies, yt-dlp's platform list and version pinning — are in the README of
each plugin's directory:
[`soundcloud/`](https://github.com/headshell/plugins/tree/main/soundcloud),
[`ytmusic/`](https://github.com/headshell/plugins/tree/main/ytmusic).
