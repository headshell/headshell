//! Plugin manifest (`plugin.json`, api 2) and permission declaration
//! (D-040, D-069).
//!
//! A plugin is a directory with a `plugin.json` in it; **the directory name
//! is the identity** (the theme system's rule, D-037). The manifest's `name`
//! must match the directory name: if it does not, it is rejected — the
//! directory name is not used silently, because which name wins must not be
//! a guess (K9).
//!
//! ## api 1 → api 2
//!
//! In api 1 the manifest pointed at a **command** (`exec`), the plugin was a
//! subprocess and the permission declaration was only a contract. In api 2
//! the manifest points at a **script** (`main`), the script runs in QuickJS
//! inside the core, and the network permission is **enforced**: the plugin
//! can reach the outside world only through the gates the engine provides,
//! and the engine checks the declaration at every gate (D-069).
//!
//! Two api 1 fields lost their meaning in api 2 and are **not silently
//! ignored**: if `exec` or `permissions.fs` is seen, the manifest is
//! rejected. Ignored, an old plugin would look "loaded" and then fail
//! incomprehensibly on its first call.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result, io_err};

use super::artifact::PLATFORMS;
use super::protocol::PLUGIN_API;

/// Name of the manifest file.
pub const MANIFEST_FILE: &str = "plugin.json";

/// The permissions a plugin declares.
///
/// The empty set means "I don't go anywhere" and is valid.
///
/// There is **no** file permission: in api 2 a plugin cannot touch the file
/// system at all. If it needs to keep something, it uses the `host.storage`
/// the engine provides, and that store is its own anyway.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    /// Hosts to connect to: `api.soundcloud.com` or `*.googlevideo.com`.
    ///
    /// A wildcard only on the far left and only for **subdomains**:
    /// `*.sndcdn.com` covers `cf-media.sndcdn.com` but not `sndcdn.com` itself.
    /// A bare `*` and single-label wildcards like `*.com` are rejected — a plugin
    /// that says "I go everywhere" must spell it out one by one, or the user
    /// must reject it (D-040).
    #[serde(default)]
    pub net: Vec<String>,
}

impl Permissions {
    /// Does it ask for no permissions at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.net.is_empty()
    }

    /// A lower-cased, sorted and deduplicated copy. Comparisons are made on it,
    /// so that a change in the manifest's order or letter case does not ask for
    /// consent again.
    #[must_use]
    pub fn normalized(&self) -> Self {
        let mut net: Vec<String> = self
            .net
            .iter()
            .map(|item| normalize_host(item))
            .filter(|item| !item.is_empty())
            .collect();
        net.sort();
        net.dedup();
        Self { net }
    }

    /// Does this set stay within `granted`?
    ///
    /// It exists to catch consent growth: if a plugin **shrinks** its
    /// permissions it is not asked again, if it grows them it is (D-040).
    /// Wildcards count: an approved `*.x.com` covers a later `a.x.com`.
    #[must_use]
    pub fn is_covered_by(&self, granted: &Self) -> bool {
        self.beyond(granted).is_empty()
    }

    /// The requests `granted` does not cover — the list shown to the user as
    /// "these are new".
    #[must_use]
    pub fn beyond(&self, granted: &Self) -> Self {
        let granted = granted.normalized();
        Self {
            net: self
                .normalized()
                .net
                .into_iter()
                .filter(|wanted| {
                    !granted
                        .net
                        .iter()
                        .any(|pattern| pattern_covers(pattern, wanted))
                })
                .collect(),
        }
    }

    /// Is connecting to a host allowed.
    #[must_use]
    pub fn allows_host(&self, host: &str) -> bool {
        let host = normalize_host(host);
        !host.is_empty()
            && self
                .net
                .iter()
                .any(|pattern| host_matches(&normalize_host(pattern), &host))
    }

    /// Is going to an address allowed; if not, **why**.
    ///
    /// The engine asks this on every request the plugin makes, on every redirect
    /// it follows, and on the stream address the plugin returns (D-069).
    ///
    /// # Errors
    /// If the address cannot be parsed, is not `http`/`https`, or its host was
    /// not declared — the message is shown to the user as is.
    pub fn check_url(&self, url: &str) -> std::result::Result<(), String> {
        let host = url_host(url)?;
        if self.allows_host(&host) {
            Ok(())
        } else {
            Err(format!(
                "not allowed: `{host}` is not among the plugin's declared network permissions \
                 (permissions.net: {})",
                if self.net.is_empty() {
                    "empty".to_owned()
                } else {
                    self.normalized().net.join(", ")
                }
            ))
        }
    }

    /// Is every entry well formed.
    fn validate(&self) -> std::result::Result<(), String> {
        for entry in &self.net {
            validate_host_pattern(entry)?;
        }
        Ok(())
    }

    /// Human-readable summary. `headshell plugin list` and `headshell diag` print
    /// it.
    #[must_use]
    pub fn describe(&self) -> String {
        let normalized = self.normalized();
        if normalized.is_empty() {
            return "does not go online".to_owned();
        }
        format!("network: {}", normalized.net.join(", "))
    }
}

/// `Api.Example.COM.` → `api.example.com`.
fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Does a normalised pattern match a normalised host.
fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        // A subdomain is required: `*.x.com` does not cover `x.com`. The suffix is
        // compared together with the dot so that `evilx.com` does not fit
        // `*.x.com`.
        Some(base) => host.len() > base.len() + 1 && host.ends_with(&format!(".{base}")),
        None => host == pattern,
    }
}

/// Does the approved `granted` pattern cover the requested `wanted` pattern.
fn pattern_covers(granted: &str, wanted: &str) -> bool {
    if granted == wanted {
        return true;
    }
    match (granted.strip_prefix("*."), wanted.strip_prefix("*.")) {
        // `*.x.com` ⊇ `*.a.x.com`
        (Some(_), Some(inner)) => host_matches(granted, inner),
        // `*.x.com` ⊇ `a.x.com`
        (Some(_), None) => host_matches(granted, wanted),
        // A bare name covers no wildcard.
        (None, _) => false,
    }
}

