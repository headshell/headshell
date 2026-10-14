# `fixtures/audio/`

**None** of the audio files here is real music. All of them were produced
synthetically or written by hand; the repository carries no copyrighted
recording and must not. When adding a new audio fixture, keep the rule: write
the command that produces it here, and don't commit a binary that can't be
reproduced.

| File | What for | How it was produced |
|---|---|---|
| `fingerprint_sample.flac` | Chromaprint fingerprint (D-045) | the `ffmpeg` command below |
| `Test Artist - Mp3 Track.mp3` | deriving tags from the file name; **deliberately too short** for a fingerprint | a 2 s synthetic tone |
| `Other Artist - Ogg Track.ogg` | container variety | a synthetic tone |
| `tagged.flac` | reading embedded tags | a synthetic tone + Vorbis comments |
| `corrupt.flac` | the decoder reporting its stage | 24 invalid bytes |
| `cover.jpg` | the cover image path | a 5-byte placeholder |
| `Dir Artist/` | inference from the directory structure | an empty directory tree |
| `hls/` | an HLS stream played and moved in (D-091) | the `ffmpeg` command below: 6 s of AAC in four MP4 fragments |

## `fingerprint_sample.flac`

15 seconds, 44.1 kHz, mono, 16 bit. A four-voice chord that shifts up a
semitone every 2.5 seconds — a steady tone leaves the chroma features flat,
while noise makes the fingerprint meaningless; a signal was needed that sits
between the two, tonal but changing.

The length is no accident: Chromaprint needs a window of a few seconds to
produce the first fingerprint item, so the 2-second
`Test Artist - Mp3 Track.mp3` gives a "too short" error — that file is now the
test of this limit.

```sh
ffmpeg -y \
  -f lavfi -i "aevalsrc='\
0.32*sin(2*PI*(220*pow(2,(2*floor(t/2.5))/12))*t) + \
0.26*sin(2*PI*(220*pow(2,(4+2*floor(t/2.5))/12))*t) + \
0.20*sin(2*PI*(220*pow(2,(7+2*floor(t/2.5))/12))*t) + \
0.10*sin(2*PI*(220*pow(2,(12+2*floor(t/2.5))/12))*t)':d=15:s=44100:c=mono" \
  -sample_fmt s16 "fixtures/audio/fingerprint_sample.flac"
```

Since it is synthetic, it **has no match** in the AcoustID database, and it
shouldn't: the live AcoustID test checks the distinction between "no match
found" and "the service couldn't be reached" (K9), not recognising a known
track.

## `hls/`

An HLS media playlist with an init file and four fragments: a 440 Hz tone,
6 s, mono, AAC at 64 kb/s, cut every 2 s — the shape SoundCloud serves (D-091).
Made with ffmpeg 7.1 (Debian 13); the fragments' lengths are in the playlist.

```sh
cd fixtures/audio/hls && ffmpeg -y \
  -f lavfi -i "sine=frequency=440:duration=6:sample_rate=44100" -ac 1 \
  -c:a aac -b:a 64k -f hls -hls_time 2 -hls_playlist_type vod \
  -hls_segment_type fmp4 -hls_fmp4_init_filename init.mp4 \
  -hls_segment_filename "data%03d.m4s" playlist.m3u8
```
