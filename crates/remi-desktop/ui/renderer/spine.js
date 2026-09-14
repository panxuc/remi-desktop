// The Spine render path — v1's renderer (brief §11, plan §5.3).
//
// `spine` is a global, defined by ./vendor/spine-webgl.js, which index.html loads as a classic
// script ahead of the module graph. That is what keeps §0 #4 true: no bundler, no node.

// Animation names are the asset's own (brief §2.2). `light` is a flavour variant and `0` is the
// empty setup pose — neither is ever played.
const ANIMATION = {
  // `a_win` is the pen pick-up and `a` is the empty-handed rest. Swapped relative to the asset's
  // first reading (brief §2.2), because `a_win` loops like being patted on the head — tolerable
  // for a pose that flashes past during a read, wrong for the one Remi holds whenever nothing is
  // happening. Pen in hand also just reads better as "on task".
  viewing: "a_win",
  idle: "a",
  thinking: "b",
  proud: "c",
  writing: "d",
  // Deliberately the same as Writing. `d_win` was the candidate; watched at M2 and not kept.
  // Replying is Claude putting words on the screen, which is near enough to writing that a
  // separate pose would be a distinction without a difference.
  replying: "d",
  waiting_for_input: "e",
  // A session that ended has nothing to draw, and the loop stops rather than spinning the GPU
  // on an empty frame.
  offline: null,
};

/// Seconds of cross-fade between two poses. Spine's `AnimationState` blends bone-for-bone, which
/// is the thing a CSS crossfade between bitmaps structurally cannot do.
const MIX_SECONDS = 0.25;

/// Fraction of the window kept clear around the fitted skeleton, so physics-driven hair and
/// ribbons have somewhere to swing without being clipped at the window edge.
const MARGIN = 0.06;

/// Poses sampled per animation when measuring the framing. 16 is enough to catch the extremes of
/// a 5 s animation and costs a few milliseconds once, at load.
const FIT_SAMPLES = 16;

/// A backgrounded webview hands back a multi-second `requestAnimationFrame` gap. Feeding that to
/// the physics solver detonates it, so clamp the step.
const MAX_STEP_SECONDS = 0.1;

let canvas = null;
let context = null;
let renderer = null;
let assets = null;
let skeleton = null;
let animationState = null;
let fit = null;
let raf = 0;
let lastFrame = 0;
let running = false;
let currentState = null;
let viewport = { width: 0, height: 0, dpr: 0 };

export async function mount(rootEl) {
  canvas = document.createElement("canvas");
  rootEl.appendChild(canvas);

  // `premultipliedAlpha: true` is what M1 proved composites correctly through WKWebView — see
  // the note on `drawSkeleton` below for why non-premultiplied *textures* are still right.
  context = new spine.ManagedWebGLRenderingContext(canvas, {
    alpha: true,
    premultipliedAlpha: true,
    antialias: true,
  });
  if (!context.gl) throw new Error("no webgl context");

  assets = new spine.AssetManager(context, "assets/");
  assets.loadTextureAtlas("remi.atlas");
  assets.loadJson("remi.json");
  await assets.loadAll();

  const atlas = assets.require("remi.atlas");
  const skeletonData = new spine.SkeletonJson(
    new spine.AtlasAttachmentLoader(atlas),
  ).readSkeletonData(assets.require("remi.json"));

  skeleton = new spine.Skeleton(skeletonData);
  const stateData = new spine.AnimationStateData(skeletonData);
  stateData.defaultMix = MIX_SECONDS;
  animationState = new spine.AnimationState(stateData);

  fit = measureFit(skeletonData);
  renderer = new spine.SceneRenderer(canvas, context);

  document.addEventListener("visibilitychange", onVisibilityChange);
  start();
}

export function setState(petState, opts = {}) {
  if (!animationState) return;
  if (petState === currentState) return;
  currentState = petState;
  start();

  const name = ANIMATION[petState];
  if (!name) {
    // Offline, or a state this renderer does not know. Empty the track so the last pose fades
    // out rather than freezing mid-motion, and let the loop stop once it has.
    animationState.setEmptyAnimation(0, MIX_SECONDS);
    return;
  }

  const entry = animationState.setAnimation(0, name, true);
  // `transition: false` is for the first pose after mount and for a jump the user should not see
  // blended — anything else crossfades on `defaultMix`.
  if (opts.transition === false) entry.mixDuration = 0;
}

export function dispose() {
  running = false;
  cancelAnimationFrame(raf);
  raf = 0;
  document.removeEventListener("visibilitychange", onVisibilityChange);
  renderer?.dispose();
  assets?.dispose();
  canvas?.remove();
  canvas = context = renderer = assets = skeleton = animationState = fit = null;
  currentState = null;
}

