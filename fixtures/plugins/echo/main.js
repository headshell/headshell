// The test plugin: the smallest example that implements the plugin contract
// (api 7) **in full**. It never goes online and answers from a fixed catalog.
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
    // api 6: the genres the service names, as it writes them.
    genres: ["Hip-Hop", "Turkish Rap"],
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

// Every word of the query must be in the artist or the title: "ezhel" finds
// the artist, "ezhel geceler" the song — the app searches videos that way.
export function search(query, limit) {
  const words = fold(String(query)).split(/\s+/).filter((word) => word !== "");
  return CATALOG.filter((track) => {
    const text = `${fold(track.artist)} ${fold(track.title)}`;
    return words.length > 0 && words.every((word) => text.includes(word));
  }).slice(0, limit);
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

// api 5: the manifest says `"video": true`, so this export is required. The
// item's video, as a source the app's own player shows: a YouTube video id
// (the app embeds YouTube's player and never downloads anything). `null`
// means "this item has no video". The id is made up — it has the shape, not
// a video behind it.
export function resolve_video(id) {
  if (id !== "track-1") {
    return null;
  }
  return { kind: "youtube", video_id: "echoTrack01" };
}

// api 6: the manifest says `"related": true`, so this export is required.
// The service's own songs like the item — its radio, its "up next" — for the
// recommendations. Only the item's id comes in: a plugin never sees the
// listener's history. An empty list means "none like it".
export function related(id, limit) {
  if (!CATALOG.some((track) => track.id === id)) {
    return [];
  }
  return CATALOG.filter((track) => track.id !== id).slice(0, limit);
}

// api 7: the manifest's `links` names templates, so this export is required.
// The core hands over what stood in `{id}` and the template the link fitted;
// the answer is an array of tracks — the song at the link. An empty array
// means "nothing there". A list's link (`/list?l=…`) gives every song in it;
// the core plays only a song's link for now, and says so.
export function link(id, template) {
  if (template.includes("/list?")) {
    return id === "all" ? CATALOG : [];
  }
  return CATALOG.filter((track) => track.id === id);
}
