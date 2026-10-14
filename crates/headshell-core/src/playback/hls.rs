//! HLS streams (D-091): a playlist of short files played as one.
//!
//! SoundCloud stopped giving plain (`progressive`) streams in October 2026:
//! every track came as HLS — a media playlist (`.m3u8`) naming an init file
//! and ten-second fragments of AAC in fragmented MP4. The fragments, read
//! one after another behind their init file, are one fragmented MP4 that
//! symphonia reads front to back; so the stream here is a chain of
//! requests, handed to the same background download as any HTTP stream.
//!
//! What is not played says so (K9): an encrypted playlist (D-026 — nothing
//! encrypted is opened), a master playlist (a provider must name the
//! variant), byte ranges.
//!
//! Moving in it: the fragment holding the second is opened behind the init
//! file; symphonia counts time from the first fragment it reads, so the
//! engine drops the packets that end before the second within it.

use std::collections::VecDeque;
use std::io::{self, Read};

use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result};
use crate::net::{HttpHeader, UreqClient};

/// The largest playlist read: a four-hour track in two-second fragments is
/// well under it; anything larger is not a playlist.
const MAX_PLAYLIST_BYTES: u64 = 4 * 1024 * 1024;

/// Is this address an HLS playlist? The provider's address says so by its
/// name — `….m3u8` — so no request is spent finding out.
pub(crate) fn is_playlist(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.ends_with(".m3u8") || name.ends_with(".m3u")
}

/// A media playlist: what is fetched, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Playlist {
    /// The init file (`#EXT-X-MAP`), read before any fragment.
    pub(crate) init: Option<String>,
    pub(crate) segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment {
    pub(crate) url: String,
    pub(crate) duration_ms: u64,
}

impl Playlist {
    /// The track's length: its fragments' lengths added up.
    pub(crate) fn duration_ms(&self) -> u64 {
        self.segments
            .iter()
            .map(|segment| segment.duration_ms)
            .sum()
    }

    /// The fragment holding `at_ms` and where it starts; `None` at or past
    /// the end.
    pub(crate) fn segment_at(&self, at_ms: u64) -> Option<(usize, u64)> {
        let mut start = 0u64;
        for (index, segment) in self.segments.iter().enumerate() {
            let end = start.saturating_add(segment.duration_ms);
            if at_ms < end {
                return Some((index, start));
            }
            start = end;
        }
        None
    }

    /// The stream from fragment `first` on: the init file, then each
    /// fragment, every one asked for only when the one before is read.
    pub(crate) fn reader_from(&self, first: usize, headers: &[HttpHeader]) -> Chain {
        let urls = self
            .init
            .iter()
            .cloned()
            .chain(
                self.segments
                    .iter()
                    .skip(first)
                    .map(|segment| segment.url.clone()),
            )
            .collect();
        Chain {
            client: UreqClient::for_streams(),
            urls,
            headers: headers.to_vec(),
            current: None,
        }
    }
}

/// Reads a media playlist (RFC 8216). `base` is the playlist's own address,
/// for the relative ones in it.
///
/// # Errors
/// If it is not a playlist, is a master playlist, is encrypted, uses byte
/// ranges, or names no fragment ([`Stage::PlaybackResolve`]).
pub(crate) fn parse(text: &str, base: &str) -> Result<Playlist> {
    let shown = crate::net::without_query(base);
    let refuse = |why: &str| {
        Error::new(
            Stage::PlaybackResolve,
            ErrorKind::Unsupported {
                provider: "the HLS reader".to_owned(),
                what: format!("{why} ({shown})"),
                capabilities: "an unencrypted media playlist of MP4 fragments".to_owned(),
            },
        )
    };
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err(refuse("not an HLS playlist: it doesn't start with #EXTM3U"));
    }
    let mut init = None;
    let mut segments = Vec::new();
    let mut pending_ms: Option<u64> = None;
    for line in lines {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let seconds = rest.split(',').next().unwrap_or_default().trim();
            let seconds: f64 = seconds
                .parse()
                .map_err(|_| refuse(&format!("a fragment length that isn't a number: {line}")))?;
            pending_ms = Some(seconds_to_ms(seconds));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            if attribute(rest, "BYTERANGE").is_some() {
                return Err(refuse("an init file given as a byte range"));
            }
            let uri = attribute(rest, "URI").ok_or_else(|| refuse("#EXT-X-MAP without a URI"))?;
            init = Some(resolve(base, &uri));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            if attribute(rest, "METHOD").as_deref() != Some("NONE") {
                return Err(refuse(
                    "an encrypted stream — nothing encrypted is played (D-026)",
                ));
            }
        } else if line.starts_with("#EXT-X-STREAM-INF") {
            return Err(refuse(
                "a master playlist — the provider must name one variant",
            ));
        } else if line.starts_with("#EXT-X-BYTERANGE") {
            return Err(refuse("fragments given as byte ranges"));
        } else if !line.starts_with('#') {
            let Some(duration_ms) = pending_ms.take() else {
                return Err(refuse(&format!(
                    "a fragment without #EXTINF: {}",
                    crate::net::without_query(line)
                )));
            };
            segments.push(Segment {
                url: resolve(base, line),
                duration_ms,
            });
        }
    }
    if segments.is_empty() {
        return Err(refuse("a playlist that names no fragment"));
    }
    Ok(Playlist { init, segments })
}

