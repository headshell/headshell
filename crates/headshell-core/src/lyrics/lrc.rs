//! Reading lyrics text (D-078): LRC, and ID3's binary `SYLT` frame.
//!
//! LRC is one convention with many dialects, and the parser takes what the
//! files in the wild carry, not only the tidy form:
//!
//! - `[mm:ss.xx]` with one to three fraction digits, `[mm:ss]` without, the
//!   old `[mm:ss:xx]` and `[h:mm:ss.xx]`; minutes past 59.
//! - Several stamps before one line (`[00:12.00][01:40.00]chorus`): the line
//!   is sung each time.
//! - `[offset:±ms]`: positive, the lines come sooner — applied here, so a
//!   line's `start_ms` is when it is sung.
//! - Metadata tags (`[ar:…]`, `[length:…]`) are dropped; a bracket that is
//!   neither, like `[Chorus]`, is text.
//! - Word stamps inside a line (`<00:12.30>`, enhanced LRC) are dropped: lines
//!   are timed, words are not (D-078).
//!
//! Text with no stamp at all is unsynced lyrics; in text that has stamps, a
//! line without one is not a sung line (a heading, a credit) and is left out.

use super::{LyricLine, Lyrics};

/// Parses LRC — or plain text, which comes out as unsynced lyrics.
pub(crate) fn parse(text: &str) -> Lyrics {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut offset_ms: i64 = 0;
    let mut stamped: Vec<(u64, String)> = Vec::new();
    let mut plain: Vec<String> = Vec::new();

    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let mut rest = raw.trim_start();
        let mut stamps = Vec::new();
        let mut metadata = false;
        while let Some(inner) = rest.strip_prefix('[') {
            let Some(end) = inner.find(']') else { break };
            let tag = &inner[..end];
            if let Some(ms) = timestamp(tag) {
                stamps.push(ms);
            } else if let Some((key, value)) = metadata_tag(tag) {
                metadata = true;
                if key.eq_ignore_ascii_case("offset")
                    && let Ok(value) = value.trim().parse::<i64>()
                {
                    offset_ms = value;
                }
            } else {
                // `[Chorus]`: a word in brackets, not a tag.
                break;
            }
            rest = inner[end + 1..].trim_start();
        }
        let words = strip_word_stamps(rest).trim().to_owned();
        if !stamps.is_empty() {
            for ms in stamps {
                stamped.push((ms, words.clone()));
            }
        } else if !(metadata && words.is_empty()) {
            plain.push(words);
        }
    }

    if stamped.is_empty() {
        return Lyrics::unsynced(tidy_plain(plain));
    }
    // Stable: two lines on one stamp keep their order.
    stamped.sort_by_key(|(ms, _)| *ms);
    Lyrics {
        synced: true,
        lines: stamped
            .into_iter()
            .map(|(ms, text)| LyricLine {
                start_ms: Some(shift(ms, offset_ms)),
                text,
            })
            .collect(),
        attribution: None,
    }
}

/// A stamp moved by an `[offset:]`, never before the track's start.
fn shift(ms: u64, offset_ms: i64) -> u64 {
    let ms = i64::try_from(ms).unwrap_or(i64::MAX);
    u64::try_from(ms.saturating_sub(offset_ms).max(0)).unwrap_or(0)
}

