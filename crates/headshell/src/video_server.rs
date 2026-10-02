//! Serves the video file being shown to the page (D-086).
//!
//! WebKitGTK's media player cannot stream from a custom scheme: Tauri's asset
//! protocol answered the file correctly (206, `video/mp4`, ranges) and
//! GStreamer still failed with "FormatError" — measured on 2026-10-02. Over
//! plain HTTP it plays and seeks. So the shell serves the one file it shows,
//! from the loopback address, under a name that cannot be guessed.
//!
//! What it serves is narrow on purpose: **only the file the core handed over
//! for the video being shown** (the local provider gives only files under the
//! music folders), only under the current token, only to `GET`. Showing
//! another video replaces the token; nothing else on the disk is reachable.
//!
//! No business logic: which file is the core's answer; this only carries the
//! bytes, as the localhost plugin carries the page's.

use std::collections::hash_map::RandomState;
use std::fs::File;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tiny_http::{Header, Method, Response, Server, StatusCode};

/// The file being shown and the name it is served under.
#[derive(Debug, Clone)]
struct Shown {
    token: String,
    path: PathBuf,
}

/// The video server: started once, serving one file at a time.
pub struct VideoServer {
    port: u16,
    shown: Arc<Mutex<Option<Shown>>>,
}

impl std::fmt::Debug for VideoServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoServer")
            .field("port", &self.port)
            .finish()
    }
}

impl VideoServer {
    /// Starts the server on a free loopback port.
    ///
    /// # Errors
    /// If no port can be bound, with its stage.
    pub fn start() -> Result<Self, String> {
        let server = Server::http(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).map_err(|err| {
            format!("STEP: VIDEO_PLAYBACK\n  the video server could not start: {err}")
        })?;
        let port = server
            .server_addr()
            .to_ip()
            .map(|addr| addr.port())
            .ok_or("STEP: VIDEO_PLAYBACK\n  the video server has no port")?;
        let shown: Arc<Mutex<Option<Shown>>> = Arc::new(Mutex::new(None));
        let serving = Arc::clone(&shown);
        std::thread::Builder::new()
            .name("headshell-video".to_owned())
            .spawn(move || {
                for request in server.incoming_requests() {
                    let current = serving.lock().ok().and_then(|shown| shown.clone());
                    let range = request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Range"))
                        .map(|header| header.value.as_str().to_owned());
                    let wanted = request.url().trim_start_matches("/video/").to_owned();
                    let answer = match (request.method(), current) {
                        (Method::Get, Some(shown)) if shown.token == wanted => {
                            serve(&shown.path, range.as_deref())
                        }
                        _ => Err(404),
                    };
                    let result = match answer {
                        Ok(response) => request.respond(response),
                        Err(code) => request.respond(Response::empty(StatusCode(code))),
                    };
                    if let Err(err) = result {
                        // The page closed the connection (a seek, a new video):
                        // ordinary, but not silent.
                        tracing::debug!(error = %err, "a video response was cut short");
                    }
                }
            })
            .map_err(|err| {
                format!("STEP: VIDEO_PLAYBACK\n  the video server thread could not start: {err}")
            })?;
        Ok(Self { port, shown })
    }

    /// Serves `path` from now on, under a new name; the address the page
    /// plays it from.
    ///
    /// # Errors
    /// If the server's lock is poisoned.
    pub fn show(&self, path: &Path) -> Result<String, String> {
        let token = token();
        let mut shown = self.shown.lock().map_err(|_| {
            "STEP: VIDEO_PLAYBACK\n  the video server's lock is poisoned".to_owned()
        })?;
        *shown = Some(Shown {
            token: token.clone(),
            path: path.to_path_buf(),
        });
        Ok(format!(
            "http://{}:{}/video/{token}",
            crate::localhost::PAGE_HOST,
            self.port
        ))
    }

    /// Serves nothing any more.
    pub fn hide(&self) {
        if let Ok(mut shown) = self.shown.lock() {
            *shown = None;
        }
    }
}

