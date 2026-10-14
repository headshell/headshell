//! The audio pipeline: symphonia (decoding) + cpal (output). D-016.
//!
//! Compiled only with the `audio` feature.
//!
//! ## How it works
//!
//! Decoding happens on a background thread, which writes samples into a
//! shared ring buffer; cpal's audio callback reads from that buffer. **No
//! lock is held** between the two sides (the audio callback must not block:
//! if it blocks, the user hears crackling).
//!
//! The position is computed from the number of frames **actually handed** to
//! the output — not from the number of decoded frames. The difference is a
//! buffer's worth of time; looking at what was decoded would put the progress
//! bar ahead of the sound.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::net::StreamHead;
use crate::provider::AudioSource;

use super::anchor::PlayState;

/// Extracts the extension from the path part of a URL (`.../a.flac?x=1` →
/// `flac`).
///
/// It only produces a **hint**: if none is found, symphonia recognises the
/// container from its contents. Giving a made-up extension would send the
/// recognition the wrong way.
#[cfg(feature = "http-client")]
fn extension_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let last = path.rsplit('/').next()?;
    let ext = last.rsplit_once('.')?.1;
    let plausible =
        !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric());
    plausible.then(|| ext.to_ascii_lowercase())
}

fn audio_err(stage: Stage, detail: impl Into<String>) -> Error {
    Error::new(
        stage,
        ErrorKind::Audio {
            detail: detail.into(),
        },
    )
}

/// A ring buffer carrying samples from the decoder to the output.
///
/// Single producer (the decoding thread), single consumer (the audio
/// callback). There is a `Mutex`, but it is held for about a `memcpy`; so the
/// audio callback does not block, if the lock cannot be taken **silence is
/// written**, it does not wait.
struct RingBuffer {
    samples: Mutex<std::collections::VecDeque<f32>>,
    capacity: usize,
}

