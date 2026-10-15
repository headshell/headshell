//! Turns an HTTP stream into a source symphonia can read (§1.3).
//!
//! Why not [`crate::net::HttpClient`]: that trait takes the body into memory
//! **whole** and was designed for metadata calls. For audio, "start when it
//! has all arrived" is not acceptable — on a 40 MB FLAC it means seconds of
//! silence. The source here keeps downloading in the background while the
//! decoder can start reading the first bytes.
//!
//! ## Why we keep the whole buffer in memory
//!
//! While recognising the container, symphonia seeks **backwards** (FLAC/MP4
//! headers). Throwing away the downloaded bytes would mean opening a new HTTP
//! request (Range) for every seek; that ties the complexity to server
//! compatibility (not every server supports Range). A typical track is a few
//! tens of MB; the cap is explicitly limited with [`MAX_BUFFER_BYTES`], and
//! exceeding it gives an **error**, it does not cut off silently.
//!
//! ## Why the length is not reported (D-077)
//!
//! symphonia 0.6 reads the **end** of a seekable source whose length it
//! knows before it reads the start: trailing tags (ID3v1, APE) and, for Ogg,
//! the last page. On a download that runs front to back the end is the last
//! thing to arrive, so every stream waited for its whole file before it made
//! a sound — 1.8 s for a 7.5 MB m4a in the measurement, longer for a large
//! FLAC. The source still knows its length and still seeks from the end (an
//! MP4 whose `moov` sits at the end needs that); it only does not report it
//! through `byte_len`. What is lost is what symphonia derives from the end:
//! the length of an Ogg stream and the estimate for an MP3 without a Xing
//! header. The provider's metadata gives both.
//!
//! ## Starting from a head (D-077)
//!
//! A head fetched ahead of time is the buffer's beginning, and the rest is
//! asked for from where it ends. If the address went stale in between, the
//! source asks for a new one through its [`StreamHandle`] and waits — the
//! head keeps playing meanwhile — then carries on from the same byte.
//!
//! ## Picking up a stream that broke off (D-094)
//!
//! A server may cut a download halfway — googlevideo did, on a lossy link,
//! every time. A stream whose length is known is asked for again from the
//! byte it stopped at, with the same address; the server must answer as a
//! range of the same file, or it is not joined. A break that moved the stream
//! on counts from zero again, so a long track on a link the server keeps
//! cutting still gets to its end; `MAX_STALLED_RESUMES` breaks in a row
//! without moving on, and it fails, saying why. A stream of unknown length
//! (an HLS playlist's files) has nothing to be picked up from: a break fails
//! it, as before.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::net::{HttpHeader, StreamHead, UreqClient};

/// The memory cap for a single track. Exceeding it makes reading fail.
const MAX_BUFFER_BYTES: usize = 256 * 1024 * 1024;

/// The block the download thread reads at a time.
const CHUNK: usize = 64 * 1024;

/// How often the reader wakes up while waiting for data.
const WAIT: Duration = Duration::from_millis(50);

/// How long a stream started from a head waits for a new address when the
/// old one no longer answers. A provider resolves one in seconds (yt-dlp:
/// 4–5 s, measured); past this the address is not coming.
const ADDRESS_WAIT: Duration = Duration::from_secs(30);

/// How many times in a row a stream may break off without moving on before
/// it is given up (D-094).
const MAX_STALLED_RESUMES: u32 = 3;

/// A break after the stream moved on by at least this much since it was last
/// picked up is not counted against it.
const RESUME_PROGRESS: u64 = CHUNK as u64;

/// The pause before a broken stream is asked for again, times the breaks in
/// a row.
const RESUME_PAUSE: Duration = Duration::from_millis(250);

/// How a body read into the buffer came to an end.
enum Pumped {
    /// The server sent all it would.
    Ended,
    /// The reader is gone, or the download failed for good — its error is
    /// set.
    Stopped,
    /// The body broke off with this error.
    BrokeOff(String),
}

/// An HTTP source that downloads in the background and can be read ahead.
pub struct HttpMediaSource {
    shared: Arc<Shared>,
    pos: u64,
    /// The total length, if `Content-Length` is known. If not, the source
    /// counts as "not seekable" — symphonia then works in streaming mode.
    total: Option<u64>,
}

