// headshell — the motion layer (D-072).
//
// Every positional motion in the interface goes through here: springs,
// momentum projection, the rubber band, velocity measurement. The web
// counterpart of Apple's "Designing Fluid Interfaces" (WWDC 2018) talk; no
// dependencies (no bundler, no npm).
//
// Three rules:
//
// 1. **Only `transform` and `opacity`** (D-028). Animating any other
//    property drops the frame rate from 58.8 to 47.6 in WebKitGTK.
// 2. **Every motion can be interrupted.** A new target starts from the
//    element's *on-screen* value and velocity; the old motion is not waited
//    for, and the velocity is not reset. An element caught halfway does not
//    jump, and a reversed motion does not hit a wall.
// 3. **The duration belongs to the theme.** The springs' response comes from
//    `--headshell-duration` (response = duration × 3; the default 120ms →
//    0.36 s). `0ms` means no motion at all — the High Contrast theme uses
//    that. `prefers-reduced-motion` turns off positional motion; the opacity
//    transition stays.
//
// The file's top level is pure: the DOM is only touched inside the functions
// that get called. That is why `tests/motion_js.rs` can evaluate it in
// embedded QuickJS — the same route as `anchor.js` (D-070).

/// The ratio of a spring's response to the theme duration. 120ms → 0.36 s:
/// inside the 0.3–0.4 s range Apple gives for moving and repositioning.
export const RESPONSE_PER_DURATION = 3;

/// The duration used if the token cannot be read — the same as the default in
/// `style.css`.
export const FALLBACK_DURATION_MS = 120;

// ————————————————————————————————————— pure arithmetic

/// `"120ms"`, `"0.2s"`, `"0"` → milliseconds. `null` if unreadable.
///
/// Unitless is only valid for `0`; CSS counts it the same way.
export function parseDuration(text) {
  const value = String(text ?? "").trim();
  const match = /^(\d*\.?\d+)(ms|s)?$/.exec(value);
  if (!match) return null;
  const number = Number(match[1]);
  if (!Number.isFinite(number)) return null;
  if (!match[2]) return number === 0 ? 0 : null;
  return match[2] === "s" ? number * 1000 : number;
}

/// The theme duration + the accessibility preference → what gets animated.
///
/// `enabled` is for everything including opacity, `spatial` for position and
/// scale. An unreadable duration falls back to the default: a theme author's
/// typo must not leave the interface motionless, but no guess is made to turn
/// motion on either.
export function motionSettings(durationMs, reducedMotion) {
  const ms = durationMs ?? FALLBACK_DURATION_MS;
  const enabled = ms > 0;
  return {
    enabled,
    spatial: enabled && !reducedMotion,
    response: (ms * RESPONSE_PER_DURATION) / 1000,
  };
}

/// The state of a spring `t` seconds later: `[position offset, velocity]`.
///
/// Apple's two parameters: **the damping ratio** (1 = settles without
/// overshooting, below 1 goes past the target and oscillates) and **the
/// response** (s). The response is not a duration — a spring has no fixed
/// duration; the settling time comes from the two.
///
/// `offset` is the position relative to the target (position − target),
/// `velocity` in units/s. A closed-form solution: even if frames are skipped,
/// however big the step, it does not drift.
export function springStep(offset, velocity, t, dampingRatio, response) {
  const omega = (2 * Math.PI) / response;
  const zeta = dampingRatio;
  if (zeta < 1) {
    const damped = omega * Math.sqrt(1 - zeta * zeta);
    const decay = Math.exp(-zeta * omega * t);
    const a = offset;
    const b = (velocity + zeta * omega * offset) / damped;
    const cos = Math.cos(damped * t);
    const sin = Math.sin(damped * t);
    return [
      decay * (a * cos + b * sin),
      decay * ((b * damped - zeta * omega * a) * cos - (a * damped + zeta * omega * b) * sin),
    ];
  }
  if (zeta === 1) {
    const decay = Math.exp(-omega * t);
    const b = velocity + omega * offset;
    return [decay * (offset + b * t), decay * (b - omega * (offset + b * t))];
  }
  // Overdamped: two real roots, no oscillation.
  const root = Math.sqrt(zeta * zeta - 1);
  const r1 = -omega * (zeta - root);
  const r2 = -omega * (zeta + root);
  const c2 = (velocity - r1 * offset) / (r2 - r1);
  const c1 = offset - c2;
  const e1 = Math.exp(r1 * t);
  const e2 = Math.exp(r2 * t);
  return [c1 * e1 + c2 * e2, c1 * r1 * e1 + c2 * r2 * e2];
}