/// Fetches and reads the playlist at `url`.
///
/// # Errors
/// If it cannot be fetched ([`Stage::NetworkRequest`], or the status's
/// stage), or as [`parse`].
pub(crate) fn fetch(url: &str, headers: &[HttpHeader]) -> Result<Playlist> {
    let shown = crate::net::without_query(url);
    let (_, reader) = UreqClient::for_streams().open_stream(url, headers)?;
    let mut text = String::new();
    reader
        .take(MAX_PLAYLIST_BYTES)
        .read_to_string(&mut text)
        .map_err(|err| {
            crate::net::network_err(shown, format!("could not read the playlist: {err}"))
        })?;
    parse(&text, url)
}

fn seconds_to_ms(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        // Lengths are a few seconds: well inside u64, and never negative here.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = (seconds * 1000.0).round() as u64;
        ms
    } else {
        0
    }
}

/// An attribute's value in an attribute list: `URI="…"` or `METHOD=NONE`.
fn attribute(list: &str, name: &str) -> Option<String> {
    let mut rest = list;
    while !rest.is_empty() {
        let (key, after) = rest.split_once('=')?;
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            let (value, tail) = quoted.split_once('"')?;
            (value, tail.strip_prefix(',').unwrap_or(tail))
        } else {
            after.split_once(',').unwrap_or((after, ""))
        };
        if key.trim().eq_ignore_ascii_case(name) {
            return Some(value.to_owned());
        }
        rest = next;
    }
    None
}

/// A playlist line's address against the playlist's own: absolute as it is,
/// `/path` on the same host, anything else next to the playlist.
fn resolve(base: &str, uri: &str) -> String {
    if uri.contains("://") {
        return uri.to_owned();
    }
    let base = base.split(['?', '#']).next().unwrap_or(base);
    if let Some(path) = uri.strip_prefix('/') {
        let origin_end = base
            .find("://")
            .and_then(|scheme| base[scheme + 3..].find('/').map(|slash| scheme + 3 + slash))
            .unwrap_or(base.len());
        return format!("{}/{path}", &base[..origin_end]);
    }
    let dir = base.rsplit_once('/').map_or(base, |(dir, _)| dir);
    format!("{dir}/{uri}")
}

/// The playlist's files as one stream: each is requested when the one
/// before it has been read to its end.
pub(crate) struct Chain {
    client: UreqClient,
    urls: VecDeque<String>,
    headers: Vec<HttpHeader>,
    current: Option<Box<dyn Read + Send>>,
}

impl Read for Chain {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(reader) = self.current.as_mut() {
                let read = reader.read(buf)?;
                if read > 0 || buf.is_empty() {
                    return Ok(read);
                }
                self.current = None;
            }
            let Some(url) = self.urls.pop_front() else {
                return Ok(0);
            };
            let (_, reader) = self
                .client
                .open_stream(&url, &self.headers)
                .map_err(|err| io::Error::other(err.chain_text().replace('\n', " ")))?;
            self.current = Some(reader);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    const BASE: &str = "https://cdn.example/aac_160k/abc/playlist.m3u8?Policy=secret";