impl RingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            samples: Mutex::new(std::collections::VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// The number of samples in the buffer.
    fn len(&self) -> usize {
        self.samples.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Writes if there is room; if full, returns how many it could not write.
    fn push(&self, data: &[f32]) -> usize {
        let Ok(mut samples) = self.samples.lock() else {
            return data.len();
        };
        let free = self.capacity.saturating_sub(samples.len());
        let take = free.min(data.len());
        samples.extend(&data[..take]);
        data.len() - take
    }

    /// Fills `out`; if there is not enough, fills the rest with silence and
    /// returns how many samples were really given.
    fn pop_into(&self, out: &mut [f32]) -> usize {
        let Ok(mut samples) = self.samples.lock() else {
            out.fill(0.0);
            return 0;
        };
        let take = samples.len().min(out.len());
        for slot in out.iter_mut().take(take) {
            *slot = samples.pop_front().unwrap_or(0.0);
        }
        out[take..].fill(0.0);
        take
    }

    fn clear(&self) {
        if let Ok(mut samples) = self.samples.lock() {
            samples.clear();
        }
    }
}

/// A slice of a single track written to the output (D-024).
///
/// This is gapless's bookkeeping: since the ring buffer can now carry the
/// samples of several tracks side by side, the "position" cannot be read from
/// a single counter. Every track knows where it started in output frames; the
/// playing track is the slice `frames_played` falls into.
#[derive(Debug, Clone)]
struct Span {
    /// The sequence number returned by [`AudioEngine::enqueue`].
    seq: u64,
    /// The absolute index in the output of this track's first frame.
    ///
    /// `None` for a track that is queued but has **not started** decoding: where
    /// it will start depends on how many frames the one before it wrote, and that
    /// is only known when that track ends.
    start_frame: Option<u64>,
    /// How many frames were written. `None` while decoding is under way.
    frames: Option<u64>,
    duration_ms: Option<u64>,
    /// Where in the track its first frame is (D-086): a track opened at a
    /// position starts there. The position adds it; the time heard does not.
    offset_ms: u64,
}

/// A job for the decoder.
///
/// There are two forms because there are two routes: when the user says
/// "play", the source is opened **on the caller's thread** (a missing file
/// should fail right away, not vanish silently in the background); with
/// read-ahead it is opened on the decoder's own thread — so the opening delay
/// is hidden while the buffer drains.
enum Job {
    Prepared(Box<PreparedTrack>),
    Lazy {
        seq: u64,
        source: AudioSource,
        /// The stream's first bytes, fetched ahead of time (D-077).
        head: Option<Arc<StreamHead>>,
    },
}

/// An opened track, ready to decode.
pub(crate) struct PreparedTrack {
    seq: u64,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    source_rate: u32,
    source_channels: usize,
    duration_ms: Option<u64>,
    /// The track's time base: what turns a seek's answer into milliseconds.
    time_base: Option<symphonia::core::units::TimeBase>,
    /// Where decoding starts in the track (D-086); `0` from the start.
    offset_ms: u64,
    /// A stream started from a head: through this it asks for a new address
    /// (D-077).
    #[cfg(feature = "http-client")]
    stream: Option<super::http_source::StreamHandle>,
}

/// The state shared between the decoding thread and the player.
struct Shared {
    ring: RingBuffer,
    /// The total number of frames handed to the output — it **grows across
    /// tracks** and is not reset at the start of a track. The position within a
    /// track is computed from the slice.
    frames_played: AtomicU64,
    /// The total number of frames written to the buffer so far. The start of a
    /// new slice.
    frames_queued: AtomicU64,
    /// The output's sample rate (frames/second).
    sample_rate: AtomicU64,
    /// The number of channels.
    channels: AtomicU64,
    /// `PlayState` — a `u8` so it can be carried atomically.
    state: AtomicU8,
    /// The "stop" signal for the decoding thread.
    stop: AtomicBool,
    /// No work left to decode: no track at hand and none in the queue. When the
    /// buffer drains too, playback ends. (The gapless counterpart of the old
    /// `drained`.)
    idle: AtomicBool,
    /// Jobs waiting to be decoded. This is where gapless happens: as the playing
    /// track ends the next one is already queued, and the device does not close.
    pending: Mutex<VecDeque<Job>>,
    /// The slices of tracks written to the output, in order.
    spans: Mutex<Vec<Span>>,
    /// An error that occurred while decoding (if any) — the caller takes it with
    /// `take_error`.
    error: Mutex<Option<String>>,
    /// The streams started from a head, by slice (D-077): the ones that may
    /// ask for a new address while they play.
    #[cfg(feature = "http-client")]
    streams: Mutex<Vec<(u64, super::http_source::StreamHandle)>>,
}

const STATE_STOPPED: u8 = 0;
const STATE_PLAYING: u8 = 1;
const STATE_PAUSED: u8 = 2;
const STATE_BUFFERING: u8 = 3;

fn state_to_u8(state: PlayState) -> u8 {
    match state {
        PlayState::Stopped => STATE_STOPPED,
        PlayState::Playing => STATE_PLAYING,
        PlayState::Paused => STATE_PAUSED,
        PlayState::Buffering => STATE_BUFFERING,
    }
}

fn u8_to_state(raw: u8) -> PlayState {
    match raw {
        STATE_PLAYING => PlayState::Playing,
        STATE_PAUSED => PlayState::Paused,
        STATE_BUFFERING => PlayState::Buffering,
        _ => PlayState::Stopped,
    }
}

impl Shared {
    fn state(&self) -> PlayState {
        u8_to_state(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, state: PlayState) {
        self.state.store(state_to_u8(state), Ordering::Release);
    }

    /// The output's own word on the state — playing, waiting, ended — which
    /// never takes it out of `Paused`. The callback reads the state, plays a
    /// buffer, then writes; a pause landing in between was overwritten with
    /// `Playing`, and the track played on paused (seen on macOS CI; D-090's
    /// seek pauses a track right as it starts, in that very window).
    fn set_state_unless_paused(&self, state: PlayState) {
        let paused = state_to_u8(PlayState::Paused);
        let next = state_to_u8(state);
        // `Err` is "paused": nothing to write.
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |now| {
                (now != paused).then_some(next)
            });
    }

    /// Turns a frame count into milliseconds.
    fn frames_to_ms(&self, frames: u64) -> u64 {
        let rate = self.sample_rate.load(Ordering::Acquire);
        if rate == 0 {
            return 0;
        }
        frames.saturating_mul(1000) / rate
    }

    /// Finds which slice the output is in right now.
    ///
    /// Only slices that have **started** count: if the next track looked like it
    /// was "playing" before being played, the transition would be announced
    /// early, and both the interface and the scrobble would show the wrong track.
    fn current_span(&self) -> Option<Span> {
        let played = self.frames_played.load(Ordering::Acquire);
        let spans = self.spans.lock().ok()?;
        spans
            .iter()
            .rev()
            .find(|span| span.start_frame.is_some_and(|start| start <= played))
            .cloned()
    }

    /// The duration to show: the playing track's, or the next one's if it has
    /// not started yet.
    ///
    /// When the engine has just been set up the decoding thread may not have
    /// written the first frame yet; rather than saying "duration unknown" we give
    /// the next track's — that is the track the user will hear.
    fn display_duration_ms(&self) -> Option<u64> {
        if let Some(span) = self.current_span() {
            return span.duration_ms;
        }
        let spans = self.spans.lock().ok()?;
        spans
            .iter()
            .find(|span| span.start_frame.is_none())
            .and_then(|span| span.duration_ms)
    }

    /// The time played within a slice (ms).
    ///
    /// If the slice has ended and the output has passed it, the result is the
    /// whole track — the number the scrobble needs (§1.6).
    fn played_ms_of(&self, span: &Span) -> u64 {
        let Some(start) = span.start_frame else {
            // It never started: its played time is zero.
            return 0;
        };
        let played = self.frames_played.load(Ordering::Acquire);
        let offset = played.saturating_sub(start);
        let clamped = match span.frames {
            Some(total) => offset.min(total),
            None => offset,
        };
        self.frames_to_ms(clamped)
    }

    /// The position of the playing track within itself (ms): where it was
    /// opened (D-086) plus what was played since.
    ///
    /// Before the first frame of a track opened at a position is played,
    /// that position is where it is — not 0: the music taken back from a
    /// video is at the video's second from the moment it is asked for, as
    /// [`Self::display_duration_ms`] gives the waiting track's length.
    fn position_ms(&self) -> u64 {
        if let Some(span) = self.current_span() {
            return span.offset_ms.saturating_add(self.played_ms_of(&span));
        }
        self.spans
            .lock()
            .ok()
            .and_then(|spans| {
                spans
                    .iter()
                    .find(|span| span.start_frame.is_none())
                    .map(|span| span.offset_ms)
            })
            .unwrap_or(0)
    }

    /// Records the slice (not started yet). Refreshes its duration if there is
    /// one.
    fn declare_span(&self, seq: u64, duration_ms: Option<u64>) {
        if let Ok(mut spans) = self.spans.lock() {
            if let Some(span) = spans.iter_mut().find(|span| span.seq == seq) {
                span.duration_ms = span.duration_ms.or(duration_ms);
                return;
            }
            spans.push(Span {
                seq,
                start_frame: None,
                frames: None,
                duration_ms,
                offset_ms: 0,
            });
        }
    }

    /// Where in the track the slice starts (D-086).
    fn set_offset(&self, seq: u64, offset_ms: u64) {
        if let Ok(mut spans) = self.spans.lock()
            && let Some(span) = spans.iter_mut().find(|span| span.seq == seq)
        {
            span.offset_ms = offset_ms;
        }
    }

    /// Starts the slice: where its first frame falls in the output is now known.
    fn begin_span(&self, seq: u64) {
        let start = self.frames_queued.load(Ordering::Acquire);
        if let Ok(mut spans) = self.spans.lock()
            && let Some(span) = spans.iter_mut().find(|span| span.seq == seq)
        {
            span.start_frame = Some(start);
        }
    }

    /// Closes the slice: how many frames were written becomes final.
    fn close_span(&self, seq: u64) {
        let queued = self.frames_queued.load(Ordering::Acquire);
        if let Ok(mut spans) = self.spans.lock()
            && let Some(span) = spans.iter_mut().find(|span| span.seq == seq)
            && let Some(start) = span.start_frame
        {
            span.frames = Some(queued.saturating_sub(start));
        }
    }

    /// Counts the frames written to the buffer (divided by the channel count).
    fn add_queued_samples(&self, samples: usize) {
        let channels = self.channels.load(Ordering::Acquire).max(1);
        self.frames_queued
            .fetch_add(samples as u64 / channels, Ordering::AcqRel);
    }

    fn record_error(&self, detail: String) {
        if let Ok(mut slot) = self.error.lock() {
            if slot.is_none() {
                *slot = Some(detail);
            }
        }
    }

    /// Keeps a head-started stream's handle under its slice number.
    fn keep_stream(&self, seq: u64, prepared: &PreparedTrack) {
        #[cfg(feature = "http-client")]
        if let Some(stream) = &prepared.stream
            && let Ok(mut streams) = self.streams.lock()
        {
            streams.push((seq, stream.clone()));
        }
        #[cfg(not(feature = "http-client"))]
        let _ = (seq, prepared);
    }

    /// A slice is decoded: its stream will not ask for anything any more.
    fn forget_stream(&self, seq: u64) {
        #[cfg(feature = "http-client")]
        if let Ok(mut streams) = self.streams.lock() {
            streams.retain(|(kept, _)| *kept != seq);
        }
        #[cfg(not(feature = "http-client"))]
        let _ = seq;
    }
}

/// The audio engine: **one** output stream, fed tracks in order (D-024).
///
/// All of gapless is here: the cpal stream and the ring buffer **stay open**
/// between tracks. As the playing track ends, the next one's samples have
/// already been appended to the same buffer; the device never closes and
/// reopens. That was also the old source of the gap: a new device + a new
/// decoder was set up for every track.
///
/// On `Drop` the stream closes and the decoding thread stops — it leaves no
/// leak.
pub struct AudioEngine {
    shared: Arc<Shared>,
    stream: Option<cpal::Stream>,
    decoder: Option<std::thread::JoinHandle<()>>,
    /// The number to give the next job.
    next_seq: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for AudioEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioEngine")
            .field("state", &self.shared.state())
            .field("position_ms", &self.shared.position_ms())
            .field("queued", &self.queued_len())
            .finish_non_exhaustive()
    }
}

