#!/usr/bin/env python3
"""Writes the lyrics fixtures (D-078) from the audio fixtures in ../audio.

The standard library only, and no ffmpeg: ffmpeg cannot write an ID3 `SYLT`
frame, and a hand-written tag is easier to read than a tool's. Run it from
anywhere; it writes next to itself, and the same inputs give the same bytes.

    python3 fixtures/lyrics/make.py

None of the words is a real song's: every line is made up (see README.md).
"""

import struct
from pathlib import Path

HERE = Path(__file__).resolve().parent
AUDIO = HERE.parent / "audio"

# --- FLAC: replace the Vorbis comment block ------------------------------------


def vorbis_comment(vendor: bytes, comments: list[str]) -> bytes:
    body = struct.pack("<I", len(vendor)) + vendor + struct.pack("<I", len(comments))
    for comment in comments:
        raw = comment.encode("utf-8")
        body += struct.pack("<I", len(raw)) + raw
    return body


def retag_flac(source: Path, target: Path, comments: list[str]) -> None:
    data = source.read_bytes()
    assert data[:4] == b"fLaC", source
    blocks = []
    pos = 4
    while True:
        header = data[pos]
        kind = header & 0x7F
        length = int.from_bytes(data[pos + 1 : pos + 4], "big")
        body = data[pos + 4 : pos + 4 + length]
        pos += 4 + length
        if kind == 4:
            body = vorbis_comment(b"headshell fixtures", comments)
        blocks.append((kind, body))
        if header & 0x80:
            break
    out = bytearray(b"fLaC")
    for index, (kind, body) in enumerate(blocks):
        last = 0x80 if index == len(blocks) - 1 else 0
        out += bytes([last | kind]) + len(body).to_bytes(3, "big") + body
    out += data[pos:]
    target.write_bytes(bytes(out))


# --- MP3: a hand-written ID3v2.3 tag --------------------------------------------


def synchsafe(n: int) -> bytes:
    return bytes([(n >> 21) & 0x7F, (n >> 14) & 0x7F, (n >> 7) & 0x7F, n & 0x7F])


def frame(frame_id: str, body: bytes) -> bytes:
    # ID3v2.3: the frame size is a plain big-endian integer, not synchsafe.
    return frame_id.encode("ascii") + struct.pack(">I", len(body)) + b"\x00\x00" + body


def text_frame(frame_id: str, text: str) -> bytes:
    # Encoding 1: UTF-16 with a byte order mark, the only Unicode one in v2.3.
    return frame(frame_id, b"\x01" + b"\xff\xfe" + text.encode("utf-16-le"))


def uslt(text: str) -> bytes:
    body = b"\x01" + b"eng" + b"\xff\xfe\x00\x00" + b"\xff\xfe" + text.encode("utf-16-le")
    return frame("USLT", body)


def sylt(pieces: list[tuple[str, int]]) -> bytes:
    # Encoding 0 (ISO-8859-1), format 2 (milliseconds), content type 1 (lyrics),
    # an empty descriptor.
    body = b"\x00" + b"eng" + b"\x02" + b"\x01" + b"\x00"
    for text, ms in pieces:
        body += text.encode("latin-1") + b"\x00" + struct.pack(">I", ms)
    return frame("SYLT", body)


def audio_without_id3(source: Path) -> bytes:
    data = source.read_bytes()
    if data[:3] == b"ID3":
        size = sum(b << (7 * (3 - i)) for i, b in enumerate(data[6:10]))
        footer = 10 if data[5] & 0x10 else 0
        data = data[10 + size + footer :]
    if data[-128:-125] == b"TAG":
        data = data[:-128]
    return data


def retag_mp3(source: Path, target: Path, frames: list[bytes]) -> None:
    tag_body = b"".join(frames)
    header = b"ID3" + b"\x03\x00" + b"\x00" + synchsafe(len(tag_body))
    target.write_bytes(header + tag_body + audio_without_id3(source))


# --- the fixtures ---------------------------------------------------------------

FLAC = AUDIO / "tagged.flac"
MP3 = AUDIO / "Test Artist - Mp3 Track.mp3"


def tags(title: str) -> list[str]:
    return ["ARTIST=Lyric Artist", f"TITLE={title}", "ALBUM=Made-up Words"]


retag_flac(
    FLAC,
    HERE / "Lyric Artist - Flac Lrc.flac",
    tags("Flac Lrc")
    + [
        "LYRICS=[ar:Lyric Artist]\n[ti:Flac Lrc]\n"
        "[00:00.50]A made-up first line\n"
        "[00:01.20]a second one with çğıİöşü\n"
        "[00:01.80]\n"
    ],
)

retag_flac(
    FLAC,
    HERE / "Lyric Artist - Sidecar.flac",
    tags("Sidecar") + ["UNSYNCEDLYRICS=unsynced words in the tags"],
)
(HERE / "Lyric Artist - Sidecar.lrc").write_text(
    "[ar:Lyric Artist]\n[ti:Sidecar]\n"
    "[00:00.40]The sidecar's made-up first line\n"
    "[00:01.40]and its second\n",
    encoding="utf-8",
)

mp3_tags = [
    text_frame("TPE1", "Lyric Artist"),
    text_frame("TALB", "Made-up Words"),
]
retag_mp3(
    MP3,
    HERE / "Lyric Artist - Mp3 Uslt.mp3",
    mp3_tags
    + [
        text_frame("TIT2", "Mp3 Uslt"),
        uslt("A made-up verse\nthat is not synced\n\nand a second verse"),
    ],
)
retag_mp3(
    MP3,
    HERE / "Lyric Artist - Mp3 Sylt.mp3",
    mp3_tags
    + [
        text_frame("TIT2", "Mp3 Sylt"),
        sylt([("A made-up line", 500), ("sung a little later", 1200)]),
    ],
)
print("written:", *sorted(p.name for p in HERE.iterdir() if p.suffix != ".py"), sep="\n  ")