/// Where a released motion will stop (px). The exponential deceleration in
/// Apple's sample code; not the physics textbook's `v²/2a`. `0.998` is the
/// scrolling feel, `0.99` shorter.
///
/// The target is chosen **by this**, not by the release point: a small flick
/// has a big effect.
export function project(velocity, decelerationRate = 0.998) {
  return ((velocity / 1000) * decelerationRate) / (1 - decelerationRate);
}

/// How much of the distance pulled past the boundary is followed.
///
/// A hard stop reads as "frozen"; growing resistance as "I hear you, but
/// there is nothing more here". However far it is pulled the result does not
/// exceed `dimension`, and the sign is kept.
export function rubberband(overshoot, dimension, constant = 0.55) {
  if (!(dimension > 0)) return 0;
  return (overshoot * dimension * constant) / (dimension + constant * Math.abs(overshoot));
}

/// The velocity (units/s) from the samples within the last `windowMs`.
///
/// Looking only at the last two samples gives a jittery velocity; a long
/// window thinks a motion released after the finger stopped is still fast.
export function createVelocityTracker(windowMs = 100) {
  const samples = [];
  return {
    add(time, value) {
      samples.push([time, value]);
      while (samples.length > 2 && time - samples[0][0] > windowMs) samples.shift();
    },
    velocity() {
      if (samples.length < 2) return 0;
      const [t0, v0] = samples[0];
      const [t1, v1] = samples[samples.length - 1];
      return t1 > t0 ? ((v1 - v0) / (t1 - t0)) * 1000 : 0;
    },
  };
}

// ————————————————————————————————————— theme and preference

let settings = null;

/// The current motion settings. [`invalidateMotion`] is called when the theme
/// changes.
export function currentMotion() {
  if (!settings) {
    const raw = getComputedStyle(document.documentElement).getPropertyValue(
      "--headshell-duration",
    );
    const reduced = globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    settings = motionSettings(parseDuration(raw), reduced);
  }
  return settings;
}

/// The theme or the system preference changed: the next motion should read
/// them again.
export function invalidateMotion() {
  settings = null;
}

/// Watches the system's motion preference. Called once at startup.
export function watchMotionPreference() {
  globalThis
    .matchMedia?.("(prefers-reduced-motion: reduce)")
    .addEventListener?.("change", invalidateMotion);
}

// ————————————————————————————————————— element springs
//
// Each element's on-screen values are kept here (`x`, `y`, `rotate` and
// `rotateX` in degrees, `scaleX`, `scaleY`, `opacity`), and a single
// `requestAnimationFrame` loop advances all the running springs. A new target
// **takes over** the spring on the same property (rule 2).

const RESTING = { x: 0, y: 0, rotate: 0, rotateX: 0, scaleX: 1, scaleY: 1, opacity: 1 };
const PRECISION = {
  x: 0.1,
  y: 0.1,
  rotate: 0.02,
  rotateX: 0.02,
  scaleX: 0.0005,
  scaleY: 0.0005,
  opacity: 0.002,
};

/// The depth a tilt (`rotateX`) is seen from, in px: a lid swinging shut
/// (D-086) foreshortens as a real one does from a step away. It is part of
/// the transform, not the parent's `perspective`, so an element carries its
/// own depth wherever it sits.
const PERSPECTIVE_PX = 1100;

const bodies = new WeakMap();
const active = new Set();
let frame = 0;
let lastTime = 0;

function bodyOf(el) {
  let body = bodies.get(el);
  if (!body) {
    body = {
      el,
      values: { ...RESTING },
      velocities: { x: 0, y: 0, rotate: 0, rotateX: 0, scaleX: 0, scaleY: 0, opacity: 0 },
      springs: new Map(),
    };
    bodies.set(el, body);
  }
  return body;
}

/// `scale` is shorthand for writing both axes at once.
function expand(values) {
  const out = {};
  for (const [prop, value] of Object.entries(values)) {
    if (prop === "scale") {
      out.scaleX = value;
      out.scaleY = value;
    } else {
      out[prop] = value;
    }
  }
  return out;
}