impl AudioEngine {
    /// Opens a file, sets up the audio device and starts playing.
    ///
    /// # Errors
    /// If the file cannot be opened/decoded ([`Stage::PlaybackDecode`]) or the
    /// audio device cannot be set up ([`Stage::PlaybackOutput`]). Which stage it
    /// was stays in the error.
    pub fn play_file(path: &std::path::Path) -> Result<Self> {
        // The source is opened **before the device**: a corrupt or missing file
        // must fail at the decoding stage even in an environment with no audio
        // output at all (CI) — a "no device" error would hide the real cause.
        let prepared = open_source(
            &AudioSource::LocalFile {
                path: path.to_path_buf(),
            },
            None,
        )?;
        let engine = Self::open()?;
        engine.play_prepared(prepared);
        Ok(engine)
    }

    /// Plays an HTTP stream (§1.3: Subsonic / Jellyfin).
    ///
    /// Decoding starts while the bytes download in the background; details in
    /// [`super::http_source`]. **K3:** the client fetches the stream itself, no
    /// server steps in to relay it.
    ///
    /// # Errors
    /// If no connection can be made or the server returns something outside 2xx
    /// ([`Stage::NetworkRequest`]); if the audio cannot be decoded
    /// ([`Stage::PlaybackDecode`]).
    #[cfg(feature = "http-client")]
    pub fn play_http(url: &str, headers: &[crate::net::HttpHeader]) -> Result<Self> {
        let prepared = open_source(
            &AudioSource::HttpStream {
                url: url.to_owned(),
                headers: headers.to_vec(),
            },
            None,
        )?;
        let engine = Self::open()?;
        engine.play_prepared(prepared);
        Ok(engine)
    }

    /// Queues a source **right away**; the opening happens on the caller's
    /// thread.
    ///
    /// A missing file or an unreachable server returns an error here — it does
    /// not vanish silently in the background. The number returned is the slice
    /// number.
    ///
    /// # Errors
    /// If the source cannot be opened or decoded.
    pub fn play_source(&self, source: &AudioSource) -> Result<u64> {
        Ok(self.play_prepared(open_source(source, None)?))
    }

    /// [`Self::play_source`] from a head fetched ahead of time (D-077): the
    /// head is the stream's start, so no request stands between the call and
    /// the sound.
    ///
    /// # Errors
    /// If the source cannot be opened or decoded.
    pub(crate) fn play_source_from(
        &self,
        source: &AudioSource,
        head: Option<&StreamHead>,
    ) -> Result<u64> {
        Ok(self.play_prepared(open_source(source, head)?))
    }

    /// Queues an opened track and returns its slice number.
    pub(crate) fn play_prepared(&self, mut prepared: PreparedTrack) -> u64 {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
        prepared.seq = seq;
        self.shared.keep_stream(seq, &prepared);
        // We announce the slice **now**: the duration is known here, and the
        // caller can ask for `duration_ms()` as soon as it has set up the engine.
        // Where it will start becomes known when the decoding thread gets there.
        self.shared.declare_span(seq, prepared.duration_ms);
        if prepared.offset_ms > 0 {
            self.shared.set_offset(seq, prepared.offset_ms);
        }
        // We are not idle **as soon as the job is sent**. Leaving this to the
        // decoding thread let the callback say "buffer empty + idle = done" and
        // set Stopped until that thread woke up; the user would see a track
        // that had not started yet as finished.
        self.shared.idle.store(false, Ordering::Release);
        // We fix the state **now** too, not by waiting for the first callback.
        // Between `open()` and this moment the callback may have seen `idle`
        // correctly and set `Stopped`, and that `Stopped` is wrong: the job is
        // queued and has not been played yet. The window in between was a few
        // milliseconds for a local file and seconds for an HTTP stream
        // (`open_source` downloads) — `Session::play`'s loop would say "it ended
        // before it started" there and exit, silently and with zero listens.
        // `Buffering` is "the pipeline waiting" (D-016), and that is exactly
        // what we want to say here.
        self.shared.set_state(PlayState::Buffering);
        if let Ok(mut pending) = self.shared.pending.lock() {
            pending.push_back(Job::Prepared(Box::new(prepared)));
        }
        seq
    }

