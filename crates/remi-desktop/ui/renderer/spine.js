// The Spine render path, and the renderer the pet uses.
//
// `spine` is a global, defined by ./vendor/spine-webgl.js, which index.html loads as a classic
// script ahead of the module graph — so there is no bundler and no node in the build.

// Animation names are the asset's own. `light` is a flavour variant and `0` is the empty setup
// pose — neither is ever played.
const ANIMATION = {
  // `a_win` is the pen pick-up and `a` is the empty-handed rest. `a_win` loops like being patted
  // on the head, which is tolerable in a pose that flashes past during a read and wrong in the one
  // Remi holds whenever nothing is happening — hence this way round, which also reads better:
  // pen in hand while on task, empty-handed at rest.
  viewing: "a_win",
  idle: "a",
  thinking: "b",
  proud: "c",
  writing: "d",
  // Deliberately the same as Writing: replying is the agent putting words on the screen, which is
  // near enough to writing that a separate pose would be a distinction without a difference.
  replying: "d",
  waiting_for_input: "e",
  // No session running: the same empty-handed rest as `idle`, told apart from it by the grey
  // `offline` treatment in index.html rather than by a pose of its own.
  //
  // ⚠️ Not `null`, and emphatically not `setEmptyAnimation` — the tempting answer for "nothing
  // to draw", and the bug this replaces. Mixing to an empty animation returns the skeleton to
  // its *setup* pose, which for this asset is a fully visible Remi standing still: 51 of 199
  // slots carry a setup attachment, and animation `0` is that pose and is empty. The loop then
  // stopped exactly as designed and left the still frame on screen for as long as no session
  // was running.
  //
  // Drawing nothing at all was the plan's intent (§5.3) and is the only version that costs no
  // GPU, but there is no tray yet (`menu.rs`), so the session menu is reachable only by
  // right-clicking Remi — a pet that fades out is a pet with no way back. Resting is the trade,
  // and it costs the plan's other loop mitigation: Offline animates, so the loop no longer
  // stops for it and `visibilitychange` is the one left.
  offline: "a",
};

/// Seconds of cross-fade between two poses. Spine's `AnimationState` blends bone-for-bone, which
/// is the thing a CSS crossfade between bitmaps structurally cannot do.
const MIX_SECONDS = 0.25;

/// Fraction of the window kept clear around the fitted skeleton. The fit itself now runs the
/// physics (see `measureFit`), so this is no longer what keeps the hair off the window edge —
/// what it covers is the one thing the fit cannot sample: a cross-fade between two poses, which
/// blends bones into positions neither animation reaches on its own. Measured over every ordered
/// pair of poses, the worst of those overshoots the fitted box by 5.4 world units, and 6 % of
/// the window is about twice that.
const MARGIN = 0.06;

/// Seconds per step when measuring the framing. Coarser than a frame: the extents come out the
/// same as at 1/60 to within a tenth of a world unit, for half the work.
const FIT_STEP_SECONDS = 1 / 30;

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

  // `premultipliedAlpha: true` is what composites correctly through the system webview — see the
  // note on `drawSkeleton` below for why non-premultiplied *textures* are still right.
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

  // A state this renderer does not know: keep whatever is playing rather than blanking her.
  // Every state the pet sends has an animation, so this is only reachable from a pet newer than
  // the webview it is serving.
  const name = ANIMATION[petState];
  if (!name) return;

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

/// The pose changes the silhouette a lot — `c` throws both arms up — so framing each animation on
/// its own would make Remi jump in size whenever the state changed. Fit the union instead: one
/// scale, chosen once, that nothing overflows.
///
/// The union covers only the animations this renderer plays, not every animation in the file. An
/// unplayed one can reach well outside the others, and including it would shrink Remi on screen to
/// make room for a pose nobody ever sees.
///
/// Every pose is played through once with the physics solver running, which costs on the order of
/// a tenth of a second — once, at mount, after an asset load that takes longer.
function measureFit(skeletonData) {
  const offset = new spine.Vector2();
  const size = new spine.Vector2();
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;

  for (const name of new Set(Object.values(ANIMATION))) {
    const animation = skeletonData.findAnimation(name);
    // A rename in a re-exported asset should fail here, at load, and not as a state that silently
    // never plays.
    if (!animation) throw new Error(`skeleton has no animation "${name}"`);

    // A skeleton of its own per animation, never the one being drawn, and never shared between
    // two measurements: posing a skeleton leaves slot attachments behind — an animation only
    // resets the slots it keys — and it leaves the physics mid-swing, which is state the next
    // animation would start from and the renderer never would.
    const skeleton = new spine.Skeleton(skeletonData);
    const probe = new spine.AnimationState(new spine.AnimationStateData(skeletonData));
    // Looping, and run for a whole cycle, because that is how the pet plays it: the hair reaches
    // its extreme on the swing back into the loop point, which a single pass does not contain.
    probe.setAnimation(0, name, true);

    const steps = Math.ceil(animation.duration / FIT_STEP_SECONDS);
    for (let i = 0; i <= steps; i++) {
      const delta = i === 0 ? 0 : FIT_STEP_SECONDS;
      probe.update(delta);
      probe.apply(skeleton);
      // ⚠️ `Physics.update`, stepped, and not `Physics.none` — which is the bug this replaces.
      // Measuring with the solver frozen gives the pose the animator keyed, and the pet draws
      // the pose the solver produces: in `d` the hair swings 38 world units further left than
      // the keyed extent, which is ~13 % of Remi's width, and it was cropped off at the window
      // edge for the whole of Writing and Replying. Anything physics-driven — hair, ribbons —
      // has to be measured swinging or the framing does not cover where it actually goes.
      skeleton.update(delta);
      skeleton.updateWorldTransform(spine.Physics.update);
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
/// Stopping on `visibilitychange` is the one way to avoid spending a GPU on an unseen window that
/// needs no help from Rust. Real occlusion — fully covered rather than hidden — is a Tauri event
/// and is not wired up.
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
}
