//! Transport-layer test: a **real** `UreqClient` ↔ a **real** socket (D-022).
//!
//! The unit tests use a fake `HttpClient`; they test provider logic (building
//! URLs, parsing JSON, translating errors) but never touch `ureq` itself, a
//! real socket or HTTP framing. The server here is written by hand with
//! `std::net` — no new dependency — and it records the requests, so **what
//! went over the wire** is verified.
//!
//! **What this does not test** (D-022, on record explicitly): the quirks of
//! a real Navidrome or Jellyfin install — redirects, transcoding, date
//! formats, version differences. The fake server locks down the protocol *as
//! we understood it*; it does not prove we understood it right.
//!
//! Without the `http-client` feature this file compiles empty.

#![cfg(feature = "http-client")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use headshell_core::ids::{ProviderId, ProviderTrackId};
use headshell_core::net::{HttpClient, UreqClient};
use headshell_core::provider::AudioSource;
use headshell_core::provider::remote::{self, NewServer, RemoteServer, ServerKind, StoredAuth};

// ————————————————————————————————————————————————————————————————
// Fake server
// ————————————————————————————————————————————————————————————————

/// A request the server saw.
#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The response to return.
struct Resp {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Resp {
    fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.into().into_bytes(),
        }
    }

    /// Only for tests that serve an audio body — they are behind `audio` too.
    #[cfg(feature = "audio")]
    fn bytes(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type,
            body,
        }
    }

    fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.into().into_bytes(),
        }
    }
}

/// A tiny HTTP server that binds to a random port and records its requests.
///
/// One request per connection (`Connection: close`) — we do not try to mimic
/// pooling and keep-alive; what we test is the client writing the right
/// bytes to a real socket and decoding the response correctly.
struct FakeServer {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Req>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeServer {
    fn start<H>(handler: H) -> Self
    where
        H: Fn(&Req) -> Resp + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the listener must bind");
        let addr = listener.local_addr().expect("the address must be readable");
        // A non-blocking accept: so the thread does not hang forever without
        // seeing the stop flag.
        listener
            .set_nonblocking(true)
            .expect("non-blocking mode must be set");

        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let seen = Arc::clone(&seen);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            serve_once(stream, &handler, &seen);
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };

        Self {
            addr,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn seen(&self) -> Vec<Req> {
        self.seen.lock().map(|log| log.clone()).unwrap_or_default()
    }

    /// The first request whose path matches.
    fn request_to(&self, path: &str) -> Option<Req> {
        self.seen().into_iter().find(|req| req.path == path)
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_once<H>(stream: TcpStream, handler: &H, seen: &Mutex<Vec<Req>>)
where
    H: Fn(&Req) -> Resp,
{
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let Ok(peer) = stream.try_clone() else { return };
    let mut reader = BufReader::new(peer);
    let Some(request) = read_request(&mut reader) else {
        return;
    };
    if let Ok(mut log) = seen.lock() {
        log.push(request.clone());
    }

    let response = handler(&request);
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason(response.status),
        response.content_type,
        response.body.len()
    );
    let mut out = stream;
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(&response.body);
    let _ = out.flush();
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Req> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();

    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            break;
        }
        let trimmed = header.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }

    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }

    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_owned(), query.to_owned()),
        None => (target.clone(), String::new()),
    };
    Some(Req {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    }
}

fn client() -> Arc<dyn HttpClient> {
    Arc::new(UreqClient::new())
}

/// The path of the audio fixture. All its callers are behind `audio`: with
/// the feature off this helper was a dead-code warning.
#[cfg(feature = "audio")]
fn fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/audio"))
        .join(name)
}

// ————————————————————————————————————————————————————————————————
// Subsonic
// ————————————————————————————————————————————————————————————————

const SUBSONIC_PING: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
    "type":"navidrome","serverVersion":"0.53.3"}}"#;
const SUBSONIC_SCAN: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
    "scanStatus":{"scanning":false,"count":8123}}}"#;
const SUBSONIC_SEARCH: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
    "searchResult3":{"song":[
    {"id":"a1","title":"Sine Track","artist":"Test Artist","album":"Fixture",
     "duration":3,"isrc":["TRABC2400001"]},
    {"title":"no id"}]}}}"#;