struct Shared {
    data: Mutex<Vec<u8>>,
    /// Has the download finished (successfully or with an error).
    done: AtomicBool,
    /// The download error; the reader turns it into an `io::Error`.
    error: Mutex<Option<String>>,
    ready: Condvar,
    /// The reader is gone — the track ended or was left: the download stops
    /// instead of fetching a file nobody will read.
    closed: AtomicBool,
    /// A stream started from a head asking for a new address.
    address: Mutex<AddressSlot>,
    address_ready: Condvar,
}

#[derive(Default)]
struct AddressSlot {
    /// The download is waiting for a new address.
    wanted: bool,
    /// One handed over and not yet taken.
    fresh: Option<(String, Vec<HttpHeader>)>,
}

/// The side of a stream started from a head that the engine keeps (D-077):
/// through it the stream asks for a new address, and is given one.
#[derive(Clone)]
pub(crate) struct StreamHandle(Arc<Shared>);

impl StreamHandle {
    /// Is the stream waiting for a new address.
    pub(crate) fn wants_address(&self) -> bool {
        self.0
            .address
            .lock()
            .is_ok_and(|slot| slot.wanted && slot.fresh.is_none())
    }

    /// Hands over a new address; the stream carries on from the byte it
    /// stopped at.
    pub(crate) fn give_address(&self, url: String, headers: Vec<HttpHeader>) {
        if let Ok(mut slot) = self.0.address.lock() {
            slot.fresh = Some((url, headers));
        }
        self.0.address_ready.notify_all();
    }
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle")
            .field("wants_address", &self.wants_address())
            .finish()
    }
}

impl std::fmt::Debug for HttpMediaSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpMediaSource")
            .field("pos", &self.pos)
            .field("total", &self.total)
            .finish()
    }
}

impl HttpMediaSource {
    /// Opens the stream and starts the download in the background.
    ///
    /// # Errors
    /// If no connection can be made or the server returns a code outside 2xx
    /// ([`Stage::NetworkRequest`]) — that is, before a "started playing but no
    /// sound" situation can arise.
    pub fn open(url: &str, headers: &[HttpHeader]) -> Result<Self> {
        let client = UreqClient::for_streams();
        let (total, reader) = client.open_stream(url, headers)?;
        // Named without its query string, which may carry a key.
        let label = crate::net::without_query(url).to_owned();
        check_size(&label, total)?;

        let shared = Shared::new(Vec::with_capacity(
            usize::try_from(total.unwrap_or(0))
                .unwrap_or(0)
                .min(CHUNK * 16),
        ));
        let writer = Arc::clone(&shared);
        let url = url.to_owned();
        let headers = headers.to_vec();
        spawn_download(move || match total {
            // No new address can be asked for here: a stream opened this way
            // has no handle.
            Some(total) => writer.keep_downloading(Some(reader), url, headers, total, &label, None),
            None => writer.read_once(reader, &label),
        })?;
        Ok(Self {
            shared,
            pos: 0,
            total,
        })
    }

    /// A stream read from `reader` in the background, its length unknown —
    /// so symphonia reads it front to back: an HLS playlist's files, one
    /// after another (D-091).
    ///
    /// # Errors
    /// If the download thread cannot start.
    pub(crate) fn from_reader(reader: Box<dyn Read + Send>, label: &str) -> Result<Self> {
        let shared = Shared::new(Vec::with_capacity(CHUNK * 16));
        let writer = Arc::clone(&shared);
        let label = label.to_owned();
        spawn_download(move || writer.read_once(reader, &label))?;
        Ok(Self {
            shared,
            pos: 0,
            total: None,
        })
    }

    /// Starts from a head fetched ahead of time and fetches the rest from
    /// where it ends (D-077). **No request is made here**: the head is
    /// already the start of the stream, and the rest is asked for in the
    /// background.
    ///
    /// # Errors
    /// If the stream is larger than the cap, or the download thread cannot
    /// start.
    pub(crate) fn open_with_head(
        url: &str,
        headers: &[HttpHeader],
        head: &StreamHead,
    ) -> Result<(Self, StreamHandle)> {
        Self::open_with_head_waiting(url, headers, head, ADDRESS_WAIT)
    }

