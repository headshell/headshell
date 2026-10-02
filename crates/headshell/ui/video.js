// headshell — the video layer (D-086).
//
// One fixed layer over the page holds the video player: YouTube's own player
// (the IFrame embed) or a `<video>` for a file. It is never moved in the
// document — moving an iframe reloads it — but laid over a **slot**: the
// closed lid's screen, the small window on the turntable, the corner. Each
// frame it is given the slot's place on screen, so it rides the sheet's
// spring and the window's resizing like the slot does.
//
// YouTube's player is spoken to with `postMessage` alone. The IFrame API's
// own script is **not** loaded: every script in this page can reach the app
// (`withGlobalTauri`), and a script from another origin must never be one of
// them. The messages are the ones that script sends: `listening`, then
// `command` — and the player answers with `infoDelivery` and its events.
//
// YouTube's rules for the player are kept here and in the slots: it is shown
// while it plays (never hidden, never playing behind another view), nothing
// is laid over it, and it is never smaller than 200×200.
//
// The file's top level is pure: the DOM is touched only inside the functions
// called — the same rule as `motion.js`, so the contract tests can parse it.

/// Where YouTube's player comes from: the no-cookie domain — it sets no
/// cookie until the video plays.
export const YOUTUBE_ORIGIN = "https://www.youtube-nocookie.com";

/// The player's states (the IFrame API's numbers) as the core's words.
const YOUTUBE_STATES = { "-1": "buffering", 0: "stopped", 1: "playing", 2: "paused", 3: "buffering", 5: "paused" };

/// The player's refusals: the video does not exist, or its owner keeps it off
/// other sites (100, 101, 150), or it cannot be played here (2, 5).
const YOUTUBE_REFUSALS = new Set([2, 5, 100, 101, 150, 153]);

/// How often a playing video tells the core where it is (ms).
const REPORT_EVERY_MS = 1000;

/// YouTube's player for a video, starting at `startMs`: its own controls off
/// (the app's are used), no keyboard of its own, no full screen, no notes over
/// the picture, related videos from the same channel only.
export function embedUrl(videoId, startMs, origin) {
  // Joined by hand, not with `URLSearchParams`: the contract tests evaluate
  // this file in QuickJS, which has no URL API.
  const params = [
    ["enablejsapi", "1"],
    ["autoplay", "1"],
    ["controls", "0"],
    ["disablekb", "1"],
    ["fs", "0"],
    ["iv_load_policy", "3"],
    ["rel", "0"],
    ["playsinline", "1"],
    ["start", String(Math.max(0, Math.floor(startMs / 1000)))],
    ["origin", origin],
  ]
    .map(([key, value]) => `${key}=${encodeURIComponent(value)}`)
    .join("&");
  return `${YOUTUBE_ORIGIN}/embed/${encodeURIComponent(videoId)}?${params}`;
}

/// The player's state number as the core's word; `null` for none.
export function youtubeState(code) {
  return YOUTUBE_STATES[String(code)] ?? null;
}

/// Is this player error a refusal of the video — so the next match is worth
/// trying?
export function youtubeRefuses(code) {
  return YOUTUBE_REFUSALS.has(Number(code));
}

