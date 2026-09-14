// The GIF fallback (plan §0 #3, §5.3). Not expected to be used: M1 proved a transparent WebGL
// canvas composites inside a transparent webview, which is the only thing this exists for.
//
// It is structurally worse and the difference is visible: GIF carries 1-bit transparency, so
// anti-aliased edges are matted against whatever the file was exported over and fringe on a
// transparent window; frames are locked at 360×360 and soft on Retina; and a state change is an
// opacity crossfade between two unrelated bitmaps, which always reads as a dissolve rather than
// as Remi moving. Kept only so that one failure mode has an answer.

// Staged by build.rs under their original names (plan §5.5), but only when the `gif-fallback`
// cargo feature is on — the art is 8.5 MiB and everything under `ui/` is embedded in the binary.
// This module ships unconditionally; its art does not. The Spine equivalence each file stands in
// for is in brief §2.1.
const GIF = {
  writing: "01writing.gif",
  // Same file as Writing, matching the Spine path — `02` is `d_win`, which was tried and dropped.
  replying: "01writing.gif",
  proud: "03pride.gif",
  thinking: "04thinking.gif",
  waiting_for_input: "05waiting-for-input.gif",
  // Swapped to match the Spine map: `07` is `a_win` (with pen) and `06` is `a` (without).
  viewing: "07view-with-pen.gif",
  idle: "06view.gif",
  offline: null,
};

let img = null;
let currentState = null;

export async function mount(rootEl) {
  // Fail here, with the reason, rather than mounting cleanly and then showing broken images for
  // every state. app.js only reaches this module when the Spine renderer has already failed, so
  // this is the message someone reads while something is already wrong.
  await new Promise((resolve, reject) => {
    const probe = new Image();
    probe.onload = resolve;
    probe.onerror = () =>
      reject(
        new Error(
          "the GIF art is not in this build — rebuild with `--features gif-fallback`",
        ),
      );
    probe.src = `assets/gif/${GIF.writing}`;
  });

  img = document.createElement("img");
  img.alt = "";
  // `07` is 257×290 where the rest are 360×360, so the frame size cannot be assumed (brief §2.1).
  img.style.objectFit = "contain";
  rootEl.appendChild(img);
}

export function setState(petState) {
  if (!img || petState === currentState) return;
  currentState = petState;
  const file = GIF[petState];
  // `removeAttribute` rather than `src = ""`, which resolves to the document URL and requests it.
  if (file) img.src = `assets/gif/${file}`;
  else img.removeAttribute("src");
}

export function dispose() {
  img?.remove();
  img = null;
  currentState = null;
}
