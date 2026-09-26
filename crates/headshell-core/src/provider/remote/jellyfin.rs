//! The Jellyfin client (D-019).
//!
//! It differs from Subsonic in three places:
//!
//! 1. **The credentials are in a header**, not in the query string:
//!    `Authorization: MediaBrowser Token="..."`. That is why the stream source
//!    uses the `headers` field of `AudioSource::HttpStream` — the source URL
//!    alone is not enough.
//! 2. **Errors come with the HTTP status code**; there is no `status: failed`
//!    in the body.
//! 3. Most endpoints want a **user id** (`/Users/{id}/Items`). It is learned
//!    at registration time; if it cannot be, it is asked lazily on first use.
//!
//! The password is not stored (D-021): `AuthenticateByName` is called once
//! and the access key it returns is written.

use std::sync::{Arc, RwLock};

use serde::Deserialize;

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::ids::{ProviderId, ProviderTrackId};
use crate::model::TrackRef;
use crate::net::{self, HttpClient, HttpHeader, HttpRequest};
use crate::provider::{
    ArtworkImage, AudioSource, Capabilities, Provider, ProviderFuture, ProviderHealth,
    ProviderInfo, ProviderLyrics, ProviderTrack,
};

use super::{RemoteServer, ServerKind, StoredAuth};

/// Jellyfin's version/client identification. We show up under this name in
/// the server's "devices" list.
const CLIENT_NAME: &str = "headshell";
const DEVICE_NAME: &str = "headshell-core";

/// `RunTimeTicks` is in units of 100 nanoseconds; the divisor for
/// milliseconds.
const TICKS_PER_MS: u64 = 10_000;

/// The Jellyfin provider.
pub struct JellyfinProvider {
    server: RemoteServer,
    http: Arc<dyn HttpClient>,
    /// If not known at registration time, it is learned on first use.
    user_id: RwLock<Option<String>>,
}

impl std::fmt::Debug for JellyfinProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinProvider")
            .field("id", &self.server.id)
            .field("url", &self.server.url)
            .finish()
    }
}

impl JellyfinProvider {
    #[must_use]
    pub fn new(server: RemoteServer, http: Arc<dyn HttpClient>) -> Self {
        let user_id = RwLock::new(server.user_id.clone());
        Self {
            server,
            http,
            user_id,
        }
    }

    /// The access key.
    fn token(&self) -> Result<&str> {
        match &self.server.auth {
            StoredAuth::ApiKey { key } => Ok(key),
            // A hand-edited record: a Subsonic token is no use on Jellyfin.
            StoredAuth::SubsonicToken { .. } => Err(Error::new(
                Stage::ProviderCall,
                ErrorKind::InvalidInput {
                    detail: format!(
                        "the {} record is Jellyfin but carries a Subsonic token; register it again with `headshell provider add`",
                        self.server.id
                    ),
                },
            )),
        }
    }

    /// The credential headers.
    fn auth_headers(&self) -> Result<Vec<HttpHeader>> {
        Ok(auth_headers(Some(self.token()?)))
    }

    /// Returns the user id; if it is not known, asks the server and keeps it.
    async fn user_id(&self) -> Result<String> {
        if let Ok(cached) = self.user_id.read()
            && let Some(id) = cached.as_ref()
        {
            return Ok(id.clone());
        }
        let id = fetch_user_id(&self.server, &*self.http).await?;
        if let Ok(mut slot) = self.user_id.write() {
            *slot = Some(id.clone());
        }
        Ok(id)
    }

    /// Sends an authenticated GET and decodes the JSON.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str, what: &str) -> Result<T> {
        let request = HttpRequest::get(url).with_headers(self.auth_headers()?);
        let response = self.http.send(&request).await?;
        response.error_for_status(url)?;
        net::parse_json(&response, what)
    }

    /// The album a track belongs to, if the server says.
    async fn album_of(&self, item_id: &str) -> Result<Option<String>> {
        let user = self.user_id().await?;
        let url = format!(
            "{}/Users/{}/Items/{}",
            self.server.url,
            net::encode_query(&user),
            net::encode_query(item_id)
        );
        let item: Item = self.get_json(&url, "jellyfin item").await?;
        Ok(item.album_id.filter(|album| !album.is_empty()))
    }

    /// A track's stream URL. The credentials go **in a header**.
    #[must_use]
    pub fn stream_url(&self, item_id: &str) -> String {
        format!(
            "{}/Audio/{}/stream?static=true",
            self.server.url,
            net::encode_query(item_id)
        )
    }
}