function write(body) {
  const { x, y, rotate, rotateX, scaleX, scaleY, opacity } = body.values;
  // An element at rest carries no transform: a permanent layer blurs text in
  // some engines. That is why an element driven by springs **must not have a
  // `transform` of its own in the stylesheet** — an element settling on the
  // identity transform drops its inline value and falls back to the
  // stylesheet's (a bar reaching full width went back to `scaleX(0)` and
  // disappeared). The starting state is given with `from`.
  const still =
    x === 0 && y === 0 && rotate === 0 && rotateX === 0 && scaleX === 1 && scaleY === 1;
  const tilt = rotateX === 0 ? "" : `perspective(${PERSPECTIVE_PX}px) rotateX(${rotateX}deg) `;
  body.el.style.transform = still
    ? ""
    : `${tilt}translate3d(${x}px, ${y}px, 0) rotate(${rotate}deg) scale(${scaleX}, ${scaleY})`;
  body.el.style.opacity = opacity === 1 ? "" : String(Math.min(1, Math.max(0, opacity)));
}

function cancel(body, prop) {
  const spring = body.springs.get(prop);
  if (spring) {
    // The `onRest` of a spring left halfway is not called: the motion did not
    // end, it was interrupted.
    active.delete(spring);
    body.springs.delete(prop);
  }
}

/// Writes the on-screen value at once; the spring on that property stops.
export function place(el, values) {
  const body = bodyOf(el);
  for (const [prop, value] of Object.entries(expand(values))) {
    cancel(body, prop);
    body.values[prop] = value;
    body.velocities[prop] = 0;
  }
  write(body);
}

/// The property's current value on screen.
export function presentation(el, prop) {
  return bodyOf(el).values[prop];
}

/// Moves the on-screen value by `delta`, and nothing else: a running spring
/// keeps its target and its velocity. For when the ground under an element
/// moves — the lyrics' scroll position jumps and a transform takes the jump
/// back (D-078) — and a motion under way must carry on from where it is seen,
/// not restart from rest.
export function rebase(el, prop, delta) {
  const body = bodyOf(el);
  body.values[prop] += delta;
  write(body);
}

/// Takes properties to their target on a spring.
///
/// Options: `from` (the start — the on-screen value if not given), `velocity`
/// (units/s; a released motion's velocity is handed over here),
/// `dampingRatio` (default 1: no overshoot — below 1 only after a motion that
/// carries momentum), `responseScale`, `onRest` (when all have settled; not
/// called if the motion is interrupted).
export function animate(el, targets, options = {}) {
  const motion = currentMotion();
  const body = bodyOf(el);
  const { dampingRatio = 1, onRest } = options;
  const response = motion.response * (options.responseScale ?? 1);

  for (const [prop, value] of Object.entries(expand(options.from ?? {}))) {
    cancel(body, prop);
    body.values[prop] = value;
    body.velocities[prop] = 0;
  }
  const velocity = expand(options.velocity ?? {});

  let pending = 0;
  const settled = () => {
    pending -= 1;
    if (pending === 0) onRest?.();
  };

  for (const [prop, target] of Object.entries(expand(targets))) {
    const moves = prop === "opacity" ? motion.enabled : motion.spatial;
    if (!moves || !(response > 0)) {
      cancel(body, prop);
      body.values[prop] = target;
      body.velocities[prop] = 0;
      continue;
    }
    const previous = body.springs.get(prop);
    if (previous) active.delete(previous);
    if (velocity[prop] !== undefined) body.velocities[prop] = velocity[prop];
    const spring = { body, prop, target, dampingRatio, response, onRest: settled };
    body.springs.set(prop, spring);
    active.add(spring);
    pending += 1;
  }

  write(body);
  if (pending === 0) {
    onRest?.();
  } else if (!frame) {
    lastTime = performance.now();
    frame = requestAnimationFrame(step);
  }
}

function step(now) {
  frame = 0;
  // Time piled up while the tab was in the background must not be spent in a
  // single frame: the element should slow down, not teleport.
  const dt = Math.min(Math.max((now - lastTime) / 1000, 0), 1 / 24);
  lastTime = now;

  const touched = new Set();
  const finished = [];
  for (const spring of active) {
    const { body, prop, target } = spring;
    const [offset, velocity] = springStep(
      body.values[prop] - target,
      body.velocities[prop],
      dt,
      spring.dampingRatio,
      spring.response,
    );
    const precision = PRECISION[prop];
    if (Math.abs(offset) < precision && Math.abs(velocity) < precision * 10) {
      body.values[prop] = target;
      body.velocities[prop] = 0;
      finished.push(spring);
    } else {
      body.values[prop] = target + offset;
      body.velocities[prop] = velocity;
    }
    touched.add(body);
  }
  for (const body of touched) write(body);
  for (const spring of finished) {
    active.delete(spring);
    if (spring.body.springs.get(spring.prop) === spring) spring.body.springs.delete(spring.prop);
    spring.onRest();
  }
  if (active.size > 0) frame = requestAnimationFrame(step);
}

