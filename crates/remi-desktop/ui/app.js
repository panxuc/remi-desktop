// Picks a renderer, mounts it, and feeds it states.
//
// Rust never learns which renderer is active — that is the point of the seam. This file is the
// only thing on either side that knows both exist.

/// `PetState`, serialised by serde as snake_case (remi-core `state.rs`). The order is the debug
/// cycle order, which is why it reads as a turn rather than alphabetically.
const STATES = [
  "idle",
  "thinking",
  "viewing",
  "writing",
  "replying",
  "waiting_for_input",
  "proud",
  "offline",
];

/// Dynamic so the fallback's module is never fetched unless it is actually used.
const RENDERERS = {
  spine: () => import("./renderer/spine.js"),
  gif: () => import("./renderer/gif.js"),
};

const root = document.getElementById("root");
const label = document.getElementById("debug-label");
let renderer = null;
let state = "idle";

async function mountRenderer(name) {
  const module = await RENDERERS[name]();
  await module.mount(root);
  renderer = module;
  document.body.dataset.renderer = name;
  return module;
}

const tauri = window.__TAURI__;

/// Rust only relays the `renderer` key from the config file; it does not decide.
/// `?renderer=gif` overrides it, for trying the other one without editing config.
async function chooseRenderer() {
  const override = new URLSearchParams(location.search).get("renderer");
  if (override in RENDERERS) return override;
  try {
    const configured = await tauri?.core?.invoke("renderer");
    if (configured in RENDERERS) return configured;
  } catch (err) {
    console.warn("could not read the configured renderer:", err);
  }
  return "spine";
}

async function boot() {
  const choice = await chooseRenderer();

  try {
    await mountRenderer(choice);
  } catch (err) {
    // This is the case `renderer/gif.js` was kept for. Say so loudly: silently degrading to the
    // worse renderer would hide exactly the failure the fallback exists to survive.
    console.error(`renderer "${choice}" failed to mount, falling back to gif:`, err);
    if (choice === "gif") throw err;
    await mountRenderer("gif");
    show(`renderer fell back to gif — ${err}`, 6000);
  }

  // Subscribe *before* asking for the current pose. The other order has a gap: a state change
  // landing between the two would be emitted to nobody and then not be in the answer either.
  await tauri?.event?.listen("pet://state", (event) => applyPayload(event.payload));

  // Then ask, because Tauri drops events that have no listener yet — the pet's first pose is
  // usually emitted while this page is still parsing, and a late reader must still be able to see
  // the current value.
  let first = null;
  try {
    first = await tauri?.core?.invoke("pet_state");
  } catch (err) {
    console.warn("could not read the current pet state:", err);
  }
  // Not blended: there is nothing to blend from.
  applyPayload(first ?? { state }, { transition: false });

  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("contextmenu", onContextMenu);
  window.addEventListener("mouseup", onContextMenuMouseUp);
  window.addEventListener("blur", cancelContextMenu);
  window.addEventListener("pointercancel", cancelContextMenu);
  // Handy from the webview inspector: `__remi.apply("proud")`, `__remi.setLive(false)`. The
  // second is the only way to see the not-live treatment without staging a real connection drop.
  window.__remi = { apply, setLive, states: STATES, get state() { return state; } };
}

/// Everything one `pet://state` carries: the pose, and whether that pose is still being heard.
/// Rust decides both — this only draws them.
function applyPayload(payload, opts) {
  if (!payload) return;
  setLive(payload.live !== false);
  apply(payload.state, opts);
}

/// Grey and dim her while the shown session's connection is not delivering (see index.html). The
/// default is live, so a payload from a build that predates the field, or none at all, leaves her
/// in colour rather than accusing a healthy session of being stale.
function setLive(live) {
  document.body.dataset.live = live ? "true" : "false";
}

function apply(next, opts) {
  if (!next || !STATES.includes(next)) {
    console.warn("ignoring unknown pet state:", next);
    return;
  }
  state = next;
  // On the body as well as in the renderer, so a treatment that belongs to *every* renderer can
  // be a CSS rule rather than a third thing each of them has to remember to do — see the grey
  // `offline` rule in index.html.
  document.body.dataset.state = next;
  renderer?.setState(next, opts);
}

/// Right-clicking Remi opens the session menu. A webview cannot draw a native menu, so all this
/// does is forward the gesture — the menu itself is built in Rust, from the registry.
///
/// `preventDefault` is what stops the webview's own menu appearing over it. Listening on the
/// window rather than on #root matters: the renderer's canvas covers the whole page, and while
/// `pointer-events: none` keeps it out of the way of a drag, nothing should depend on that here.
///
/// The click's position goes along because Rust cannot find the cursor itself everywhere: Wayland
/// gives no app the global pointer position. `clientX`/`clientY` are logical pixels from the
/// window's top-left, which is what the menu is placed in.
let pendingContextMenu = null;

function onContextMenu(event) {
  event.preventDefault();
  const at = { x: event.clientX, y: event.clientY };

  // WebKitGTK emits contextmenu while the right button is still down. Opening the native
  // menu then lets that same button's release dismiss it, especially after a long press.
  // Wait for release instead. Backends that emit contextmenu on release, and keyboard
  // invocation, can open immediately because the right button is already up.
  pendingContextMenu = null;
  if (event.buttons & 2) {
    pendingContextMenu = at;
    return;
  }
  openContextMenu(at);
}

function onContextMenuMouseUp(event) {
  if (event.button !== 2 || !pendingContextMenu) return;
  const at = pendingContextMenu;
  pendingContextMenu = null;
  openContextMenu(at);
}

function cancelContextMenu() {
  pendingContextMenu = null;
}

function openContextMenu(at) {
  tauri?.core?.invoke("context_menu", at).catch((err) => {
    console.error("could not open the session menu:", err);
  });
}

/// Digits pick a state directly, arrows step through them, so every pose can be reached without an
/// agent running. The window takes focus on click like any other.
function onKeyDown(event) {
  if (event.metaKey || event.ctrlKey || event.altKey) return;

  let next = null;
  const digit = Number.parseInt(event.key, 10);
  if (digit >= 1 && digit <= STATES.length) {
    next = STATES[digit - 1];
  } else if (event.key === "ArrowRight" || event.key === "ArrowDown") {
    next = STATES[(STATES.indexOf(state) + 1) % STATES.length];
  } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
    next = STATES[(STATES.indexOf(state) + STATES.length - 1) % STATES.length];
  }
  if (!next) return;

  event.preventDefault();
  apply(next);
  show(next.replace(/_/g, " "));
}

/// A caption, not UI: the window is transparent and always on top, so anything permanent here is
/// something the user has to look at forever. It fades itself out.
///
/// Who Remi is showing is deliberately *not* here. It lives in the session menu, which has the
/// width for a machine name, a session name and an ssh failure reason side by side — none of which
/// fit on one line inside a 180 px window without either shrinking Remi or covering her.
let hideTimer = 0;
function show(text, ms = 1400) {
  label.textContent = text;
  label.classList.add("visible");
  clearTimeout(hideTimer);
  hideTimer = setTimeout(() => label.classList.remove("visible"), ms);
}

boot().catch((err) => {
  console.error(err);
  show(String(err), 30000);
});