    /// SoundCloud's shape, as fetched on 2026-10-15 (addresses shortened).
    const SOUNDCLOUD: &str = "#EXTM3U
#EXT-X-VERSION:7
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:0
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MAP:URI=\"https://cdn.example/aac_160k/abc/init.mp4?expires=1&Policy=p\"
#EXTINF:10.007800,
https://cdn.example/aac_160k/abc/data000.m4s?expires=1&Policy=p
#EXTINF:9.984583,
https://cdn.example/aac_160k/abc/data001.m4s?expires=1&Policy=p
#EXTINF:3.2,
https://cdn.example/aac_160k/abc/data002.m4s?expires=1&Policy=p
#EXT-X-ENDLIST
";

    #[test]
    fn a_soundcloud_playlist_reads_as_an_init_file_and_its_fragments() {
        let playlist = parse(SOUNDCLOUD, BASE).unwrap();
        assert_eq!(
            playlist.init.as_deref(),
            Some("https://cdn.example/aac_160k/abc/init.mp4?expires=1&Policy=p")
        );
        let lengths: Vec<u64> = playlist.segments.iter().map(|s| s.duration_ms).collect();
        assert_eq!(lengths, [10_008, 9_985, 3_200]);
        assert_eq!(playlist.duration_ms(), 23_193);
    }

    #[test]
    fn a_second_falls_in_the_fragment_that_holds_it() {
        let playlist = parse(SOUNDCLOUD, BASE).unwrap();
        assert_eq!(playlist.segment_at(0), Some((0, 0)));
        assert_eq!(playlist.segment_at(10_007), Some((0, 0)));
        assert_eq!(playlist.segment_at(10_008), Some((1, 10_008)));
        assert_eq!(playlist.segment_at(23_192), Some((2, 19_993)));
        assert_eq!(
            playlist.segment_at(23_193),
            None,
            "the end is past the last fragment"
        );
    }

    #[test]
    fn relative_addresses_are_read_against_the_playlists() {
        let text =
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\nseg/0.m4s\n#EXTINF:2,\n/abs/1.m4s\n";
        let playlist = parse(text, BASE).unwrap();
        assert_eq!(
            playlist.init.as_deref(),
            Some("https://cdn.example/aac_160k/abc/init.mp4")
        );
        assert_eq!(
            playlist.segments[0].url,
            "https://cdn.example/aac_160k/abc/seg/0.m4s"
        );
        assert_eq!(playlist.segments[1].url, "https://cdn.example/abs/1.m4s");
    }

    /// What is not played says what it is, and never names the address's
    /// key (K9).
    #[test]
    fn what_is_not_played_says_why() {
        let cases = [
            ("<html>", "doesn't start with #EXTM3U"),
            (
                "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:2,\na.ts\n",
                "encrypted",
            ),
            (
                "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"k\"\n#EXTINF:2,\na.m4s\n",
                "encrypted",
            ),
            (
                "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nlow.m3u8\n",
                "master playlist",
            ),
            (
                "#EXTM3U\n#EXTINF:2,\n#EXT-X-BYTERANGE:100@0\na.m4s\n",
                "byte ranges",
            ),
            ("#EXTM3U\na.m4s\n", "without #EXTINF"),
            ("#EXTM3U\n#EXT-X-ENDLIST\n", "names no fragment"),
        ];
        for (text, said) in cases {
            let err = parse(text, BASE).unwrap_err();
            let chain = err.chain_text();
            assert_eq!(err.stage(), Stage::PlaybackResolve, "{chain}");
            assert!(chain.contains(said), "{text:?} → {chain}");
            assert!(!chain.contains("secret"), "the key leaked: {chain}");
        }
        assert!(parse("#EXTM3U\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:2,\na.m4s\n", BASE).is_ok());
    }

    #[test]
    fn a_playlist_is_known_by_its_name() {
        assert!(is_playlist(BASE));
        assert!(is_playlist("https://x/a/LIST.M3U8"));
        assert!(!is_playlist("https://x/a/track.mp3?m3u8=1"));
        assert!(!is_playlist("https://x/rest/stream?id=1"));
    }
}
