//! The concrete client of the `http-client` feature: `ureq` + rustls (D-020).
//!
//! **This file is the only place that touches `ureq`.** If the crate changes,
//! this changes; providers see the [`super::HttpClient`] trait and are not
//! affected.
//!
//! ## A note on blocking
//!
//! `ureq` is a blocking API. [`UreqClient::send`] blocks inside an async
//! signature — since the core does not choose a runtime (it does not set up
//! `#[tokio::main]`) we cannot call `spawn_blocking`. For the CLI this is not
//! a problem: the command is waiting for the answer anyway. A caller that
//! wants a non-blocking transport (the GUI's event loop, mobile) supplies its
//! own `HttpClient`; the trait boundary is what makes this trade reversible.
//!
//! ## Compressed answers (D-094)
//!
//! The metadata and plugin clients ask for gzip, and ureq unpacks it:
//! InnerTube's search answer is 673 KB plain and 13 KB compressed, and plain
//! it ran past the plugin budget on a lossy link. The clients of
//! `UreqClient::long_body` — audio streams and artifact downloads — do not ask:
//! their byte offsets, `Content-Length` and hashes are the file's own bytes.
//!
//! ureq's body limit counts the bytes **as they come**, before unpacking; the
//! cap on what they unpack to is `read_capped`'s.

use std::io::Read;
use std::time::Duration;

use super::{HttpClient, HttpFuture, HttpHeader, HttpMethod, HttpRequest, HttpResponse};
use crate::error::Result;

/// The cap for metadata responses. Audio bytes do not go through here (see
/// [`UreqClient::open_stream`]); a hostile server must not fill memory.
const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// The general timeout: connecting + reading. If the server hangs, the
/// command must not hang with it.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The time allowed for plugin requests — it must stay below the call budget
/// (20 s).
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(15);

/// Time limits for requests whose body takes long: artifact downloads and
/// audio streams.
///
/// The general timeout is no use here, because it covers the body too: a
/// 40 MB binary or FLAC easily takes more than 30 seconds on a slow
/// connection.
///
/// **There is no total limit**, on purpose: in ureq 3.4 an expired deadline is
/// not an error but becomes a 1 s read timeout, so no total limit ever cuts a
/// body that keeps flowing (measured, D-069 addendum). The protection against
/// stalling is the silence limit; the protection against size is the caller's
/// cap (artifacts 128 MB, streams 256 MB). A slow but flowing download is not
/// cut off — which is also what is right for the user.
#[derive(Debug, Clone, Copy)]
struct LongBody {
    /// The limit for resolving the name, connecting, sending the request and
    /// waiting for the response headers: a server that never answers does not
    /// leave us stuck for longer than this.
    handshake: Duration,
    /// The longest silence between two reads of the body.
    idle: Duration,
}

/// The shared time limits of artifact downloads and audio streams.
const LONG_BODY: LongBody = LongBody {
    handshake: TIMEOUT,
    idle: TIMEOUT,
};

/// A `ureq`-based HTTP client.
pub struct UreqClient {
    agent: ureq::Agent,
}

impl std::fmt::Debug for UreqClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UreqClient")
    }
}