fn subsonic_routes(req: &Req) -> Resp {
    match req.path.as_str() {
        "/rest/ping" => Resp::json(SUBSONIC_PING),
        "/rest/getScanStatus" => Resp::json(SUBSONIC_SCAN),
        "/rest/search3" => Resp::json(SUBSONIC_SEARCH),
        _ => Resp::status(404, "no such path"),
    }
}

fn subsonic_spec(url: &str) -> NewServer {
    NewServer {
        id: ProviderId::new("ev"),
        kind: ServerKind::Subsonic,
        url: url.to_owned(),
        username: "enai".to_owned(),
        password: Some("sesame".to_owned()),
        api_key: None,
        verify: true,
    }
}

/// Registration connects to a real socket and **the password never goes over
/// the wire** (D-021).
#[tokio::test]
async fn registering_a_subsonic_server_verifies_over_a_real_socket_without_sending_the_password() {
    let server = FakeServer::start(subsonic_routes);
    let (stored, notes) = remote::prepare_server(&subsonic_spec(&server.url()), client())
        .await
        .expect("the registration must verify");

    assert_eq!(stored.kind, ServerKind::Subsonic);
    match &stored.auth {
        StoredAuth::SubsonicToken { salt, token } => {
            assert_eq!(token.len(), 32, "an md5 is 32 hex characters: {token}");
            assert!(!salt.is_empty());
        }
        other => panic!("a Subsonic token was expected: {other:?}"),
    }
    assert!(
        notes.iter().all(|note| !note.contains("weak")),
        "/dev/urandom must be readable on this machine: {notes:?}"
    );

    // The real claim: what went over the wire?
    let ping = server
        .request_to("/rest/ping")
        .expect("a ping must be sent");
    assert_eq!(ping.method, "GET");
    assert!(ping.query.contains("u=enai"), "{}", ping.query);
    assert!(ping.query.contains("&t="), "{}", ping.query);
    assert!(ping.query.contains("&s="), "{}", ping.query);
    assert!(
        !ping.query.contains("p="),
        "the password must not end up in the query string: {}",
        ping.query
    );
    assert!(
        !server
            .seen()
            .iter()
            .any(|req| req.query.contains("sesame") || req.body_text().contains("sesame")),
        "the password must not appear in any request"
    );

    // The verification must have read the health too.
    assert!(server.request_to("/rest/getScanStatus").is_some());
}

/// Search and stream resolution through a real socket from a registered
/// server.
#[tokio::test]
async fn a_registered_subsonic_server_searches_and_resolves_a_stream_url() {
    let server = FakeServer::start(subsonic_routes);
    let (stored, _) = remote::prepare_server(&subsonic_spec(&server.url()), client())
        .await
        .expect("registration");
    let provider = remote::provider_for(&stored, client());

    let hits = provider.search("sine", 10).await.expect("search");
    assert_eq!(hits.len(), 1, "a row without an id must be skipped");
    assert_eq!(hits[0].track.title, "Sine Track");
    assert_eq!(hits[0].track.duration_ms, Some(3_000));
    assert_eq!(hits[0].id.id, "a1");

    let source = provider
        .resolve_source(&hits[0].id)
        .await
        .expect("resolution")
        .expect("source");
    match source {
        AudioSource::HttpStream { url, headers } => {
            assert!(url.contains("/rest/stream?"), "{url}");
            assert!(url.contains("id=a1"), "{url}");
            assert!(
                headers.is_empty(),
                "Subsonic's credentials are in the query string"
            );
        }
        other => panic!("an HTTP stream was expected: {other:?}"),
    }
}

/// Subsonic sends errors **with HTTP 200**; even though the transport
/// succeeded, the command must fail. We lock this trap down with a real
/// client too.
#[tokio::test]
async fn a_subsonic_failure_arrives_with_http_200_and_still_fails() {
    let server = FakeServer::start(|_| {
        Resp::json(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
                "error":{"code":40,"message":"Wrong username or password."}}}"#,
        )
    });

    let err = remote::prepare_server(&subsonic_spec(&server.url()), client())
        .await
        .expect_err("a registration with a wrong password must not pass");
    let text = err.chain_text();
    assert!(text.contains("could not verify"), "{text}");
    assert!(text.contains("Wrong username or password"), "{text}");
    assert!(
        text.contains("40"),
        "the error code must be visible: {text}"
    );
    // The server **was reached**, the credentials were refused. If the outer
    // sentence said "unreachable", it would send the user hunting for a network
    // error; that was the flaw seen against a real Navidrome.
    assert!(
        !text.contains("unreachable"),
        "being refused is not being unreachable: {text}"
    );
    assert!(
        text.contains("PROVIDER_CALL") && !text.contains("NETWORK_REQUEST"),
        "the stage must show the application layer: {text}"
    );
}