    /// [`Self::open_with_head`] with the wait for a new address given: a
    /// test must not wait 30 s to see the stream give up.
    fn open_with_head_waiting(
        url: &str,
        headers: &[HttpHeader],
        head: &StreamHead,
        address_wait: Duration,
    ) -> Result<(Self, StreamHandle)> {
        let label = crate::net::without_query(url).to_owned();
        check_size(&label, Some(head.total))?;

        let shared = Shared::new(head.bytes.clone());
        if head.bytes.len() as u64 >= head.total {
            shared.finish();
        } else {
            let writer = Arc::clone(&shared);
            let url = url.to_owned();
            let headers = headers.to_vec();
            let total = head.total;
            spawn_download(move || {
                writer.keep_downloading(None, url, headers, total, &label, Some(address_wait));
            })?;
        }
        let handle = StreamHandle(Arc::clone(&shared));
        Ok((
            Self {
                shared,
                pos: 0,
                total: Some(head.total),
            },
            handle,
        ))
    }
}

fn check_size(label: &str, total: Option<u64>) -> Result<()> {
    if let Some(len) = total
        && len > MAX_BUFFER_BYTES as u64
    {
        return Err(Error::new(
            Stage::PlaybackDecode,
            ErrorKind::Audio {
                detail: format!(
                    "{label} is {len} bytes; the cap for a single track is {MAX_BUFFER_BYTES} bytes"
                ),
            },
        ));
    }
    Ok(())
}

fn spawn_download(work: impl FnOnce() + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .name("headshell-http-stream".to_owned())
        .spawn(work)
        .map(drop)
        .map_err(|source| {
            Error::new(
                Stage::PlaybackDecode,
                ErrorKind::Audio {
                    detail: format!("could not start the download thread: {source}"),
                },
            )
        })
}

