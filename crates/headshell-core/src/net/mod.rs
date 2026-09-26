//! The HTTP transport boundary (D-020).
//!
//! The core does not connect to the network **directly**: [`HttpClient`] is a
//! trait, and the concrete client is behind the `http-client` feature. This
//! has three consequences:
//!
//! 1. Provider logic (Subsonic, Jellyfin) can be tested without a network —
//!    the tests supply a fake client.
//! 2. Mobile bindings can supply their own HTTP stacks as
//!    `Arc<dyn HttpClient>`; they do not carry a second TLS tree (K7).
//! 3. The choice of crate inside the feature becomes a detail that can be
//!    undone.
//!
//! ## The `uniffi` constraint (K7)
//!
//! The trait is `dyn` compatible: it returns boxed futures and carries no
//! generics/lifetimes/closures. The body is a `Vec<u8>`, the headers a list of
//! name/value pairs — all types `uniffi` can express.

#[cfg(feature = "http-client")]
mod ureq_client;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};

#[cfg(feature = "http-client")]
pub use ureq_client::UreqClient;

/// A single HTTP header.
///
/// A list, not a `HashMap`: simpler for `uniffi`, and the order is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

impl HttpHeader {
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// The supported methods.
///
/// Deliberately narrow: `GET` and `POST` are enough for remote providers.
/// Others are added if needed; carrying a method nobody needs today is a
/// pointless burden on foreign implementations (mobile).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
}

impl HttpMethod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The request to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<HttpHeader>,
    /// The body (for POST). `None` means no body.
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    #[must_use]
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: HttpMethod::Get,
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    #[must_use]
    pub fn post_json(url: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            method: HttpMethod::Post,
            url: url.into(),
            headers: vec![HttpHeader::new("Content-Type", "application/json")],
            body: Some(body.into()),
        }
    }

    /// A form-encoded POST (`application/x-www-form-urlencoded`).
    ///
    /// For where GET is not enough: a Chromaprint fingerprint takes thousands of
    /// characters once turned into base64, and many servers/proxies cut URLs at
    /// around 8 KB. A cut URL looks like "no match" — that is, the hardest kind
    /// of failure to diagnose (K9). The body has no such limit.
    #[must_use]
    pub fn post_form(url: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            method: HttpMethod::Post,
            url: url.into(),
            headers: vec![HttpHeader::new(
                "Content-Type",
                "application/x-www-form-urlencoded",
            )],
            body: Some(body.into()),
        }
    }

    #[must_use]
    pub fn with_headers(mut self, headers: Vec<HttpHeader>) -> Self {
        self.headers.extend(headers);
        self
    }
}

/// The response that came back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<HttpHeader>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Reads a header (case-insensitively).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }

    /// Returns the body as text (lossy if it is not UTF-8; it is for
    /// diagnostics).
    #[must_use]
    pub fn text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Returns an error if not 2xx.
    ///
    /// The start of the body goes into the error message: "it returned 500" is
    /// not a diagnosis, what the server said is (K9).
    ///
    /// # Errors
    /// If the status code is outside 2xx.
    pub fn error_for_status(&self, url: &str) -> Result<()> {
        if self.is_success() {
            return Ok(());
        }
        Err(Error::new(
            stage_for_status(self.status),
            ErrorKind::HttpStatus {
                url: url.to_owned(),
                status: self.status,
                detail: clip(&self.text_lossy(), DETAIL_LIMIT),
            },
        ))
    }
}

/// How much of the body (in bytes) is taken into the error detail.
const DETAIL_LIMIT: usize = 200;