/// Checks the form of a permission entry.
fn validate_host_pattern(entry: &str) -> std::result::Result<(), String> {
    let normalized = normalize_host(entry);
    let (wildcard, host) = match normalized.strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, normalized.as_str()),
    };
    if host.is_empty() || host.contains('*') {
        return Err(format!(
            "`permissions.net` entry `{entry}`: the wildcard can only be leftmost, in the form `*.domain.name`; \
             a bare `*` is not accepted"
        ));
    }
    if let Some(bad) = host
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '.' || *c == '-'))
    {
        return Err(format!(
            "`permissions.net` entry `{entry}`: `{bad}` cannot be in a host name — \
             no scheme, port or path, just the name (`api.example.com`)"
        ));
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels
        .iter()
        .any(|label| label.is_empty() || label.starts_with('-') || label.ends_with('-'))
    {
        return Err(format!(
            "`permissions.net` entry `{entry}`: invalid host name"
        ));
    }
    if wildcard && labels.len() < 2 {
        return Err(format!(
            "`permissions.net` entry `{entry}`: a single-label wildcard would allow a whole top-level \
             domain (`.{host}`); write at least `*.example.{host}`"
        ));
    }
    Ok(())
}

/// Extracts an address's host — **only** for `http` and `https`.
///
/// No URL crate was added (the tree must stay small); instead there is a
/// narrow, suspicious parser. Being suspicious is essential: the host read
/// here and the host the HTTP client connects to must be **the same**. Every
/// form two parsers could disagree on (backslash, space, percent-encoding,
/// IPv6 brackets) is a door around the permission check; so anything not
/// understood is **rejected**, not guessed.
///
/// # Errors
/// If the address does not follow these rules, with the reason.
pub fn url_host(url: &str) -> std::result::Result<String, String> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(format!("address not understood (no scheme): {url}"));
    };
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return Err(format!(
            "only http and https addresses can be reached (scheme found: `{scheme}`)"
        ));
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.contains('\\')
        || authority
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!(
            "the address's host part could not be understood: {url}"
        ));
    }
    // User info (`user@`) is dropped; the host starts after the **last** `@`.
    // `allowed.com@evil.com` reads as `evil.com` here, exactly as the client
    // would read it.
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host_port.starts_with('[') {
        return Err(format!(
            "an IPv6 address cannot be reached directly; permissions are written with host names: {url}"
        ));
    }
    let host = host_port
        .split_once(':')
        .map_or(host_port, |(host, _)| host);
    let host = normalize_host(host);
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(format!(
            "the address's host part could not be understood: {url}"
        ));
    }
    Ok(host)
}

/// An artifact's release for one platform: where it downloads from, what its
/// hash is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// Where to download it from. `https://` is required.
    pub url: String,
    /// Expected sha256, hex. If it does not match, the artifact is **not put in
    /// place**.
    pub sha256: String,
}

/// An artifact the plugin asks the engine for (D-050 S2, D-055, D-069).
///
/// The plugin **declares, it does not install.** The engine does the
/// download, the hash check and putting it in place; while running, the
/// plugin can only call it through `host.tools.run`. That is exactly D-049's
/// "no long arms" condition.
///
/// In api 2 an artifact is declared **per platform**: yt-dlp publishes
/// separate binaries for Windows, macOS and Linux, each carrying its own
/// Python — the route that asks the user to install nothing. The engine looks
/// up the key of the platform it runs on
/// ([`super::artifact::current_platform`]) in the map; if it is missing it
/// **says so**, and does not try another platform's binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// The artifact's name (`yt-dlp`). This is `host.tools.run`'s first
    /// argument.
    pub name: String,
    /// The pinned version. It goes into the file name; when the version changes
    /// there is a new file and the old one stays where it is.
    pub version: String,
    /// Platform key → release. The keys come from [`PLATFORMS`].
    pub assets: BTreeMap<String, Asset>,
}

impl Requirement {
    /// The file name on disk: `<name>-<version>-<platform>` (+ `.exe` on
    /// Windows).
    ///
    /// The version is in the name so that two plugins asking for two versions of
    /// the same artifact do not overwrite each other's file; the platform is in
    /// it so that in a shared data directory (two machines, one home directory)
    /// one platform's binary does not take the other's place. The extension is
    /// required on Windows: `CreateProcess` does not treat a file without one as
    /// executable.
    ///
    /// The extension comes from the **platform key**, not the machine it was
    /// built on: the same artifact's name comes out the same whichever machine
    /// computes it.
    #[must_use]
    pub fn file_name(&self, platform: &str) -> String {
        let suffix = if platform.starts_with("windows-") {
            ".exe"
        } else {
            ""
        };
        format!(
            "{}-{}-{}{suffix}",
            sanitize(&self.name),
            sanitize(&self.version),
            sanitize(platform),
        )
    }

    /// The release for the given platform.
    #[must_use]
    pub fn asset_for(&self, platform: &str) -> Option<&Asset> {
        self.assets.get(platform)
    }

    /// Is the declaration consistent in itself.
    fn validate(&self) -> std::result::Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("the `name` of a `requires` entry is empty".to_owned());
        }
        if self.version.trim().is_empty() {
            return Err(format!(
                "`requires` entry `{}`: `version` is empty",
                self.name
            ));
        }
        if self.assets.is_empty() {
            return Err(format!(
                "`requires` entry `{}`: `assets` is empty — no release for any platform",
                self.name
            ));
        }
        for (platform, asset) in &self.assets {
            if !PLATFORMS.contains(&platform.as_str()) {
                return Err(format!(
                    "`requires` entry `{}`: `{platform}` is not a recognised platform. \
                     Valid keys: {}",
                    self.name,
                    PLATFORMS.join(", ")
                ));
            }
            // The `https` requirement: the hash check verifies what was downloaded
            // afterwards, but over plain HTTP **which** address it came from is not
            // verified either.
            if !asset.url.starts_with("https://") {
                return Err(format!(
                    "`requires` entry `{}` ({platform}): `url` must start with https:// \
                     (found: {})",
                    self.name, asset.url
                ));
            }
            let hash = asset.sha256.trim();
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!(
                    "`requires` entry `{}` ({platform}): `sha256` must be 64 hex \
                     digits (found: {} digits)",
                    self.name,
                    hash.len()
                ));
            }
        }
        Ok(())
    }
}