/// The pose changes the silhouette a lot — `c` throws both arms up, `d_win` reaches 50 units
/// further left than anything else — so framing each animation on its own would make Remi jump in
/// size whenever the state changed. Fit the union instead: one scale, chosen once, that nothing
/// overflows.
///
/// The union is taken over the animations this renderer actually plays, not over every animation
/// in the file. Including `light`, which nothing maps to, would cost about 15% of Remi's on-screen
/// size to accommodate a pose the user never sees.
function measureFit(skeletonData) {
  // Measured on its own skeleton, never on the one being drawn. Posing a skeleton leaves slot
  // attachments behind, and an animation only resets the slots it keys: measuring on the render
  // skeleton left it wearing the last sampled pose's attachments, so the first state to play
  // inherited whichever of them it did not key itself — visibly, a missing mouth until some other
  // animation happened to set one.
  const skeleton = new spine.Skeleton(skeletonData);
  const probe = new spine.AnimationState(new spine.AnimationStateData(skeletonData));
  const offset = new spine.Vector2();
  const size = new spine.Vector2();
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;

  for (const name of new Set(Object.values(ANIMATION))) {
    if (!name) continue; // offline
    const animation = skeletonData.findAnimation(name);
    // A rename in a re-exported asset should fail here, at load, and not as a state that silently
    // never plays.
    if (!animation) throw new Error(`skeleton has no animation "${name}"`);
    probe.setAnimation(0, name, false);
    const step = animation.duration / FIT_SAMPLES;
    for (let i = 0; i < FIT_SAMPLES; i++) {
      skeleton.setToSetupPose();
      probe.update(i === 0 ? 0 : step);
      probe.apply(skeleton);
      // `Physics.none` so the measurement is the animation's own extent and not wherever the
      // hair happened to be swinging on this particular pass.
      skeleton.updateWorldTransform(spine.Physics.none);
      skeleton.getBounds(offset, size);
      if (!(size.x > 0 && size.y > 0)) continue;
      minX = Math.min(minX, offset.x);
      minY = Math.min(minY, offset.y);
      maxX = Math.max(maxX, offset.x + size.x);
      maxY = Math.max(maxY, offset.y + size.y);
    }
  }

  if (!Number.isFinite(minX)) {
    // Nothing measurable — fall back to the setup-pose box the Spine editor recorded in the
    // skeleton header, which is always present.
    const { x, y, width, height } = skeletonData;
    return { x: x + width / 2, y: y + height / 2, width, height };
  }
  return {
    x: (minX + maxX) / 2,
    y: (minY + maxY) / 2,
    width: maxX - minX,
    height: maxY - minY,
  };
}

/// The camera's viewport is in device pixels and `zoom` divides it, so the world-units-per-CSS-
/// pixel that comes out is the same on a Retina display and a 1x one — only the sampling changes.
function resizeIfNeeded() {
  const dpr = window.devicePixelRatio || 1;
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  if (width === viewport.width && height === viewport.height && dpr === viewport.dpr) return;
  viewport = { width, height, dpr };

  canvas.width = Math.max(1, Math.round(width * dpr));
  canvas.height = Math.max(1, Math.round(height * dpr));
  context.gl.viewport(0, 0, canvas.width, canvas.height);

  const camera = renderer.camera;
  camera.viewportWidth = canvas.width;
  camera.viewportHeight = canvas.height;
  camera.position.x = fit.x;
  camera.position.y = fit.y;
  camera.zoom =
    Math.max(fit.width / canvas.width, fit.height / canvas.height) / (1 - MARGIN);
}

function start() {
  if (running || !skeleton || document.hidden) return;
  running = true;
  lastFrame = 0;
  raf = requestAnimationFrame(frame);
}

function stop() {
  running = false;
  cancelAnimationFrame(raf);
  raf = 0;
}

/// A window the compositor is not showing still gets rAF callbacks on some paths; stopping on
/// `visibilitychange` is the first of plan §5.3's battery mitigations and the only one that needs
/// no help from Rust. Real occlusion (fully covered, not hidden) is a Tauri event, and lands with
/// the bridge at M3.
function onVisibilityChange() {
  if (document.hidden) stop();
  else start();
}

function frame(now) {
  if (!running) return;
  raf = requestAnimationFrame(frame);

  const delta = lastFrame ? Math.min((now - lastFrame) / 1000, MAX_STEP_SECONDS) : 0;
  lastFrame = now;

  resizeIfNeeded();

  animationState.update(delta);
  animationState.apply(skeleton);
  skeleton.update(delta);
  skeleton.updateWorldTransform(spine.Physics.update);

  const gl = context.gl;
  gl.clearColor(0, 0, 0, 0);
  gl.clear(gl.COLOR_BUFFER_BIT);

  renderer.begin();
  // `false` describes the *textures*: `leimi.atlas` carries no `pma` flag, so the atlas page is
  // straight alpha. The framebuffer still ends up premultiplied — the batcher blends colour with
  // (SRC_ALPHA, ONE_MINUS_SRC_ALPHA) and alpha separately with (ONE, ONE_MINUS_SRC_ALPHA) — which
  // is exactly what the `premultipliedAlpha: true` canvas hands the compositor.
  renderer.drawSkeleton(skeleton, false);
  renderer.end();

  // Once the empty animation has finished mixing out there is nothing left to draw, so Offline
  // costs no GPU at all until a state arrives.
  if (!ANIMATION[currentState] && !animationState.tracks[0]) stop();
}
