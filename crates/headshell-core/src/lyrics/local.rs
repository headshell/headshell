//! A local file's own lyrics (D-078): a `.lrc` next to it with its name, then
//! the lyrics in its tags.
//!
//! What symphonia 0.6 gives was checked in its source before this was
//! written: ID3v2 `USLT`, Vorbis `LYRICS`/`UNSYNCEDLYRICS`, APE `lyrics` and
//! MP4 `©lyr` arrive as the standard `Lyrics` tag — as text, which may be LRC
//! — and ID3v2 `SYLT` as a raw binary tag, the frame's body. A tagger's
//! `SYNCEDLYRICS` has no standard mapping and is read by its key. An MP3's
//! ID3v2 tag is a metadata revision of its own, before the container's, so
//! every revision is looked at (as for covers, D-076).

use std::path::{Path, PathBuf};

use super::{LyricsSource, decode_text, lrc};
use crate::diag::Stage;
use crate::error::{Error, ErrorKind, Result, io_err};
use crate::provider::ProviderLyrics;

/// The file's own lyrics: a synced one first — the `.lrc`'s, then the tags' —
/// and without one, the unsynced ones in the same order.
///
/// A link that failed while the other found lyrics is logged, and the
/// lyrics are the answer; with nothing found, the failure is — the `.lrc`
/// may be in a code page this does not guess (K9).
///
/// # Errors
/// If nothing was found and the `.lrc` or the tags could not be read.
pub(crate) fn file_lyrics(path: &Path) -> Result<Option<ProviderLyrics>> {
    let mut found: Vec<ProviderLyrics> = Vec::new();
    let mut failed: Vec<Error> = Vec::new();
    for outcome in [sidecar(path), embedded(path)] {
        match outcome {
            Ok(Some(lyrics)) => found.push(lyrics),
            Ok(None) => {}
            Err(err) => failed.push(err),
        }
    }
    let best = found
        .iter()
        .position(|candidate| candidate.lyrics.synced)
        .or_else(|| (!found.is_empty()).then_some(0));
    match best {
        Some(index) => {
            for err in &failed {
                tracing::warn!(
                    file = %path.display(),
                    error = %err.chain_text().replace('\n', " "),
                    "lyrics: the file had lyrics, but one of its links failed"
                );
            }
            Ok(Some(found.swap_remove(index)))
        }
        None => failed.into_iter().next().map_or(Ok(None), Err),
    }
}

/// The `.lrc` next to the file: its name with the `lrc` extension, in any
/// case.
///
/// # Errors
/// If the folder cannot be listed, or the `.lrc` cannot be read or is not
/// UTF-8 or UTF-16 text.
fn sidecar(path: &Path) -> Result<Option<ProviderLyrics>> {
    let Some(lrc_path) = sidecar_path(path)? else {
        return Ok(None);
    };
    let bytes =
        std::fs::read(&lrc_path).map_err(|source| io_err(Stage::LyricsRead, &lrc_path, source))?;
    let text = decode_text(&bytes).ok_or_else(|| {
        Error::new(
            Stage::LyricsRead,
            ErrorKind::Lyrics {
                detail: format!(
                    "{} is not UTF-8 or UTF-16 text; save it as UTF-8",
                    lrc_path.display()
                ),
            },
        )
    })?;
    let lyrics = lrc::parse(&text);
    Ok((!lyrics.is_empty()).then_some(ProviderLyrics {
        lyrics,
        instrumental: false,
        source: LyricsSource::Sidecar,
    }))
}

fn sidecar_path(path: &Path) -> Result<Option<PathBuf>> {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) else {
        return Ok(None);
    };
    let entries =
        std::fs::read_dir(dir).map_err(|source| io_err(Stage::LyricsRead, dir, source))?;
    let mut matches: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate.file_stem() == Some(stem)
                && candidate
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("lrc"))
                && candidate.is_file()
        })
        .collect();
    // `Song.lrc` and `Song.LRC` side by side: the same one every time.
    matches.sort();
    Ok(matches.into_iter().next())
}