/// Makes text that goes into a file name harmless.
///
/// The manifest is a file the user downloaded: if a name in it carried `../`
/// the artifact would be written outside the data directory.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `plugin.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Must be the same as the directory name. This is the provider id.
    pub name: String,
    /// The name shown to the user.
    pub display_name: String,
    /// The plugin's own version. Not the protocol version.
    #[serde(default)]
    pub version: Option<String>,
    /// The protocol version it speaks ([`PLUGIN_API`]).
    pub api: u32,
    /// The script's path relative to the plugin directory (`main.js`). It is
    /// evaluated as an ES module; the functions it exports are the plugin's
    /// face.
    pub main: String,
    /// Capability names (`search`, `stream`).
    ///
    /// **This is the only source.** In api 1 the handshake declared capabilities
    /// too and won in a conflict; api 2 has no handshake. When starting the
    /// plugin, the engine checks that the functions matching the declaration are
    /// exported: a plugin that says `stream` but provides no `resolve_source`
    /// does not start (K9).
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub permissions: Permissions,
    /// Artifacts asked of the engine (D-055, D-069). An empty list means "I need
    /// nothing".
    #[serde(default)]
    pub requires: Vec<Requirement>,
    /// Does the plugin give covers (api 3, D-076)? **Required**: whether it
    /// does is said, not guessed. `true` makes the engine expect an `artwork`
    /// export; with `false` its tracks go to the classic chain.
    ///
    /// The `default` only lets the struct parse: a manifest without the field
    /// is refused before, with its own message ([`Self::parse`]).
    #[serde(default)]
    pub artwork: bool,
    /// Does the plugin give lyrics (api 4, D-078)? **Required**, as
    /// `artwork` is: `true` makes the engine expect a `lyrics` export; with
    /// `false` its tracks' lyrics come from LRCLIB.
    ///
    /// The `default` only lets the struct parse; a manifest without the field
    /// is refused before, with its own message ([`Self::parse`]).
    #[serde(default)]
    pub lyrics: bool,
    /// Does the plugin give videos (api 5, D-086)? **Required**, as
    /// `artwork` and `lyrics` are: `true` makes the engine expect a
    /// `resolve_video` export.
    ///
    /// The `default` only lets the struct parse; a manifest without the field
    /// is refused before, with its own message ([`Self::parse`]).
    #[serde(default)]
    pub video: bool,
    /// Does the plugin name songs like one of its items (api 6, D-087)?
    /// **Required**, as the three above are: `true` makes the engine expect
    /// a `related(id, limit)` export, which the recommendations ask.
    ///
    /// The `default` only lets the struct parse; a manifest without the field
    /// is refused before, with its own message ([`Self::parse`]).
    #[serde(default)]
    pub related: bool,
    /// The links the plugin takes, as templates (api 7, D-095): a link
    /// written out whole, `{id}` where the song's part stands
    /// (`https://youtu.be/{id}`). **Required**, as the four above are: `[]`
    /// says it takes none; a template makes the engine expect a
    /// `link(id, template)` export.
    ///
    /// The `default` only lets the struct parse; a manifest without the field
    /// is refused before, with its own message ([`Self::parse`]).
    #[serde(default)]
    pub links: Vec<String>,
    /// An optional one-sentence description.
    #[serde(default)]
    pub description: Option<String>,
}

/// The smallest form of the manifest that can be read before validation.
///
/// Discovery wants to show a version mismatch as **a separate diagnosis**: an
/// api 1 manifest carries no `main`, so a full parse would fail with "no
/// `main` field" — correct, but misleading. The version is checked first;
/// if it does not fit, the thing to say is "this plugin speaks the old
/// protocol".
#[derive(Debug, Clone, Deserialize)]
struct ManifestProbe {
    #[serde(default)]
    api: Option<u32>,
}