impl Provider for JellyfinProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.server.id.clone(),
            display_name: self.server.display_name(),
            // ARTWORK: the item's `Primary` image, or its album's (D-076).
            // LYRICS: `/Audio/{id}/Lyrics`, Jellyfin 10.9 and later (D-078).
            capabilities: Capabilities::SEARCH
                | Capabilities::BROWSE
                | Capabilities::STREAM
                | Capabilities::ARTWORK
                | Capabilities::LYRICS,
        }
    }

    fn health<'a>(&'a self) -> ProviderFuture<'a, ProviderHealth> {
        Box::pin(async move {
            let unreachable = |detail: String| ProviderHealth {
                id: self.server.id.clone(),
                reachable: false,
                track_count: None,
                detail: Some(detail),
            };

            // An endpoint without credentials: is the server up?
            let public_url = format!("{}/System/Info/Public", self.server.url);
            let public: PublicInfo = match self.get_json(&public_url, "jellyfin public info").await
            {
                Ok(info) => info,
                Err(err) => return Ok(unreachable(err.chain_text().replace('\n', " "))),
            };
            let mut detail = format!(
                "{} {}",
                public.server_name.unwrap_or_else(|| "Jellyfin".to_owned()),
                public
                    .version
                    .unwrap_or_else(|| "(no version reported)".to_owned())
            );

            // An authenticated endpoint: is the key still valid? A server that
            // is up but refuses is not "reachable" — the user has a different
            // job to do.
            let user_id = match self.user_id().await {
                Ok(id) => id,
                Err(err) => {
                    return Ok(unreachable(format!(
                        "{detail} — could not authenticate: {}",
                        err.chain_text().replace('\n', " ")
                    )));
                }
            };

            let count_url = format!(
                "{}/Users/{}/Items?IncludeItemTypes=Audio&Recursive=true&Limit=0",
                self.server.url,
                net::encode_query(&user_id)
            );
            let track_count = match self
                .get_json::<ItemsPage>(&count_url, "jellyfin count")
                .await
            {
                Ok(page) => page.total_record_count.map(|n| n as usize),
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
                track_count,
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
            let user_id = self.user_id().await?;
            let url = format!(
                "{}/Users/{}/Items?searchTerm={}&IncludeItemTypes=Audio&Recursive=true&Limit={}&Fields=RunTimeTicks",
                self.server.url,
                net::encode_query(&user_id),
                net::encode_query(query),
                limit
            );
            let page: ItemsPage = self.get_json(&url, "jellyfin search").await?;

            let items = match page.items {
                Some(items) => items,
                // If the field is missing altogether there are no results; not a silent
                // loss.
                None => return Ok(Vec::new()),
            };

            let mut hits = Vec::with_capacity(items.len());
            let mut skipped = 0usize;
            for item in items {
                match item.into_track(&self.server.id) {
                    Some(track) => hits.push(track),
                    None => skipped += 1,
                }
            }
            if skipped > 0 {
                tracing::warn!(
                    provider = %self.server.id,
                    skipped,
                    "skipped items without a name or an id"
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
            Ok(Some(AudioSource::HttpStream {
                url: self.stream_url(&id.id),
                // The credentials are in a header, not in the URL: if the stream
                // address ends up in the log or on screen, the key must not leak.
                headers: self.auth_headers()?,
            }))
        })
    }

    /// The item's `Primary` image at the width asked for; an audio item often
    /// has none of its own and shows its album's, so that is asked next
    /// (D-076). `Ok(None)`: neither has one.
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
            let headers = self.auth_headers()?;
            let mut item = id.id.clone();
            for asking_the_album in [false, true] {
                if asking_the_album {
                    match self.album_of(&id.id).await? {
                        Some(album) => item = album,
                        None => return Ok(None),
                    }
                }
                let url = format!(
                    "{}/Items/{}/Images/Primary?maxWidth={size}",
                    self.server.url,
                    net::encode_query(&item)
                );
                let request = HttpRequest::get(&url).with_headers(headers.clone());
                let response = self.http.send(&request).await?;
                if response.status == 404 {
                    continue;
                }
                response.error_for_status(&url)?;
                return match super::as_image(&response) {
                    Some(image) => Ok(Some(image)),
                    None => Err(Error::new(
                        Stage::ArtworkRead,
                        ErrorKind::Artwork {
                            detail: format!("{url} answered with something that is not an image"),
                        },
                    )),
                };
            }
            Ok(None)
        })
    }

    /// `/Audio/{id}/Lyrics` (Jellyfin 10.9+, D-078): the lyrics the server
    /// has for the item — its `.lrc`, or what a lyrics plugin on the server
    /// fetched. A line's `Start` is in ticks (100 ns). `404`: none — an older
    /// Jellyfin without the route answers the same, and it has none to give.
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
            let url = format!(
                "{}/Audio/{}/Lyrics",
                self.server.url,
                net::encode_query(&id.id)
            );
            let request = HttpRequest::get(&url).with_headers(self.auth_headers()?);
            let response = self.http.send(&request).await?;
            if response.status == 404 {
                return Ok(None);
            }
            response.error_for_status(&url)?;
            let dto: LyricDto = net::parse_json(&response, "jellyfin lyrics")?;
            Ok(dto.into_lyrics().map(|lyrics| ProviderLyrics {
                lyrics,
                instrumental: false,
                source: crate::lyrics::LyricsSource::Provider,
            }))
        })
    }
}

