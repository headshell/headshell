//! The **data format** of the plugin contract (api 6; D-069, D-076, D-078,
//! D-086, D-087).
//!
//! api 1 was a wire protocol: line-based JSON-RPC, a subprocess, a handshake.
//! In api 2 there is no wire — the plugin runs in QuickJS inside the core and
//! the contract is **the exported functions**. Values cross between JS and
//! Rust as JSON; this file is the Rust-side shape of that JSON.
//!
//! ## The functions of api 6
//!
//! | Function | Required | Returns | Counterpart |
//! |---|---|---|---|
//! | `health()` | yes | [`HealthResult`] | [`crate::provider::Provider::health`] |
//! | `search(query, limit)` | if it has the `search` capability | an array of [`WireTrack`] | [`crate::provider::Provider::search`] |
//! | `resolve_source(id)` | if it has the `stream` capability | [`AudioSource`] or `null` | [`crate::provider::Provider::resolve_source`] |
//! | `artwork(id, size)` | if the manifest says `"artwork": true` | [`WireArtwork`] or `null` | [`crate::provider::Provider::artwork`] |
//! | `lyrics(id)` | if the manifest says `"lyrics": true` | [`WireLyrics`] or `null` | [`crate::provider::Provider::lyrics`] |
//! | `resolve_video(id)` | if the manifest says `"video": true` | [`crate::video::VideoSource`] (`{ kind: "youtube", video_id }`) or `null` | [`crate::provider::Provider::resolve_video`] |
//! | `related(id, limit)` | if the manifest says `"related": true` | an array of [`WireTrack`] | [`crate::provider::Provider::related`] |
//!
//! The function names are deliberately the same as api 1's method names
//! (`resolve_source`, not `resolveSource`): when the document, the provider
//! trait and the plugin use the same name, no "which name maps to which"
//! table is needed.
//!
//! ## Versioning
//!
//! [`PLUGIN_API`] is a single integer and is written in the manifest; **if it
//! is not equal the plugin is not loaded** and the user sees both numbers
//! ([`crate::ErrorKind::PluginIncompatible`]). The rule is the same as
//! D-039's: **adding does not bump the version, removing or changing a
//! meaning does.** The move from api 1 to 2 was the latter: `exec` was
//! removed, and where the plugin runs changed.
//!
//! api 2 → 3 (D-076) was a user's decision, though the change adds more than
//! it removes: every manifest **must** now declare `artwork` — `true` or
//! `false`. Whether a plugin gives covers is said, not guessed: `true` makes
//! the engine expect an `artwork` export; with `false`, or when the plugin's
//! answer is empty or fails, the track goes to the classic chain
//! (MusicBrainz → the Cover Art Archive). The plugin fetches its image
//! itself, through `host.http` in binary mode — the core fetches no address
//! a plugin names (K5).
//!
//! api 3 → 4 (D-078) is the same kind of step, the user's decision too:
//! every manifest **must** declare `lyrics` — `true` or `false`. `true`
//! makes the engine expect a `lyrics` export: the service's own lyrics for
//! the track, synced as LRC text when it knows the timing. They are the
//! recording's own — the same item the audio comes from — so they are not
//! measured against its length as LRCLIB's are.
//!
//! api 4 → 5 (D-086), the user's decision again: every manifest **must**
//! declare `video` — `true` or `false`. `true` makes the engine expect a
//! `resolve_video` export: the item's video, as a source the app's own
//! player shows (a YouTube video id; the app embeds YouTube's player). A
//! plugin with `video` and without `stream` is a video service: its tracks
//! are videos only, and the video is their sound.
//!
//! api 5 → 6 (D-087), the user's decision again: every manifest **must**
//! declare `related` — `true` or `false`. `true` makes the engine expect a
//! `related(id, limit)` export: the service's own songs like the item (its
//! radio, its "up next"), for the recommendations. Only the item's id is
//! passed: a plugin never sees the history or the taste worked out from it.
//! A [`WireTrack`] may now carry `genres`, which the recommendations learn.

use crate::ids::{Isrc, ProviderId, ProviderTrackId};
use crate::model::TrackRef;
use crate::provider::{AudioSource, Capabilities};

use serde::{Deserialize, Serialize};

/// The version of the plugin contract the core speaks.
pub const PLUGIN_API: u32 = 6;