/// The layer. `handlers`: `report(state, positionMs, durationMs)` — at every
/// change and every second while playing; `ended()`; `refused(code)`.
export function createVideoLayer({ layer, frame, file }, handlers) {
  let kind = null;
  let slot = null;
  let following = 0;
  let placed = "";
  let ready = false;
  let listening = 0;
  let state = "buffering";
  let positionMs = 0;
  let durationMs = null;
  let reportedAt = 0;
  let reportedState = "";

  // — following the slot

  function follow() {
    following = 0;
    if (!slot) return;
    const rect = slot.getBoundingClientRect();
    const key = `${rect.left}|${rect.top}|${rect.width}|${rect.height}`;
    if (key !== placed) {
      placed = key;
      layer.style.width = `${rect.width}px`;
      layer.style.height = `${rect.height}px`;
      layer.style.transform = `translate3d(${rect.left}px, ${rect.top}px, 0)`;
    }
    following = requestAnimationFrame(follow);
  }

  function attach(el) {
    slot = el;
    layer.hidden = !el;
    placed = "";
    if (el && !following) following = requestAnimationFrame(follow);
    if (el) follow();
  }

  // — telling the app

  function report(force = false) {
    const now = performance.now();
    if (!force && state === reportedState && (state !== "playing" || now - reportedAt < REPORT_EVERY_MS)) {
      return;
    }
    reportedAt = now;
    reportedState = state;
    handlers.report(state, Math.max(0, Math.round(positionMs)), durationMs);
  }

  function setState(next) {
    if (!next || next === state) return;
    state = next;
    report(true);
  }

  // — YouTube

  function send(message) {
    frame.contentWindow?.postMessage(JSON.stringify({ ...message, id: 1, channel: "widget" }), YOUTUBE_ORIGIN);
  }

  function command(func, args = []) {
    send({ event: "command", func, args });
  }

  function onMessage(event) {
    if (kind !== "youtube" || event.origin !== YOUTUBE_ORIGIN || event.source !== frame.contentWindow) return;
    let data;
    try {
      data = JSON.parse(event.data);
    } catch {
      return;
    }
    if (data.event === "onReady") {
      ready = true;
      clearInterval(listening);
      // `autoplay` is asked for in the address; this asks again, for a
      // player that waited for its first frame.
      command("playVideo");
    } else if (data.event === "infoDelivery" && data.info) {
      const info = data.info;
      if (Number.isFinite(info.currentTime)) positionMs = info.currentTime * 1000;
      if (Number.isFinite(info.duration) && info.duration > 0) durationMs = Math.round(info.duration * 1000);
      if (info.playerState !== undefined) {
        if (Number(info.playerState) === 0) ended();
        else setState(youtubeState(info.playerState));
      }
      report();
    } else if (data.event === "onStateChange") {
      if (Number(data.info) === 0) ended();
      else setState(youtubeState(data.info));
    } else if (data.event === "onError") {
      if (youtubeRefuses(data.info)) handlers.refused(Number(data.info));
    }
  }

  function startListening() {
    ready = false;
    clearInterval(listening);
    // The player only talks to a page that said it listens; it is said
    // until the player answers.
    listening = setInterval(() => {
      if (ready) clearInterval(listening);
      else send({ event: "listening" });
    }, 250);
  }

  // — a file

  function bindFile() {
    file.addEventListener("playing", () => setState("playing"));
    file.addEventListener("pause", () => {
      if (!file.ended) setState("paused");
    });
    file.addEventListener("waiting", () => setState("buffering"));
    file.addEventListener("ended", ended);
    file.addEventListener("timeupdate", () => {
      positionMs = file.currentTime * 1000;
      report();
    });
    file.addEventListener("durationchange", () => {
      if (Number.isFinite(file.duration)) durationMs = Math.round(file.duration * 1000);
    });
    file.addEventListener("error", () => handlers.refused(file.error?.code ?? 0));
  }

  let endedOnce = false;
  function ended() {
    if (endedOnce) return;
    endedOnce = true;
    state = "stopped";
    handlers.ended();
  }

  bindFile();
  frame.addEventListener("load", () => {
    if (kind === "youtube") startListening();
  });
  window.addEventListener("message", onMessage);

  return {
    /// Shows a video: `{ kind: "youtube", videoId, startMs }` or `{ kind:
    /// "file", url, startMs }`.
    load(video) {
      endedOnce = false;
      positionMs = video.startMs ?? 0;
      durationMs = null;
      state = "buffering";
      reportedState = "";
      if (video.kind === "youtube") {
        file.pause();
        file.removeAttribute("src");
        file.load();
        file.hidden = true;
        frame.hidden = false;
        kind = "youtube";
        frame.src = embedUrl(video.videoId, positionMs, location.origin);
      } else {
        frame.hidden = true;
        frame.removeAttribute("src");
        kind = "file";
        file.hidden = false;
        file.src = video.url;
        file.currentTime = positionMs / 1000;
        file.play().catch(() => setState("paused"));
      }
    },
    play() {
      if (kind === "youtube") command("playVideo");
      else if (kind === "file") file.play().catch(() => setState("paused"));
    },
    pause() {
      if (kind === "youtube") command("pauseVideo");
      else if (kind === "file") file.pause();
    },
    toggle() {
      if (state === "playing" || state === "buffering") this.pause();
      else this.play();
    },
    /// Stops and empties the player: nothing plays where it is not shown.
    clear() {
      clearInterval(listening);
      file.pause();
      file.removeAttribute("src");
      file.load();
      frame.removeAttribute("src");
      kind = null;
      attach(null);
    },
    attach,
    slot: () => slot,
    loaded: () => kind !== null,
    state: () => state,
    positionMs: () => Math.max(0, Math.round(positionMs)),
  };
}
