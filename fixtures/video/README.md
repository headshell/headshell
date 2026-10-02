# `fixtures/video/`

Synthetic video files for D-086 (music videos): a test pattern and a sine
tone, two seconds each, no tags. The same rule as `fixtures/audio/`: no real
recording, and every file can be made again with the commands below.

| File | What for |
|---|---|
| `Video Artist - Clip.mp4` | a video with a sound the core can decode (H.264 + AAC): it plays as music and shows as a video |
| `Webm Artist - Opus Clip.webm` | a video whose sound the core **cannot** decode (VP9 + Opus — symphonia has no Opus): it is a video only |
| `Audio Artist - Plain.mp4` | an `.mp4` with no picture (AAC alone): the extension says nothing, the tracks inside do — it has no video |

```sh
ffmpeg -y -f lavfi -i "testsrc=duration=2:size=64x36:rate=10" -f lavfi -i "sine=frequency=440:duration=2" \
  -c:v libx264 -pix_fmt yuv420p -c:a aac -b:a 32k -shortest -map_metadata -1 "Video Artist - Clip.mp4"
ffmpeg -y -f lavfi -i "testsrc=duration=2:size=64x36:rate=10" -f lavfi -i "sine=frequency=330:duration=2" \
  -c:v libvpx-vp9 -b:v 50k -deadline realtime -c:a libopus -b:a 24k -shortest -map_metadata -1 "Webm Artist - Opus Clip.webm"
ffmpeg -y -f lavfi -i "sine=frequency=550:duration=2" -c:a aac -b:a 32k -map_metadata -1 "Audio Artist - Plain.mp4"
```