/// The names of the functions a plugin exports. Identifiers are in English
/// (D-036).
pub mod export {
    pub const HEALTH: &str = "health";
    pub const SEARCH: &str = "search";
    pub const RESOLVE_SOURCE: &str = "resolve_source";
    /// api 3 (D-076).
    pub const ARTWORK: &str = "artwork";
    /// api 4 (D-078).
    pub const LYRICS: &str = "lyrics";
    /// api 5 (D-086).
    pub const RESOLVE_VIDEO: &str = "resolve_video";
    /// api 6 (D-087).
    pub const RELATED: &str = "related";
}

/// Turns capability names into a bit mask.
///
/// Returns: `(mask, unrecognised names)`. We do not swallow the
/// unrecognised — the caller reports them as a note (K9). An empty list is
/// not an error: a plugin without capabilities (only `health`) is valid.
#[must_use]
pub fn parse_capabilities(names: &[String]) -> (Capabilities, Vec<String>) {
    let mut caps = Capabilities::NONE;
    let mut unknown = Vec::new();
    for name in names {
        match name.to_ascii_lowercase().as_str() {
            "search" => caps = caps | Capabilities::SEARCH,
            "browse" => caps = caps | Capabilities::BROWSE,
            "stream" => caps = caps | Capabilities::STREAM,
            "control" => caps = caps | Capabilities::CONTROL,
            _ => unknown.push(name.clone()),
        }
    }
    (caps, unknown)
}

/// The return value of `health()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResult {
    pub reachable: bool,
    #[serde(default)]
    pub track_count: Option<usize>,
    #[serde(default)]
    pub detail: Option<String>,
}

/// A track returned by a plugin.
///
/// Not the serde form of [`crate::provider::ProviderTrack`], but a
/// deliberately plainer shape: `id` is a bare string, and there is **no**
/// provider name. The core adds the provider name — so a plugin cannot make
/// up an id in another provider's namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireTrack {
    pub id: String,
    pub artist: String,
    pub title: String,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// ISRC — if it does not fit the format it is **dropped and counted**, not
    /// silently accepted (K6: the first link of the chain must not be corrupted).
    #[serde(default)]
    pub isrc: Option<String>,
    /// The genres the service names for it (api 6, D-087), as it writes them.
    #[serde(default)]
    pub genres: Vec<String>,
}

impl WireTrack {
    /// Converts to core types. `dropped_isrc`: whether the ISRC was malformed.
    #[must_use]
    pub fn into_provider_track(
        self,
        provider: &ProviderId,
    ) -> (crate::provider::ProviderTrack, bool) {
        let raw_isrc = self.isrc;
        let isrc = raw_isrc.as_deref().and_then(Isrc::parse);
        let dropped_isrc = raw_isrc.is_some() && isrc.is_none();
        let id = ProviderTrackId::new(provider.clone(), self.id);
        let track = TrackRef {
            artist: self.artist,
            title: self.title,
            album: self.album,
            duration_ms: self.duration_ms,
            isrc,
            provider_track_id: Some(id.clone()),
        };
        (
            crate::provider::ProviderTrack {
                id,
                track,
                genres: self.genres,
            },
            dropped_isrc,
        )
    }
}

/// The return value of `artwork(id, size)` when there is a cover (D-076):
/// the image the plugin fetched through `host.http` in binary mode, as
/// base64. `null` means "no cover" — an answer, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireArtwork {
    /// What the image is (`image/jpeg`). A hint; the core reads the bytes.
    #[serde(default)]
    pub mime: Option<String>,
    /// The image, base64 (either alphabet, padding optional).
    pub data: String,
}

/// The return value of `lyrics(id)` (api 4, D-078): the service's own
/// lyrics for the track, or `null` when it has none.
///
/// `synced` is LRC text (`[mm:ss.xx]line`): the core reads it with the same
/// parser as a `.lrc` file, so a plugin does not write its own. `plain` is
/// the words without timing, for a service that has only those. With both,
/// `synced` is taken. `instrumental: true` says the track has no words.
/// `attribution` is shown under the lyrics when the service credits someone
/// ("Source: LyricFind").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireLyrics {
    #[serde(default)]
    pub synced: Option<String>,
    #[serde(default)]
    pub plain: Option<String>,
    #[serde(default)]
    pub instrumental: bool,
    #[serde(default)]
    pub attribution: Option<String>,
}