impl PluginManifest {
    /// Reads a plugin directory and validates it.
    ///
    /// # Errors
    /// If the file is missing or unreadable, the JSON is broken, the protocol
    /// version does not match ([`ErrorKind::PluginIncompatible`]), the name does
    /// not match the directory name, `main` is invalid, or a field left over from
    /// api 1 is present.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST_FILE);
        let raw =
            std::fs::read_to_string(&path).map_err(|err| io_err(Stage::PluginLoad, &path, err))?;
        Self::parse(&raw, &dir_name(dir), &path)
    }

    /// Reads and validates a manifest that is not on disk — like a `plugin.json`
    /// downloaded from the catalog (D-071).
    ///
    /// `expected_name` stands for the directory name: the manifest will be
    /// installed under it, and the identity is the directory name. `origin` only
    /// appears in error messages (the index address or a file path).
    ///
    /// It goes through **the same** validation as [`Self::load`]; the catalog does
    /// not write its own rule. A copied rule drifts (D-057).
    ///
    /// # Errors
    /// Every reason [`Self::load`] has, except reading the file.
    pub fn parse(raw: &str, expected_name: &str, origin: &Path) -> Result<Self> {
        let path = origin;
        let json_err = |source| {
            Error::new(
                Stage::PluginLoad,
                ErrorKind::Json {
                    entry: path.display().to_string(),
                    source,
                },
            )
        };

        let probe: ManifestProbe = serde_json::from_str(raw).map_err(json_err)?;
        if let Some(api) = probe.api
            && api != PLUGIN_API
        {
            return Err(Error::new(
                Stage::PluginLoad,
                ErrorKind::PluginIncompatible {
                    plugin: expected_name.to_owned(),
                    plugin_api: api,
                    host_api: PLUGIN_API,
                },
            ));
        }

        let value: serde_json::Value = serde_json::from_str(raw).map_err(json_err)?;
        reject_api1_leftovers(&value, path)?;
        require_declaration(&value, path, "artwork", ARTWORK_DECLARATION)?;
        require_declaration(&value, path, "lyrics", LYRICS_DECLARATION)?;
        require_declaration(&value, path, "video", VIDEO_DECLARATION)?;
        require_declaration(&value, path, "related", RELATED_DECLARATION)?;
        require_links(&value, path)?;

        let manifest: Self = serde_json::from_value(value).map_err(json_err)?;
        manifest.validate(expected_name, path)?;
        Ok(manifest)
    }

    /// The script's path in the plugin directory, with a leading `./` dropped
    /// and `/`-separated: `./src/main.js` → `src/main.js`. The catalog's file
    /// list uses this form (D-071).
    #[must_use]
    pub fn main_file(&self) -> String {
        Path::new(&self.main)
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    fn validate(&self, expected_name: &str, path: &Path) -> Result<()> {
        let invalid = |detail: String| {
            Err(Error::new(
                Stage::PluginLoad,
                ErrorKind::PluginManifest {
                    path: path.to_path_buf(),
                    detail,
                },
            ))
        };

        if self.name.trim().is_empty() {
            return invalid("`name` is empty".to_owned());
        }
        if self.name != expected_name {
            return invalid(format!(
                "`name` ({}) does not match the directory name ({expected_name}) — the identity is the directory name",
                self.name
            ));
        }
        if self.display_name.trim().is_empty() {
            return invalid("`display_name` is empty".to_owned());
        }
        if let Err(detail) = validate_main(&self.main) {
            return invalid(detail);
        }
        if let Err(detail) = self.permissions.validate() {
            return invalid(detail);
        }
        // The declaration is validated **at load**, not at install: a plugin with an
        // invalid `requires` must not be listed at all. Left to install time, the
        // defect would only show once the user typed the command.
        for requirement in &self.requires {
            if let Err(detail) = requirement.validate() {
                return invalid(detail);
            }
        }
        for template in &self.links {
            if let Err(detail) = crate::link::LinkTemplate::parse(template) {
                return invalid(format!("`links`: {detail}"));
            }
        }
        let mut names: Vec<&str> = self.requires.iter().map(|r| r.name.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        if names.len() != count {
            return invalid(
                "`requires` contains the same name twice — which one wins must not be guessed"
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// The link templates, read (D-095). Validation already read every one;
    /// one that would not read now is left out, not guessed at.
    #[must_use]
    pub fn link_templates(&self) -> Vec<crate::link::LinkTemplate> {
        self.links
            .iter()
            .filter_map(|template| crate::link::LinkTemplate::parse(template).ok())
            .collect()
    }

    /// The script's path inside the plugin directory.
    ///
    /// Validation already guaranteed that `main` cannot leave the directory;
    /// here it is only joined.
    #[must_use]
    pub fn main_path(&self, dir: &Path) -> PathBuf {
        dir.join(&self.main)
    }
}

/// What a missing `artwork` asks for (api 3, D-076).
const ARTWORK_DECLARATION: &str = "a manifest says whether the plugin gives covers: \
     `\"artwork\": true` (it exports `artwork(id, size)`) or `\"artwork\": false`";

/// What a missing `lyrics` asks for (api 4, D-078).
const LYRICS_DECLARATION: &str = "a manifest says whether the plugin gives lyrics: \
     `\"lyrics\": true` (it exports `lyrics(id)`) or `\"lyrics\": false`";

/// What a missing `video` asks for (api 5, D-086).
const VIDEO_DECLARATION: &str = "a manifest says whether the plugin gives videos: \
     `\"video\": true` (it exports `resolve_video(id)`) or `\"video\": false`";

/// What a missing `related` asks for (api 6, D-087).
const RELATED_DECLARATION: &str = "a manifest says whether the plugin names similar songs: \
     `\"related\": true` (it exports `related(id, limit)`) or `\"related\": false`";

/// The links a manifest must name (api 7, D-095): an array of templates,
/// `[]` when it takes none — and not left out.
fn require_links(value: &serde_json::Value, path: &Path) -> Result<()> {
    let detail = match value.get("links") {
        Some(serde_json::Value::Array(items)) if items.iter().all(serde_json::Value::is_string) => {
            return Ok(());
        }
        Some(serde_json::Value::Array(_)) => {
            "`links` holds templates, each a text (`\"https://youtu.be/{id}\"`)".to_owned()
        }
        Some(other) => format!("`links` must be an array of templates, not {other}"),
        None => format!("`links` is missing — {LINKS_DECLARATION}"),
    };
    Err(Error::new(
        Stage::PluginLoad,
        ErrorKind::PluginManifest {
            path: path.to_path_buf(),
            detail,
        },
    ))
}

/// What a missing `links` asks for (api 7, D-095).
const LINKS_DECLARATION: &str = "a manifest names the links the plugin plays: \
     `\"links\": [\"https://example.com/track/{id}\"]` (it exports `link(id, template)`) \
     or `\"links\": []`";

/// A yes-or-no a manifest must say — `artwork` (api 3, D-076), `lyrics`
/// (api 4, D-078), `video` (api 5, D-086), `related` (api 6, D-087):
/// `true` or `false`, nothing else, and not left out.
/// Whether a plugin gives something is said, never guessed.
fn require_declaration(
    value: &serde_json::Value,
    path: &Path,
    field: &str,
    asks: &str,
) -> Result<()> {
    let detail = match value.get(field) {
        Some(serde_json::Value::Bool(_)) => return Ok(()),
        Some(other) => format!("`{field}` must be true or false, not {other}"),
        None => format!("`{field}` is missing — {asks}"),
    };
    Err(Error::new(
        Stage::PluginLoad,
        ErrorKind::PluginManifest {
            path: path.to_path_buf(),
            detail,
        },
    ))
}

fn dir_name(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Is a plugin name **a single directory name**: no separator, not `.`/`..`,
/// not empty.
///
/// Every local operation that joins the name to a path (install, removal)
/// asks this first: a name carrying `../` would reach outside the data
/// directory, and in the remove command that would mean deleting another of
/// the user's directories.
///
/// A plugin installed by hand does not have to follow more than this; the
/// narrow rule is for the catalog ([`validate_catalog_name`]).
///
/// # Errors
/// If the name is not a single directory name, with the reason.
pub fn validate_local_name(name: &str) -> std::result::Result<(), String> {
    let mut components = Path::new(name).components();
    let single = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    if !single || name.contains(['/', '\\']) {
        return Err(format!(
            "`{name}` is not a plugin name: a name is a single directory name — it cannot contain \
             a separator, `.` or `..`"
        ));
    }
    Ok(())
}

/// The name of a plugin in the catalog (D-071): lower-case ASCII letters,
/// digits, `-`, `_`, `.`; starts with a letter or digit, at most 64
/// characters.
///
/// Narrower than a local name, because this name will be a directory **on
/// someone else's disk** and will go into an address: upper and lower case
/// are the same directory on Windows and macOS (`SoundCloud` and
/// `soundcloud` collide), a trailing dot is dropped on Windows, `con` or
/// `nul` is a device on Windows, and `{name}` must not need percent-encoding
/// inside the index address.
///
/// # Errors
/// If the name does not follow these rules, with the one it breaks.
pub fn validate_catalog_name(name: &str) -> std::result::Result<(), String> {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if name.is_empty() || name.len() > 64 {
        return Err(format!(
            "catalog name `{name}` must be 1–64 characters (found: {})",
            name.len()
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')))
    {
        return Err(format!(
            "catalog name `{name}`: `{bad}` cannot be used — lower-case ASCII, digits, `-`, `_`, `.`"
        ));
    }
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) || name.ends_with('.') {
        return Err(format!(
            "catalog name `{name}` must start with a letter or digit and must not end with a dot"
        ));
    }
    let stem = name.split('.').next().unwrap_or(name);
    if RESERVED.contains(&stem) {
        return Err(format!(
            "catalog name `{name}` is a device name on Windows (`{stem}`); it cannot be opened as a directory"
        ));
    }
    Ok(())
}

/// Is `main` a `.js` file inside the plugin directory.
///
/// Absolute paths and `..` are rejected: the manifest is a downloaded file,
/// and if the script path could leave the directory, one plugin could run
/// another plugin's code — or a file that is no plugin at all.
fn validate_main(main: &str) -> std::result::Result<(), String> {
    if main.trim().is_empty() {
        return Err("`main` is empty — there is no script to run".to_owned());
    }
    let path = Path::new(main);
    let escapes = path
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir));
    if escapes || main.contains('\\') {
        return Err(format!(
            "`main` ({main}) must be a relative path inside the plugin directory"
        ));
    }
    if !main.ends_with(".js") {
        return Err(format!(
            "`main` ({main}) must be a `.js` file — plugins run in QuickJS (D-069)"
        ));
    }
    Ok(())
}

/// Rejects fields left over from api 1 that mean nothing in api 2.
///
/// `serde` ignores fields it does not know, and that is usually right (a
/// field added later must not break an old core). These two fields, however,
/// **carried meaning**: ignored, a manifest with `exec` would look loaded,
/// and a plugin asking for `fs` would think it could access files.
fn reject_api1_leftovers(value: &serde_json::Value, path: &Path) -> Result<()> {
    let invalid = |detail: &str| {
        Err(Error::new(
            Stage::PluginLoad,
            ErrorKind::PluginManifest {
                path: path.to_path_buf(),
                detail: detail.to_owned(),
            },
        ))
    };
    if value.get("exec").is_some() {
        return invalid(
            "`exec` does not exist in api 2: a plugin is a script, not a command — write `\"main\": \"main.js\"` \
             (D-069)",
        );
    }
    let fs = value
        .get("permissions")
        .and_then(|permissions| permissions.get("fs"))
        .and_then(serde_json::Value::as_array);
    if fs.is_some_and(|entries| !entries.is_empty()) {
        return invalid(
            "`permissions.fs` does not exist in api 2: a plugin cannot access the file system; persistent data \
             uses `host.storage` (D-069)",
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<temp>/<name>`: the manifest tests want the directory name to match the
    /// plugin name. The root directory is deleted when the value is dropped.
    struct PluginDir {
        _root: crate::test_support::TempDir,
        path: PathBuf,
    }

    impl std::ops::Deref for PluginDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.path
        }
    }

    fn temp_dir(name: &str) -> PluginDir {
        let root = crate::test_support::TempDir::new("plugin-manifest");
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        PluginDir { _root: root, path }
    }

    fn write_manifest(dir: &Path, json: &str) {
        std::fs::write(dir.join(MANIFEST_FILE), json).unwrap();
    }

    fn net(hosts: &[&str]) -> Permissions {
        Permissions {
            net: hosts.iter().map(|h| (*h).to_owned()).collect(),
        }
    }

    fn asset_json() -> String {
        format!(
            r#"{{"url":"https://example.invalid/tool","sha256":"{}"}}"#,
            "a".repeat(64)
        )
    }

    #[test]
    fn a_valid_manifest_loads_with_its_permissions() {
        let dir = temp_dir("soundcloud");
        write_manifest(
            &dir,
            r#"{
                "name": "soundcloud",
                "display_name": "SoundCloud",
                "version": "0.2.0",
                "api": 7,
                "artwork": false,
                "lyrics": false,
                "video": false,
                "related": false,
                "links": ["https://soundcloud.com/{id}"],
                "main": "main.js",
                "capabilities": ["search", "stream"],
                "permissions": {"net": ["api-v2.soundcloud.com", "*.sndcdn.com"]}
            }"#,
        );
        let manifest = PluginManifest::load(&dir).unwrap();
        assert_eq!(manifest.name, "soundcloud");
        assert_eq!(manifest.api, PLUGIN_API);
        assert_eq!(manifest.main_path(&dir), dir.join("main.js"));
        assert!(manifest.permissions.allows_host("cf-media.sndcdn.com"));
    }

    #[test]
    fn a_name_that_disagrees_with_the_directory_is_rejected_not_guessed() {
        let dir = temp_dir("soundcloud");
        write_manifest(
            &dir,
            r#"{"name":"other","display_name":"X","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"main.js"}"#,
        );
        let err = PluginManifest::load(&dir).unwrap_err();
        assert_eq!(err.stage(), Stage::PluginLoad);
        assert!(
            err.chain_text().contains("the directory name"),
            "{}",
            err.chain_text()
        );
    }

    /// An api 1 manifest must be rejected as **a version mismatch**, not as "no
    /// `main`" — that is what the user needs to be told.
    #[test]
    fn an_api1_manifest_is_reported_as_incompatible_not_as_broken() {
        let dir = temp_dir("old");
        write_manifest(
            &dir,
            r#"{"name":"old","display_name":"Old","api":1,"exec":["python3","./main.py"]}"#,
        );
        let err = PluginManifest::load(&dir).unwrap_err();
        match err.kind() {
            ErrorKind::PluginIncompatible {
                plugin_api,
                host_api,
                ..
            } => {
                assert_eq!(*plugin_api, 1);
                assert_eq!(*host_api, PLUGIN_API);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn api1_fields_are_refused_not_silently_ignored() {
        for (manifest, expected) in [
            (
                r#"{"name":"p","display_name":"P","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"main.js","exec":["x"]}"#,
                "`exec` does not exist in api 2",
            ),
            (
                r#"{"name":"p","display_name":"P","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"main.js",
                    "permissions":{"net":[],"fs":["/home"]}}"#,
                "`permissions.fs` does not exist in api 2",
            ),
        ] {
            let dir = temp_dir("p");
            write_manifest(&dir, manifest);
            let err = PluginManifest::load(&dir).unwrap_err();
            assert!(err.chain_text().contains(expected), "{}", err.chain_text());
        }
    }

    #[test]
    fn main_must_stay_inside_the_plugin_directory_and_be_javascript() {
        for (main, expected) in [
            ("", "`main` is empty"),
            ("../other/main.js", "a relative path"),
            ("/etc/passwd.js", "a relative path"),
            ("alt\\\\main.js", "a relative path"),
            ("main.py", "a `.js` file"),
        ] {
            let dir = temp_dir("p");
            write_manifest(
                &dir,
                &format!(
                    r#"{{"name":"p","display_name":"P","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"{main}"}}"#
                ),
            );
            let err = PluginManifest::load(&dir).unwrap_err();
            assert!(
                err.chain_text().contains(expected),
                "{main}: expected {expected:?} missing from: {}",
                err.chain_text()
            );
        }
        // A subdirectory is fine.
        let dir = temp_dir("p");
        write_manifest(
            &dir,
            r#"{"name":"p","display_name":"P","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"./src/main.js"}"#,
        );
        assert!(PluginManifest::load(&dir).is_ok());
    }

    #[test]
    fn a_requires_entry_must_be_pinned_verifiable_and_per_platform() {
        let good = asset_json();
        let cases = [
            (
                format!(r#"{{"name":"yt-dlp","version":"","assets":{{"linux-x86_64":{good}}}}}"#),
                "`version` is empty",
            ),
            (
                r#"{"name":"yt-dlp","version":"1","assets":{}}"#.to_owned(),
                "`assets` is empty",
            ),
            (
                format!(r#"{{"name":"yt-dlp","version":"1","assets":{{"linux-amd64":{good}}}}}"#),
                "is not a recognised platform",
            ),
            (
                r#"{"name":"yt-dlp","version":"1","assets":{"linux-x86_64":{"url":"http://a/b","sha256":"aa"}}}"#
                    .to_owned(),
                "https://",
            ),
            (
                r#"{"name":"yt-dlp","version":"1","assets":{"linux-x86_64":{"url":"https://a/b","sha256":"short"}}}"#
                    .to_owned(),
                "64 hex digits",
            ),
        ];
        for (entry, expected) in cases {
            let dir = temp_dir("p");
            write_manifest(
                &dir,
                &format!(
                    r#"{{"name":"p","display_name":"P","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"main.js","requires":[{entry}]}}"#
                ),
            );
            let err = PluginManifest::load(&dir).unwrap_err();
            assert!(
                err.chain_text().contains(expected),
                "expected {expected:?} missing from: {}",
                err.chain_text()
            );
        }
    }

    #[test]
    fn the_same_requirement_twice_is_rejected_not_silently_deduplicated() {
        let dir = temp_dir("p");
        let one = format!(
            r#"{{"name":"yt-dlp","version":"1","assets":{{"linux-x86_64":{}}}}}"#,
            asset_json()
        );
        write_manifest(
            &dir,
            &format!(
                r#"{{"name":"p","display_name":"P","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"main.js","requires":[{one},{one}]}}"#
            ),
        );
        let err = PluginManifest::load(&dir).unwrap_err();
        assert!(err.chain_text().contains("twice"), "{}", err.chain_text());
    }

    #[test]
    fn the_artifact_file_name_carries_version_and_platform_and_cannot_escape() {
        let requirement = Requirement {
            name: "../../../etc/cron.d/x".to_owned(),
            version: "1".to_owned(),
            assets: BTreeMap::new(),
        };
        let name = requirement.file_name("linux-x86_64");
        assert!(!name.contains('/'), "{name}");
        assert!(name.ends_with("-1-linux-x86_64"), "{name}");
        // The extension comes from the platform, not the machine: this test must see
        // the same two names whatever system it runs on.
        assert!(
            requirement
                .file_name("windows-x86_64")
                .ends_with("-1-windows-x86_64.exe"),
            "{}",
            requirement.file_name("windows-x86_64")
        );
    }

    #[test]
    fn bad_permission_entries_are_refused_with_the_reason() {
        for (entry, expected) in [
            ("*", "a bare `*`"),
            ("*.com", "single-label wildcard"),
            ("https://api.example.com", "no scheme, port or path"),
            ("api.example.com:443", "no scheme, port or path"),
            ("api.*.example.com", "can only be leftmost"),
            ("-evil.example.com", "invalid host name"),
        ] {
            let err = validate_host_pattern(entry).unwrap_err();
            assert!(err.contains(expected), "{entry}: {err}");
        }
        assert!(validate_host_pattern("API.Example.COM.").is_ok());
        assert!(validate_host_pattern("*.googlevideo.com").is_ok());
    }

    #[test]
    fn a_wildcard_covers_subdomains_but_not_the_apex_or_lookalikes() {
        let permissions = net(&["*.sndcdn.com", "soundcloud.com"]);
        assert!(permissions.allows_host("cf-media.sndcdn.com"));
        assert!(permissions.allows_host("A-V2.SNDCDN.COM."));
        assert!(
            !permissions.allows_host("sndcdn.com"),
            "the apex must not be covered"
        );
        assert!(
            !permissions.allows_host("evilsndcdn.com"),
            "suffix impersonation"
        );
        assert!(permissions.allows_host("soundcloud.com"));
        assert!(
            !permissions.allows_host("api.soundcloud.com"),
            "a bare name does not cover a subdomain"
        );
    }

    /// The permission check must read the host the client will connect to —
    /// every form the parsers disagree on is an escape hatch.
    #[test]
    fn url_host_reads_what_the_client_would_connect_to_and_refuses_the_rest() {
        assert_eq!(
            url_host("https://API.example.com/path?q=1").unwrap(),
            "api.example.com"
        );
        assert_eq!(url_host("http://example.com:8080").unwrap(), "example.com");
        assert_eq!(url_host("https://example.com#frag").unwrap(), "example.com");
        assert_eq!(
            url_host("https://allowed.com@evil.com/").unwrap(),
            "evil.com",
            "user info is not the host"
        );
        assert_eq!(
            url_host("https://evil.com#@allowed.com").unwrap(),
            "evil.com",
            "the fragment is not part of the host"
        );
        for bad in [
            "ftp://example.com/",
            "file:///etc/passwd",
            "example.com/path",
            "https://allowed.com\\@evil.com/",
            "https://[::1]/",
            "https://example com/",
            "https:///path",
            "https://%6b%6f%74%75.com/",
        ] {
            assert!(url_host(bad).is_err(), "{bad} was accepted");
        }
    }

    #[test]
    fn check_url_names_the_host_that_was_not_declared() {
        let permissions = net(&["api.example.com"]);
        assert!(permissions.check_url("https://api.example.com/x").is_ok());
        let err = permissions.check_url("https://evil.com/x").unwrap_err();
        assert!(err.contains("`evil.com`"), "{err}");
        assert!(err.contains("api.example.com"), "{err}");
    }

    #[test]
    fn shrinking_permissions_stays_covered_but_growing_them_does_not() {
        let granted = net(&["a.example", "b.example"]);
        assert!(net(&["a.example"]).is_covered_by(&granted));
        let bigger = net(&["a.example", "c.example"]);
        assert!(!bigger.is_covered_by(&granted));
        assert_eq!(bigger.beyond(&granted).net, vec!["c.example".to_owned()]);
    }

    #[test]
    fn a_granted_wildcard_covers_narrower_requests_but_not_the_other_way() {
        let granted = net(&["*.example.com"]);
        assert!(net(&["a.example.com"]).is_covered_by(&granted));
        assert!(net(&["*.sub.example.com"]).is_covered_by(&granted));
        assert!(
            !net(&["example.com"]).is_covered_by(&granted),
            "the apex is not covered"
        );

        let narrow = net(&["a.example.com"]);
        assert!(
            !net(&["*.example.com"]).is_covered_by(&narrow),
            "widening to a wildcard must ask for consent again"
        );
    }

    #[test]
    fn reordering_or_recasing_permissions_does_not_ask_the_user_again() {
        let granted = net(&["b.example", "a.example"]);
        assert!(net(&["A.example", "b.example.", " "]).is_covered_by(&granted));
    }

    /// The remove command joins the name to a path: `../` would delete another
    /// directory.
    #[test]
    fn a_local_name_is_a_single_directory_name() {
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", "/etc"] {
            assert!(validate_local_name(bad).is_err(), "{bad:?} was accepted");
        }
        for good in ["soundcloud", "My Plugin", "ytmusic.dev"] {
            assert!(validate_local_name(good).is_ok(), "{good:?} was rejected");
        }
    }

    #[test]
    fn a_catalog_name_is_narrow_enough_to_be_a_directory_everywhere() {
        for (bad, why) in [
            ("", "1–64"),
            ("SoundCloud", "`S` cannot be used"),
            ("-x", "start with a letter or digit"),
            ("x.", "must not end with a dot"),
            ("con", "a device name"),
            ("nul.js", "a device name"),
            ("a/b", "`/` cannot be used"),
            ("ş", "`ş` cannot be used"),
        ] {
            let err = validate_catalog_name(bad).unwrap_err();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
        for good in ["soundcloud", "ytmusic", "echo", "my-plugin_2.1", "console"] {
            assert!(validate_catalog_name(good).is_ok(), "{good:?} was rejected");
        }
    }

    /// A manifest downloaded from the catalog goes through **the same** rules as
    /// one read from a directory.
    /// api 4 (D-078): whether a plugin gives lyrics is said, never guessed —
    /// a manifest without the field is refused with what to write.
    #[test]
    fn a_manifest_must_say_whether_it_gives_lyrics() {
        let origin = Path::new("plugin.json");
        let silent =
            r#"{"name":"echo","display_name":"E","api":7,"artwork":false,"main":"main.js"}"#;
        let err = PluginManifest::parse(silent, "echo", origin).unwrap_err();
        let text = err.chain_text();
        assert!(text.contains("`lyrics` is missing"), "{text}");
        assert!(text.contains("`\"lyrics\": false`"), "{text}");

        let wrong = r#"{"name":"echo","display_name":"E","api":7,"artwork":false,"lyrics":"yes","main":"main.js"}"#;
        let err = PluginManifest::parse(wrong, "echo", origin).unwrap_err();
        assert!(
            err.chain_text().contains("must be true or false"),
            "{}",
            err.chain_text()
        );
    }

    /// api 5 (D-086): whether a plugin gives videos is said, never guessed.
    #[test]
    fn a_manifest_must_say_whether_it_gives_videos() {
        let origin = Path::new("plugin.json");
        let silent = r#"{"name":"echo","display_name":"E","api":7,"artwork":false,"lyrics":false,"main":"main.js"}"#;
        let err = PluginManifest::parse(silent, "echo", origin).unwrap_err();
        let text = err.chain_text();
        assert!(text.contains("`video` is missing"), "{text}");
        assert!(text.contains("`\"video\": false`"), "{text}");

        let video = r#"{"name":"echo","display_name":"E","api":7,"artwork":false,"lyrics":false,"video":true,"related":false,"links":[],"main":"main.js"}"#;
        assert!(PluginManifest::parse(video, "echo", origin).unwrap().video);
    }

    /// api 6 (D-087): whether a plugin names similar songs is said, never
    /// guessed.
    #[test]
    fn a_manifest_must_say_whether_it_names_similar_songs() {
        let origin = Path::new("plugin.json");
        let silent = r#"{"name":"echo","display_name":"E","api":7,"artwork":false,"lyrics":false,"video":false,"main":"main.js"}"#;
        let err = PluginManifest::parse(silent, "echo", origin).unwrap_err();
        let text = err.chain_text();
        assert!(text.contains("`related` is missing"), "{text}");
        assert!(text.contains("`\"related\": false`"), "{text}");

        let related = r#"{"name":"echo","display_name":"E","api":7,"artwork":false,"lyrics":false,"video":false,"related":true,"links":[],"main":"main.js"}"#;
        assert!(
            PluginManifest::parse(related, "echo", origin)
                .unwrap()
                .related
        );
    }

    /// api 7 (D-095): the links a plugin takes are named, `[]` when none —
    /// and every template is read when the manifest is.
    #[test]
    fn a_manifest_must_name_the_links_it_takes() {
        let origin = Path::new("plugin.json");
        let with = |links: &str| {
            format!(
                r#"{{"name":"echo","display_name":"E","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,{links}"main":"main.js"}}"#
            )
        };
        let err = PluginManifest::parse(&with(""), "echo", origin).unwrap_err();
        let text = err.chain_text();
        assert!(text.contains("`links` is missing"), "{text}");
        assert!(text.contains("`\"links\": []`"), "{text}");

        for (links, says) in [
            (r#""links":"https://youtu.be/{id}","#, "must be an array"),
            (r#""links":[1],"#, "each a text"),
            (r#""links":["youtu.be/{id}"],"#, "must begin with"),
            (r#""links":["https://youtu.be/"],"#, "exactly once"),
        ] {
            let err = PluginManifest::parse(&with(links), "echo", origin).unwrap_err();
            assert!(
                err.chain_text().contains(says),
                "{links}: {}",
                err.chain_text()
            );
        }

        let none = PluginManifest::parse(&with(r#""links":[],"#), "echo", origin).unwrap();
        assert!(none.link_templates().is_empty());
        let two = PluginManifest::parse(
            &with(r#""links":["https://youtu.be/{id}","https://www.youtube.com/watch?v={id}"],"#),
            "echo",
            origin,
        )
        .unwrap();
        let hosts: Vec<String> = two
            .link_templates()
            .iter()
            .map(|template| template.host().to_owned())
            .collect();
        assert_eq!(hosts, ["youtu.be", "youtube.com"]);
    }

    #[test]
    fn parse_applies_the_same_rules_as_load() {
        let origin = Path::new("https://catalog.example/index.json");
        let raw = r#"{"name":"echo","display_name":"E","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"main.js"}"#;
        assert!(PluginManifest::parse(raw, "echo", origin).is_ok());

        let err = PluginManifest::parse(raw, "other", origin).unwrap_err();
        assert!(
            err.chain_text().contains("the directory name"),
            "{}",
            err.chain_text()
        );

        let newer = r#"{"name":"echo","display_name":"E","api":8,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"main.js"}"#;
        let err = PluginManifest::parse(newer, "echo", origin).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                ErrorKind::PluginIncompatible { plugin_api: 8, .. }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn main_file_drops_the_leading_dot_and_uses_slashes() {
        let origin = Path::new("plugin.json");
        for (main, expected) in [("main.js", "main.js"), ("./src/main.js", "src/main.js")] {
            let raw = format!(
                r#"{{"name":"p","display_name":"P","api":7,"artwork":false,"lyrics":false,"video":false,"related":false,"links":[],"main":"{main}"}}"#
            );
            let manifest = PluginManifest::parse(&raw, "p", origin).unwrap();
            assert_eq!(manifest.main_file(), expected);
        }
    }

    #[test]
    fn describe_says_what_is_asked_for_in_turkish() {
        assert_eq!(
            net(&["api.soundcloud.com"]).describe(),
            "network: api.soundcloud.com"
        );
        assert_eq!(Permissions::default().describe(), "does not go online");
    }
}