/// The lyrics in the file's tags: a synced one (`SYLT`, or LRC text) first.
///
/// `Ok(None)`: the file opened and carries none.
///
/// # Errors
/// If the file cannot be opened or is not an audio file symphonia reads, or
/// its only lyrics are a `SYLT` frame that cannot be read.
#[cfg(feature = "audio")]
fn embedded(path: &Path) -> Result<Option<ProviderLyrics>> {
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, RawValue, StandardTag, Tag};

    let file =
        std::fs::File::open(path).map_err(|source| io_err(Stage::LyricsRead, path, source))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            Default::default(),
            MetadataOptions::default(),
        )
        .map_err(|source| {
            Error::new(
                Stage::LyricsRead,
                ErrorKind::Audio {
                    detail: format!("could not open {}: {source}", path.display()),
                },
            )
        })?;

    let mut texts: Vec<String> = Vec::new();
    let mut sylt: Vec<std::result::Result<super::Lyrics, String>> = Vec::new();
    let mut take = |tag: &Tag| {
        if let Some(StandardTag::Lyrics(text)) = &tag.std {
            texts.push(text.to_string());
            return;
        }
        let key = tag.raw.key.to_ascii_uppercase();
        match (&tag.raw.value, key.as_str()) {
            (RawValue::Binary(body), "SYLT") => sylt.push(lrc::parse_sylt(body)),
            (RawValue::String(text), "SYNCEDLYRICS" | "SYNCED LYRICS" | "UNSYNCED LYRICS") => {
                texts.push(text.to_string());
            }
            _ => {}
        }
    };
    let mut log = reader.metadata();
    loop {
        if let Some(revision) = log.current() {
            let tags = revision.media.tags.iter().chain(
                revision
                    .per_track
                    .iter()
                    .flat_map(|track| &track.metadata.tags),
            );
            for tag in tags {
                take(tag);
            }
        }
        if log.pop().is_none() {
            break;
        }
    }

    let mut candidates: Vec<super::Lyrics> = Vec::new();
    let mut sylt_error: Option<String> = None;
    for outcome in sylt {
        match outcome {
            Ok(lyrics) => candidates.push(lyrics),
            Err(reason) => {
                sylt_error.get_or_insert(reason);
            }
        }
    }
    candidates.extend(texts.iter().map(|text| lrc::parse(text)));
    candidates.retain(|lyrics| !lyrics.is_empty());
    let best = candidates
        .iter()
        .position(|lyrics| lyrics.synced)
        .or_else(|| (!candidates.is_empty()).then_some(0));
    match (best, sylt_error) {
        (Some(index), _) => Ok(Some(ProviderLyrics {
            lyrics: candidates.swap_remove(index),
            instrumental: false,
            source: LyricsSource::Embedded,
        })),
        (None, Some(reason)) => Err(Error::new(
            Stage::LyricsRead,
            ErrorKind::Lyrics {
                detail: format!("{}: {reason}", path.display()),
            },
        )),
        (None, None) => Ok(None),
    }
}