    /// Queues a source as **read-ahead** (gapless).
    ///
    /// The source is opened on the decoding thread: the opening delay is hidden
    /// while the playing track's buffer drains. If it cannot be opened the error
    /// is collected with `take_error` — we do not return a `Result` because the
    /// caller is not waiting at that moment, but the error **is not swallowed**
    /// (K9).
    pub fn enqueue(&self, source: &AudioSource) -> u64 {
        self.enqueue_from(source, None)
    }

    /// [`Self::enqueue`] with the stream's head fetched ahead of time
    /// (D-077).
    pub(crate) fn enqueue_from(&self, source: &AudioSource, head: Option<Arc<StreamHead>>) -> u64 {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
        self.shared.idle.store(false, Ordering::Release);
        if let Ok(mut pending) = self.shared.pending.lock() {
            pending.push_back(Job::Lazy {
                seq,
                source: source.clone(),
                head,
            });
        }
        seq
    }

    /// The slices whose stream, started from a head, asks for a new address
    /// (D-077): the one it had no longer answers.
    #[must_use]
    pub(crate) fn wanting_address(&self) -> Vec<u64> {
        #[cfg(feature = "http-client")]
        {
            self.shared.streams.lock().map_or_else(
                |_| Vec::new(),
                |streams| {
                    streams
                        .iter()
                        .filter(|(_, stream)| stream.wants_address())
                        .map(|(seq, _)| *seq)
                        .collect()
                },
            )
        }
        #[cfg(not(feature = "http-client"))]
        Vec::new()
    }

    /// Gives a slice's stream a new address; it carries on from the byte it
    /// stopped at. `false` when that slice has no stream asking.
    pub(crate) fn give_address(&self, seq: u64, source: &AudioSource) -> bool {
        #[cfg(feature = "http-client")]
        {
            let AudioSource::HttpStream { url, headers } = source else {
                return false;
            };
            let Ok(streams) = self.shared.streams.lock() else {
                return false;
            };
            match streams.iter().find(|(kept, _)| *kept == seq) {
                Some((_, stream)) if stream.wants_address() => {
                    stream.give_address(url.clone(), headers.clone());
                    true
                }
                _ => false,
            }
        }
        #[cfg(not(feature = "http-client"))]
        {
            let _ = (seq, source);
            false
        }
    }

    /// The number of jobs waiting in the queue (not yet started decoding).
    #[must_use]
    pub fn queued_len(&self) -> usize {
        self.shared
            .pending
            .lock()
            .map_or(0, |pending| pending.len())
    }

    /// The number of the slice the output is playing right now.
    ///
    /// When this value changes the track transition has been **heard**; the
    /// player advances the scrobble and the queue cursor accordingly.
    #[must_use]
    pub fn current_seq(&self) -> Option<u64> {
        self.shared.current_span().map(|span| span.seq)
    }

    /// The played time of a given slice (ms). `None` if unknown.
    #[must_use]
    pub fn played_ms_of(&self, seq: u64) -> Option<u64> {
        let spans = self.shared.spans.lock().ok()?;
        let span = spans.iter().rev().find(|span| span.seq == seq)?;
        Some(self.shared.played_ms_of(span))
    }

    /// The total duration of a given slice **read from the container**.
    ///
    /// For files with no duration in their tags this is the only reliable
    /// source; since the library catalog is fed from tags it can be `None`
    /// there.
    #[must_use]
    pub fn duration_of(&self, seq: u64) -> Option<u64> {
        let spans = self.shared.spans.lock().ok()?;
        spans.iter().rev().find(|span| span.seq == seq)?.duration_ms
    }

    /// Opens the audio device and starts the decoding thread; no track yet.
    ///
    /// # Errors
    /// If no audio device is found or the stream cannot be set up
    /// ([`Stage::PlaybackOutput`]).
    pub fn open() -> Result<Self> {
        // — Open the audio device.
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| audio_err(Stage::PlaybackOutput, "no default audio output found"))?;
        let supported = device.default_output_config().map_err(|source| {
            audio_err(
                Stage::PlaybackOutput,
                format!("could not read the output configuration: {source}"),
            )
        })?;
        let out_rate = supported.sample_rate();
        let out_channels = supported.channels() as usize;

        // A buffer of about 2 seconds: enough against crackling, an acceptable
        // latency.
        let capacity = out_rate as usize * out_channels * 2;
        let shared = Arc::new(Shared {
            ring: RingBuffer::new(capacity),
            frames_played: AtomicU64::new(0),
            frames_queued: AtomicU64::new(0),
            sample_rate: AtomicU64::new(u64::from(out_rate)),
            channels: AtomicU64::new(out_channels as u64),
            state: AtomicU8::new(STATE_BUFFERING),
            stop: AtomicBool::new(false),
            idle: AtomicBool::new(true),
            pending: Mutex::new(VecDeque::new()),
            spans: Mutex::new(Vec::new()),
            error: Mutex::new(None),
            #[cfg(feature = "http-client")]
            streams: Mutex::new(Vec::new()),
        });