/// The return value of `resolve_source(id)`: a source or `null`.
///
/// `null` is not an error, it is the answer "this track cannot be played".
/// A plugin returning a **local file**, however, is not accepted: it has no
/// file system access, and if it could return a path it could make the core
/// open any file it liked ([`check_source`]).
pub type SourceResult = Option<AudioSource>;

/// Whether the source a plugin returned stays within its permissions.
///
/// The core fetches the stream address (K3), but the plugin picks the
/// address: without a check, a plugin could send the core in its place to a
/// host it did not declare. The rule is the same as for requests — no
/// undeclared address is visited (D-069).
///
/// # Errors
/// If the source is a local file or its address is outside the permissions,
/// with the reason.
pub fn check_source(
    source: &AudioSource,
    permissions: &super::manifest::Permissions,
) -> std::result::Result<(), String> {
    match source {
        AudioSource::HttpStream { url, .. } => permissions.check_url(url),
        AudioSource::LocalFile { .. } => Err(
            "the plugin returned a local file source; plugins have no file system access \
             (api 2)"
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::Permissions;

    #[test]
    fn capability_names_become_a_mask_and_unknown_names_are_reported() {
        let names = vec![
            "search".to_owned(),
            "STREAM".to_owned(),
            "teleport".to_owned(),
        ];
        let (caps, unknown) = parse_capabilities(&names);
        assert!(caps.contains(Capabilities::SEARCH | Capabilities::STREAM));
        assert!(!caps.contains(Capabilities::CONTROL));
        assert_eq!(unknown, vec!["teleport".to_owned()]);
    }

    #[test]
    fn a_wire_track_cannot_claim_another_providers_namespace() {
        let wire = WireTrack {
            id: "12345".to_owned(),
            artist: "Artist".to_owned(),
            title: "Track".to_owned(),
            album: None,
            duration_ms: Some(1000),
            isrc: Some("USRC17607839".to_owned()),
            genres: vec!["Rock".to_owned()],
        };
        let provider = ProviderId::new("soundcloud");
        let (track, dropped) = wire.into_provider_track(&provider);
        assert!(!dropped);
        assert_eq!(track.id.provider, provider);
        assert_eq!(track.id.id, "12345");
        assert!(track.track.isrc.is_some());
        assert_eq!(track.genres, vec!["Rock".to_owned()]);
    }

    #[test]
    fn a_malformed_isrc_is_dropped_and_counted_not_accepted() {
        let wire = WireTrack {
            id: "1".to_owned(),
            artist: "A".to_owned(),
            title: "B".to_owned(),
            album: None,
            duration_ms: None,
            isrc: Some("bogus".to_owned()),
            genres: Vec::new(),
        };
        let (track, dropped) = wire.into_provider_track(&ProviderId::new("p"));
        assert!(dropped, "a malformed ISRC must be counted");
        assert!(
            track.track.isrc.is_none(),
            "a malformed ISRC must not be accepted"
        );
    }

    #[test]
    fn an_audio_source_round_trips_through_the_json_shape() {
        let json = r#"{"kind":"http_stream","url":"https://x/y","headers":[]}"#;
        let result: SourceResult = serde_json::from_str(json).unwrap();
        match result {
            Some(AudioSource::HttpStream { url, headers }) => {
                assert_eq!(url, "https://x/y");
                assert!(headers.is_empty());
            }
            other => panic!("unexpected source: {other:?}"),
        }
        let none: SourceResult = serde_json::from_str("null").unwrap();
        assert!(none.is_none(), "`null` is an answer, not an error");
    }

    #[test]
    fn a_source_outside_the_permissions_or_on_disk_is_refused() {
        let permissions = Permissions {
            net: vec!["*.googlevideo.com".to_owned()],
        };
        let allowed = AudioSource::HttpStream {
            url: "https://rr1---sn-x.googlevideo.com/videoplayback".to_owned(),
            headers: Vec::new(),
        };
        assert!(check_source(&allowed, &permissions).is_ok());

        let elsewhere = AudioSource::HttpStream {
            url: "http://192.168.1.1/admin".to_owned(),
            headers: Vec::new(),
        };
        assert!(check_source(&elsewhere, &permissions).is_err());

        let local = AudioSource::LocalFile {
            path: "/etc/passwd".into(),
        };
        let err = check_source(&local, &permissions).unwrap_err();
        assert!(err.contains("local file"), "{err}");
    }
}