impl Default for UreqClient {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqClient {
    #[must_use]
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // We want the status code as data, not as an error: 401 and "could not
            // connect" are different diagnoses (K9).
            .http_status_as_error(false)
            .user_agent(concat!("headshell/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// The client given to plugins: it **does not follow redirects** (D-069).
    ///
    /// The permission check looks at the request address; if the client followed
    /// redirects itself, an allowed address could carry the plugin somewhere it
    /// is not allowed. A 3xx comes back as it is, and the engine follows each
    /// step and asks again.
    ///
    /// The time limit is shorter too: a plugin call's budget is 20 s and a single
    /// request must not eat all of it — a slow request should come back to the
    /// plugin as an error, rather than the call itself timing out.
    #[must_use]
    pub fn without_redirects() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(PLUGIN_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(concat!("headshell/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// The client for the engine's artifact downloads (D-069).
    #[must_use]
    pub fn for_downloads() -> Self {
        Self::long_body(LONG_BODY)
    }

    /// The client for audio streams (with [`Self::open_stream`]).
    ///
    /// [`Self::new`] used to be used, and its 30 s general timeout covered the
    /// body too: a track that did not download completely in 30 s (FLAC from a
    /// distant server, anything over a slow connection) was cut off halfway.
    #[must_use]
    pub fn for_streams() -> Self {
        Self::long_body(LONG_BODY)
    }

    /// A client without a general timeout, limited stage by stage.
    ///
    /// Waiting for the headers is limited with `timeout_send_request`, **not**
    /// with `timeout_recv_response`. ureq 3.4 checks a stage's time limit in the
    /// following stage too, and counts it from the moment that stage *ended*:
    /// `recv_response` limited the body as well, counting from the moment the
    /// headers arrived. In the download client it was 30 s, and every artifact
    /// that did not download within 30 s was cut off with "timeout: receive
    /// response" (D-069 addendum). The same rule carries `send_request` over to
    /// the header wait and leaves it there: the only predecessor of the body is
    /// `recv_response`. `recv_body` is counted again on every read, so it is a
    /// silence limit, not a total. If the rule changes with ureq, the tests below
    /// fail.
    fn long_body(limits: LongBody) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_resolve(Some(limits.handshake))
            .timeout_connect(Some(limits.handshake))
            .timeout_send_request(Some(limits.handshake))
            .timeout_recv_body(Some(limits.idle))
            // The file's own bytes (D-094): a range, a length and a hash are
            // counted on them.
            .accept_encoding(ureq::config::AutoHeaderValue::None)
            .http_status_as_error(false)
            .user_agent(concat!("headshell/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    fn send_blocking(&self, request: &HttpRequest) -> Result<HttpResponse> {
        let mut response = match request.method {
            HttpMethod::Get => {
                let mut builder = self.agent.get(&request.url);
                for header in &request.headers {
                    builder = builder.header(&header.name, &header.value);
                }
                builder.call()
            }
            HttpMethod::Post => {
                let mut builder = self.agent.post(&request.url);
                for header in &request.headers {
                    builder = builder.header(&header.name, &header.value);
                }
                match &request.body {
                    Some(body) => builder.send(&body[..]),
                    None => builder.send_empty(),
                }
            }
        }
        .map_err(|err| super::network_err(&request.url, err))?;

        let status = response.status().as_u16();
        let mut headers = collect_headers(response.headers());
        if unpacked(response.headers()) {
            // ureq unpacks the body: these two describe the bytes on the wire,
            // not the ones handed on.
            headers.retain(|header| {
                !header.name.eq_ignore_ascii_case("content-encoding")
                    && !header.name.eq_ignore_ascii_case("content-length")
            });
        }
        let body = read_capped(
            response
                .body_mut()
                .with_config()
                .limit(MAX_BODY_BYTES)
                .reader(),
            MAX_BODY_BYTES,
        )
        .map_err(|detail| super::network_err(&request.url, detail))?;

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    /// Opens a **progressive** reader for an audio stream.
    ///
    /// Separate from [`HttpClient::send`], because that one takes the body into
    /// memory whole; downloading a 40 MB FLAC before starting to play it is the
    /// opposite of "gapless playback". It returns a pair: the known total length
    /// (if any) and the reader.
    ///
    /// # Errors
    /// If no connection can be made or the server returns a code outside 2xx.
    pub fn open_stream(
        &self,
        url: &str,
        headers: &[HttpHeader],
    ) -> Result<(Option<u64>, Box<dyn Read + Send>)> {
        let mut builder = self.agent.get(url);
        for header in headers {
            builder = builder.header(&header.name, &header.value);
        }
        // A stream address carries its key in the query string (Subsonic's
        // token, a service's signature): errors name it without.
        let shown = super::without_query(url);
        let response = builder
            .call()
            .map_err(|err| super::network_err(shown, err.to_string().replace(url, shown)))?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            // Producing an error without reading the body leaves "what did it say?"
            // unanswered; we take a short piece of it.
            let mut response = response;
            let detail = response
                .body_mut()
                .with_config()
                .limit(1024)
                .read_to_string()
                .unwrap_or_else(|err| format!("(could not read the body: {err})"));
            // The stage is chosen by the same rule as in `error_for_status` (D-023):
            // getting a 401 while opening a stream is a credential problem too, not a
            // network problem.
            return Err(crate::Error::new(
                super::stage_for_status(status),
                crate::ErrorKind::HttpStatus {
                    url: shown.to_owned(),
                    status,
                    detail,
                },
            ));
        }

        let length = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());

        Ok((length, Box::new(response.into_body().into_reader())))
    }

    /// Fetches the first `len` bytes of a stream ahead of time (D-077).
    ///
    /// `Ok(None)` when the server does not answer the range as a range — a
    /// `200` with the whole body, a `206` without a total or from another
    /// byte: the rest of a head has to be fetched from where it ends, and a
    /// server that does not cut ranges cannot promise the rest belongs to the
    /// same file. A refusal (`4xx`/`5xx`) is an error, as when opening.
    ///
    /// # Errors
    /// If no connection can be made, the server refuses, or the body breaks
    /// off before the head is complete.
    pub fn fetch_head(
        &self,
        url: &str,
        headers: &[HttpHeader],
        len: u64,
    ) -> Result<Option<super::StreamHead>> {
        let Some(last) = len.checked_sub(1) else {
            return Ok(None);
        };
        let shown = super::without_query(url);
        let response = self.call_range(url, headers, &format!("bytes=0-{last}"))?;
        if response.status().as_u16() != 206 {
            return Ok(None);
        }
        let Some((0, end, Some(total))) = content_range(&response) else {
            return Ok(None);
        };
        // A server may cut a shorter range than asked, or send the whole rest
        // under a `206`: what it said it sent, up to what was asked.
        let want = len.min(end.saturating_add(1)).min(total);
        let mut bytes = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
        response
            .into_body()
            .into_reader()
            .take(want)
            .read_to_end(&mut bytes)
            .map_err(|err| super::network_err(shown, err.to_string().replace(url, shown)))?;
        if bytes.len() as u64 != want {
            return Err(super::network_err(
                shown,
                format!("the head broke off after {} of {want} bytes", bytes.len()),
            ));
        }
        Ok(Some(super::StreamHead { bytes, total }))
    }

    /// Opens a stream from byte `offset` of a `total`-byte file (D-077): the
    /// rest after a head.
    ///
    /// The server has to answer as a range — a `206` from exactly `offset` of
    /// a file exactly `total` long. Anything else is an error, not a guess:
    /// bytes from another offset, or of a file that changed, would be joined
    /// to the head as if they followed it.
    ///
    /// # Errors
    /// If no connection can be made, the server refuses, or it does not
    /// continue the same file from `offset`.
    pub fn open_stream_at(
        &self,
        url: &str,
        headers: &[HttpHeader],
        offset: u64,
        total: u64,
    ) -> Result<Box<dyn Read + Send>> {
        let shown = super::without_query(url);
        let response = self.call_range(url, headers, &format!("bytes={offset}-"))?;
        let status = response.status().as_u16();
        let range = content_range(&response);
        if status != 206
            || !matches!(range, Some((start, _, Some(whole))) if start == offset && whole == total)
        {
            let got = match range {
                Some((start, end, whole)) => format!(
                    "bytes {start}-{end} of {}",
                    whole.map_or_else(|| "an unknown length".to_owned(), |n| n.to_string())
                ),
                None => "no range".to_owned(),
            };
            return Err(crate::Error::new(
                crate::diag::Stage::NetworkRequest,
                crate::ErrorKind::HttpStatus {
                    url: shown.to_owned(),
                    status,
                    detail: format!(
                        "asked for the rest of a {total}-byte file from byte {offset}, got {got}"
                    ),
                },
            ));
        }
        Ok(Box::new(response.into_body().into_reader()))
    }

    /// A `GET` with `Range: <range>` in place of any `Range` the source
    /// carried; a status outside 2xx is an error that says what the server
    /// said.
    fn call_range(
        &self,
        url: &str,
        headers: &[HttpHeader],
        range: &str,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let mut builder = self.agent.get(url);
        for header in super::with_range(headers, range) {
            builder = builder.header(&header.name, &header.value);
        }
        let shown = super::without_query(url);
        let mut response = builder
            .call()
            .map_err(|err| super::network_err(shown, err.to_string().replace(url, shown)))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let detail = response
                .body_mut()
                .with_config()
                .limit(1024)
                .read_to_string()
                .unwrap_or_else(|err| format!("(could not read the body: {err})"));
            return Err(crate::Error::new(
                super::stage_for_status(status),
                crate::ErrorKind::HttpStatus {
                    url: shown.to_owned(),
                    status,
                    detail,
                },
            ));
        }
        Ok(response)
    }
}

/// Does ureq unpack this body: it does a gzip one (D-094).
fn unpacked(headers: &ureq::http::HeaderMap) -> bool {
    headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("gzip"))
}

/// Reads a metadata body whole, up to `cap` bytes **unpacked** (D-094): a
/// gzip answer small on the wire can unpack to any size, and ureq's own limit
/// counts it before unpacking.
fn read_capped(reader: impl Read, cap: u64) -> std::result::Result<Vec<u8>, String> {
    let mut body = Vec::new();
    reader
        .take(cap.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|err| err.to_string())?;
    if body.len() as u64 > cap {
        return Err(format!("the answer is larger than {cap} bytes"));
    }
    Ok(body)
}

/// The response's `Content-Range`, read.
fn content_range(response: &ureq::http::Response<ureq::Body>) -> Option<(u64, u64, Option<u64>)> {
    response
        .headers()
        .get("content-range")
        .and_then(|value| value.to_str().ok())
        .and_then(super::parse_content_range)
}

impl crate::plugin::artifact::ArtifactSource for UreqClient {
    fn open(&self, url: &str) -> Result<crate::plugin::artifact::ArtifactResponse> {
        let response = self
            .agent
            .get(url)
            .call()
            .map_err(|err| super::network_err(url, err))?;
        let status = response.status().as_u16();
        let length = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        Ok(crate::plugin::artifact::ArtifactResponse {
            status,
            length,
            body: Box::new(response.into_body().into_reader()),
        })
    }
}

impl HttpClient for UreqClient {
    fn send<'a>(&'a self, request: &'a HttpRequest) -> HttpFuture<'a> {
        // The blocking is deliberate and documented (module header).
        Box::pin(std::future::ready(self.send_blocking(request)))
    }
}

fn collect_headers(map: &ureq::http::HeaderMap) -> Vec<HttpHeader> {
    map.iter()
        .map(|(name, value)| {
            HttpHeader::new(
                name.as_str(),
                // We **do not drop** a header with invalid UTF-8: its presence is
                // information too. The lossy form is for diagnostics.
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! The time limits are tested against a real local server: which limit ureq
    //! counts at which stage could not be read from its documentation; it was
    //! found by measuring.

    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    use super::{LongBody, UreqClient};
    use crate::plugin::artifact::ArtifactSource as _;

    const LIMITS: LongBody = LongBody {
        handshake: Duration::from_millis(500),
        idle: Duration::from_secs(2),
    };

    /// A local server for a single connection: reads the request's headers and
    /// hands the connection to `serve`.
    fn serve_once(serve: impl FnOnce(TcpStream) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut request = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            // The headers end with an empty line.
            while request.read_line(&mut line).unwrap_or(0) > 2 {
                line.clear();
            }
            serve(stream);
        });
        format!("http://{addr}/artifact")
    }

    fn send_headers(stream: &mut TcpStream, length: usize) {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        stream.flush().unwrap();
    }

    /// D-069 addendum: the body used to be cut off 30 s after the headers.
    #[test]
    fn a_body_slower_than_the_handshake_budget_still_arrives_whole() {
        // 10 × 150 ms: the body downloads over three times the header wait limit.
        let url = serve_once(|mut stream| {
            send_headers(&mut stream, 10 * 1024);
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(150));
                if stream.write_all(&[7; 1024]).is_err() {
                    return;
                }
            }
        });
        let mut body = UreqClient::long_body(LIMITS).open(&url).unwrap().body;
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 10 * 1024);
    }

    /// A stream refused: the error says where, not with which key.
    #[test]
    fn a_refused_stream_is_named_without_its_query_string() {
        let url = serve_once(|mut stream| {
            let _ = write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        });
        let url = format!("{url}?u=enai&t=26719a1196d2a940&s=c19b2d&id=a1");
        let Err(err) = UreqClient::for_streams().open_stream(&url, &[]) else {
            panic!("a 401 must be an error");
        };
        let text = err.chain_text();
        assert!(text.contains("401") && text.contains("/artifact"), "{text}");
        assert!(
            !text.contains("26719a") && !text.contains("c19b2d"),
            "the key must not reach the error: {text}"
        );
    }

    #[test]
    fn a_server_that_never_answers_does_not_hold_the_request() {
        let url = serve_once(|stream| {
            std::thread::sleep(Duration::from_secs(4));
            drop(stream);
        });
        let started = Instant::now();
        let Err(err) = UreqClient::long_body(LIMITS).open(&url) else {
            panic!("a server that does not answer must be an error");
        };
        let elapsed = started.elapsed();
        assert!(err.chain_text().contains("timeout"), "{}", err.chain_text());
        assert!(elapsed < Duration::from_millis(2500), "{elapsed:?}");
    }

    #[test]
    fn a_body_that_goes_silent_fails_after_the_idle_budget() {
        let limits = LongBody {
            idle: Duration::from_millis(300),
            ..LIMITS
        };
        let url = serve_once(|mut stream| {
            send_headers(&mut stream, 4096);
            let _ = stream.write_all(&[1; 1024]);
            std::thread::sleep(Duration::from_secs(4));
        });
        let started = Instant::now();
        let mut body = UreqClient::long_body(limits).open(&url).unwrap().body;
        let mut bytes = Vec::new();
        let err = body.read_to_end(&mut bytes).unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(
            bytes.len(),
            1024,
            "the part that arrived must have been read"
        );
        assert!(err.to_string().contains("timeout"), "{err}");
        assert!(elapsed < Duration::from_millis(2500), "{elapsed:?}");
    }

    // — Heads and the rest after them (D-077) —————————————————————————————

    use crate::net::HttpHeader;
    use crate::test_support::{Answer, LocalServer};

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// The head is cut as a range, with the file's length; the rest comes
    /// from where it ends, and the plugin's own `Range` gives way to both.
    #[test]
    fn a_head_and_the_rest_after_it_make_the_whole_file() {
        let file = body(10_000);
        let served = file.clone();
        let server = LocalServer::start(move |req| Answer::file(&served, req.range.as_deref()));
        let url = server.url("/videoplayback?sig=secret");
        let headers = [HttpHeader::new("Range", "bytes=0-")];

        let head = UreqClient::for_streams()
            .fetch_head(&url, &headers, 4_096)
            .unwrap()
            .expect("a server that cuts ranges gives a head");
        assert_eq!(head.total, 10_000);
        assert_eq!(head.bytes, file[..4_096]);

        let mut rest = Vec::new();
        UreqClient::for_streams()
            .open_stream_at(&url, &headers, 4_096, 10_000)
            .unwrap()
            .read_to_end(&mut rest)
            .unwrap();
        assert_eq!(rest, file[4_096..]);

        let ranges: Vec<_> = server.seen().into_iter().map(|req| req.range).collect();
        assert_eq!(
            ranges,
            vec![
                Some("bytes=0-4095".to_owned()),
                Some("bytes=4096-".to_owned())
            ]
        );
    }

    /// A file shorter than the head asked for is its own head.
    #[test]
    fn a_file_shorter_than_the_head_comes_whole() {
        let file = body(1_000);
        let served = file.clone();
        let server = LocalServer::start(move |req| Answer::file(&served, req.range.as_deref()));
        let head = UreqClient::for_streams()
            .fetch_head(&server.url("/short"), &[], 4_096)
            .unwrap()
            .expect("a head");
        assert_eq!(head.total, 1_000);
        assert_eq!(head.bytes, file);
    }

    /// A server that ignores `Range` gives no head: its rest could not be
    /// fetched from where the head ends.
    #[test]
    fn a_server_that_does_not_cut_ranges_gives_no_head() {
        let file = body(10_000);
        let server = LocalServer::start(move |_| Answer::whole(&file));
        let head = UreqClient::for_streams()
            .fetch_head(&server.url("/whole"), &[], 4_096)
            .unwrap();
        assert_eq!(head, None);
    }

    /// A refused head is an error with the status, not "no head": the
    /// address itself may be dead.
    #[test]
    fn a_refused_head_is_an_error() {
        let server = LocalServer::start(|_| Answer::status(403, "expired"));
        let err = UreqClient::for_streams()
            .fetch_head(&server.url("/old?sig=secret"), &[], 4_096)
            .unwrap_err();
        let text = err.chain_text();
        assert!(text.contains("403") && text.contains("expired"), "{text}");
        assert!(
            !text.contains("secret"),
            "the key must not reach the error: {text}"
        );
    }

    /// The rest is refused when the server sends anything but the same file
    /// from the same byte: those bytes would be joined to the head as if they
    /// followed it.
    #[test]
    fn the_rest_must_be_the_same_file_from_the_same_byte() {
        let file = body(10_000);
        let whole = file.clone();
        let ignores = LocalServer::start(move |_| Answer::whole(&whole));
        let err = UreqClient::for_streams()
            .open_stream_at(&ignores.url("/a"), &[], 4_096, 10_000)
            .err()
            .expect("a 200 is not the rest");
        assert!(
            err.chain_text().contains("from byte 4096"),
            "{}",
            err.chain_text()
        );

        let longer = body(12_000);
        let changed = LocalServer::start(move |req| Answer::file(&longer, req.range.as_deref()));
        let err = UreqClient::for_streams()
            .open_stream_at(&changed.url("/b"), &[], 4_096, 10_000)
            .err()
            .expect("another length is another file");
        assert!(err.chain_text().contains("12000"), "{}", err.chain_text());
    }

    // — Compressed answers (D-094) ————————————————————————————————————————

    const ANSWER_GZ: &[u8] = include_bytes!("../../../../fixtures/net/answer.json.gz");
    const ZEROS_GZ: &[u8] = include_bytes!("../../../../fixtures/net/zeros-1mib.gz");

    /// A local server for a single connection that hands over the request's
    /// header lines, lowercased, with the connection.
    fn serve_once_seeing(
        serve: impl FnOnce(Vec<String>, TcpStream) + Send + 'static,
    ) -> (String, std::sync::mpsc::Receiver<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut request = BufReader::new(stream.try_clone().unwrap());
            let mut lines = Vec::new();
            let mut line = String::new();
            while request.read_line(&mut line).unwrap_or(0) > 2 {
                lines.push(line.trim_end().to_ascii_lowercase());
                line.clear();
            }
            let _ = seen.send(lines.clone());
            serve(lines, stream);
        });
        (format!("http://{addr}/youtubei/v1/search"), heard)
    }