/// Measures before and after a layout change and closes the difference on a
/// spring (FLIP). When a sibling goes, the others do not jump; they slide
/// into place.
export function flip(elements, mutate) {
  const before = new Map(elements.map((el) => [el, el.getBoundingClientRect().top]));
  mutate();
  for (const [el, top] of before) {
    if (!el.isConnected) continue;
    const delta = top - el.getBoundingClientRect().top;
    if (Math.abs(delta) < 0.5) continue;
    // Added on top of a running motion: the on-screen position is kept.
    const body = bodyOf(el);
    body.values.y += delta;
    write(body);
    animate(el, { y: 0 });
  }
}

// ————————————————————————————————————— dragging

/// Horizontal dragging: follows 1:1, keeping the point that was grabbed, and
/// hands over the velocity on release.
///
/// Dragging does not start before `threshold` px are passed — so clicks and
/// text selection are not in the way. If vertical movement wins first,
/// dragging never starts and the gesture is left to the page. A press that
/// starts on an element matching the `ignore` selector (a button, a folding
/// detail) does not drag.
///
/// `onStart()` returns the element's on-screen `x`: an element caught in the
/// middle of a motion carries on from where it is.
export function horizontalDrag(el, options) {
  axisDrag(el, "x", options);
}

/// The same, on the vertical axis: `onStart()` returns the on-screen `y`, and
/// a gesture that goes sideways first is left to the page.
export function verticalDrag(el, options) {
  axisDrag(el, "y", options);
}

function axisDrag(el, axis, { threshold = 8, ignore, onStart, onMove, onEnd }) {
  const along = axis === "x" ? (event) => event.clientX : (event) => event.clientY;
  const across = axis === "x" ? (event) => event.clientY : (event) => event.clientX;
  let pointer = null;

  el.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    if (ignore && event.target.closest(ignore)) return;
    pointer = {
      id: event.pointerId,
      start: along(event),
      startAcross: across(event),
      origin: 0,
      dragging: false,
      tracker: createVelocityTracker(),
    };
    pointer.tracker.add(event.timeStamp, along(event));
  });

  el.addEventListener("pointermove", (event) => {
    if (!pointer || event.pointerId !== pointer.id) return;
    const moved = along(event) - pointer.start;
    const sideways = across(event) - pointer.startAcross;
    if (!pointer.dragging) {
      if (Math.abs(sideways) > threshold && Math.abs(sideways) > Math.abs(moved)) {
        pointer = null;
        return;
      }
      if (Math.abs(moved) < threshold) return;
      pointer.dragging = true;
      el.setPointerCapture(event.pointerId);
      // The movement before the threshold may have begun a text selection;
      // a drag is never one.
      globalThis.getSelection?.()?.removeAllRanges();
      pointer.origin = onStart?.() ?? 0;
      // The measurement starts where the threshold was passed: the element
      // does not jump by the threshold, and it does not lose the distance
      // beyond it either. The first move delivered can be far past the
      // threshold — the engine merges moves that arrive within a frame — and
      // starting from that move put the element behind the pointer for the
      // whole drag.
      pointer.start += Math.sign(moved) * threshold;
    }
    pointer.tracker.add(event.timeStamp, along(event));
    onMove?.(pointer.origin + (along(event) - pointer.start));
  });

  const finish = (event) => {
    if (!pointer || event.pointerId !== pointer.id) return;
    const ended = pointer;
    pointer = null;
    if (ended.dragging) {
      // The release is a sample too. A pointer held still sends no moves, so
      // without it the velocity would be the last move's: a careful drop,
      // after a pause, would be thrown.
      if (event.type === "pointerup") ended.tracker.add(event.timeStamp, along(event));
      swallowNextClick();
      onEnd?.(ended.tracker.velocity());
    }
  };
  el.addEventListener("pointerup", finish);
  el.addEventListener("pointercancel", finish);
}

/// The release that ends a drag is followed by a `click` on what was pressed:
/// a sheet dragged by its handle and put back would then close on its own.
/// The click that closes a drag is not a click. The trap only lives until the
/// current input event is over — if no click comes (the release was outside),
/// it must not eat the next real one.
function swallowNextClick() {
  const swallow = (event) => {
    event.stopPropagation();
    event.preventDefault();
  };
  globalThis.addEventListener("click", swallow, { capture: true, once: true });
  setTimeout(() => globalThis.removeEventListener("click", swallow, { capture: true }), 0);
}