/// A limiter that makes sure at least `interval` passes between calls.
///
/// For every service with a quota: MusicBrainz one request per second,
/// AcoustID three per second. One instance is kept per service — the limit is
/// not shared, each service's quota is measured with its own counter.
///
/// ## Why `thread::sleep` rather than an `async` sleep
///
/// The core does not set up a runtime (K7 / convention: "let the caller pick
/// the runtime") and so cannot call `tokio::time::sleep` — `tokio` is not a
/// dependency of the core. The HTTP client beneath us (`ureq`) is synchronous
/// anyway: every request blocks the calling thread. So the limiter blocking
/// the same thread adds no new constraint; it is consistent with the existing
/// one.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: Mutex::new(None),
        }
    }

    /// Waits until its turn and updates the stamp on the way out.
    ///
    /// The lock is **not held** during the sleep: if two threads come in at once
    /// both wait, but one does not lengthen the other's sleep.
    pub(crate) fn acquire(&self) {
        let wait = {
            // If the lock is poisoned (another thread panicked) we do not drop the
            // limit: instead of `unwrap`, "I don't know, wait the full interval".
            let Ok(mut last) = self.last.lock() else {
                std::thread::sleep(self.interval);
                return;
            };
            let now = Instant::now();
            let wait = last
                .map(|prev| self.interval.saturating_sub(now.duration_since(prev)))
                .unwrap_or_default();
            // Move the stamp forward now: the next caller should account for our sleep
            // too, otherwise both wake up together.
            *last = Some(now + wait);
            wait
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

/// Clips text to at most `limit` bytes — **on a character boundary**.
///
/// `String::truncate` panics if it lands in the middle of a character; a
/// server's Turkish (or any multi-byte) error message could bring
/// `headshell` down. K8: no panics in the core.
pub(crate) fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Which stage an HTTP status code belongs to (D-023).
///
/// `401`/`403` **are not transport errors**: the connection was made, the
/// request went out, the server read it and refused. Reporting this as
/// `NETWORK_REQUEST` sends the user off to check their network, when what they
/// need to do is fix their credentials. K9's "could not reach ≠ said no"
/// distinction holds here too. The remaining codes (404, 5xx, 429…) stay in
/// the transport layer: for those, which layer failed cannot be known without
/// reading the body.
pub(crate) fn stage_for_status(status: u16) -> Stage {
    match status {
        401 | 403 => Stage::ProviderCall,
        _ => Stage::NetworkRequest,
    }
}

/// The return value of an HTTP call.
///
/// A boxed future instead of `async fn`: the trait has to be `dyn`
/// compatible (the same reasoning as K7 / D-006).
pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + 'a>>;

/// Anything that can go online.
///
/// The response body is **taken into memory whole**: this trait is for
/// metadata calls (search, ping, track info). Audio bytes do not go through
/// here — the stream is read progressively on the [`crate::playback`] side,
/// otherwise a 40 MB FLAC would have to download completely before it
/// started playing.
pub trait HttpClient: Send + Sync {
    fn send<'a>(&'a self, request: &'a HttpRequest) -> HttpFuture<'a>;
}

/// This build's default HTTP client.
///
/// # Errors
/// If the `http-client` feature is off: the caller must supply its own
/// client. Rather than silently saying "no network" we say which build
/// decision caused it.
#[cfg(feature = "http-client")]
pub fn default_http_client() -> Result<Arc<dyn HttpClient>> {
    Ok(Arc::new(UreqClient::new()))
}

/// This build's default HTTP client.
///
/// # Errors
/// In this build `http-client` is off, so it **always** returns an error.
#[cfg(not(feature = "http-client"))]
pub fn default_http_client() -> Result<Arc<dyn HttpClient>> {
    Err(no_http_client())
}

/// The HTTP client given to plugins: one that **does not follow redirects**
/// (D-069).
///
/// The engine follows every redirect itself and asks the allow list again; a
/// client that followed redirects itself would punch through that check.
///
/// # Errors
/// If the `http-client` feature is off.
#[cfg(feature = "http-client")]
pub fn plugin_http_client() -> Result<Arc<dyn HttpClient>> {
    Ok(Arc::new(UreqClient::without_redirects()))
}

/// The HTTP client given to plugins.
///
/// # Errors
/// In this build `http-client` is off, so it **always** returns an error.
#[cfg(not(feature = "http-client"))]
pub fn plugin_http_client() -> Result<Arc<dyn HttpClient>> {
    Err(no_http_client())
}

#[cfg(not(feature = "http-client"))]
fn no_http_client() -> Error {
    Error::new(
        Stage::NetworkRequest,
        ErrorKind::Unsupported {
            provider: "net".to_owned(),
            what: "HTTP client (a build with the `http-client` feature off)".to_owned(),
            capabilities: "NONE".to_owned(),
        },
    )
}

/// Decodes the response body as JSON.
///
/// `pub(crate)`: if a generic signature were public, K7 would break. Callers
/// see the typed provider surface, not this helper.
pub(crate) fn parse_json<T: serde::de::DeserializeOwned>(
    response: &HttpResponse,
    what: &str,
) -> Result<T> {
    serde_json::from_slice(&response.body).map_err(|source| {
        Error::new(
            Stage::ProviderCall,
            ErrorKind::Json {
                entry: what.to_owned(),
                source,
            },
        )
    })
}

/// Produces a network-layer error (could not connect, timeout, TLS…).
///
/// It exists only in builds that really touch a socket: the concrete client
/// (`http-client`) and the fake client in tests. In a core built with the
/// default features it has no caller — keeping it there was a dead-code
/// warning.
#[cfg(any(feature = "http-client", test))]
pub(crate) fn network_err(url: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(
        Stage::NetworkRequest,
        ErrorKind::Network {
            url: url.to_owned(),
            detail: detail.to_string(),
        },
    )
}

/// Percent-encodes a query parameter.
///
/// We write it ourselves: adding an encoding crate for a single use grows the
/// tree. The rule is RFC 3986's `unreserved` set — every other byte becomes
/// `%XX`, so searches with spaces and Turkish characters are not mangled.
pub(crate) fn encode_query(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[usize::from(byte >> 4)] as char);
                out.push(HEX[usize::from(byte & 0x0f)] as char);
            }
        }
    }
    out
}