/// Blank lines at the ends dropped, runs of them kept as one: they part the
/// verses.
fn tidy_plain(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        let blank = line.is_empty();
        if blank && out.last().is_none_or(String::is_empty) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// `mm:ss`, `mm:ss.f`, `mm:ss.ff`, `mm:ss.fff`, `mm:ss:ff`, `h:mm:ss.ff` →
/// milliseconds.
fn timestamp(tag: &str) -> Option<u64> {
    let parts: Vec<&str> = tag.trim().split(':').collect();
    let (hours, minutes, seconds, fraction) = match parts.as_slice() {
        [m, s] => {
            let (s, f) = split_fraction(s);
            (None, *m, s, f)
        }
        // `h:mm:ss.ff` when the last part has a fraction; the old
        // `mm:ss:ff` when it has two digits and nothing else.
        [h, m, s] if s.contains('.') => {
            let (s, f) = split_fraction(s);
            (Some(*h), *m, s, f)
        }
        [m, s, f] => (None, *m, *s, Some(*f)),
        _ => return None,
    };
    let number = |text: &str| -> Option<u64> {
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    let hours = match hours {
        Some(h) => number(h)?,
        None => 0,
    };
    let minutes = number(minutes)?;
    let seconds = number(seconds)?;
    if seconds >= 60 || (hours > 0 && minutes >= 60) {
        return None;
    }
    let millis = match fraction {
        None => 0,
        Some(digits) => {
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // Tenths, hundredths, thousandths; digits past the third are noise.
            let kept: String = digits.chars().take(3).collect();
            let value: u64 = kept.parse().ok()?;
            value * 10u64.pow(3 - u32::try_from(kept.len()).ok()?)
        }
    };
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add(seconds * 1_000 + millis)
}

fn split_fraction(seconds: &str) -> (&str, Option<&str>) {
    match seconds.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (seconds, None),
    }
}

/// `ar:Artist` → `("ar", "Artist")`. The key is letters (or `#`, a comment):
/// a stamp is not metadata.
fn metadata_tag(tag: &str) -> Option<(&str, &str)> {
    let (key, value) = tag.split_once(':')?;
    let key = key.trim();
    let keyish = !key.is_empty()
        && (key == "#" || key.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_'));
    keyish.then_some((key, value))
}

/// Drops the word stamps of enhanced LRC (`<00:12.30>`), and a stamp a
/// broken file left inside a line (`[00:15.00]`).
fn strip_word_stamps(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(['<', '[']) {
        let close = if rest[open..].starts_with('<') {
            '>'
        } else {
            ']'
        };
        let Some(length) = rest[open + 1..].find(close) else {
            break;
        };
        let inner = &rest[open + 1..open + 1 + length];
        out.push_str(&rest[..open]);
        if timestamp(inner).is_none() {
            out.push_str(&rest[open..=open + 1 + length]);
        }
        rest = &rest[open + 2 + length..];
    }
    out.push_str(rest);
    out
}

/// ID3v2's `SYLT` frame body → synced lyrics.
///
/// `encoding(1) language(3) format(1) type(1) descriptor(text, terminated)`,
/// then `text(terminated) stamp(4, big endian)` pairs. Only stamps in
/// milliseconds (format 2) are read: MPEG frames (format 1) need the frame
/// length, and a guessed one would put every line in the wrong place.
///
/// A `SYLT` may time syllables rather than lines — a new line then begins
/// with a line feed — so pieces are joined into lines when any of them
/// starts with one.
///
/// # Errors
/// With the reason, when the frame is cut short, counts MPEG frames, or its
/// text is not in the encoding it names.
// Only an MP3's tags carry one: read where tags are (`audio`).
#[cfg(any(feature = "audio", test))]
pub(crate) fn parse_sylt(body: &[u8]) -> Result<Lyrics, String> {
    let [encoding, _, _, _, format, _content, rest @ ..] = body else {
        return Err("the SYLT frame is shorter than its header".to_owned());
    };
    if *format != 2 {
        return Err(format!(
            "the SYLT frame's stamps are in {} — only milliseconds are read",
            if *format == 1 {
                "MPEG frames".to_owned()
            } else {
                format!("an unknown unit ({format})")
            }
        ));
    }
    let wide = matches!(encoding, 1 | 2);
    let (_, mut rest) = terminated(rest, wide).ok_or("the SYLT descriptor is not terminated")?;

    let mut pieces: Vec<(u64, String)> = Vec::new();
    while !rest.is_empty() {
        let (text, after) = terminated(rest, wide).ok_or("a SYLT text is not terminated")?;
        let [a, b, c, d, after @ ..] = after else {
            return Err("a SYLT text has no stamp after it".to_owned());
        };
        let ms = u64::from(u32::from_be_bytes([*a, *b, *c, *d]));
        pieces.push((ms, decode(text, *encoding)?));
        rest = after;
    }

    let syllables = pieces
        .iter()
        .skip(1)
        .any(|(_, text)| text.starts_with(['\n', '\r']));
    let mut lines: Vec<LyricLine> = Vec::new();
    for (ms, text) in pieces {
        let new_line = text.starts_with(['\n', '\r']);
        let words = text.trim_start_matches(['\n', '\r']);
        match lines.last_mut() {
            Some(line) if syllables && !new_line => line.text.push_str(words),
            _ => lines.push(LyricLine {
                start_ms: Some(ms),
                text: words.to_owned(),
            }),
        }
    }
    for line in &mut lines {
        line.text = line.text.trim().to_owned();
    }
    lines.sort_by_key(|line| line.start_ms);
    Ok(Lyrics {
        synced: true,
        lines,
        attribution: None,
    })
}

/// Splits at the terminator: one zero byte, or two on a two-byte boundary for
/// UTF-16. `None` when there is none.
#[cfg(any(feature = "audio", test))]
fn terminated(bytes: &[u8], wide: bool) -> Option<(&[u8], &[u8])> {
    if wide {
        let at = bytes
            .chunks_exact(2)
            .position(|pair| pair == [0, 0])?
            .checked_mul(2)?;
        Some((&bytes[..at], &bytes[at + 2..]))
    } else {
        let at = bytes.iter().position(|&b| b == 0)?;
        Some((&bytes[..at], &bytes[at + 1..]))
    }
}

/// ID3v2's four text encodings.
#[cfg(any(feature = "audio", test))]
fn decode(bytes: &[u8], encoding: u8) -> Result<String, String> {
    let text = match encoding {
        // ISO-8859-1: every byte is the code point of the same number.
        0 => Some(bytes.iter().map(|&b| char::from(b)).collect()),
        1 => super::decode_text(bytes),
        2 => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16(&units).ok()
        }
        3 => String::from_utf8(bytes.to_vec()).ok(),
        other => {
            return Err(format!(
                "the SYLT frame names an unknown text encoding ({other})"
            ));
        }
    };
    text.ok_or_else(|| "a SYLT text is not in the encoding its frame names".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn starts(lyrics: &Lyrics) -> Vec<(Option<u64>, &str)> {
        lyrics
            .lines
            .iter()
            .map(|line| (line.start_ms, line.text.as_str()))
            .collect()
    }

    #[test]
    fn stamps_of_every_common_shape_are_read() {
        for (tag, ms) in [
            ("00:12.34", 12_340),
            ("00:12.3", 12_300),
            ("00:12.345", 12_345),
            ("00:12.3456", 12_345),
            ("00:12", 12_000),
            ("01:02:03", 62_030),
            ("1:02:03.5", 3_723_500),
            ("75:00.00", 4_500_000),
            (" 00:01.00 ", 1_000),
        ] {
            assert_eq!(timestamp(tag), Some(ms), "{tag}");
        }
        for tag in [
            "ar:x",
            "00:60.00",
            "aa:bb",
            "00:12.",
            "-1:00",
            "",
            "1:60:00.0",
        ] {
            assert_eq!(timestamp(tag), None, "{tag:?}");
        }
    }

    #[test]
    fn a_line_with_several_stamps_is_sung_each_time_and_the_order_is_by_time() {
        let lyrics = parse(
            "[ar:Made-up Band]\n[ti:A Song]\n[00:20.00]second\n[00:05.00][00:40.00]chorus\n[00:30.00]third\n",
        );
        assert!(lyrics.synced);
        assert_eq!(
            starts(&lyrics),
            [
                (Some(5_000), "chorus"),
                (Some(20_000), "second"),
                (Some(30_000), "third"),
                (Some(40_000), "chorus")
            ]
        );
    }

    #[test]
    fn the_offset_tag_moves_every_line_and_never_before_the_start() {
        let lyrics = parse("[offset:+500]\n[00:00.20]first\n[00:10.00]second");
        assert_eq!(
            starts(&lyrics),
            [(Some(0), "first"), (Some(9_500), "second")]
        );
        let later = parse("[offset:-250]\n[00:10.00]x");
        assert_eq!(starts(&later), [(Some(10_250), "x")]);
    }

    #[test]
    fn a_pause_is_kept_word_stamps_are_dropped_and_crlf_and_bom_are_not_text() {
        let lyrics = parse(
            "\u{feff}[00:01.00]<00:01.00>one <00:01.50>two\r\n[00:03.00]\r\n[00:04.00] three [00:05.00]",
        );
        assert_eq!(
            starts(&lyrics),
            [
                (Some(1_000), "one two"),
                (Some(3_000), ""),
                (Some(4_000), "three")
            ]
        );
    }

    #[test]
    fn a_bracketed_word_is_text_and_an_unstamped_line_among_stamps_is_left_out() {
        let lyrics =
            parse("Lyrics by someone\n[00:01.00][Chorus] la la\n[00:02.00]<not a stamp> ok");
        assert_eq!(
            starts(&lyrics),
            [
                (Some(1_000), "[Chorus] la la"),
                (Some(2_000), "<not a stamp> ok")
            ]
        );
    }

    #[test]
    fn text_without_stamps_is_unsynced_with_verses_kept_apart() {
        let lyrics = parse("\n\nFirst verse\nstill first\n\n\n\nSecond verse\n\n");
        assert!(!lyrics.synced);
        assert_eq!(
            starts(&lyrics),
            [
                (None, "First verse"),
                (None, "still first"),
                (None, ""),
                (None, "Second verse")
            ]
        );
        // Metadata alone is no lyrics.
        assert!(parse("[ar:x]\n[ti:y]").lines.is_empty());
    }

    /// A `SYLT` frame body: `encoding language format type descriptor`, then
    /// `text\0` + a four-byte stamp for every piece.
    fn sylt(encoding: u8, format: u8, pieces: &[(&str, u32)]) -> Vec<u8> {
        let mut body = vec![encoding, b'e', b'n', b'g', format, 1];
        let text = |s: &str| -> Vec<u8> {
            match encoding {
                1 => {
                    let mut out = vec![0xFF, 0xFE];
                    out.extend(s.encode_utf16().flat_map(u16::to_le_bytes));
                    out.extend([0, 0]);
                    out
                }
                _ => {
                    let mut out = s.as_bytes().to_vec();
                    out.push(0);
                    out
                }
            }
        };
        body.extend(text("descriptor"));
        for (words, ms) in pieces {
            body.extend(text(words));
            body.extend(ms.to_be_bytes());
        }
        body
    }

    #[test]
    fn a_sylt_frame_of_lines_is_read_line_by_line() {
        let lyrics = parse_sylt(&sylt(3, 2, &[("Sil baştan", 1_000), ("ikinci", 4_000)])).unwrap();
        assert_eq!(
            starts(&lyrics),
            [(Some(1_000), "Sil baştan"), (Some(4_000), "ikinci")]
        );
        let wide = parse_sylt(&sylt(1, 2, &[("şarkı", 500)])).unwrap();
        assert_eq!(starts(&wide), [(Some(500), "şarkı")]);
    }

    #[test]
    fn a_sylt_frame_of_syllables_is_joined_into_lines() {
        let lyrics = parse_sylt(&sylt(
            3,
            2,
            &[
                ("Hel", 1_000),
                ("lo ", 1_300),
                ("there", 1_600),
                ("\nNext", 3_000),
                (" line", 3_400),
            ],
        ))
        .unwrap();
        assert_eq!(
            starts(&lyrics),
            [(Some(1_000), "Hello there"), (Some(3_000), "Next line")]
        );
    }

    #[test]
    fn a_sylt_frame_in_mpeg_frames_or_cut_short_is_refused_with_the_reason() {
        let err = parse_sylt(&sylt(3, 1, &[("x", 1)])).unwrap_err();
        assert!(err.contains("MPEG frames"), "{err}");
        assert!(parse_sylt(&[3, b'e']).unwrap_err().contains("shorter"));
        let mut cut = sylt(3, 2, &[("x", 1)]);
        cut.truncate(cut.len() - 2);
        assert!(parse_sylt(&cut).unwrap_err().contains("no stamp"));
    }
}
