// The line being sung — the JS copy of
// `headshell_core::lyrics::Lyrics::line_at` (D-078).
//
// A second copy on purpose, like `anchor.js`: the interface lights the line
// being sung every frame from the position it estimates from the anchor, and
// cannot ask the core sixty times a second. Two copies drift where nobody
// looks — a line lit a frame early on one side only — so
// `fixtures/lyrics/line_at_cases.json` binds them:
// `headshell-core/tests/lyrics_parity.rs` the Rust side,
// `headshell/tests/lyrics_parity_js.rs` this one, in embedded QuickJS.
//
// The file's top level is pure, like `anchor.js` and `motion.js`'s
// arithmetic: nothing here touches the DOM.

/// The index of the line sung at `positionMs`, the lines shifted by
/// `offsetMs` (positive: they come sooner — the LRC convention): the last one
/// that has started. `null` before the first one, and for unsynced lyrics.
///
/// `lyrics` as the core sends it: `{ synced, lines: [{ start_ms, text }] }`,
/// the lines in time order.
export function lineAt(lyrics, positionMs, offsetMs) {
  if (!lyrics || !lyrics.synced) return null;
  const lines = lyrics.lines;
  // The number of lines that have started; a line without a start never has.
  let low = 0;
  let high = lines.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    const start = lines[middle].start_ms;
    const started = start !== undefined && start !== null && start - offsetMs <= positionMs;
    if (started) low = middle + 1;
    else high = middle;
  }
  return low === 0 ? null : low - 1;
}

/// Where the track is when line `index` starts (D-090) — where a click on
/// the line moves to: its start with the shift taken off, as `lineAt` takes
/// it, and never before the track's start. `null` for unsynced lyrics and a
/// line without a start.
export function lineStart(lyrics, index, offsetMs) {
  if (!lyrics || !lyrics.synced) return null;
  const start = lyrics.lines[index]?.start_ms;
  if (start === undefined || start === null) return null;
  return Math.max(0, start - offsetMs);
}

/// Where the list scrolls to put a line at the reading position: a third of
/// the way down, the way a reader's eye rests — clamped to what the list can
/// scroll.
export function readingTop(lineTop, lineHeight, viewportHeight, contentHeight) {
  const wanted = lineTop + lineHeight / 2 - viewportHeight * 0.36;
  return Math.max(0, Math.min(wanted, Math.max(0, contentHeight - viewportHeight)));
}