/// The `Authorization: MediaBrowser ...` header.
///
/// If there is no token (a login request) the field is left out — that is
/// what Jellyfin expects.
fn auth_headers(token: Option<&str>) -> Vec<HttpHeader> {
    let version = env!("CARGO_PKG_VERSION");
    let mut value = format!(
        "MediaBrowser Client=\"{CLIENT_NAME}\", Device=\"{DEVICE_NAME}\", DeviceId=\"{CLIENT_NAME}\", Version=\"{version}\""
    );
    if let Some(token) = token {
        value.push_str(&format!(", Token=\"{token}\""));
    }
    vec![HttpHeader::new("Authorization", value)]
}

/// Turns a user name + password into an access key (D-021).
///
/// # Errors
/// If the server cannot be reached, the credentials are refused or the
/// response contains no key.
pub async fn authenticate(
    url: &str,
    username: &str,
    password: &str,
    id: ProviderId,
    http: &dyn HttpClient,
) -> Result<RemoteServer> {
    let endpoint = format!("{url}/Users/AuthenticateByName");
    let body = serde_json::to_vec(&serde_json::json!({
        "Username": username,
        "Pw": password,
    }))
    .map_err(|source| {
        Error::new(
            Stage::ProviderCall,
            ErrorKind::Json {
                entry: "jellyfin auth request".to_owned(),
                source,
            },
        )
    })?;

    let request = HttpRequest::post_json(&endpoint, body).with_headers(auth_headers(None));
    let response = http.send(&request).await?;
    response.error_for_status(&endpoint)?;

    let auth: AuthResult = net::parse_json(&response, "jellyfin auth response")?;
    let key = auth.access_token.ok_or_else(|| {
        Error::new(
            Stage::ProviderCall,
            ErrorKind::NotFound {
                what: "AccessToken in the Jellyfin response".to_owned(),
            },
        )
    })?;

    Ok(RemoteServer {
        id,
        kind: ServerKind::Jellyfin,
        url: url.to_owned(),
        username: username.to_owned(),
        auth: StoredAuth::ApiKey { key },
        user_id: auth.user.and_then(|user| user.id),
    })
}