/// An address without its query string and fragment, for an error text.
///
/// Some addresses carry their key there: a Subsonic request its token and
/// salt, a stream its signature. An error text ends up in `headshell diag`,
/// which is made to be pasted into a bug report; the host and the path say
/// where it failed without handing anyone the key.
#[must_use]
pub(crate) fn without_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// The first bytes of an audio stream, fetched before the track is asked
/// for (D-077), and the stream's whole length as the server stated it.
///
/// Only a stream the server cut as a range is kept this way: the rest is
/// fetched later from where the head ends, and two bodies that were not cut
/// from the same file cannot be joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamHead {
    pub bytes: Vec<u8>,
    /// The whole stream's length, from the head's `Content-Range`.
    pub total: u64,
}

/// The headers with `Range` set to `value`, whatever `Range` they carried.
///
/// A plugin may hand its stream over with a `Range` of its own (YouTube's
/// `bytes=0-`, D-048); fetching a head or the rest after it has to ask for
/// its own range instead.
#[cfg(feature = "http-client")]
#[must_use]
pub(crate) fn with_range(headers: &[HttpHeader], value: &str) -> Vec<HttpHeader> {
    let mut out: Vec<HttpHeader> = headers
        .iter()
        .filter(|header| !header.name.eq_ignore_ascii_case("range"))
        .cloned()
        .collect();
    out.push(HttpHeader::new("Range", value));
    out
}

/// Reads `Content-Range: bytes <start>-<end>/<total>`.
///
/// Returns the first byte, the last byte and the total; the total is `None`
/// when the server wrote `*`. Anything else — another unit, an unsatisfied
/// range (`*/<total>`), a last byte before the first — is `None`.
#[cfg(feature = "http-client")]
#[must_use]
pub(crate) fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.trim().split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        text => Some(text.parse::<u64>().ok()?),
    };
    Some((start, end, total))
}