impl Shared {
    fn new(data: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(data),
            done: AtomicBool::new(false),
            error: Mutex::new(None),
            ready: Condvar::new(),
            closed: AtomicBool::new(false),
            address: Mutex::new(AddressSlot::default()),
            address_ready: Condvar::new(),
        })
    }

    fn len(&self) -> Option<u64> {
        self.data.lock().ok().map(|data| data.len() as u64)
    }

    fn finish(&self) {
        self.done.store(true, Ordering::Release);
        self.ready.notify_all();
    }

    fn fail(&self, detail: String) {
        if let Ok(mut slot) = self.error.lock() {
            *slot = Some(detail);
        }
        self.finish();
    }

    fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Reads a body of unknown length to its end: if it breaks off, there is
    /// nothing to pick it up from.
    fn read_once(&self, reader: Box<dyn Read + Send>, label: &str) {
        match self.pump(reader, label) {
            Pumped::Ended | Pumped::Stopped => self.finish(),
            Pumped::BrokeOff(reason) => {
                self.fail(format!("the {label} stream broke off: {reason}"));
            }
        }
    }

    /// Reads the body into the buffer until it ends, breaks off, or nobody
    /// reads it any more. It does not mark the download done: the caller
    /// knows whether there is more to fetch.
    fn pump(&self, mut reader: Box<dyn Read + Send>, label: &str) -> Pumped {
        let mut chunk = vec![0u8; CHUNK];
        loop {
            if self.closed.load(Ordering::Acquire) {
                return Pumped::Stopped;
            }
            match reader.read(&mut chunk) {
                Ok(0) => return Pumped::Ended,
                Ok(n) => {
                    let Ok(mut data) = self.data.lock() else {
                        self.fail("could not lock the download buffer".to_owned());
                        return Pumped::Stopped;
                    };
                    if data.len() + n > MAX_BUFFER_BYTES {
                        drop(data);
                        self.fail(format!(
                            "{label} exceeded the memory cap ({MAX_BUFFER_BYTES} bytes)"
                        ));
                        return Pumped::Stopped;
                    }
                    data.extend_from_slice(&chunk[..n]);
                    drop(data);
                    self.ready.notify_all();
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Pumped::BrokeOff(err.to_string()),
            }
        }
    }

    /// Downloads a `total`-byte file into the buffer, from where the buffer
    /// ends to the file's end: `reader` when one is open already, otherwise
    /// a range asked for from that byte. A body that breaks off is asked for
    /// again from the byte it stopped at (D-094). With `address_wait` — a
    /// stream started from a head, whose handle the engine keeps — an
    /// address that no longer answers is replaced **once**, and the stream
    /// carries on from the same byte with the new one.
    fn keep_downloading(
        &self,
        mut reader: Option<Box<dyn Read + Send>>,
        url: String,
        headers: Vec<HttpHeader>,
        total: u64,
        label: &str,
        address_wait: Option<Duration>,
    ) {
        let client = UreqClient::for_streams();
        let mut address = (url, headers);
        let mut replaced = false;
        let mut stalled = 0u32;
        // Why the stream is being picked up again, while it is.
        let mut broke: Option<String> = None;
        loop {
            if self.closed.load(Ordering::Acquire) {
                self.finish();
                return;
            }
            let Some(offset) = self.len() else {
                self.fail("could not lock the download buffer".to_owned());
                return;
            };
            if offset >= total {
                self.finish();
                return;
            }
            let current = match reader.take() {
                Some(current) => current,
                None => match client.open_stream_at(&address.0, &address.1, offset, total) {
                    Ok(current) => current,
                    Err(err) => {
                        let reason = err.chain_text().replace('\n', " ");
                        let after_break = broke.as_deref().map(|why| {
                            format!("the {label} stream broke off at byte {offset} ({why}), and ")
                        });
                        let Some(wait) = address_wait.filter(|_| !replaced) else {
                            self.fail(match (after_break, replaced) {
                                (Some(before), false) => {
                                    format!("{before}could not be picked up again: {reason}")
                                }
                                (_, true) => format!(
                                    "the rest of {label} could not be fetched with a new address either: {reason}"
                                ),
                                (None, false) => {
                                    format!("the rest of {label} could not be fetched: {reason}")
                                }
                            });
                            return;
                        };
                        tracing::info!(stream = %label, offset, error = %reason, "the stream's address no longer answers; asking for a new one");
                        let Some(fresh) = self.wait_for_address(wait) else {
                            self.fail(format!(
                                "{}the rest of {label} could not be fetched ({reason}), and no new address came within {:.1} s",
                                after_break.unwrap_or_default(),
                                wait.as_secs_f64()
                            ));
                            return;
                        };
                        address = fresh;
                        replaced = true;
                        continue;
                    }
                },
            };
            let reason = match self.pump(current, label) {
                Pumped::Stopped => {
                    self.finish();
                    return;
                }
                Pumped::BrokeOff(reason) => reason,
                // A body that ends short of the file broke off too: what it
                // left out is not silence.
                Pumped::Ended => match self.len() {
                    Some(now) if now >= total => {
                        self.finish();
                        return;
                    }
                    Some(now) => format!("the body ended at byte {now} of {total}"),
                    None => {
                        self.fail("could not lock the download buffer".to_owned());
                        return;
                    }
                },
            };
            let now = self.len().unwrap_or(offset);
            stalled = if now.saturating_sub(offset) >= RESUME_PROGRESS {
                0
            } else {
                stalled + 1
            };
            if stalled >= MAX_STALLED_RESUMES {
                self.fail(format!(
                    "the {label} stream broke off {MAX_STALLED_RESUMES} times in a row at byte {now} of {total}; the last time: {reason}"
                ));
                return;
            }
            tracing::info!(stream = %label, at = now, total, error = %reason, "the stream broke off; asking for it again from the same byte");
            std::thread::sleep(RESUME_PAUSE * (stalled + 1));
            broke = Some(reason);
        }
    }

    fn wait_for_address(&self, limit: Duration) -> Option<(String, Vec<HttpHeader>)> {
        let deadline = Instant::now() + limit;
        let mut slot = self.address.lock().ok()?;
        slot.wanted = true;
        let fresh = loop {
            if let Some(fresh) = slot.fresh.take() {
                break Some(fresh);
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break None;
            };
            if self.closed.load(Ordering::Acquire) {
                break None;
            }
            slot = self
                .address_ready
                .wait_timeout(slot, left.min(WAIT * 4))
                .ok()?
                .0;
        };
        // Given up or given one: either way it no longer asks.
        slot.wanted = false;
        fresh
    }
}

impl Drop for HttpMediaSource {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.address_ready.notify_all();
    }
}