        // — The decoding thread: it lives as long as the engine, not the track.
        // When a track ends it takes the next one from the queue and carries
        // on writing **into the same buffer**; that is exactly gapless.
        let decode_shared = Arc::clone(&shared);
        let decoder_thread = std::thread::Builder::new()
            .name("headshell-decode".to_owned())
            .spawn(move || {
                let mut scratch: Vec<f32> = Vec::new();
                loop {
                    if decode_shared.stop.load(Ordering::Acquire) {
                        break;
                    }

                    let Some(job) = take_job(&decode_shared) else {
                        // No work: playback may have ended, but the engine is up.
                        // If the caller hands over a new track, it carries on from here.
                        decode_shared.idle.store(true, Ordering::Release);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    };

                    let prepared = match job {
                        Job::Prepared(track) => *track,
                        Job::Lazy { seq, source, head } => {
                            match open_source(&source, head.as_deref()) {
                                Ok(mut track) => {
                                    track.seq = seq;
                                    decode_shared.keep_stream(seq, &track);
                                    track
                                }
                                Err(err) => {
                                    // The read-ahead track could not be opened: do not swallow it,
                                    // record it and move on to the next (K9).
                                    decode_shared.record_error(
                                        err.chain_text().replace('\n', " ").to_string(),
                                    );
                                    continue;
                                }
                            }
                        }
                    };

                    decode_shared.idle.store(false, Ordering::Release);
                    decode_track(
                        &decode_shared,
                        prepared,
                        out_rate,
                        out_channels,
                        capacity,
                        &mut scratch,
                    );
                }
            })
            .map_err(|source| {
                audio_err(
                    Stage::PlaybackOutput,
                    format!("could not start the decoding thread: {source}"),
                )
            })?;

        // — The audio callback.
        let cb_shared = Arc::clone(&shared);
        let err_shared = Arc::clone(&shared);
        let config: cpal::StreamConfig = supported.config();
        let stream = device
            .build_output_stream(
                config,
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // If paused, write silence; the buffer is kept.
                    if cb_shared.state() == PlayState::Paused {
                        out.fill(0.0);
                        return;
                    }
                    let given = cb_shared.ring.pop_into(out);
                    let channels = cb_shared.channels.load(Ordering::Acquire).max(1);
                    cb_shared
                        .frames_played
                        .fetch_add(given as u64 / channels, Ordering::AcqRel);

                    if given > 0 {
                        cb_shared.set_state_unless_paused(PlayState::Playing);
                    } else if cb_shared.idle.load(Ordering::Acquire) {
                        // The buffer is empty and there is no work to decode: it really ended.
                        cb_shared.set_state_unless_paused(PlayState::Stopped);
                    } else {
                        // The data ran out but there is a track to decode: we wait.
                        cb_shared.set_state_unless_paused(PlayState::Buffering);
                    }
                },
                move |err| {
                    err_shared.record_error(format!("audio stream error: {err}"));
                },
                None,
            )
            .map_err(|source| {
                audio_err(
                    Stage::PlaybackOutput,
                    format!("could not set up the audio stream: {source}"),
                )
            })?;

        stream.play().map_err(|source| {
            audio_err(
                Stage::PlaybackOutput,
                format!("could not start the audio stream: {source}"),
            )
        })?;

        Ok(Self {
            shared,
            stream: Some(stream),
            decoder: Some(decoder_thread),
            next_seq: AtomicU64::new(0),
        })
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> PlayState {
        self.shared.state()
    }

    /// The position of the playing track **within itself**.
    #[must_use]
    pub fn position_ms(&self) -> u64 {
        self.shared.position_ms()
    }

    /// The time heard of the playing track — what was played since it was
    /// opened, without where it was opened (D-086).
    #[must_use]
    pub fn played_ms_now(&self) -> u64 {
        self.shared
            .current_span()
            .map_or(0, |span| self.shared.played_ms_of(&span))
    }

    /// The duration of the playing track (if read from the container).
    #[must_use]
    pub fn duration_ms(&self) -> Option<u64> {
        self.shared.display_duration_ms()
    }

    /// Is there nothing left to play (the queue empty, decoding done, the buffer
    /// drained).
    ///
    /// With gapless this does **not** mean the track ended: while there is a next
    /// track the engine does not count as finished; only the slice changes.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.shared.idle.load(Ordering::Acquire)
            && self.shared.ring.len() == 0
            && self.queued_len() == 0
    }

    /// Pauses. The buffer is kept, the position freezes.
    pub fn pause(&self) {
        self.shared.set_state(PlayState::Paused);
    }

    /// Resumes if paused.
    pub fn resume(&self) {
        if self.shared.state() == PlayState::Paused {
            self.shared.set_state(PlayState::Playing);
        }
    }

    /// Takes the error that occurred while decoding (if any) and clears it.
    ///
    /// So it is not swallowed silently: the caller should read it and write it to
    /// the diagnostics record (K9).
    pub fn take_error(&self) -> Option<String> {
        self.shared.error.lock().ok().and_then(|mut e| e.take())
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.ring.clear();
        // Close the stream first: the callback must no longer touch the shared
        // state.
        drop(self.stream.take());
        if let Some(handle) = self.decoder.take() {
            let _ = handle.join();
        }
    }
}

/// Takes a job from the queue (if any).
fn take_job(shared: &Arc<Shared>) -> Option<Job> {
    shared.pending.lock().ok()?.pop_front()
}

