//! The Subsonic / OpenSubsonic client (D-019).
//!
//! Navidrome, Airsonic, Gonic, LMS and Jellyfin with the Subsonic plugin speak
//! this API. Authentication is Subsonic's own salt/token scheme (D-021): every
//! request carries `u`, `t`, `s`; the password never goes over the wire.
//!
//! **Subsonic errors arrive with HTTP 200** — if the `status: "failed"` in the
//! body is not read, "all is well" is assumed. That is why the envelope is
//! checked on every response and an error becomes
//! [`crate::ErrorKind::RemoteApi`] (separate from a transport error).

use std::sync::Arc;

use serde::Deserialize;

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::ids::ProviderTrackId;
use crate::model::TrackRef;
use crate::net::{self, HttpClient, HttpRequest};
use crate::provider::{
    ArtworkImage, AudioSource, Capabilities, Provider, ProviderFuture, ProviderHealth,
    ProviderInfo, ProviderLyrics, ProviderTrack,
};

use super::{RemoteServer, StoredAuth};

/// The protocol version we speak. 1.16.1 = Subsonic 6.1; `search3` and
/// `getScanStatus` exist in this version.
const API_VERSION: &str = "1.16.1";

/// The client name servers will see in their logs.
const CLIENT_NAME: &str = "headshell";

/// The Subsonic provider.
pub struct SubsonicProvider {
    server: RemoteServer,
    http: Arc<dyn HttpClient>,
    /// Does the server speak OpenSubsonic's `songLyrics` (D-078)? Learned
    /// once, from `getOpenSubsonicExtensions` — only an answer is kept, not a
    /// failure: a server that was down is asked again.
    song_lyrics: std::sync::OnceLock<bool>,
}

impl std::fmt::Debug for SubsonicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicProvider")
            .field("id", &self.server.id)
            .field("url", &self.server.url)
            .finish()
    }
}

impl SubsonicProvider {
    #[must_use]
    pub fn new(server: RemoteServer, http: Arc<dyn HttpClient>) -> Self {
        Self {
            server,
            http,
            song_lyrics: std::sync::OnceLock::new(),
        }
    }

    /// Does the server offer `getLyricsBySongId`? A plain Subsonic server
    /// has no `getOpenSubsonicExtensions` at all: its refusal is a "no", not
    /// a failure.
    async fn speaks_song_lyrics(&self) -> Result<bool> {
        if let Some(known) = self.song_lyrics.get() {
            return Ok(*known);
        }
        let known = match self.call("getOpenSubsonicExtensions", &[]).await {
            Ok(body) => body
                .open_subsonic_extensions
                .iter()
                .any(|extension| extension.name == "songLyrics"),
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::RemoteApi { .. } | ErrorKind::HttpStatus { status: 404, .. }
                ) =>
            {
                false
            }
            Err(err) => return Err(err),
        };
        let _ = self.song_lyrics.set(known);
        Ok(known)
    }

    /// Builds an endpoint URL together with the credential parameters.
    ///
    /// The credentials go in the query string: that is the scheme Subsonic
    /// defines. That is why **using it over `http://` exposes the token to the
    /// network** — which is also why `normalize_url` does not guess the scheme.
    pub(crate) fn endpoint(&self, name: &str, params: &[(&str, &str)]) -> String {
        let mut url = format!(
            "{}/rest/{}?u={}&v={API_VERSION}&c={CLIENT_NAME}&f=json",
            self.server.url,
            name,
            net::encode_query(&self.server.username),
        );
        match &self.server.auth {
            StoredAuth::SubsonicToken { salt, token } => {
                url.push_str(&format!(
                    "&t={}&s={}",
                    net::encode_query(token),
                    net::encode_query(salt)
                ));
            }
            // This can happen in a hand-edited record: OpenSubsonic's
            // `apiKey` route. Better than silently sending requests without credentials.
            StoredAuth::ApiKey { key } => {
                url.push_str(&format!("&apiKey={}", net::encode_query(key)));
            }
        }
        for (key, value) in params {
            url.push_str(&format!("&{key}={}", net::encode_query(value)));
        }
        url
    }

    /// Calls the endpoint and validates the envelope.
    ///
    /// The credentials are in the query string (the `u/t/s` triple, D-021):
    /// an error names the endpoint, not the address — its text ends up in
    /// `headshell diag`, which is made to be pasted.
    async fn call(&self, name: &str, params: &[(&str, &str)]) -> Result<SubsonicBody> {
        let url = self.endpoint(name, params);
        let shown = net::without_query(&url);
        let request = HttpRequest::get(&url);
        let response = self
            .http
            .send(&request)
            .await
            .map_err(|err| without_credentials(err, &url, shown))?;
        response.error_for_status(shown)?;

        let envelope: Envelope = net::parse_json(&response, &format!("subsonic {name}"))?;
        let body = envelope.response;
        if body.status.as_deref() == Some("failed") {
            let error = body.error.unwrap_or(ApiError {
                code: -1,
                message: None,
            });
            return Err(Error::new(
                Stage::ProviderCall,
                ErrorKind::RemoteApi {
                    server: self.server.id.to_string(),
                    endpoint: name.to_owned(),
                    code: error.code,
                    message: error
                        .message
                        .unwrap_or_else(|| "the server gave no message".to_owned()),
                },
            ));
        }
        Ok(body)
    }

    /// A track's stream URL.
    #[must_use]
    pub fn stream_url(&self, song_id: &str) -> String {
        self.endpoint("stream", &[("id", song_id)])
    }
}