/// A name nobody can guess: 128 bits from the standard library's randomly
/// keyed hasher, which reads the system's randomness — no new crate.
fn token() -> String {
    let half = || {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos()),
        );
        hasher.finish()
    };
    format!("{:016x}{:016x}", half(), half())
}

/// The file, whole or the range asked for.
fn serve(path: &Path, range: Option<&str>) -> Result<Response<std::io::Take<File>>, u16> {
    let mut file = File::open(path).map_err(|_| 404u16)?;
    let size = file.metadata().map_err(|_| 404u16)?.len();
    let (start, end) = match range.and_then(|range| parse_range(range, size)) {
        Some(bounds) => bounds,
        None if range.is_some() => return Err(416),
        None => (0, size.saturating_sub(1)),
    };
    let length = end.saturating_sub(start).saturating_add(1);
    file.seek(SeekFrom::Start(start)).map_err(|_| 500u16)?;
    let mut headers = vec![
        header("Content-Type", content_type(path)),
        header("Accept-Ranges", "bytes"),
        header("Cache-Control", "no-store"),
    ];
    let status = if range.is_some() {
        headers.push(header(
            "Content-Range",
            &format!("bytes {start}-{end}/{size}"),
        ));
        206
    } else {
        200
    };
    let length_usize = usize::try_from(length).map_err(|_| 500u16)?;
    Ok(Response::new(
        StatusCode(status),
        headers.into_iter().flatten().collect(),
        file.take(length),
        Some(length_usize),
        None,
    ))
}

fn header(name: &str, value: &str) -> Option<Header> {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).ok()
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n` → the inclusive bounds within `size`.
fn parse_range(range: &str, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let spec = range.trim().strip_prefix("bytes=")?;
    // Only one range: a player asks for one at a time.
    let (from, to) = spec.split(',').next()?.split_once('-')?;
    let last = size - 1;
    let (start, end) = match (from.trim(), to.trim()) {
        ("", suffix) => {
            let count: u64 = suffix.parse().ok()?;
            (size.saturating_sub(count), last)
        }
        (from, "") => (from.parse().ok()?, last),
        (from, to) => (from.parse().ok()?, to.parse::<u64>().ok()?.min(last)),
    };
    (start <= end && start <= last).then_some((start, end))
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        _ => "video/mp4",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn ranges_are_read_as_players_ask() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=90-200", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None, "past the end");
        assert_eq!(parse_range("bytes=5-1", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
    }

    #[test]
    fn tokens_are_long_and_differ() {
        let (a, b) = (token(), token());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }

    /// Deletes itself — on a failing assertion too (the shell's other tests'
    /// pattern, D-070).
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Only the file shown, only under its token; a range comes back as 206.
    #[test]
    fn only_the_shown_file_is_served() {
        use std::io::Write as _;
        let server = VideoServer::start().unwrap();
        let dir = TempDir(std::env::temp_dir().join(format!(
            "headshell-video-server-test-{}",
            std::process::id()
        )));
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("clip.mp4");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"0123456789")
            .unwrap();
        let url = server.show(&path).unwrap();
        let address = url.replace("http://localhost:", "127.0.0.1:");
        let (host, rest) = address.split_once('/').unwrap();
        let get = |path: &str, range: Option<&str>| {
            use std::io::{Read as _, Write as _};
            let mut stream = std::net::TcpStream::connect(host).unwrap();
            let range = range.map_or(String::new(), |r| format!("Range: {r}\r\n"));
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: x\r\n{range}Connection: close\r\n\r\n"
            )
            .unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        };
        let whole = get(&format!("/{rest}"), None);
        assert!(whole.starts_with("HTTP/1.1 200"), "{whole}");
        assert!(whole.ends_with("0123456789"), "{whole}");
        let part = get(&format!("/{rest}"), Some("bytes=2-4"));
        assert!(part.starts_with("HTTP/1.1 206"), "{part}");
        assert!(part.contains("Content-Range: bytes 2-4/10"), "{part}");
        assert!(part.ends_with("234"), "{part}");
        assert!(get("/video/guess", None).starts_with("HTTP/1.1 404"));
        server.hide();
        assert!(get(&format!("/{rest}"), None).starts_with("HTTP/1.1 404"));
    }
}
