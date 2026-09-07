# spine-viewer

Built to answer one question — **does `assets/spine-asset` already contain the animations we
shipped as GIFs?** It does (see below). Keeping it around: it doubles as a working spike of
the `rusty_spine` + egui fallback render path.

Two independent viewers, so a disagreement tells you where the fault is:

| | what it is | trust |
|---|---|---|
| **Rust** (`src/main.rs`) | macroquad + `rusty_spine` (official Spine 4.2 C runtime) | ✅ confirmed working on macOS 2026-09-08 |
| **Web** (`web/index.html`) | the official `spine-player` widget, vendored | battle-tested; also shows the GIF side by side |

If they ever disagree, the web one is right — it's Esoteric's own player, not my drawing code.

## Run the web one first (no build)

```sh
cd <repo root>
python3 -m http.server 8000
# open http://localhost:8000/tools/spine-viewer/web/
```

Spine on the left with an animation picker, the matching GIF on the right, shared
backdrop toggle (checker / dark / light / magenta). `file://` will **not** work —
the runtime fetches the atlas and hits CORS.

## Run the Rust one

```sh
cd tools/spine-viewer
cargo run --release            # defaults to ../../assets/spine-asset
cargo run --release -- /path/to/other/spine-asset
```

`1`-`8` or arrows switch animation · `space` pause · `[` `]` speed · `L` loop ·
`R` restart · `B` backdrop · scroll zoom · drag pan · `H` hide help.

No `cmake` needed — `rusty_spine` builds spine-c through the `cc` crate. Verified.

## What's already established

Verified by loading the file with the real runtime, not by reading JSON:

- Parses clean on **Spine 4.2** (`rusty_spine` 0.8). The asset is `4.2.43`.
  A 4.1 runtime would reject it — physics constraints are a 4.2 feature.
- **257 bones, 199 slots, 222 attachments** (144 meshes), **80 physics constraints**,
  6 IK, 35 transform. Atlas is a single 2048×790 page, straight alpha (no `pma`).
- Exactly **one** slot uses additive blending (the `light` effect). The Rust viewer
  draws everything with normal alpha, so `light` will look slightly flat there.
  The web player handles it correctly.

### Animations vs. GIFs — RESOLVED ✅ (2026-09-08)

Watched in this viewer: **the Spine asset contains the same animations as the GIFs**, and
plays them noticeably smoother. Content descriptions below are the user's, from watching.

| animation | duration | content | PetState |
|---|---|---|---|
| `a` | 4.00 s | read / idle | `Viewing` |
| `a_win` | 5.27 s | read / idle **with a pen** — reads as picking the pen up | `Idle` |
| `b` | 5.33 s | thinking, with pen | `Thinking` |
| `c` | 2.00 s | pride | `Proud` |
| `d` | 2.13 s | writing, continuous | `Writing` |
| `d_win` | 1.07 s | writing intermittently — starts writing, returns to `a_win` | transition |
| `e` | 5.33 s | **waiting for input**, with pen | `WaitingForInput` |
| `light` | 1.27 s | reading without pen; book glows yellow, glow falls on her face | flavour |
| `0` | 0 s | empty setup pose — never play it | — |

Every `PetState` is covered. The `_win` animations are transitions, which the GIF path
can't do smoothly. See `docs/PROJECT-BRIEF.md` §2.2 and §11.

Two things the duration arithmetic got wrong, for the record: `04thinking` (41f / 2.74 s)
matches no Spine duration yet is clearly `b`, and `01writing` / `02write-to-view` share a
duration but are `d` and `d_win`. Frame-count matching was a weak signal; watching was not.

### `07other-to-view.gif` is not a GIF

It's a saved GitHub HTML page (`</html>` at EOF), 222 KB, from:

```
HanaAyane/remielle-codex-pet — gif/3.gif @ 7a86d4d
```

The raw blob was never downloaded. Fix with the `raw.githubusercontent.com` URL, or
re-export it from Spine if the animation turns out to live in this asset. That repo
is presumably where the other six came from too.

Note `docs/PROJECT-BRIEF.md` §2 is also stale: it lists six GIFs and calls one
`02write-to-pause.gif`, but the file is `02write-to-view.gif`.