impl Provider for SubsonicProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.server.id.clone(),
            display_name: self.server.display_name(),
            // No CONTROL: Subsonic does not control a remote player,
            // it gives us the audio bytes. ARTWORK: `getCoverArt` (D-076).
            // LYRICS: OpenSubsonic's `getLyricsBySongId` (D-078) — a plain
            // Subsonic server says it has none.
            capabilities: Capabilities::SEARCH
                | Capabilities::BROWSE
                | Capabilities::STREAM
                | Capabilities::ARTWORK
                | Capabilities::LYRICS,
        }
    }

    fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
        Box::pin(async move {
            let ping = match self.call("ping", &[]).await {
                Ok(body) => body,
                Err(err) => {
                    // Being unreachable is a health **answer**, not an error of the
                    // command: the `provider test` output must show the reason.
                    return Ok(ProviderHealth {
                        id: self.server.id.clone(),
                        reachable: false,
                        track_count: None,
                        detail: Some(err.chain_text().replace('\n', " ")),
                    });
                }
            };

            let mut detail = match (&ping.server_type, &ping.server_version, &ping.version) {
                (Some(kind), Some(version), _) => format!("{kind} {version}"),
                (_, _, Some(api)) => format!("Subsonic API {api}"),
                _ => "Subsonic".to_owned(),
            };

            // The track count comes from an optional endpoint; if it is
            // missing we say "I don't know" (None), not zero.
            let track_count = match self.call("getScanStatus", &[]).await {
                Ok(body) => body.scan_status.and_then(|status| status.count),
                Err(err) => {
                    detail.push_str(&format!(
                        " — the track count could not be read ({})",
                        err.chain_text().replace('\n', " ")
                    ));
                    None
                }
            };

            Ok(ProviderHealth {
                id: self.server.id.clone(),
                reachable: true,
                track_count: track_count.map(|count| count as usize),
                detail: Some(detail),
            })
        })
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> ProviderFuture<'a, Vec<ProviderTrack>> {
        Box::pin(async move {
            if query.trim().is_empty() {
                return Ok(Vec::new());
            }
            let limit_text = limit.to_string();
            let body = self
                .call(
                    "search3",
                    &[
                        ("query", query),
                        ("songCount", &limit_text),
                        ("artistCount", "0"),
                        ("albumCount", "0"),
                    ],
                )
                .await?;

            let songs = body
                .search_result3
                .map(|result| result.song)
                .unwrap_or_default();

            let mut hits = Vec::with_capacity(songs.len());
            let mut skipped = 0usize;
            for song in songs {
                match song.into_track(&self.server) {
                    Some(track) => hits.push(track),
                    None => skipped += 1,
                }
            }
            if skipped > 0 {
                // We do not swallow it silently: the count goes to the log (K9).
                tracing::warn!(
                    provider = %self.server.id,
                    skipped,
                    "skipped tracks without a title or an id"
                );
            }
            Ok(hits)
        })
    }

    fn resolve_source<'a>(
        &'a self,
        id: &'a ProviderTrackId,
    ) -> ProviderFuture<'a, Option<AudioSource>> {
        Box::pin(async move {
            if id.provider != self.server.id {
                return Err(Error::new(
                    Stage::PlaybackResolve,
                    ErrorKind::InvalidInput {
                        detail: format!(
                            "{id} does not belong to this provider ({})",
                            self.server.id
                        ),
                    },
                ));
            }
            // We do not send a separate `getSong` call to check existence:
            // an id that does not exist gives a clear HTTP error when the stream
            // is opened (STEP: NETWORK_REQUEST), and it is not worth adding a
            // network round trip before every play.
            Ok(Some(AudioSource::HttpStream {
                url: self.stream_url(&id.id),
                // The credentials are in the query string; no extra header needed.
                headers: Vec::new(),
            }))
        })
    }

    /// `getCoverArt`, at the size asked for (D-076). The Subsonic API takes
    /// "the ID of a song, album or artist" there: the song's own id is enough,
    /// no `getSong` round trip first.
    ///
    /// `Ok(None)`: the server has no cover for it — a `404`, or error `70`
    /// ("the requested data was not found").
    fn artwork<'a>(
        &'a self,
        id: &'a ProviderTrackId,
        size: u32,
    ) -> ProviderFuture<'a, Option<ArtworkImage>> {
        Box::pin(async move {
            if id.provider != self.server.id {
                return Err(Error::new(
                    Stage::ArtworkRead,
                    ErrorKind::InvalidInput {
                        detail: format!(
                            "{id} does not belong to this provider ({})",
                            self.server.id
                        ),
                    },
                ));
            }
            let size = size.to_string();
            let url = self.endpoint("getCoverArt", &[("id", &id.id), ("size", &size)]);
            // An error names the endpoint, not the address (see `call`).
            let shown = net::without_query(&url);
            let response = self
                .http
                .send(&HttpRequest::get(&url))
                .await
                .map_err(|err| without_credentials(err, &url, shown))?;
            if response.status == 404 {
                return Ok(None);
            }
            response.error_for_status(shown)?;
            if let Some(image) = super::as_image(&response) {
                return Ok(Some(image));
            }
            // Not an image: the error envelope (`f=json`).
            let envelope: Envelope = net::parse_json(&response, "subsonic getCoverArt")?;
            match envelope.response.error {
                Some(error) if error.code == 70 => Ok(None),
                Some(error) => Err(Error::new(
                    Stage::ArtworkRead,
                    ErrorKind::RemoteApi {
                        server: self.server.id.to_string(),
                        endpoint: "getCoverArt".to_owned(),
                        code: error.code,
                        message: error
                            .message
                            .unwrap_or_else(|| "the server gave no message".to_owned()),
                    },
                )),
                None => Err(Error::new(
                    Stage::ArtworkRead,
                    ErrorKind::Artwork {
                        detail: format!(
                            "{} answered getCoverArt with neither an image nor an error",
                            self.server.id
                        ),
                    },
                )),
            }
        })
    }

    /// OpenSubsonic's `getLyricsBySongId` (D-078): the lyrics the server
    /// has for the file — its `.lrc`, its tags. Its main lyrics, synced ones
    /// first; `offset` applied (positive: the lines come sooner — the spec's
    /// words).
    ///
    /// A server without the `songLyrics` extension gives `Unsupported`: it
    /// cannot, which is not "none" (K9). Error `70` and an empty list are
    /// "none".
    fn lyrics<'a>(&'a self, id: &'a ProviderTrackId) -> ProviderFuture<'a, Option<ProviderLyrics>> {
        Box::pin(async move {
            if id.provider != self.server.id {
                return Err(Error::new(
                    Stage::LyricsRead,
                    ErrorKind::InvalidInput {
                        detail: format!(
                            "{id} does not belong to this provider ({})",
                            self.server.id
                        ),
                    },
                ));
            }
            if !self.speaks_song_lyrics().await? {
                return Err(Error::new(
                    Stage::LyricsRead,
                    ErrorKind::Unsupported {
                        provider: self.server.id.to_string(),
                        what: "lyrics (the server has no OpenSubsonic `songLyrics`)".to_owned(),
                        capabilities: self.info().capabilities.describe(),
                    },
                ));
            }
            let body = match self.call("getLyricsBySongId", &[("id", &id.id)]).await {
                Ok(body) => body,
                Err(err) if matches!(err.kind(), ErrorKind::RemoteApi { code: 70, .. }) => {
                    return Ok(None);
                }
                Err(err) => return Err(err),
            };
            let entries = body
                .lyrics_list
                .map(|list| list.structured_lyrics)
                .unwrap_or_default();
            Ok(choose_structured(entries).map(|lyrics| ProviderLyrics {
                lyrics,
                instrumental: false,
                source: crate::lyrics::LyricsSource::Provider,
            }))
        })
    }

    // `scan_catalog` is not implemented (the default `None`): Subsonic has no
    // endpoint that dumps the whole catalog cheaply — it would take hundreds of
    // requests, artist → album → track. Remote search goes straight to the
    // server.
}