impl Read for HttpMediaSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let mut data = self
            .shared
            .data
            .lock()
            .map_err(|_| io::Error::other("could not lock the download buffer"))?;

        loop {
            let available = data.len() as u64;
            if self.pos < available {
                let start = usize::try_from(self.pos).map_err(|_| {
                    io::Error::other("the position does not fit into a machine word")
                })?;
                let take = out.len().min(data.len() - start);
                out[..take].copy_from_slice(&data[start..start + take]);
                self.pos += take as u64;
                return Ok(take);
            }
            if self.shared.done.load(Ordering::Acquire) {
                // If there is an error we do not act as if it were the end of the
                // file: a silently truncated track is worse than one that fails.
                return match self.shared.take_error() {
                    Some(detail) => Err(io::Error::other(detail)),
                    None => Ok(0),
                };
            }
            let (guard, _) = self
                .shared
                .ready
                .wait_timeout(data, WAIT)
                .map_err(|_| io::Error::other("could not lock the download buffer"))?;
            data = guard;
        }
    }
}

impl Seek for HttpMediaSource {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(delta) => add_offset(self.pos, delta)?,
            SeekFrom::End(delta) => {
                let total = self.total.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "the server reported no length; cannot seek from the end",
                    )
                })?;
                add_offset(total, delta)?
            }
        };
        // Jumping to a point not yet downloaded is not forbidden: reading
        // waits until those bytes arrive.
        self.pos = target;
        Ok(self.pos)
    }
}

fn add_offset(base: u64, delta: i64) -> io::Result<u64> {
    let result = i128::from(base) + i128::from(delta);
    if result < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "seek before the start of the file",
        ));
    }
    u64::try_from(result).map_err(|_| io::Error::other("the seek position overflows"))
}

impl MediaSource for HttpMediaSource {
    fn is_seekable(&self) -> bool {
        self.total.is_some()
    }