/// A closed port is a **health answer**, not a crash of the command — and
/// it says why.
#[tokio::test]
async fn a_closed_port_is_a_health_answer_with_a_reason() {
    // Bind, take the address, close it: a port nobody listens on any more.
    let listener = TcpListener::bind("127.0.0.1:0").expect("must bind");
    let addr = listener.local_addr().expect("address");
    drop(listener);

    let stored = RemoteServer {
        id: ProviderId::new("closed"),
        kind: ServerKind::Subsonic,
        url: format!("http://{addr}"),
        username: "enai".to_owned(),
        auth: StoredAuth::SubsonicToken {
            salt: "abc".to_owned(),
            token: "0".repeat(32),
        },
        user_id: None,
    };
    let health = remote::provider_for(&stored, client())
        .health()
        .await
        .expect("the health query must not return an error");

    assert!(!health.reachable);
    assert_eq!(health.track_count, None, "unknown is not zero");
    let detail = health.detail.unwrap_or_default();
    assert!(
        detail.contains("NETWORK_REQUEST"),
        "the transport-layer stage must be visible: {detail}"
    );
}

// ————————————————————————————————————————————————————————————————
// Jellyfin
// ————————————————————————————————————————————————————————————————

const JELLYFIN_TOKEN: &str = "access-key";
const JELLYFIN_USER: &str = "u42";

fn jellyfin_routes(req: &Req) -> Resp {
    let authorized = req
        .header("Authorization")
        .is_some_and(|value| value.contains(&format!("Token=\"{JELLYFIN_TOKEN}\"")));

    if req.path == "/Users/AuthenticateByName" {
        if req.method != "POST" || !req.body_text().contains("sesame") {
            return Resp::status(401, "credentials refused");
        }
        return Resp::json(format!(
            r#"{{"AccessToken":"{JELLYFIN_TOKEN}","User":{{"Id":"{JELLYFIN_USER}","Name":"enai"}}}}"#
        ));
    }
    if req.path == "/System/Info/Public" {
        return Resp::json(r#"{"ServerName":"Home Jellyfin","Version":"10.9.11"}"#);
    }
    if !authorized {
        return Resp::status(401, "Access token is invalid or expired.");
    }
    if req.path == "/Users/Me" {
        return Resp::json(format!(r#"{{"Id":"{JELLYFIN_USER}","Name":"enai"}}"#));
    }
    if req.path == format!("/Users/{JELLYFIN_USER}/Items") {
        return Resp::json(
            r#"{"Items":[{"Id":"i1","Name":"Sine Track","Album":"Fixture",
                "AlbumArtist":"Test Artist","RunTimeTicks":30000000}],
                "TotalRecordCount":4211}"#,
        );
    }
    Resp::status(404, "no such path")
}