#[cfg(test)]
pub(crate) mod fake;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_in_an_error_keeps_its_host_and_path_but_not_its_query() {
        assert_eq!(
            without_query("https://music.home/rest/stream?u=enai&t=26719a&s=c19b2d&id=a1"),
            "https://music.home/rest/stream"
        );
        assert_eq!(
            without_query("https://cf-media.sndcdn.com/x.mp3#t=1"),
            "https://cf-media.sndcdn.com/x.mp3"
        );
        assert_eq!(without_query("http://ev/rest/ping"), "http://ev/rest/ping");
    }

    #[cfg(feature = "http-client")]
    #[test]
    fn a_content_range_gives_its_first_byte_last_byte_and_total() {
        assert_eq!(
            parse_content_range("bytes 0-524287/7570557"),
            Some((0, 524_287, Some(7_570_557)))
        );
        assert_eq!(
            parse_content_range("bytes 524288-7570556/*"),
            Some((524_288, 7_570_556, None))
        );
        // An unsatisfied range, another unit, a backwards range: none is a range.
        assert_eq!(parse_content_range("bytes */7570557"), None);
        assert_eq!(parse_content_range("items 0-9/10"), None);
        assert_eq!(parse_content_range("bytes 10-9/20"), None);
        assert_eq!(parse_content_range("bytes 0-9/lots"), None);
    }

    /// The plugin's own `Range` (YouTube's `bytes=0-`) gives way to the one
    /// asked for; the other headers stay.
    #[cfg(feature = "http-client")]
    #[test]
    fn asking_for_a_range_replaces_the_range_the_source_carried() {
        let headers = vec![
            HttpHeader::new("range", "bytes=0-"),
            HttpHeader::new("Authorization", "MediaBrowser Token=x"),
        ];
        let out = with_range(&headers, "bytes=100-");
        assert_eq!(
            out,
            vec![
                HttpHeader::new("Authorization", "MediaBrowser Token=x"),
                HttpHeader::new("Range", "bytes=100-"),
            ]
        );
    }

    #[test]
    fn query_encoding_survives_spaces_and_turkish() {
        assert_eq!(encode_query("Ezhel - Geceler"), "Ezhel%20-%20Geceler");
        assert_eq!(encode_query("Şebnem"), "%C5%9Eebnem");
        assert_eq!(encode_query("a~b_c.d-e"), "a~b_c.d-e");
    }

    #[test]
    fn non_2xx_becomes_an_error_carrying_the_body() {
        let response = HttpResponse {
            status: 500,
            headers: Vec::new(),
            body: b"nope".to_vec(),
        };
        let err = response
            .error_for_status("http://ev/rest/ping")
            .unwrap_err();
        let text = err.chain_text();
        assert!(text.starts_with("STEP: NETWORK_REQUEST"), "{text}");
        assert!(text.contains("500"), "{text}");
        assert!(
            text.contains("nope"),
            "what the server said must be visible: {text}"
        );
    }

    /// D-023: being refused is not a network error — the stage must say so.
    #[test]
    fn a_rejected_request_is_reported_at_the_provider_stage_not_the_network() {
        for status in [401, 403] {
            let response = HttpResponse {
                status,
                headers: Vec::new(),
                body: b"Access token is invalid or expired.".to_vec(),
            };
            let text = response
                .error_for_status("http://ev/Users/Me")
                .unwrap_err()
                .chain_text();
            assert!(
                text.starts_with("STEP: PROVIDER_CALL"),
                "{status} credential refusal: {text}"
            );
            assert!(text.contains(&status.to_string()), "{text}");
        }

        // The rest stay in the transport layer: which layer failed cannot be known
        // without reading the body.
        for status in [404, 429, 500, 502] {
            let response = HttpResponse {
                status,
                headers: Vec::new(),
                body: Vec::new(),
            };
            let text = response
                .error_for_status("http://ev/rest/ping")
                .unwrap_err()
                .chain_text();
            assert!(
                text.starts_with("STEP: NETWORK_REQUEST"),
                "{status}: {text}"
            );
        }
    }

    /// A server's multi-byte error message must **not bring `headshell` down**
    /// (K8).
    #[test]
    fn a_long_multibyte_body_is_clipped_without_panicking() {
        // "ğ" is 2 bytes: the 200-byte limit lands in the middle of a character.
        let response = HttpResponse {
            status: 500,
            headers: Vec::new(),
            body: "ğ".repeat(300).into_bytes(),
        };
        let text = response
            .error_for_status("http://ev/rest/ping")
            .unwrap_err()
            .chain_text();
        assert!(text.contains('ğ'), "{text}");

        assert_eq!(clip("short", 200), "short");
        assert_eq!(clip(&"ğ".repeat(300), 200).len(), 200);
        // It must step back one character; the boundary must not stay in the middle.
        assert_eq!(clip(&"ğ".repeat(300), 201).len(), 200);
    }

    #[test]
    fn headers_are_read_case_insensitively() {
        let response = HttpResponse {
            status: 200,
            headers: vec![HttpHeader::new("Content-Length", "42")],
            body: Vec::new(),
        };
        assert_eq!(response.header("content-length"), Some("42"));
        assert_eq!(response.header("CONTENT-LENGTH"), Some("42"));
        assert_eq!(response.header("etag"), None);
    }

    /// Does the limiter really make callers wait — measured by time.
    #[test]
    fn the_rate_limiter_actually_spaces_calls_apart() {
        let limiter = RateLimiter::new(Duration::from_millis(40));
        let start = Instant::now();
        limiter.acquire(); // the first one does not wait
        limiter.acquire();
        limiter.acquire();
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(80),
            "it should have waited two intervals, {elapsed:?} passed"
        );
    }
}