/// Opens an [`AudioSource`] and gets it ready to decode.
///
/// `play_source` calls this on the caller's thread, read-ahead on the
/// decoding thread — the body is the same in both.
fn open_source(source: &AudioSource, head: Option<&StreamHead>) -> Result<PreparedTrack> {
    #[cfg(not(feature = "http-client"))]
    let _ = head;
    match source {
        AudioSource::LocalFile { path } => {
            let file = std::fs::File::open(path)
                .map_err(|err| crate::error::io_err(Stage::PlaybackDecode, path, err))?;
            let mut hint = Hint::new();
            if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
                hint.with_extension(ext);
            }
            prepare(Box::new(file), hint, &path.display().to_string())
        }
        #[cfg(feature = "http-client")]
        AudioSource::HttpStream { url, headers } => {
            let mut hint = Hint::new();
            // The extension is only a **hint**: servers give extension-less
            // addresses like `?id=...`, and then symphonia recognises the container
            // from its contents.
            if let Some(ext) = extension_from_url(url) {
                hint.with_extension(&ext);
            }
            let label = crate::net::without_query(url);
            match head {
                Some(head) => {
                    let (stream, handle) =
                        super::http_source::HttpMediaSource::open_with_head(url, headers, head)?;
                    let mut prepared = prepare(Box::new(stream), hint, label)?;
                    prepared.stream = Some(handle);
                    Ok(prepared)
                }
                None => {
                    let stream = super::http_source::HttpMediaSource::open(url, headers)?;
                    prepare(Box::new(stream), hint, label)
                }
            }
        }
        #[cfg(not(feature = "http-client"))]
        AudioSource::HttpStream { .. } => Err(Error::new(
            Stage::PlaybackResolve,
            ErrorKind::Unsupported {
                provider: "net".to_owned(),
                what: "HTTP stream (a build with the `http-client` feature off)".to_owned(),
                capabilities: "STREAM".to_owned(),
            },
        )),
    }
}

/// Recognises an opened source, sets up its decoder and reads its duration.
fn prepare(source: Box<dyn MediaSource>, hint: Hint, label: &str) -> Result<PreparedTrack> {
    let mss = MediaSourceStream::new(source, Default::default());

    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|err| {
            audio_err(
                Stage::PlaybackDecode,
                format!("could not open {label}: {err}"),
            )
        })?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| audio_err(Stage::PlaybackDecode, format!("no audio track in {label}")))?;
    let track_id = track.id;
    let time_base = track.time_base;

    let duration_ms = track
        .time_base
        .zip(track.duration)
        .and_then(|(tb, dur)| u64::try_from(tb.calc_duration(dur)?.as_millis()).ok());

    let codec_params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| {
            audio_err(
                Stage::PlaybackDecode,
                format!("could not read the decoder parameters for {label}"),
            )
        })?
        .clone();

    let source_rate = codec_params.sample_rate.unwrap_or(0);
    let source_channels = codec_params
        .channels
        .as_ref()
        .map_or(0, symphonia::core::audio::Channels::count);

    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&codec_params, &AudioDecoderOptions::default())
        .map_err(|err| {
            audio_err(
                Stage::PlaybackDecode,
                format!("could not set up the decoder: {err}"),
            )
        })?;

    Ok(PreparedTrack {
        // The number is assigned when queued; it is not known yet at open time.
        seq: 0,
        format,
        decoder,
        track_id,
        source_rate,
        source_channels,
        duration_ms,
        time_base,
        offset_ms: 0,
        #[cfg(feature = "http-client")]
        stream: None,
    })
}

/// Opens a source at `start_ms` (D-086, D-090) — the device is not touched,
/// so a position that can't be reached leaves what plays alone.
///
/// The seek is the container's coarse one: it lands on a packet near
/// `start_ms`, and the position reports where it landed.
///
/// `None`: the position is past the track's end. The length a container
/// states is not where its last packet starts — measured: an Ogg track of
/// 1000 ms refuses 999, a FLAC of 15000 ms reaches 14999 but not 15000 —
/// so "past the end" is the container's answer, not a comparison here.
///
/// # Errors
/// If the source cannot be opened or decoded, or the container cannot move
/// for another reason (a stream that can't seek).
pub(crate) fn open_source_at(
    source: &AudioSource,
    head: Option<&StreamHead>,
    start_ms: u64,
) -> Result<Option<PreparedTrack>> {
    let mut prepared = open_source(source, head)?;
    if start_ms > 0 && !seek_prepared(&mut prepared, start_ms)? {
        return Ok(None);
    }
    Ok(Some(prepared))
}

/// Moves an opened track to `start_ms` before its first packet is decoded
/// (D-086), and notes where it landed. `false`: the position is past the
/// track's end.
fn seek_prepared(prepared: &mut PreparedTrack, start_ms: u64) -> Result<bool> {
    use symphonia::core::errors::{Error as SymphoniaError, SeekErrorKind};
    use symphonia::core::formats::{SeekMode, SeekTo};
    use symphonia::core::units::Time;

    let seeked = match prepared.format.seek(
        SeekMode::Coarse,
        SeekTo::Time {
            time: Time::from_millis_u64(start_ms),
            track_id: Some(prepared.track_id),
        },
    ) {
        Ok(seeked) => seeked,
        // Past the last packet the readers answer one of two ways: FLAC
        // reads off the end, Ogg and MP3 say the time is out of range.
        Err(SymphoniaError::SeekError(SeekErrorKind::OutOfRange)) => return Ok(false),
        Err(SymphoniaError::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(false);
        }
        Err(err) => {
            return Err(audio_err(
                Stage::PlaybackDecode,
                format!("could not move to {start_ms} ms in the track: {err}"),
            ));
        }
    };
    // The decoder's state belongs to where it was; after a jump it starts
    // over.
    prepared.decoder.reset();
    prepared.offset_ms = prepared
        .time_base
        .and_then(|base| base.calc_time(seeked.actual_ts))
        .and_then(|time| u64::try_from(time.as_millis()).ok())
        .unwrap_or(start_ms);
    Ok(true)
}