/// On Jellyfin the password is turned into a key once and **the key is
/// stored**; later requests carry the credentials in a header (D-021).
#[tokio::test]
async fn jellyfin_exchanges_the_password_for_a_token_over_a_real_socket() {
    let server = FakeServer::start(jellyfin_routes);
    let spec = NewServer {
        id: ProviderId::new("jf"),
        kind: ServerKind::Jellyfin,
        url: server.url(),
        username: "enai".to_owned(),
        password: Some("sesame".to_owned()),
        api_key: None,
        verify: true,
    };

    let (stored, notes) = remote::prepare_server(&spec, client())
        .await
        .expect("the registration must verify");

    match &stored.auth {
        StoredAuth::ApiKey { key } => assert_eq!(key, JELLYFIN_TOKEN),
        other => panic!("an access key was expected: {other:?}"),
    }
    assert_eq!(
        stored.user_id.as_deref(),
        Some(JELLYFIN_USER),
        "the user id must be learned at registration time"
    );
    assert!(
        notes
            .iter()
            .any(|note| note.contains("the password is not stored")),
        "the user must be told what is stored: {notes:?}"
    );

    // The password only went out in the body of the login request; it did not
    // get into the record.
    let stored_json = serde_json::to_string(&stored).expect("the record must serialise");
    assert!(!stored_json.contains("sesame"), "{stored_json}");

    let auth_call = server
        .request_to("/Users/AuthenticateByName")
        .expect("the login request");
    assert_eq!(auth_call.method, "POST");
    assert_eq!(
        auth_call.header("Content-Type"),
        Some("application/json"),
        "the body must be declared as JSON"
    );

    // Later requests: the key in a header, not in the URL.
    let items = server
        .request_to(&format!("/Users/{JELLYFIN_USER}/Items"))
        .expect("the track count request");
    assert!(
        !items.query.contains(JELLYFIN_TOKEN),
        "the key must not leak into the URL: {}",
        items.query
    );
    assert!(
        items
            .header("Authorization")
            .is_some_and(|value| value.contains(JELLYFIN_TOKEN)),
        "the credentials must go in a header: {:?}",
        items.header("Authorization")
    );
}

/// A server that is up but refuses the key is not "reachable" — the user
/// has a different job to do.
#[tokio::test]
async fn a_jellyfin_server_that_rejects_the_key_is_not_reachable() {
    let server = FakeServer::start(jellyfin_routes);
    let stored = RemoteServer {
        id: ProviderId::new("jf"),
        kind: ServerKind::Jellyfin,
        url: server.url(),
        username: "enai".to_owned(),
        auth: StoredAuth::ApiKey {
            key: "wrong-key".to_owned(),
        },
        user_id: None,
    };

    let health = remote::provider_for(&stored, client())
        .health()
        .await
        .expect("the health query must not return an error");
    assert!(!health.reachable, "{health:?}");
    let detail = health.detail.unwrap_or_default();
    assert!(
        detail.contains("Home Jellyfin"),
        "it must show that it is up: {detail}"
    );
    assert!(
        detail.contains("401"),
        "it must show that it was refused: {detail}"
    );
}

// ————————————————————————————————————————————————————————————————
// Streaming (`audio` + `http-client`)
// ————————————————————————————————————————————————————————————————

/// A remote stream really downloads and can seek backwards.
///
/// **No** audio device needed: what is tested here is the source layer, not
/// the output. symphonia seeks backwards while recognising the container;
/// since `is_seekable` is only true when the server reports a length,
/// `Content-Length` is tested indirectly too.
#[cfg(feature = "audio")]
#[test]
fn an_http_stream_downloads_completely_and_seeks_backwards() {
    use headshell_core::playback::http_source::HttpMediaSource;
    use std::io::{Seek, SeekFrom};

    let flac = std::fs::read(fixture("tagged.flac")).expect("the fixture must be readable");
    let served = flac.clone();
    let server = FakeServer::start(move |req| match req.path.as_str() {
        "/audio.flac" => Resp::bytes("audio/flac", served.clone()),
        _ => Resp::status(404, "no such path"),
    });

    let mut source = HttpMediaSource::open(&format!("{}/audio.flac", server.url()), &[])
        .expect("the stream must open");

    let mut downloaded = Vec::new();
    source
        .read_to_end(&mut downloaded)
        .expect("the bytes must be readable");
    assert_eq!(
        downloaded, flac,
        "the downloaded bytes must be identical to the file"
    );

    source
        .seek(SeekFrom::Start(0))
        .expect("it must go back to the start");
    let mut magic = [0u8; 4];
    source
        .read_exact(&mut magic)
        .expect("the header must be readable");
    assert_eq!(
        &magic, b"fLaC",
        "seeking backwards must give the same bytes"
    );

    source
        .seek(SeekFrom::End(-1))
        .expect("it must seek from the end");
    let mut tail = [0u8; 1];
    source
        .read_exact(&mut tail)
        .expect("the last byte must be readable");
    assert_eq!(tail[0], *flac.last().expect("the file is not empty"));
}