/// Without the `audio` feature symphonia is not in the build: the tags
/// cannot be read. That is said, not answered with "none" (K9).
#[cfg(not(feature = "audio"))]
fn embedded(path: &Path) -> Result<Option<ProviderLyrics>> {
    let _ = path;
    Err(Error::new(
        Stage::LyricsRead,
        ErrorKind::Unsupported {
            provider: "local".to_owned(),
            what: "reading lyrics from tags (a build without the `audio` feature)".to_owned(),
            capabilities: "NONE".to_owned(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[cfg(feature = "audio")]
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/lyrics")
            .join(name)
    }

    #[test]
    fn a_sidecar_with_the_files_name_is_read_in_any_case_of_its_extension() {
        let dir = TempDir::new("lyrics-sidecar");
        let audio = dir.path().join("Made-up Band - Song.flac");
        std::fs::write(&audio, b"not audio").unwrap();
        std::fs::write(
            dir.path().join("Made-up Band - Song.LRC"),
            "[00:01.00]one\n[00:02.00]two\n",
        )
        .unwrap();
        // Another song's `.lrc` in the same folder is not this one's.
        std::fs::write(dir.path().join("Other.lrc"), "[00:01.00]other\n").unwrap();

        let found = file_lyrics(&audio)
            .unwrap()
            .expect("the .lrc must be found");
        assert_eq!(found.source, LyricsSource::Sidecar);
        assert!(found.lyrics.synced);
        assert_eq!(found.lyrics.lines[1].text, "two");
    }

    #[test]
    fn a_sidecar_in_a_legacy_code_page_is_refused_with_the_reason_not_guessed() {
        let dir = TempDir::new("lyrics-sidecar-1254");
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"not audio").unwrap();
        // Windows-1254 `ş` (0xFE): not UTF-8.
        std::fs::write(dir.path().join("Song.lrc"), b"[00:01.00]ba\xfe").unwrap();
        let err = file_lyrics(&audio).unwrap_err();
        let text = err.chain_text();
        assert!(text.contains("LYRICS_READ"), "{text}");
        assert!(text.contains("not UTF-8"), "{text}");
    }

    #[test]
    fn a_folder_without_a_sidecar_and_a_file_without_tags_have_none() {
        // A real audio file with no lyrics, and nothing next to it.
        let dir = TempDir::new("lyrics-none");
        let audio = dir.path().join("Plain.ogg");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/audio/Other Artist - Ogg Track.ogg"),
            &audio,
        )
        .unwrap();
        let outcome = file_lyrics(&audio);
        if cfg!(feature = "audio") {
            assert_eq!(outcome.unwrap(), None);
        } else {
            assert!(
                outcome.is_err(),
                "without `audio` the tags cannot be read — and that is said"
            );
        }
    }

    /// The fixtures (`fixtures/lyrics/README.md`): LRC text in a FLAC's
    /// Vorbis `LYRICS`, unsynced words in an MP3's `USLT`, a `SYLT` frame in
    /// milliseconds, and a `.lrc` next to a FLAC that also carries tags.
    #[cfg(feature = "audio")]
    #[test]
    fn lyrics_are_read_from_every_kind_of_tag_the_fixtures_carry() {
        let flac = file_lyrics(&fixture("Lyric Artist - Flac Lrc.flac"))
            .unwrap()
            .expect("the FLAC's LYRICS comment");
        assert_eq!(flac.source, LyricsSource::Embedded);
        assert!(flac.lyrics.synced, "LRC text in a tag is synced");
        assert_eq!(flac.lyrics.lines[0].start_ms, Some(500));

        let uslt = file_lyrics(&fixture("Lyric Artist - Mp3 Uslt.mp3"))
            .unwrap()
            .expect("the MP3's USLT frame");
        assert_eq!(uslt.source, LyricsSource::Embedded);
        assert!(!uslt.lyrics.synced);
        assert!(
            uslt.lyrics
                .lines
                .iter()
                .any(|line| line.text.contains("made-up"))
        );

        let sylt = file_lyrics(&fixture("Lyric Artist - Mp3 Sylt.mp3"))
            .unwrap()
            .expect("the MP3's SYLT frame");
        assert!(sylt.lyrics.synced);
        assert_eq!(sylt.lyrics.lines[1].start_ms, Some(1_200));

        // A `.lrc` beside a file whose tags carry unsynced words: the synced
        // one wins.
        let both = file_lyrics(&fixture("Lyric Artist - Sidecar.flac"))
            .unwrap()
            .expect("the .lrc");
        assert_eq!(both.source, LyricsSource::Sidecar);
        assert!(both.lyrics.synced);
    }
}