/// The `{"subsonic-response": {...}}` envelope.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: SubsonicBody,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubsonicBody {
    status: Option<String>,
    version: Option<String>,
    /// OpenSubsonic: `"navidrome"`, `"gonic"`…
    #[serde(rename = "type")]
    server_type: Option<String>,
    server_version: Option<String>,
    error: Option<ApiError>,
    search_result3: Option<SearchResult3>,
    scan_status: Option<ScanStatus>,
    /// `getOpenSubsonicExtensions` (D-078).
    #[serde(default)]
    open_subsonic_extensions: Vec<Extension>,
    /// `getLyricsBySongId` (D-078).
    lyrics_list: Option<LyricsList>,
}

#[derive(Debug, Deserialize)]
struct Extension {
    name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LyricsList {
    #[serde(default)]
    structured_lyrics: Vec<StructuredLyrics>,
}

/// One `structuredLyrics` entry (OpenSubsonic, read from its specification
/// and Navidrome's source, D-078).
#[derive(Debug, Deserialize)]
struct StructuredLyrics {
    #[serde(default)]
    synced: bool,
    /// Milliseconds; positive, the lines appear sooner.
    #[serde(default)]
    offset: Option<i64>,
    /// `main`, `translation`, `pronunciation`; absent means `main`.
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    line: Vec<LyricsLine>,
}

#[derive(Debug, Deserialize)]
struct LyricsLine {
    /// Milliseconds from the track's start; absent in unsynced lyrics.
    #[serde(default)]
    start: Option<i64>,
    #[serde(default)]
    value: String,
}

/// The main lyrics — not a translation or a pronunciation — synced ones
/// first, as core lyrics.
fn choose_structured(entries: Vec<StructuredLyrics>) -> Option<crate::lyrics::Lyrics> {
    use crate::lyrics::{LyricLine, Lyrics};
    let mut main: Vec<StructuredLyrics> = entries
        .into_iter()
        .filter(|entry| entry.kind.as_deref().is_none_or(|kind| kind == "main"))
        .filter(|entry| entry.line.iter().any(|line| !line.value.trim().is_empty()))
        .collect();
    // Stable: the server's order among equals.
    main.sort_by_key(|entry| !(entry.synced && entry.line.iter().all(|l| l.start.is_some())));
    let entry = main.into_iter().next()?;
    let synced = entry.synced && entry.line.iter().all(|line| line.start.is_some());
    let offset = entry.offset.unwrap_or(0);
    let mut lines: Vec<LyricLine> = entry
        .line
        .into_iter()
        .map(|line| LyricLine {
            start_ms: if synced {
                line.start
                    .map(|start| u64::try_from(start.saturating_sub(offset).max(0)).unwrap_or(0))
            } else {
                None
            },
            text: line.value.trim().to_owned(),
        })
        .collect();
    if synced {
        lines.sort_by_key(|line| line.start_ms);
    }
    Some(Lyrics {
        synced,
        lines,
        attribution: None,
    })
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: i64,
    message: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SearchResult3 {
    #[serde(default)]
    song: Vec<Song>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanStatus {
    count: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Song {
    id: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    /// In seconds (that is how Subsonic gives it).
    duration: Option<u64>,
    /// An OpenSubsonic extension; if present, the first link of the identity
    /// chain (K6).
    isrc: Option<Vec<String>>,
}

impl Song {
    /// A row without an id or a title returns `None` — the caller counts it.
    fn into_track(self, server: &RemoteServer) -> Option<ProviderTrack> {
        let id = self.id?;
        let title = self.title?;
        let isrc = self
            .isrc
            .as_ref()
            .and_then(|list| list.first())
            .and_then(|raw| crate::ids::Isrc::parse(raw));

        Some(ProviderTrack {
            id: ProviderTrackId::new(server.id.clone(), id),
            track: TrackRef::new(
                self.artist.unwrap_or_else(|| "Unknown artist".to_owned()),
                title,
            )
            .with_album(self.album)
            .with_duration_ms(self.duration.map(|secs| secs.saturating_mul(1000)))
            .with_isrc(isrc),
        })
    }
}

/// A request error with the address swapped for the endpoint's: the query
/// string carries the token and the salt (or the API key).
fn without_credentials(err: Error, url: &str, shown: &str) -> Error {
    match err.kind() {
        ErrorKind::Network { detail, .. } => Error::new(
            err.stage(),
            ErrorKind::Network {
                url: shown.to_owned(),
                detail: detail.replace(url, shown),
            },
        ),
        _ => err,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ProviderId;
    use crate::net::fake::FakeHttp;

    fn server() -> RemoteServer {
        RemoteServer {
            id: ProviderId::new("ev"),
            kind: super::super::ServerKind::Subsonic,
            url: "https://music.home".to_owned(),
            username: "enai".to_owned(),
            auth: StoredAuth::SubsonicToken {
                salt: "c19b2d".to_owned(),
                token: "26719a1196d2a940705a59634eb18eab".to_owned(),
            },
            user_id: None,
        }
    }

    const SEARCH_JSON: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
        "type":"navidrome","serverVersion":"0.53.3","searchResult3":{"song":[
        {"id":"a1","title":"Geceler","artist":"Ezhel","album":"Müptezhel","duration":215,
         "isrc":["TRA123456789"]},
        {"id":"a2","title":"Felaket","artist":"Ezhel","duration":180},
        {"title":"no-id"}]}}}"#;

    #[tokio::test]
    async fn a_songs_cover_comes_from_get_cover_art_and_its_absence_is_none() {
        let png = crate::artwork::image::tests_support::png_2x2();
        let http = Arc::new(FakeHttp::new().route_bytes("/rest/getCoverArt", "image/png", &png));
        let provider = SubsonicProvider::new(server(), http.clone());
        let id = ProviderTrackId::new(ProviderId::new("ev"), "a1");
        let image = provider.artwork(&id, 500).await.unwrap().unwrap();
        assert_eq!(image.bytes, png);
        assert_eq!(image.mime.as_deref(), Some("image/png"));
        let url = http.last_url();
        assert!(url.contains("id=a1") && url.contains("size=500"), "{url}");

        let none = SubsonicProvider::new(
            server(),
            Arc::new(FakeHttp::new().route(
                "/rest/getCoverArt",
                r#"{"subsonic-response":{"status":"failed","error":{"code":70,"message":"Cover not found"}}}"#,
            )),
        );
        assert_eq!(
            none.artwork(&id, 500).await.unwrap(),
            None,
            "error 70 is \"no cover\""
        );

        let refused = SubsonicProvider::new(
            server(),
            Arc::new(FakeHttp::new().route(
                "/rest/getCoverArt",
                r#"{"subsonic-response":{"status":"failed","error":{"code":50,"message":"not authorized"}}}"#,
            )),
        );
        let err = refused.artwork(&id, 500).await.unwrap_err();
        let text = err.chain_text();
        assert!(
            text.contains("code 50") && text.starts_with("STEP: ARTWORK_READ"),
            "{text}"
        );
        assert!(
            !text.contains("26719a"),
            "the token must not reach the error: {text}"
        );

        // Unreachable: the network error names the endpoint, not the URL.
        let unreachable = SubsonicProvider::new(server(), Arc::new(FakeHttp::new()));
        let text = unreachable
            .artwork(&id, 500)
            .await
            .unwrap_err()
            .chain_text();
        assert!(text.contains("/rest/getCoverArt"), "{text}");
        assert!(
            !text.contains("26719a") && !text.contains("c19b2d"),
            "the credentials must not reach a network error: {text}"
        );
    }

    #[test]
    fn the_url_carries_credentials_and_encodes_the_query() {
        let provider = SubsonicProvider::new(server(), Arc::new(FakeHttp::new()));
        let url = provider.endpoint("search3", &[("query", "Ezhel Geceler")]);
        assert!(url.starts_with("https://music.home/rest/search3?"), "{url}");
        assert!(url.contains("u=enai"), "{url}");
        assert!(url.contains("&t=26719a1196d2a940705a59634eb18eab"), "{url}");
        assert!(url.contains("&s=c19b2d"), "{url}");
        assert!(url.contains("&f=json"), "{url}");
        assert!(url.contains("query=Ezhel%20Geceler"), "{url}");
        // The password must not appear anywhere.
        assert!(!url.contains("p="), "{url}");
    }

    #[tokio::test]
    async fn search_maps_songs_and_skips_broken_rows() {
        let http = Arc::new(FakeHttp::new().route("/rest/search3", SEARCH_JSON));
        let provider = SubsonicProvider::new(server(), Arc::clone(&http) as Arc<dyn HttpClient>);

        let hits = provider.search("Ezhel", 10).await.unwrap();
        assert_eq!(hits.len(), 2, "a row without a title must be skipped");
        assert_eq!(hits[0].track.title, "Geceler");
        assert_eq!(hits[0].track.artist, "Ezhel");
        assert_eq!(hits[0].track.album.as_deref(), Some("Müptezhel"));
        assert_eq!(hits[0].track.duration_ms, Some(215_000));
        assert_eq!(
            hits[0].track.isrc.as_ref().map(|i| i.as_str()),
            Some("TRA123456789"),
            "if there is an ISRC it is the first link of the identity chain (K6)"
        );
        assert_eq!(hits[0].id.provider.as_str(), "ev");
        assert_eq!(hits[0].id.id, "a1");
        assert!(
            http.last_url().contains("songCount=10"),
            "{}",
            http.last_url()
        );
    }

    #[tokio::test]
    async fn a_failed_envelope_becomes_a_remote_api_error_not_success() {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":40,"message":"Wrong username or password"}}}"#;
        let http = Arc::new(FakeHttp::new().route("/rest/", body));
        let provider = SubsonicProvider::new(server(), http);

        let err = provider.search("Ezhel", 10).await.unwrap_err();
        let text = err.chain_text();
        assert!(text.starts_with("STEP: PROVIDER_CALL"), "{text}");
        assert!(text.contains("Wrong username or password"), "{text}");
        assert!(text.contains("40"), "{text}");
    }

    /// The `u/t/s` triple is in every request's query string (D-021); an
    /// error names the endpoint, not the address — on the network path and
    /// on the status path alike.
    #[tokio::test]
    async fn the_credentials_do_not_reach_an_error_text() {
        let unreachable = SubsonicProvider::new(server(), Arc::new(FakeHttp::new()));
        let refused = SubsonicProvider::new(
            server(),
            Arc::new(FakeHttp::new().route_status("/rest/", 500, "broken")),
        );
        for provider in [unreachable, refused] {
            let text = provider.search("Ezhel", 10).await.unwrap_err().chain_text();
            assert!(text.contains("https://music.home/rest/search3"), "{text}");
            assert!(
                !text.contains("26719a") && !text.contains("c19b2d"),
                "the credentials must not reach the error: {text}"
            );
        }
    }

    #[tokio::test]
    async fn health_reports_the_server_build_and_track_count() {
        let ping = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "type":"navidrome","serverVersion":"0.53.3"}}"#;
        let scan = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
            "scanStatus":{"scanning":false,"count":8123}}}"#;
        let http = Arc::new(
            FakeHttp::new()
                .route("/rest/getScanStatus", scan)
                .route("/rest/ping", ping),
        );
        let provider = SubsonicProvider::new(server(), http);