    fn send_gzip(stream: &mut TcpStream, body: &[u8]) {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }

    /// The metadata client and the plugins' ask for gzip and hand on the text
    /// unpacked; the two headers that describe the packed bytes are gone.
    #[test]
    fn the_metadata_and_plugin_clients_ask_for_gzip_and_hand_on_the_unpacked_body() {
        for (name, client) in [
            ("metadata", UreqClient::new()),
            ("plugins", UreqClient::without_redirects()),
        ] {
            let (url, heard) = serve_once_seeing(|_, mut stream| send_gzip(&mut stream, ANSWER_GZ));
            let response = client
                .send_blocking(&crate::net::HttpRequest::get(url))
                .unwrap();
            let lines = heard.recv().unwrap();

            assert!(
                lines.iter().any(|line| line == "accept-encoding: gzip"),
                "{name}: {lines:?}"
            );
            assert_eq!(response.status, 200);
            assert_eq!(
                response.body, br#"{"contents":"unpacked by the client"}"#,
                "{name}"
            );
            assert_eq!(response.header("content-encoding"), None, "{name}");
            assert_eq!(response.header("content-length"), None, "{name}");
            assert_eq!(
                response.header("content-type"),
                Some("application/json"),
                "{name}"
            );
        }
    }

    /// Streams and downloads count ranges, lengths and hashes on the file's
    /// own bytes: they don't ask for them packed.
    #[test]
    fn the_stream_and_download_clients_ask_for_the_files_own_bytes() {
        let (url, heard) = serve_once_seeing(|_, mut stream| send_headers(&mut stream, 0));
        UreqClient::for_streams().open_stream(&url, &[]).unwrap();
        let lines = heard.recv().unwrap();
        assert!(
            !lines.iter().any(|line| line.starts_with("accept-encoding")),
            "{lines:?}"
        );

        let (url, heard) = serve_once_seeing(|_, mut stream| send_headers(&mut stream, 0));
        UreqClient::for_downloads().open(&url).unwrap();
        let lines = heard.recv().unwrap();
        assert!(
            !lines.iter().any(|line| line.starts_with("accept-encoding")),
            "{lines:?}"
        );
    }

    /// 33 KB on the wire that unpack to 33 MiB: ureq's own limit counts the
    /// 33 KB, the client's counts what they unpack to.
    #[test]
    fn a_body_that_unpacks_past_the_cap_is_refused() {
        let copies = usize::try_from(super::MAX_BODY_BYTES / (1024 * 1024)).unwrap() + 1;
        let (url, _heard) = serve_once_seeing(move |_, mut stream| {
            send_gzip(&mut stream, &ZEROS_GZ.repeat(copies));
        });
        let err = UreqClient::new()
            .send_blocking(&crate::net::HttpRequest::get(url))
            .unwrap_err();
        assert!(
            err.chain_text().contains("larger than"),
            "{}",
            err.chain_text()
        );
    }
}