/// Decodes a single track to the end and writes it into the buffer.
///
/// When it returns, the track's slice is closed; the calling loop moves on to
/// the next job. **The buffer is not emptied** — that is how gapless works:
/// the next track's samples are appended behind the finished one.
fn decode_track(
    shared: &Arc<Shared>,
    prepared: PreparedTrack,
    out_rate: u32,
    out_channels: usize,
    capacity: usize,
    scratch: &mut Vec<f32>,
) {
    let PreparedTrack {
        seq,
        mut format,
        mut decoder,
        track_id,
        source_rate,
        source_channels,
        duration_ms,
        ..
    } = prepared;

    // If it could not be read from the container, fall back to the output's
    // values: no conversion is done.
    let source_rate = if source_rate == 0 {
        out_rate
    } else {
        source_rate
    };
    let source_channels = if source_channels == 0 {
        out_channels
    } else {
        source_channels
    };

    shared.declare_span(seq, duration_ms);
    shared.begin_span(seq);

    loop {
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        // If the buffer is full, wait: decoding must not run ahead of playback.
        if shared.ring.len() >= capacity.saturating_sub(capacity / 8) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }

        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(err) => {
                shared.record_error(format!("could not read a packet: {err}"));
                break;
            }
        };
        if packet.track_id != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(buffer) => {
                interleave_f32(&buffer, scratch);
                resample_into(
                    scratch,
                    source_rate,
                    source_channels,
                    out_rate,
                    out_channels,
                    shared,
                    capacity,
                );
            }
            Err(symphonia::core::errors::Error::DecodeError(msg)) => {
                // A single corrupt packet must not drop the track; count it and carry on.
                tracing::warn!(error = %msg, "could not decode a packet, skipping it");
            }
            Err(err) => {
                shared.record_error(format!("decoding stopped: {err}"));
                break;
            }
        }
    }

    shared.close_span(seq);
    shared.forget_stream(seq);
}

/// Turns a symphonia buffer into interleaved `f32` samples.
fn interleave_f32(buffer: &GenericAudioBufferRef<'_>, out: &mut Vec<f32>) {
    out.clear();
    let frames = buffer.frames();
    let channels = buffer.spec().channels().count();
    out.reserve(frames * channels);
    // `copy_to_vec_interleaved` does the type conversion too.
    buffer.copy_to_vec_interleaved(out);
}

/// Fits the source samples to the output rate/channel count and writes them
/// into the buffer.
///
/// Nearest-neighbour sampling: simple and cheap. Phase 1's goal is producing
/// correct sound; if high-quality resampling (sinc) is needed, it is measured
/// separately.
fn resample_into(
    input: &[f32],
    source_rate: u32,
    source_channels: usize,
    out_rate: u32,
    out_channels: usize,
    shared: &Arc<Shared>,
    capacity: usize,
) {
    if input.is_empty() || source_channels == 0 {
        return;
    }
    let in_frames = input.len() / source_channels;
    if in_frames == 0 {
        return;
    }

    // The same rate and channel count: no conversion.
    if source_rate == out_rate && source_channels == out_channels {
        write_all(shared, input, capacity);
        return;
    }

    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "sample indices; the loss is inaudible at audio scale"
    )]
    let out_frames = ((in_frames as f64) * f64::from(out_rate) / f64::from(source_rate)) as usize;
    let mut converted = Vec::with_capacity(out_frames * out_channels);

    for frame in 0..out_frames {
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "nearest-neighbour sampling"
        )]
        let src_frame = ((frame as f64) * f64::from(source_rate) / f64::from(out_rate)) as usize;
        let src_frame = src_frame.min(in_frames - 1);
        let base = src_frame * source_channels;

        for channel in 0..out_channels {
            // Mono → multi-channel: the same sample is copied.
            // Multi-channel → fewer channels: the extra ones drop.
            let src_channel = if source_channels == 1 {
                0
            } else {
                channel.min(source_channels - 1)
            };
            converted.push(input.get(base + src_channel).copied().unwrap_or(0.0));
        }
    }
    write_all(shared, &converted, capacity);
}

