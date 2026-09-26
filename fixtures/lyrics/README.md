# `fixtures/lyrics/`

Audio files and a `.lrc` for the lyrics chain (D-078). Like everything under
`fixtures/`, **nothing here is real music or real lyrics**: the audio is the
synthetic tone of `fixtures/audio/`, and every line of words is made up.

| File | Tags | Lyrics | What for |
|---|---|---|---|
| `Lyric Artist - Flac Lrc.flac` | Lyric Artist · Flac Lrc · *Made-up Words* | LRC text in the Vorbis `LYRICS` comment, a pause line at the end, Turkish letters | LRC in a tag is synced; UTF-8 through a tag |
| `Lyric Artist - Mp3 Uslt.mp3` | Lyric Artist · Mp3 Uslt · *Made-up Words* | an ID3v2.3 `USLT` frame, UTF-16, two verses | unsynced words from a tag |
| `Lyric Artist - Mp3 Sylt.mp3` | Lyric Artist · Mp3 Sylt · *Made-up Words* | an ID3v2.3 `SYLT` frame, stamps in milliseconds | the binary synced frame symphonia hands over raw |
| `Lyric Artist - Sidecar.flac` | Lyric Artist · Sidecar · *Made-up Words* | unsynced words in `UNSYNCEDLYRICS` | a synced `.lrc` next to it wins over its unsynced tag |
| `Lyric Artist - Sidecar.lrc` | — | four LRC lines, two of them metadata | the `.lrc` next to a file with its name |

They are written by `make.py` — the standard library only, no ffmpeg: ffmpeg
cannot write a `SYLT` frame, and a hand-written tag is easier to check than a
tool's. The same inputs give the same bytes:

```sh
python3 fixtures/lyrics/make.py
```

`line_at_cases.json` is not an audio fixture: it is the accuracy set that
binds the core's `Lyrics::line_at` and the interface's `lyrics.js` (D-078),
as `fixtures/anchor/position_cases.json` binds the position (D-033).
