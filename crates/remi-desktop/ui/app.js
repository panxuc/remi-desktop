// Picks a renderer, mounts it, and feeds it states.
//
// Rust never learns which renderer is active — that is the whole point of the seam (plan §5.3).
// This file is the only thing on either side that knows both exist.

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

/// Rust never decides which renderer runs — it only relays the `renderer` key from config.toml
/// (plan §5.4). `?renderer=gif` overrides it, for trying the other one without editing config.
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
  await tauri?.event?.listen("pet://state", (event) => apply(event.payload?.state));

  // Then ask, because Tauri drops events that have no listener yet — the pet's first pose is
  // usually emitted while this page is still parsing. Same reasoning as the session store being a
  // register rather than a channel (plan §3.1): a late reader must still see the current value.
  let first = state;
  try {
    const current = await tauri?.core?.invoke("pet_state");
    if (current?.state) first = current.state;
  } catch (err) {
    console.warn("could not read the current pet state:", err);
  }
  // Not blended: there is nothing to blend from.
  apply(first, { transition: false });

  window.addEventListener("keydown", onKeyDown);
  // Handy from the webview inspector: `__remi.apply("proud")`.
  window.__remi = { apply, states: STATES, get state() { return state; } };
}

function apply(next, opts) {
  if (!next || !STATES.includes(next)) {
    console.warn("ignoring unknown pet state:", next);
    return;
  }
  state = next;
  renderer?.setState(next, opts);
}

/// M2's exit criterion is "all 7 states switchable from a debug key", so: digits pick a state
/// directly, arrows step through them. The window takes focus on click like any other.
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