/// Writes into the buffer; if full, waits until there is room.
///
/// Every sample written is also added to `frames_queued`: the counter the
/// slice boundaries (and so the position) rest on.
fn write_all(shared: &Arc<Shared>, mut data: &[f32], capacity: usize) {
    let _ = capacity;
    while !data.is_empty() {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let leftover = shared.ring.push(data);
        let written = data.len() - leftover;
        if written > 0 {
            shared.add_queued_samples(written);
        }
        if leftover == 0 {
            return;
        }
        data = &data[written..];
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_reports_what_it_could_not_take() {
        let ring = RingBuffer::new(4);
        assert_eq!(
            ring.push(&[1.0, 2.0]),
            0,
            "all must be taken while there is room"
        );
        assert_eq!(ring.push(&[3.0, 4.0, 5.0]), 1, "1 sample must not fit");
        assert_eq!(ring.len(), 4);
    }

    #[test]
    fn ring_buffer_pads_with_silence_when_starved() {
        let ring = RingBuffer::new(8);
        ring.push(&[1.0, 2.0]);
        let mut out = [9.0; 4];
        let given = ring.pop_into(&mut out);
        assert_eq!(given, 2, "only 2 samples can be given");
        assert_eq!(out, [1.0, 2.0, 0.0, 0.0], "the rest must be silence");
    }

    #[test]
    fn state_survives_the_atomic_round_trip() {
        for state in [
            PlayState::Stopped,
            PlayState::Playing,
            PlayState::Paused,
            PlayState::Buffering,
        ] {
            assert_eq!(u8_to_state(state_to_u8(state)), state);
        }
    }

    /// A newly sent job must count the engine as "not finished" **at once**.
    ///
    /// Regression: `open()` started the audio stream right away, and the callback
    /// saw an empty buffer + `idle` and set `Stopped`. Since `play_source` did not
    /// fix this state and left it to the first callback, a window remained in
    /// between; when `Session::play`'s loop landed there, it said "it ended
    /// before it started" and exited silently. The window was milliseconds for a
    /// local file and seconds for an HTTP stream (`open_source` downloads) — so
    /// the flaw only showed on remote sources.
    ///
    /// If there is no audio device the test skips itself, writing why.
    #[test]
    fn a_freshly_queued_track_is_never_reported_as_stopped() {
        let Ok(engine) = AudioEngine::open() else {
            eprintln!("no audio device — skipping (this is not a failure)");
            return;
        };

        // Wait for the callback to run at least once and set `Stopped`: the
        // test's precondition, the starting state in which the flaw occurred.
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert_eq!(
            engine.state(),
            PlayState::Stopped,
            "precondition: an empty engine must look stopped"
        );

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/audio/tagged.flac");
        let source = AudioSource::LocalFile { path: fixture };
        let Ok(_seq) = engine.play_source(&source) else {
            eprintln!("could not decode the fixture — skipping");
            return;
        };

        // **No** sleep: the whole point of the fix is that the state is right
        // without waiting for the callback.
        assert_ne!(
            engine.state(),
            PlayState::Stopped,
            "the engine cannot look finished while a job is queued"
        );
    }

    /// A bare shared state for tests (without opening a device).
    fn shared_for_test(capacity: usize, rate: u64, channels: u64) -> Arc<Shared> {
        Arc::new(Shared {
            ring: RingBuffer::new(capacity),
            frames_played: AtomicU64::new(0),
            frames_queued: AtomicU64::new(0),
            sample_rate: AtomicU64::new(rate),
            channels: AtomicU64::new(channels),
            state: AtomicU8::new(STATE_BUFFERING),
            stop: AtomicBool::new(false),
            idle: AtomicBool::new(true),
            pending: Mutex::new(VecDeque::new()),
            spans: Mutex::new(Vec::new()),
            error: Mutex::new(None),
            #[cfg(feature = "http-client")]
            streams: Mutex::new(Vec::new()),
        })
    }

    #[test]
    fn position_is_zero_before_any_frame_is_played() {
        let shared = shared_for_test(16, 48_000, 2);
        // With no slice the position is zero: which track we are in is unknown.
        assert_eq!(shared.position_ms(), 0);

        shared.declare_span(0, Some(5_000));
        shared.begin_span(0);
        assert_eq!(shared.position_ms(), 0);
        shared.frames_played.store(48_000, Ordering::Release);
        assert_eq!(shared.position_ms(), 1000, "48k frames = 1 second");
    }

    /// D-024's bookkeeping: while the buffer carries two tracks side by side,
    /// the position must give the location **within the track itself**, not the
    /// output's total.
    #[test]
    fn position_restarts_at_each_track_boundary() {
        let shared = shared_for_test(16, 48_000, 2);

        // The first track: 2 seconds (96k frames) written.
        shared.declare_span(0, Some(2_000));
        shared.begin_span(0);
        shared.frames_queued.store(96_000, Ordering::Release);
        shared.close_span(0);

        // The second track is appended right behind it.
        shared.declare_span(1, Some(3_000));
        shared.begin_span(1);

        // The output is in the middle of the first track.
        shared.frames_played.store(48_000, Ordering::Release);
        assert_eq!(shared.current_span().map(|s| s.seq), Some(0));
        assert_eq!(shared.position_ms(), 1000);

        // It crossed the boundary: now the second track is playing and its position
        // starts **from the beginning**.
        shared
            .frames_played
            .store(96_000 + 24_000, Ordering::Release);
        assert_eq!(shared.current_span().map(|s| s.seq), Some(1));
        assert_eq!(shared.position_ms(), 500, "the new track's own position");
        assert_eq!(
            shared.current_span().and_then(|s| s.duration_ms),
            Some(3_000),
            "the duration must be the new track's too"
        );

        // The finished track's scrobble must see its full length, not the output's
        // total.
        let spans = shared.spans.lock().unwrap();
        let first = spans.iter().find(|s| s.seq == 0).unwrap();
        assert_eq!(shared.played_ms_of(first), 2_000);
    }

    #[test]
    fn resampling_mono_to_stereo_duplicates_the_channel() {
        let shared = shared_for_test(64, 8000, 2);
        // 2 frames of mono, same rate → 2 frames of stereo.
        resample_into(&[0.5, -0.5], 8000, 1, 8000, 2, &shared, 64);

        let mut out = [0.0; 4];
        let given = shared.ring.pop_into(&mut out);
        assert_eq!(given, 4);
        assert_eq!(
            out,
            [0.5, 0.5, -0.5, -0.5],
            "a mono sample must be copied to both channels"
        );
        assert_eq!(
            shared.frames_queued.load(Ordering::Acquire),
            2,
            "the frames written must go into the slice bookkeeping"
        );
    }

    #[test]
    fn resampling_doubles_the_frames_when_the_rate_doubles() {
        let shared = shared_for_test(1024, 16_000, 1);
        // 4 frames @8kHz → 8 frames at 16kHz.
        resample_into(&[1.0, 2.0, 3.0, 4.0], 8000, 1, 16_000, 1, &shared, 1024);
        assert_eq!(shared.ring.len(), 8);
    }

    /// D-090: a position inside the track is reached; one past its last
    /// packet is "past the end", not an error — in every container, though
    /// each says it its own way (FLAC reads off the end, Ogg and MP3 say
    /// out of range). The positions are the measured edges: the Ogg refuses
    /// even 1 ms short of its stated length. No device is needed.
    #[test]
    fn a_position_past_the_last_packet_is_the_end_not_an_error() {
        let cases = [
            ("fingerprint_sample.flac", 14_900, 15_000),
            ("tagged.flac", 900, 1_000),
            ("Other Artist - Ogg Track.ogg", 900, 999),
            ("Test Artist - Mp3 Track.mp3", 1_900, 7_000),
        ];
        for (name, inside, past) in cases {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/audio")
                .join(name);
            let source = AudioSource::LocalFile { path };

            let landed = open_source_at(&source, None, inside)
                .unwrap_or_else(|err| panic!("{name}: {}", err.chain_text()))
                .unwrap_or_else(|| panic!("{name}: {inside} ms is inside the track"));
            assert!(
                landed.offset_ms <= inside && inside - landed.offset_ms < 200,
                "{name}: a coarse seek lands on a packet just before {inside} ms, not at {}",
                landed.offset_ms
            );

            let past_end = open_source_at(&source, None, past).unwrap_or_else(|err| {
                panic!("{name}: past the end is not an error: {}", err.chain_text())
            });
            assert!(
                past_end.is_none(),
                "{name}: {past} ms is past the last packet"
            );
        }
    }
}