/// If the server returns 404 it must fail **before** starting to play:
/// better than a "playing but no sound" situation.
#[cfg(feature = "audio")]
#[test]
fn a_missing_stream_fails_before_playback_starts() {
    use headshell_core::playback::http_source::HttpMediaSource;

    let server = FakeServer::start(|_| Resp::status(404, "no such track"));
    let err = HttpMediaSource::open(&format!("{}/missing.flac", server.url()), &[])
        .expect_err("a 404 must not silently turn into an empty stream");

    let text = err.chain_text();
    assert!(text.starts_with("STEP: NETWORK_REQUEST"), "{text}");
    assert!(text.contains("404"), "{text}");
    assert!(
        text.contains("no such track"),
        "what the server said must be visible: {text}"
    );
}

/// The remote half of Phase 1's done criterion: audio arriving over HTTP
/// is really decoded and handed to the device.
///
/// In an environment without an audio device it skips itself — not
/// silencing it, but saying the condition is not met and moving on (the same
/// procedure as `playback_local.rs`).
#[cfg(feature = "audio")]
#[test]
fn an_http_stream_really_decodes_and_plays() {
    use cpal::traits::HostTrait;
    use headshell_core::playback::{AudioEngine, PlayState};

    if cpal::default_host().default_output_device().is_none() {
        eprintln!("no audio output — skipping the remote playback test (this is not a failure)");
        return;
    }

    let flac = std::fs::read(fixture("tagged.flac")).expect("the fixture must be readable");
    let server = FakeServer::start(move |req| match req.path.as_str() {
        // An address without an extension, on purpose: Subsonic gives
        // `/rest/stream?id=...`, so the container must be recognised from its
        // contents.
        "/rest/stream" => Resp::bytes("audio/flac", flac.clone()),
        _ => Resp::status(404, "no such path"),
    });

    let engine = match AudioEngine::play_http(&format!("{}/rest/stream?id=a1", server.url()), &[]) {
        Ok(engine) => engine,
        Err(err) => {
            // The device seemed to be there but could not be opened (locked, no
            // permission…).
            eprintln!(
                "could not open the audio device, skipping the test:\n{}",
                err.chain_text()
            );
            return;
        }
    };

    // The duration was read from the container: even with an extension-less
    // address, the container was recognised from its contents.
    let duration = engine
        .duration_ms()
        .expect("the duration must be read from the container");
    assert!(
        (900..=1100).contains(&duration),
        "{duration}ms while a 1-second fixture was expected"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while engine.position_ms() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        engine.position_ms() > 0,
        "not a single frame of the remote stream played (state: {})",
        engine.state()
    );

    // The buffer drains in the callback that plays its last samples, and that
    // callback still says `Playing`; `Stopped` comes with the next one, a
    // buffer later. macOS CI plays ~450 ms buffers (D-090 addendum): reading
    // the state the moment `finished()` turns true failed there once
    // (2026-10-16). Both are waited for, and both must come.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !(engine.finished() && engine.state() == PlayState::Stopped)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        engine.finished(),
        "the track did not end (state: {})",
        engine.state()
    );
    assert_eq!(engine.state(), PlayState::Stopped);
    assert_eq!(
        engine.take_error(),
        None,
        "the stream should have finished without errors"
    );
}

// ————————————————————————————————————————————————————————————————
// Record file ↔ provider record
// ————————————————————————————————————————————————————————————————

/// A record written to disk gives a working provider when read back: the
/// bridge between `headshell provider add` and `headshell provider test`.
#[tokio::test]
async fn a_saved_server_comes_back_as_a_working_provider() {
    let server = FakeServer::start(subsonic_routes);
    let (stored, _) = remote::prepare_server(&subsonic_spec(&server.url()), client())
        .await
        .expect("registration");

    let dir = support::TempDir::new("remote");
    let path = dir.join("servers.json");
    remote::save_servers(&path, &[stored]).expect("must be written");

    let loaded = remote::load_servers(&path).expect("must be read");
    assert_eq!(loaded.len(), 1);
    let provider = remote::provider_for(&loaded[0], client());
    assert_eq!(provider.info().id, ProviderId::new("ev"));

    let health = provider.health().await.expect("health");
    assert!(health.reachable, "{health:?}");
    assert_eq!(health.track_count, Some(8123));

    // Another provider's id must not be accepted silently.
    let foreign = ProviderTrackId::new(ProviderId::new("other"), "a1");
    assert!(provider.resolve_source(&foreign).await.is_err());

    std::fs::remove_dir_all(&dir).ok();
}