        let health = provider.health().await.unwrap();
        assert!(health.reachable);
        assert_eq!(health.track_count, Some(8123));
        assert!(
            health
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("navidrome"),
            "{health:?}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_health_answer_not_a_command_failure() {
        // The fake client has no routes at all: every request is a transport error.
        let http = Arc::new(FakeHttp::new());
        let provider = SubsonicProvider::new(server(), http);

        let health = provider.health().await.unwrap();
        assert!(!health.reachable);
        assert!(health.detail.is_some(), "the reason must be shown");
        assert_eq!(health.track_count, None, "unknown is not zero");
    }

    #[tokio::test]
    async fn resolve_source_builds_a_stream_url_for_this_server_only() {
        let http = Arc::new(FakeHttp::new());
        let provider = SubsonicProvider::new(server(), http);

        let id = ProviderTrackId::new(ProviderId::new("ev"), "a1");
        let source = provider.resolve_source(&id).await.unwrap().unwrap();
        match source {
            AudioSource::HttpStream { url, headers } => {
                assert!(url.contains("/rest/stream?"), "{url}");
                assert!(url.contains("id=a1"), "{url}");
                assert!(headers.is_empty());
            }
            other => panic!("an HTTP stream was expected: {other:?}"),
        }

        let foreign = ProviderTrackId::new(ProviderId::new("other"), "a1");
        assert!(
            provider.resolve_source(&foreign).await.is_err(),
            "another provider's id must not be accepted silently"
        );
    }

