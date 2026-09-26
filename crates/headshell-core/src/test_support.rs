//! Shared helpers for unit tests. Compiled only under `cfg(test)`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::Config;

/// A temporary directory that deletes itself.
///
/// Tests **delete** their temporary directories, even a failing test: `Drop`
/// runs during a panic too. Once they did not, and 1,100 directories, 1.2 GB,
/// piled up in a development machine's `/tmp` — a tmpfs there, that is,
/// memory (D-070).
///
/// The name is `headshell-<label>-<process>-<seq>`. The sequence is a counter
/// that grows within the process, not the clock: Windows' clock resolution
/// could give two parallel tests the same name. If an old directory with the
/// same name exists (from a killed run) it **is not used**, the next sequence
/// number is taken — so a test does not mistake someone else's leftovers for
/// its own data.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        loop {
            let dir = base.join(format!(
                "headshell-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Self(dir),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!(
                    "could not open a temporary directory ({}): {err}",
                    dir.display()
                ),
            }
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // If it cannot be deleted (a file left open on Windows) the test is not
        // failed; but it does not stay silent either, what was left behind is
        // written out.
        if let Err(err) = std::fs::remove_dir_all(&self.0)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "warning: could not delete the temporary directory ({}): {err}",
                self.0.display()
            );
        }
    }
}

/// A `Config` on top of a data directory that deletes itself.
///
/// It derefs to `Config`: `&test_config` can be passed anywhere a `&Config`
/// is expected, and when the value is dropped the directory goes too.
pub(crate) struct TestConfig {
    _dir: TempDir,
    config: Config,
}

impl TestConfig {
    pub(crate) fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        let config = Config::with_data_dir(dir.path());
        Self { _dir: dir, config }
    }
}

impl std::ops::Deref for TestConfig {
    type Target = Config;

    fn deref(&self) -> &Config {
        &self.config
    }
}

/// A request the local server saw: its path and its `Range`, if any.
#[cfg(feature = "http-client")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeenRequest {
    pub(crate) path: String,
    pub(crate) range: Option<String>,
}

/// What the local server answers one request with.
#[cfg(feature = "http-client")]
pub(crate) struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// Sends this many bytes of the body, then waits before the rest: a
    /// stream whose end comes late.
    pause: Option<(usize, std::time::Duration)>,
}

#[cfg(feature = "http-client")]
impl Answer {
    /// `body` served as a file server serves it: a `Range` gets a `206` with
    /// its `Content-Range`, a request without one a `200`.
    pub(crate) fn file(body: &[u8], range: Option<&str>) -> Self {
        let total = body.len();
        let asked = range
            .and_then(|value| value.trim().strip_prefix("bytes="))
            .and_then(|spec| spec.split_once('-'))
            .and_then(|(start, end)| {
                let start = start.parse::<usize>().ok()?;
                let end = if end.is_empty() {
                    total.checked_sub(1)?
                } else {
                    end.parse::<usize>().ok()?.min(total.checked_sub(1)?)
                };
                (start <= end).then_some((start, end))
            });
        match asked {
            Some((start, end)) => Self {
                status: 206,
                headers: vec![(
                    "Content-Range".to_owned(),
                    format!("bytes {start}-{end}/{total}"),
                )],
                body: body[start..=end].to_vec(),
                pause: None,
            },
            None => Self::whole(body),
        }
    }

    /// The whole body with a `200`, whatever was asked: a server that does
    /// not cut ranges.
    pub(crate) fn whole(body: &[u8]) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.to_vec(),
            pause: None,
        }
    }

    pub(crate) fn status(status: u16, text: &str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: text.as_bytes().to_vec(),
            pause: None,
        }
    }

    /// Used by the audio stream's tests.
    #[cfg(feature = "audio")]
    pub(crate) fn pausing_after(mut self, bytes: usize, pause: std::time::Duration) -> Self {
        self.pause = Some((bytes, pause));
        self
    }
}

/// A local HTTP/1.1 server on a real socket (D-077): every connection on a
/// thread of its own, so one that pauses does not hold the others.
#[cfg(feature = "http-client")]
pub(crate) struct LocalServer {
    addr: std::net::SocketAddr,
    seen: std::sync::Arc<std::sync::Mutex<Vec<SeenRequest>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "http-client")]
impl LocalServer {
    pub(crate) fn start(handler: impl Fn(&SeenRequest) -> Answer + Send + Sync + 'static) -> Self {
        use std::io::{BufRead as _, BufReader, Write as _};
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("the listener must bind");
        let addr = listener.local_addr().expect("the address must be readable");
        listener
            .set_nonblocking(true)
            .expect("non-blocking mode must be set");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);

        let thread = {
            let seen = Arc::clone(&seen);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let seen = Arc::clone(&seen);
                    let handler = Arc::clone(&handler);
                    std::thread::spawn(move || {
                        let _ = stream.set_nonblocking(false);
                        let Ok(peer) = stream.try_clone() else { return };
                        let mut reader = BufReader::new(peer);
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                        let mut range = None;
                        loop {
                            let mut header = String::new();
                            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                                break;
                            }
                            let header = header.trim_end();
                            if header.is_empty() {
                                break;
                            }
                            if let Some((name, value)) = header.split_once(':')
                                && name.trim().eq_ignore_ascii_case("range")
                            {
                                range = Some(value.trim().to_owned());
                            }
                        }
                        let request = SeenRequest { path, range };
                        if let Ok(mut log) = seen.lock() {
                            log.push(request.clone());
                        }
                        let answer = handler(&request);
                        let mut head = format!(
                            "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                            answer.status,
                            answer.body.len()
                        );
                        for (name, value) in &answer.headers {
                            head.push_str(&format!("{name}: {value}\r\n"));
                        }
                        head.push_str("\r\n");
                        let mut out = stream;
                        if out.write_all(head.as_bytes()).is_err() {
                            return;
                        }
                        match answer.pause {
                            Some((first, pause)) => {
                                let first = first.min(answer.body.len());
                                let _ = out.write_all(&answer.body[..first]);
                                let _ = out.flush();
                                std::thread::sleep(pause);
                                let _ = out.write_all(&answer.body[first..]);
                            }
                            None => {
                                let _ = out.write_all(&answer.body);
                            }
                        }
                        let _ = out.flush();
                    });
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

    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub(crate) fn seen(&self) -> Vec<SeenRequest> {
        self.seen.lock().map(|log| log.clone()).unwrap_or_default()
    }
}

#[cfg(feature = "http-client")]
impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temp_dir_is_unique_and_gone_after_drop() {
        let first = TempDir::new("support");
        let second = TempDir::new("support");
        assert_ne!(first.path(), second.path());
        std::fs::write(first.join("file"), "x").unwrap();
        let path = first.path().to_path_buf();
        drop(first);
        assert!(
            !path.exists(),
            "the directory was not deleted: {}",
            path.display()
        );
    }
}
