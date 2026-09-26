// The test plugin: the smallest example that implements the plugin contract
// (api 4) **in full**. It never goes online and answers from a fixed catalog.
//
// There is no misbehaving-plugin act here — the timeout, throwing, unpermitted
// network and missing export tests write their own small scripts. This file
// stays clean as the working form of the skeleton in `docs/writing-plugins.md`.

const CATALOG = [
  {
    id: "track-1",
    artist: "Ezhel",
    title: "Geceler",
    album: "Müptezhel",
    duration_ms: 213000,
    isrc: "TR1234567890",
  },
  {
    id: "track-2",
    artist: "Sezen Aksu",
    title: "Gülümse",
    album: "Gülümse",
    duration_ms: 254000,
    // Malformed on purpose: the core must drop and count it, not accept it.
    isrc: "bogus",
  },
];

// Search must treat "ezhel" and "EZHEL" as the same. QuickJS has no `Intl`;
// locale-aware folding, if it were needed, would have to be written by hand.
const fold = (text) => text.toLowerCase();

export function health() {
  return {
    reachable: true,
    track_count: CATALOG.length,
    // The secret's presence is reported, not its **value**.
    detail: host.secrets.get("token") === null ? "no secret" : "has secret",
  };
}

export function search(query, limit) {
  const needle = fold(String(query));
  return CATALOG.filter(
    (track) => fold(track.artist).includes(needle) || fold(track.title).includes(needle),
  ).slice(0, limit);
}

export function resolve_source(id) {
  if (!CATALOG.some((track) => track.id === id)) {
    // "None" is an answer, not an error.
    return null;
  }
  return {
    kind: "http_stream",
    url: `https://example.invalid/${id}.mp3`,
    headers: [{ name: "authorization", value: host.secrets.get("token") ?? "" }],
  };
}

// api 3: the manifest says `"artwork": true`, so this export is required. A
// real plugin fetches the image with `host.http.request({ url, binary: true })`
// and returns the base64 body; this one never goes online, so its cover is a
// 4×4 amber PNG written into the file. `null` means "no cover" — the core then
// asks MusicBrainz and the Cover Art Archive.
const COVER = "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAIAAAAmkwkpAAAAEUlEQVR42mP4vyUEjhiI4wAAVnEgcX64o8EAAAAASUVORK5CYII=";

export function artwork(id, size) {
  if (id !== "track-1") {
    return null;
  }
  return { mime: "image/png", data: COVER };
}

// api 4: the manifest says `"lyrics": true`, so this export is required. The
// service's own lyrics for the track: `synced` as LRC text (the core parses
// it), `plain` for words without timing, `attribution` when the service
// credits someone. `null` means "none" — the core then asks LRCLIB. The
// words are made up.
export function lyrics(id) {
  if (id !== "track-1") {
    return null;
  }
  return {
    synced: "[00:01.00]a made-up first line\n[00:04.50]and a second one",
    attribution: "Source: the echo test plugin",
  };
}