/// Asks for the id of the user who owns the key.
///
/// # Errors
/// If the key is invalid or the server cannot be reached.
pub async fn fetch_user_id(server: &RemoteServer, http: &dyn HttpClient) -> Result<String> {
    let token = match &server.auth {
        StoredAuth::ApiKey { key } => key.as_str(),
        StoredAuth::SubsonicToken { .. } => {
            return Err(Error::new(
                Stage::ProviderCall,
                ErrorKind::InvalidInput {
                    detail: format!("{} is not a Jellyfin record", server.id),
                },
            ));
        }
    };
    let url = format!("{}/Users/Me", server.url);
    let request = HttpRequest::get(&url).with_headers(auth_headers(Some(token)));
    let response = http.send(&request).await?;
    response.error_for_status(&url)?;

    let me: UserInfo = net::parse_json(&response, "jellyfin /Users/Me")?;
    me.id.ok_or_else(|| {
        Error::new(
            Stage::ProviderCall,
            ErrorKind::NotFound {
                what: "the user id (Id) in the Jellyfin response".to_owned(),
            },
        )
    })
}

/// Jellyfin's `LyricDto` (read in its source, D-078).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LyricDto {
    #[serde(default)]
    metadata: Option<LyricMetadata>,
    #[serde(default)]
    lyrics: Vec<LyricLineDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LyricMetadata {
    /// Ticks; the LRC `[offset:]` convention — positive, sooner. Jellyfin's
    /// own parser does not fill it today; it is applied when present.
    #[serde(default)]
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LyricLineDto {
    #[serde(default)]
    text: String,
    /// Ticks from the track's start; absent in unsynced lyrics.
    #[serde(default)]
    start: Option<i64>,
}

impl LyricDto {
    fn into_lyrics(self) -> Option<crate::lyrics::Lyrics> {
        use crate::lyrics::{LyricLine, Lyrics};
        if self.lyrics.iter().all(|line| line.text.trim().is_empty()) {
            return None;
        }
        let synced = self.lyrics.iter().all(|line| line.start.is_some());
        // Signed here: an offset may be negative.
        let per_ms = TICKS_PER_MS as i64;
        let offset = self
            .metadata
            .and_then(|metadata| metadata.offset)
            .unwrap_or(0)
            / per_ms;
        let mut lines: Vec<LyricLine> = self
            .lyrics
            .into_iter()
            .map(|line| LyricLine {
                start_ms: if synced {
                    line.start.map(|ticks| {
                        u64::try_from((ticks / per_ms).saturating_sub(offset).max(0)).unwrap_or(0)
                    })
                } else {
                    None
                },
                text: line.text.trim().to_owned(),
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PublicInfo {
    server_name: Option<String>,
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AuthResult {
    access_token: Option<String>,
    user: Option<UserInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UserInfo {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemsPage {
    items: Option<Vec<Item>>,
    total_record_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    id: Option<String>,
    name: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    #[serde(default)]
    artists: Vec<String>,
    run_time_ticks: Option<u64>,
    /// The album the item belongs to — where its cover is when it has none
    /// of its own (D-076).
    #[serde(default)]
    album_id: Option<String>,
}

impl Item {
    fn into_track(self, provider: &ProviderId) -> Option<ProviderTrack> {
        let id = self.id?;
        let title = self.name?;
        let artist = self
            .album_artist
            .or_else(|| self.artists.first().cloned())
            .unwrap_or_else(|| "Unknown artist".to_owned());

        Some(ProviderTrack {
            id: ProviderTrackId::new(provider.clone(), id),
            track: TrackRef::new(artist, title)
                .with_album(self.album)
                .with_duration_ms(self.run_time_ticks.map(|ticks| ticks / TICKS_PER_MS)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::fake::FakeHttp;

    fn server() -> RemoteServer {
        RemoteServer {
            id: ProviderId::new("jf"),
            kind: ServerKind::Jellyfin,
            url: "https://jf.ev".to_owned(),
            username: "enai".to_owned(),
            auth: StoredAuth::ApiKey {
                key: "secret-key".to_owned(),
            },
            user_id: Some("u1".to_owned()),
        }
    }

    const SEARCH_JSON: &str = r#"{"Items":[
        {"Id":"i1","Name":"Geceler","Album":"Müptezhel","AlbumArtist":"Ezhel",
         "Artists":["Ezhel"],"RunTimeTicks":2150000000},
        {"Id":"i2","Name":"Felaket","Artists":["Ezhel"]},
        {"Name":"no-id"}],"TotalRecordCount":3}"#;

    #[tokio::test]
    async fn a_track_without_its_own_image_shows_its_albums() {
        let png = crate::artwork::image::tests_support::png_2x2();
        let http = Arc::new(
            FakeHttp::new()
                .route_status("/Items/i1/Images/Primary", 404, "")
                .route("/Users/u1/Items/i1", r#"{"Id":"i1","AlbumId":"al1"}"#)
                .route_bytes("/Items/al1/Images/Primary", "image/png", &png),
        );
        let provider = JellyfinProvider::new(server(), Arc::clone(&http) as Arc<dyn HttpClient>);
        let id = ProviderTrackId::new(ProviderId::new("jf"), "i1");
        let image = provider.artwork(&id, 500).await.unwrap().unwrap();
        assert_eq!(image.bytes, png);
        let asked: Vec<String> = http.requests().iter().map(|r| r.url.clone()).collect();
        assert!(
            asked[0].ends_with("/Items/i1/Images/Primary?maxWidth=500"),
            "{asked:?}"
        );
        assert!(
            asked.last().unwrap().contains("/Items/al1/Images/Primary"),
            "{asked:?}"
        );
        assert!(
            http.requests()
                .iter()
                .all(|r| !r.url.contains("secret-key"))
        );

        let bare = Arc::new(
            FakeHttp::new()
                .route_status("/Items/i1/Images/Primary", 404, "")
                .route("/Users/u1/Items/i1", r#"{"Id":"i1"}"#),
        );
        let provider = JellyfinProvider::new(server(), bare as Arc<dyn HttpClient>);
        assert_eq!(
            provider.artwork(&id, 500).await.unwrap(),
            None,
            "no album, no image"
        );
    }

    #[tokio::test]
    async fn search_maps_items_and_converts_ticks_to_ms() {
        let http = Arc::new(FakeHttp::new().route("/Users/u1/Items", SEARCH_JSON));
        let provider = JellyfinProvider::new(server(), Arc::clone(&http) as Arc<dyn HttpClient>);

        let hits = provider.search("Ezhel", 5).await.unwrap();
        assert_eq!(hits.len(), 2, "an item without an id must be skipped");
        assert_eq!(hits[0].track.title, "Geceler");
        assert_eq!(hits[0].track.artist, "Ezhel");
        assert_eq!(hits[0].track.duration_ms, Some(215_000));
        assert_eq!(
            hits[1].track.artist, "Ezhel",
            "Artists[0] if there is no AlbumArtist"
        );

        let url = http.last_url();
        assert!(url.contains("searchTerm=Ezhel"), "{url}");
        assert!(url.contains("IncludeItemTypes=Audio"), "{url}");
        assert!(url.contains("Limit=5"), "{url}");
    }

    #[tokio::test]
    async fn credentials_travel_in_the_header_not_the_url() {
        let http = Arc::new(FakeHttp::new().route("/Users/u1/Items", SEARCH_JSON));
        let provider = JellyfinProvider::new(server(), Arc::clone(&http) as Arc<dyn HttpClient>);
        provider.search("Ezhel", 5).await.unwrap();

        let request = http.requests().pop().unwrap();
        assert!(
            !request.url.contains("secret-key"),
            "the key must not be in the URL: {}",
            request.url
        );
        let auth = request
            .headers
            .iter()
            .find(|h| h.name == "Authorization")
            .expect("the Authorization header");
        assert!(auth.value.contains("Token=\"secret-key\""), "{auth:?}");
        assert!(auth.value.contains("Client=\"headshell\""), "{auth:?}");
    }

    #[tokio::test]
    async fn resolve_source_carries_the_auth_header() {
        let provider = JellyfinProvider::new(server(), Arc::new(FakeHttp::new()));
        let id = ProviderTrackId::new(ProviderId::new("jf"), "i1");
        let source = provider.resolve_source(&id).await.unwrap().unwrap();
        match source {
            AudioSource::HttpStream { url, headers } => {
                assert_eq!(url, "https://jf.ev/Audio/i1/stream?static=true");
                assert_eq!(headers.len(), 1);
                assert!(headers[0].value.contains("Token="), "{headers:?}");
            }
            other => panic!("an HTTP stream was expected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_http_error_is_reported_with_its_status_and_body() {
        let http = Arc::new(FakeHttp::new().route_status(
            "/Users/u1/Items",
            401,
            "Access token is invalid or expired.",
        ));
        let provider = JellyfinProvider::new(server(), http);

        let err = provider.search("Ezhel", 5).await.unwrap_err();
        let text = err.chain_text();
        // D-023: the server was up and **refused**; this is not a network error.
        // The user's job is to renew their key, not to fiddle with their network.
        assert!(text.starts_with("STEP: PROVIDER_CALL"), "{text}");
        assert!(text.contains("401"), "{text}");
        assert!(text.contains("expired"), "{text}");
    }

    #[tokio::test]
    async fn the_user_id_is_learned_once_when_missing() {
        let mut server = server();
        server.user_id = None;
        let http = Arc::new(
            FakeHttp::new()
                .route("/Users/Me", r#"{"Id":"u9","Name":"enai"}"#)
                .route("/Users/u9/Items", SEARCH_JSON),
        );
        let provider = JellyfinProvider::new(server, Arc::clone(&http) as Arc<dyn HttpClient>);

        provider.search("Ezhel", 5).await.unwrap();
        provider.search("Ezhel", 5).await.unwrap();

        let me_calls = http
            .requests()
            .iter()
            .filter(|req| req.url.ends_with("/Users/Me"))
            .count();
        assert_eq!(me_calls, 1, "the user id must not be asked on every search");
    }

    #[tokio::test]
    async fn authentication_stores_a_token_not_the_password() {
        let http = FakeHttp::new().route(
            "/Users/AuthenticateByName",
            r#"{"AccessToken":"tok123","User":{"Id":"u1","Name":"enai"}}"#,
        );
        let stored = authenticate(
            "https://jf.ev",
            "enai",
            "sesame",
            ProviderId::new("jf"),
            &http,
        )
        .await
        .unwrap();

        assert_eq!(stored.user_id.as_deref(), Some("u1"));
        match &stored.auth {
            StoredAuth::ApiKey { key } => assert_eq!(key, "tok123"),
            other => panic!("a key was expected: {other:?}"),
        }
        // The password only went out in the request; it did not get into the
        // record.
        let stored_json = serde_json::to_string(&stored).unwrap();
        assert!(!stored_json.contains("sesame"), "{stored_json}");
    }

    #[tokio::test]
    async fn a_songs_lyrics_come_in_ticks_and_their_absence_is_none() {
        let id = ProviderTrackId::new(ProviderId::new("jf"), "i1");
        let synced = r#"{"Metadata":{"IsSynced":true},"Lyrics":[
            {"Text":"second","Start":45000000},{"Text":"first","Start":12340000}]}"#;
        let http = Arc::new(FakeHttp::new().route("/Audio/i1/Lyrics", synced));
        let provider = JellyfinProvider::new(server(), Arc::clone(&http) as Arc<dyn HttpClient>);
        assert!(provider.info().capabilities.contains(Capabilities::LYRICS));
        let found = provider.lyrics(&id).await.unwrap().unwrap();
        assert!(found.lyrics.synced);
        let lines: Vec<(Option<u64>, &str)> = found
            .lyrics
            .lines
            .iter()
            .map(|line| (line.start_ms, line.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [(Some(1_234), "first"), (Some(4_500), "second")],
            "ticks are 100 ns; the lines in time order"
        );
        assert!(
            http.requests()
                .iter()
                .all(|r| !r.url.contains("secret-key")),
            "the key goes in a header"
        );

        let unsynced = r#"{"Metadata":{},"Lyrics":[{"Text":"a"},{"Text":"b"}]}"#;
        let provider = JellyfinProvider::new(
            server(),
            Arc::new(FakeHttp::new().route("/Audio/i1/Lyrics", unsynced)) as Arc<dyn HttpClient>,
        );
        let found = provider.lyrics(&id).await.unwrap().unwrap();
        assert!(!found.lyrics.synced);
        assert!(
            found
                .lyrics
                .lines
                .iter()
                .all(|line| line.start_ms.is_none())
        );

        let provider = JellyfinProvider::new(
            server(),
            Arc::new(FakeHttp::new().route_status("/Audio/i1/Lyrics", 404, ""))
                as Arc<dyn HttpClient>,
        );
        assert_eq!(provider.lyrics(&id).await.unwrap(), None);
    }
}