    const EXTENSIONS_JSON: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
        "openSubsonic":true,"openSubsonicExtensions":[{"name":"transcodeOffset","versions":[1]},
        {"name":"songLyrics","versions":[1]}]}}"#;

    /// A translation, unsynced words and the synced main lyrics — out of order,
    /// with an offset — as Navidrome can answer.
    const LYRICS_JSON: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
        "lyricsList":{"structuredLyrics":[
        {"lang":"eng","synced":true,"kind":"translation","line":[{"start":0,"value":"not these"}]},
        {"lang":"und","synced":false,"line":[{"value":"unsynced words"}]},
        {"lang":"und","synced":true,"offset":500,
         "line":[{"start":4000,"value":"second"},{"start":1000,"value":"first"}]}]}}}"#;

    fn lines(lyrics: &crate::lyrics::Lyrics) -> Vec<(Option<u64>, &str)> {
        lyrics
            .lines
            .iter()
            .map(|line| (line.start_ms, line.text.as_str()))
            .collect()
    }

    #[tokio::test]
    async fn a_songs_lyrics_come_from_get_lyrics_by_song_id_main_and_synced_first() {
        let http = Arc::new(
            FakeHttp::new()
                .route("/rest/getOpenSubsonicExtensions", EXTENSIONS_JSON)
                .route("/rest/getLyricsBySongId", LYRICS_JSON),
        );
        let provider = SubsonicProvider::new(server(), http.clone());
        assert!(provider.info().capabilities.contains(Capabilities::LYRICS));
        let id = ProviderTrackId::new(ProviderId::new("ev"), "a1");
        let found = provider.lyrics(&id).await.unwrap().unwrap();
        assert_eq!(found.source, crate::lyrics::LyricsSource::Provider);
        assert!(found.lyrics.synced);
        assert_eq!(
            lines(&found.lyrics),
            [(Some(500), "first"), (Some(3_500), "second")],
            "in time order, and a positive offset brings them sooner"
        );
        assert!(http.last_url().contains("id=a1"), "{}", http.last_url());

        // The extension is learned once.
        provider.lyrics(&id).await.unwrap();
        let asked = http
            .requests()
            .iter()
            .filter(|r| r.url.contains("getOpenSubsonicExtensions"))
            .count();
        assert_eq!(asked, 1);
    }

    #[tokio::test]
    async fn a_server_without_song_lyrics_says_it_cannot_and_none_is_an_answer() {
        let id = ProviderTrackId::new(ProviderId::new("ev"), "a1");
        // A plain Subsonic server has no `getOpenSubsonicExtensions`.
        let plain = Arc::new(FakeHttp::new().route_status(
            "/rest/getOpenSubsonicExtensions",
            404,
            "Not Found",
        ));
        let err = SubsonicProvider::new(server(), plain)
            .lyrics(&id)
            .await
            .unwrap_err();
        assert!(
            matches!(err.kind(), ErrorKind::Unsupported { .. }),
            "{}",
            err.chain_text()
        );

        // Error 70, and an empty list: "none".
        let failed = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":70,"message":"lyrics not found"}}}"#;
        let empty = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","lyricsList":{}}}"#;
        for answer in [failed, empty] {
            let http = Arc::new(
                FakeHttp::new()
                    .route("/rest/getOpenSubsonicExtensions", EXTENSIONS_JSON)
                    .route("/rest/getLyricsBySongId", answer),
            );
            assert_eq!(
                SubsonicProvider::new(server(), http)
                    .lyrics(&id)
                    .await
                    .unwrap(),
                None,
                "{answer}"
            );
        }
    }
}