    /// Always `None`, although the length is known: reported, it makes
    /// symphonia read the end of the stream first (see the module header).
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {

    use symphonia::core::formats::FormatOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    use super::*;
    use crate::test_support::{Answer, LocalServer};

    fn fixture() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/audio/tagged.flac"
        ))
        .unwrap()
    }

    fn head_of(server: &LocalServer, path: &str, len: u64) -> StreamHead {
        UreqClient::for_streams()
            .fetch_head(&server.url(path), &[], len)
            .unwrap()
            .expect("the test server cuts ranges")
    }

    /// **The reason for this test is a measured delay** (D-077): symphonia
    /// reads trailing tags at the end of a source whose length it knows, and
    /// on a download the end comes last — every stream waited for its whole
    /// file before it could start. Here the server holds the file's end back
    /// for three seconds; the container must be recognised without it.
    #[test]
    fn a_stream_is_recognised_without_waiting_for_its_end() {
        let flac = fixture();
        let served = flac.clone();
        let server = LocalServer::start(move |_| {
            Answer::whole(&served).pausing_after(served.len() / 2, Duration::from_secs(3))
        });

        let started = Instant::now();
        let source = HttpMediaSource::open(&server.url("/rest/stream?id=a1"), &[]).unwrap();
        assert!(source.is_seekable(), "the length is known, so it seeks");
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let probed = symphonia::default::get_probe().probe(
            &Hint::new(),
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        );
        let took = started.elapsed();

        assert!(probed.is_ok(), "{:?}", probed.err());
        assert!(
            took < Duration::from_millis(1500),
            "recognising the stream waited for its end: {took:?}"
        );
    }

    /// The head and the rest fetched after it read as one file — and the
    /// rest was asked for from where the head ends.
    #[test]
    fn a_head_and_the_rest_read_as_one_file() {
        let flac = fixture();
        let served = flac.clone();
        let server = LocalServer::start(move |req| Answer::file(&served, req.range.as_deref()));
        let head = head_of(&server, "/a.flac", 4_096);

        let (mut source, _handle) =
            HttpMediaSource::open_with_head(&server.url("/a.flac"), &[], &head).unwrap();
        let mut read = Vec::new();
        source.read_to_end(&mut read).unwrap();

        assert_eq!(read, flac);
        let ranges: Vec<_> = server.seen().into_iter().map(|req| req.range).collect();
        assert_eq!(
            ranges,
            vec![
                Some("bytes=0-4095".to_owned()),
                Some("bytes=4096-".to_owned())
            ]
        );
    }

    /// The address the head came from no longer answers: the stream asks for
    /// a new one, and carries on from the same byte with it.
    #[test]
    fn a_stale_address_is_replaced_and_the_stream_carries_on() {
        let flac = fixture();
        let old = {
            let served = flac.clone();
            LocalServer::start(move |req| match req.range.as_deref() {
                Some(range) if range.starts_with("bytes=0-") => Answer::file(&served, Some(range)),
                _ => Answer::status(403, "expired"),
            })
        };
        let new = {
            let served = flac.clone();
            LocalServer::start(move |req| Answer::file(&served, req.range.as_deref()))
        };
        let head = head_of(&old, "/v?sig=1", 4_096);

        let (mut source, handle) =
            HttpMediaSource::open_with_head(&old.url("/v?sig=1"), &[], &head).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !handle.wants_address() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            handle.wants_address(),
            "the stream must ask for a new address"
        );
        handle.give_address(new.url("/v?sig=2"), Vec::new());

        let mut read = Vec::new();
        source.read_to_end(&mut read).unwrap();
        assert_eq!(read, flac);
        assert!(!handle.wants_address());
        assert_eq!(
            new.seen()
                .into_iter()
                .map(|req| req.range)
                .collect::<Vec<_>>(),
            vec![Some("bytes=4096-".to_owned())]
        );
    }

    /// Without a new address the stream fails rather than ending where the
    /// head ends: a track cut short silently is worse than one that fails.
    #[test]
    fn without_a_new_address_the_stream_fails_instead_of_ending_early() {
        let flac = fixture();
        let server = LocalServer::start(move |req| match req.range.as_deref() {
            Some(range) if range.starts_with("bytes=0-") => Answer::file(&flac, Some(range)),
            _ => Answer::status(403, "expired"),
        });
        let head = head_of(&server, "/v", 4_096);

        let (mut source, handle) = HttpMediaSource::open_with_head_waiting(
            &server.url("/v"),
            &[],
            &head,
            Duration::from_millis(300),
        )
        .unwrap();
        let mut read = Vec::new();
        let err = source.read_to_end(&mut read).unwrap_err();

        assert_eq!(read.len(), 4_096, "the head was read");
        let text = err.to_string();
        assert!(
            text.contains("403") && text.contains("no new address"),
            "{text}"
        );
        assert!(!handle.wants_address(), "it gave up; it no longer asks");
    }

    /// A reader that is gone stops the download: a skipped track is not
    /// fetched to its end.
    #[test]
    fn a_download_stops_when_its_reader_is_gone() {
        struct Endless(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        impl Read for Endless {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                self.0.fetch_add(1, Ordering::Relaxed);
                out.fill(7);
                Ok(out.len())
            }
        }
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let shared = Shared::new(Vec::new());
        shared.closed.store(true, Ordering::Release);
        shared.read_once(Box::new(Endless(std::sync::Arc::clone(&reads))), "x");

        assert_eq!(reads.load(Ordering::Relaxed), 0);
        assert!(shared.done.load(Ordering::Acquire));
    }

    // — A stream that breaks off (D-094) ————————————————————————————————

    /// Bytes that say where they are: a joint at the wrong byte shows.
    fn numbered(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// googlevideo cut the download halfway on a lossy link, every time: the
    /// stream is asked for again from the byte it stopped at, and the track
    /// reads whole.
    #[test]
    fn a_stream_that_breaks_off_is_picked_up_from_the_byte_it_stopped_at() {
        let flac = fixture();
        let served = flac.clone();
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let server = LocalServer::start(move |req| match asked.fetch_add(1, Ordering::Relaxed) {
            0 => Answer::whole(&served).breaking_off_after(8_000),
            1 => Answer::file(&served, req.range.as_deref()).breaking_off_after(8_000),
            _ => Answer::file(&served, req.range.as_deref()),
        });

        let mut source = HttpMediaSource::open(&server.url("/videoplayback?sig=1"), &[]).unwrap();
        let mut read = Vec::new();
        source.read_to_end(&mut read).unwrap();

        assert_eq!(read, flac);
        let ranges: Vec<_> = server.seen().into_iter().map(|req| req.range).collect();
        assert_eq!(
            ranges,
            vec![
                None,
                Some("bytes=8000-".to_owned()),
                Some("bytes=16000-".to_owned())
            ]
        );
    }

    /// A long track on a link the server keeps cutting: every break that
    /// moved the stream on counts from zero again, so more breaks than the
    /// limit still get it to its end.
    #[test]
    fn breaks_that_move_the_stream_on_do_not_add_up() {
        let file = numbered(400_000);
        let served = file.clone();
        let step = usize::try_from(RESUME_PROGRESS).unwrap() + 6_000;
        let server = LocalServer::start(move |req| {
            Answer::file(&served, req.range.as_deref()).breaking_off_after(step)
        });

        let mut source =
            HttpMediaSource::open(&server.url("/v"), &[HttpHeader::new("Range", "bytes=0-")])
                .unwrap();
        let mut read = Vec::new();
        source.read_to_end(&mut read).unwrap();

        assert_eq!(read, file);
        assert!(
            server.seen().len() > MAX_STALLED_RESUMES as usize + 1,
            "the test must break more often than the limit: {}",
            server.seen().len()
        );
    }

    /// A server that cuts it at the same byte every time: the stream gives up
    /// after the limit, and says where and why.
    #[test]
    fn a_stream_that_keeps_breaking_off_without_moving_on_fails_and_says_so() {
        let file = numbered(50_000);
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let server = LocalServer::start(move |req| {
            if asked.fetch_add(1, Ordering::Relaxed) == 0 {
                Answer::whole(&file).breaking_off_after(1_000)
            } else {
                Answer::file(&file, req.range.as_deref()).breaking_off_after(0)
            }
        });

        let mut source = HttpMediaSource::open(&server.url("/v?sig=secret"), &[]).unwrap();
        let mut read = Vec::new();
        let err = source.read_to_end(&mut read).unwrap_err();

        assert_eq!(read.len(), 1_000, "what arrived was read");
        let text = err.to_string();
        assert!(
            text.contains(&format!("{MAX_STALLED_RESUMES} times in a row"))
                && text.contains("byte 1000 of 50000"),
            "{text}"
        );
        assert!(
            !text.contains("secret"),
            "the key must not reach the error: {text}"
        );
        // Every request broke off, the first one too: 1 000 bytes is not
        // moving on.
        assert_eq!(server.seen().len(), MAX_STALLED_RESUMES as usize);
    }

    /// A server that answers the second request with the whole file instead
    /// of the rest: its bytes are not joined on, and the error names both the
    /// break and the refusal (K9).
    #[test]
    fn a_stream_the_server_will_not_continue_fails_with_both_reasons() {
        let file = numbered(50_000);
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let server = LocalServer::start(move |_| {
            if asked.fetch_add(1, Ordering::Relaxed) == 0 {
                Answer::whole(&file).breaking_off_after(1_000)
            } else {
                Answer::whole(&file)
            }
        });

        let mut source = HttpMediaSource::open(&server.url("/v"), &[]).unwrap();
        let mut read = Vec::new();
        let err = source.read_to_end(&mut read).unwrap_err();

        assert_eq!(read.len(), 1_000);
        let text = err.to_string();
        assert!(
            text.contains("broke off at byte 1000")
                && text.contains("could not be picked up again")
                && text.contains("from byte 1000"),
            "{text}"
        );
    }

    /// A body of unknown length has nothing to be picked up from: a break
    /// fails it, as before (an HLS playlist's files).
    #[test]
    fn a_stream_of_unknown_length_that_breaks_off_fails() {
        struct Breaking(bool);
        impl Read for Breaking {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if std::mem::replace(&mut self.0, true) {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "Peer disconnected",
                    ));
                }
                out[..10].fill(3);
                Ok(10)
            }
        }
        let mut source = HttpMediaSource::from_reader(Box::new(Breaking(false)), "x.m3u8").unwrap();
        let mut read = Vec::new();
        let err = source.read_to_end(&mut read).unwrap_err();

        assert_eq!(read, vec![3; 10]);
        assert!(
            err.to_string()
                .contains("the x.m3u8 stream broke off: Peer disconnected"),
            "{err}"
        );
    }
}
