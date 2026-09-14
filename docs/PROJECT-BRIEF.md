# Remi Desktop Pet — Project Brief

> **Purpose of this file:** hand-off context. Drop this in the repo root and tell Claude
> "read PROJECT-BRIEF.md" to resume without re-deriving any of the decisions below.
> Written 2026-09-06. Everything marked ✅ was verified empirically on the dev machine;
> everything marked ❓ is an open question.

---

## 1. What we are building

A **desktop pet** — a small transparent always-on-top window showing an animated anime
character (Remi) — whose animation state reflects **what Claude Code is currently doing on a
remote machine**.

Two status inputs, in priority order of value:

1. **Remote agent status** (the main event). Claude Code — or another supported harness,
   §12 — runs on a headless server inside a `zellij` session, reached over SSH. The pet must
   keep showing accurate status **even while the user is disconnected from zellij** — that's
   the whole point.
2. **Local input method (IME)** — nice-to-have, macOS `TISCopyCurrentKeyboardInputSource`,
   deferred until the Claude status path works.

The single highest-value behaviour: **show unmistakably when Claude is blocked waiting for
user approval**, so the user notices without watching the terminal.

---

## 2. Assets ✅

### 2.1 GIFs (v1 render path)

`assets/`, **360×360 except `07`**. Note GIF only has 1-bit transparency — anti-aliased edges
are matted, so they fringe slightly on an arbitrary background; that limit is why Spine is the
v1 renderer (§11), and it was confirmed against the real compositor at plan M1 — the WebGL path
carries true 8-bit alpha, the GIF path cannot. Re-verified with `file` + `ffprobe` 2026-09-14:
all seven are now genuine GIFs, and two were renamed since the 09-07 measurement.

| file | frames | duration | size | Spine equivalent |
|---|---|---|---|---|
| `01writing.gif` | 17 | 1.14 s | 360×360 | `d` |
| `02intermittent-writing.gif` | 17 | 1.14 s | 360×360 | `d_win` |
| `03pride.gif` | 31 | 2.07 s | 360×360 | `c` |
| `04thinking.gif` | 41 | 2.74 s | 360×360 | `b` |
| `05waiting-for-input.gif` | 41 | 2.74 s | 360×360 | `e` |
| `06view.gif` | 61 | 4.07 s | 360×360 | `a` |
| `07view-with-pen.gif` | 158 | 4.74 s | **257×290** | `a_win` |

✅ **`07` is resolved.** It was a saved GitHub HTML page; the raw blob has since been fetched
and it is now a real GIF. Two things about it still differ from the rest: it is **257×290, not
360×360**, so `renderer/gif.js` must not assume one frame size, and at 158 frames it is nearly
triple the next-densest file. Neither matters unless the GIF fallback is ever actually used.

The `02`/`07` names now match the Spine `_win` transition pair (§2.2) rather than describing a
crossfade between poses, which is the mapping the reducer already assumes.

### 2.2 Spine asset ✅ — same character, same animations, better

`assets/spine-asset/`, from the same bilibili author. **Confirmed visually 2026-09-08**
(via `tools/spine-viewer`) to contain *the same animations as the GIFs*, rendered
noticeably smoother.

| file | role |
|---|---|
| `Q蕾米.json` | skeleton + animation data — what a runtime consumes |
| `leimi.atlas` + `leimi.png` | texture atlas, one 2048×790 page, straight alpha (no `pma`) |
| `images/` (218 PNGs), `Q版蕾米.zip` | unpacked source parts; only needed to repack at higher res |
| `Q蕾米.spine` | Spine **Editor** project file — opens only in the paid editor |

Exported from **Spine 4.2.43**. 257 bones, 199 slots, 222 attachments (144 of them meshes),
**80 physics constraints** (hair, ribbons, earrings, wings), 6 IK, 35 transform.
Exactly **one** slot uses additive blending (the `light` effect).

**Animation → PetState map** (content descriptions from the user, who watched them all):

| animation | duration | content | PetState |
|---|---|---|---|
| `a` | 4.00 s | read / idle, empty-handed | `Idle` |
| `a_win` | 5.27 s | read / idle **with a pen** — reads as picking the pen up | `Viewing` |
| `b` | 5.33 s | thinking, with pen | `Thinking` |
| `c` | 2.00 s | pride | `Proud` |
| `d` | 2.13 s | writing, continuous | `Writing` |
| `d_win` | 1.07 s | writing intermittently — starts writing, returns to `a_win` | transition |
| `e` | 5.33 s | **waiting for input**, with pen | `WaitingForInput` |
| `light` | 1.27 s | reading without pen; book glows yellow, glow falls on her face | flavour / variant |
| `0` | 0 s | empty setup pose — never play it | — |

**Every PetState in §5 has a Spine animation.** The `_win` pair are transitions, which is
something the GIF path cannot do smoothly at all.

---

## 3. Tech stack — SETTLED

**Tauri v2 (Rust) + `rumqttc` for MQTT.** Target **macOS and Windows only** for v1.

Why Tauri: the assets are GIFs, so animation is literally `<img src="04thinking.gif">` —
no frame decoding, no timing loop, no texture management. State transitions become CSS
crossfades. All real logic stays in Rust. Uses the system webview (WKWebView / WebView2),
so ~5–10 MB, not Electron-sized.

Why `rumqttc` specifically: pure Rust, async/tokio, supports retained messages + LWT + TLS.
**Not `paho-mqtt`** — it wraps a C library and needs `cmake`, which is not installed and
would have to be provisioned on every platform.

### Crate layout

```
remi-core/      # pure Rust lib. PetState enum, MQTT client, config, host list.
                #   ZERO UI dependencies — this is what keeps the GUI swappable.
remi-desktop/   # Tauri shell: transparent window, tray icon, host dropdown, GIF rendering
remi-hook/      # tiny CLI invoked by Claude Code hooks on the remote machine
```

### Fallback if Tauri disappoints — ✅ not needed

**Tauri did not disappoint.** Plan M1 passed on the dev Mac 2026-09-14: a transparent
always-on-top borderless window with a **WebGL2 canvas compositing inside it**, no opaque box,
and a premultiplied alpha ramp fading cleanly to nothing — so true 8-bit alpha survives
WKWebView and the macOS compositor. That was the one result that could have invalidated this
section. Details and the two Tauri traps it surfaced are in plan §5.1 and §9.

The fallback is kept on the page because it stays cheap, not because it is expected:
`egui`/`eframe` — pure Rust, no webview, `ViewportBuilder` has `with_transparent`,
`with_always_on_top`, `with_mouse_passthrough` directly. Costs: decode GIF frames manually
(`image` crate, `gif` feature, ~40 lines) + separate `tray-icon` crate. Roughly one extra day.
Keeping `remi-core` UI-free makes this a pivot, not a rewrite — and `tools/spine-viewer` is
already a working `rusty_spine` spike of it.

---

## 4. Status transport design — SETTLED

### The core principle: transmit a LEVEL, not EDGES

Send *"current state is X as of timestamp T"* — never *"event Y happened."* This is the single
most important design decision and it makes everything else easy:

- a dropped message self-heals on the next update — no retries, no queue
- no ordering guarantees needed, last-write-wins by timestamp
- receiver is stateless; it renders whatever it last heard
- swapping transports later touches ~50 lines

Payload is ~70 bytes, bursty at maybe 5–30 events/min while active, zero when idle.
**Bandwidth and reliability are non-issues** — choose transports on operational simplicity alone.

### Concrete scheme

MQTT topic per host, **published with `retain=true`** so a reconnecting pet gets current state
instantly:

```
remi/<host>/<session>/state     retained
{"v":1,"session":"a1b2c3d4","host":"plume","state":"thinking","ts":1757150400,"cwd":"remi-desktop"}
```

Broker: self-hosted **Mosquitto** on the user's home server (they own `*.anything.moe` and have
a home server, so this is not a blocker).

### Session selection

The pet **renders one session at a time**, chosen by the user from a menu on right-click (and
the identical menu on the tray), the way VS Code's remote picker works. Sessions from every
*connected* source are listed; only the selected one is drawn. The default selection is
"follow most recent", a deterministic tiebreak on `ts`.

**Which sources are connected is the user's call, one machine at a time** (decided 2026-09-14).
This machine is always watched. Every host in `~/.ssh/config` is offered in the menu from the
first launch, and none is connected to until the user presses Connect on it — so the pet never
spends its first seconds failing to reach machines nobody asked about, and never holds an ssh
connection open to a host the user does not care about today. Connecting remembers the host, so
it comes back on the next launch. Plan §4.4 and §5.2.

Still explicitly NOT aggregating — because only one session is ever rendered, no priority
merge rule exists anywhere in the system. Selection is the user's, not the app's.

### No LWT, and no staleness timeout (for v1)

**Important gotcha:** MQTT Last-Will-and-Testament only fires when a *persistent* connection
drops. A fire-and-forget hook that connects, publishes, and disconnects cleanly will **never**
trigger LWT. Getting real LWT requires a resident `remi-agent` on the remote holding the
connection open, with hooks feeding it over a unix socket.

**Decision: skip LWT for v1 — and don't stand a staleness timeout in for it.** An earlier
version of this decision showed idle once a record was ~60 s old. That is wrong for a writer
that only fires on events: while Claude waits on an approval prompt, or runs a long tool,
nothing is written, so the timeout turns exactly the waiting pose into idle. Decided
2026-09-14: no written pose times out; only `Proud` fades to `Idle`. A session that dies
without ending keeps its last pose until a newer session takes over, it is removed, or its
file is pruned after 24 h. Add the agent later only if that becomes annoying.

---

## 5. Pet state machine

`PetState` enum in `remi-core`, mapped to Claude Code hooks. Rechecked against the raw Claude
Code hooks reference, changelog and settings schema 2026-09-14 (local binary is v2.1.270).

| Claude Code hook | matcher | PetState | Spine |
|---|---|---|---|
| `UserPromptSubmit` | — | `Thinking` | `b` |
| `PreToolUse` | `Read\|Grep\|Glob` | `Viewing` | `a_win` |
| `PreToolUse` | `Edit\|Write` | `Writing` | `d` |
| `MessageDisplay` | — | `Replying` | `d` (same as `Writing`) |
| `PermissionRequest` | `*` | `WaitingForInput` | `e` |
| `Notification` | `permission_prompt\|agent_needs_input\|elicitation_dialog\|elicitation_url_dialog` | `WaitingForInput` | `e` |
| `PostToolUse` | `*` (every tool) | `Thinking` (clears a prompt) | `b` |
| `PostToolUseFailure` | `*` | `Thinking` | `b` |
| `Stop` / `StopFailure` | — | `Proud` → decays to `Idle` | `c` |
| `SessionEnd` | — | `Offline` | — |
| (8 s after `Stop`) | — | `Idle` | `a` |

`MessageDisplay` (Claude Code 2.1.152+) runs while reply text streams to the screen, and never
for hidden thinking, so it tells `Replying` apart from `Thinking`. `PermissionRequest` fires the
moment a prompt appears, where the `permission_prompt` notification waits about six seconds.
`PostToolUse` runs only for tools that succeed, hence `PostToolUseFailure`. `StopFailure` runs
instead of `Stop` when a turn dies on an API error. Details in plan §3.5.

⚠️ **`PostToolUse`, matched on `*`, is what clears the waiting pose, and it is not optional.**
Claude Code has no "approval granted" hook. The sequence is `PreToolUse` → `Writing`,
`Notification` → `WaitingForInput`, you approve, the tool runs, and then *nothing publishes*
until the next tool call or `Stop`. Remi holds the waiting pose while Claude is already
working again — a false positive on the one pose this project exists for. `PostToolUse` is
the only event that can stand in for approval-granted, which is also why the mapping needs
one field of memory (the pose to return to) rather than being a pure lookup. Scoping the
matcher to `Edit|Write` reintroduces the bug for prompts raised by any other tool, so `*` is
part of the fix. Reducer rules in plan §3.5; the settings block in plan §6.

This table is the Claude Code instance of a general mechanism: adapters emit a neutral
event vocabulary and one reducer turns those into poses, so the same policy is not
written down once per harness. Plan §3.4–3.6 holds the mechanism and the OpenCode table.

⚠️ **`Notification` must be matched on `notification_type`, never taken bare.** It fires for
at least `permission_prompt`, `idle_prompt`, `auth_success`, `elicitation_dialog`,
`elicitation_url_dialog`, `elicitation_complete`, `elicitation_response`, `agent_needs_input`,
`agent_completed`, and several `quota_auto_resume_*` types. An unmatched `Notification` hook
would put Remi in the waiting pose on a successful auth or a quota resume. `idle_prompt` is
deliberately excluded — it means *you* have gone quiet, not that Claude is blocked.

### Hook payload ✅

Every event carries `session_id`, `transcript_path`, `cwd`, and `hook_event_name`; that is
what `remi-hook` reads. Also present and worth knowing:

- `permission_mode` — `default` / `plan` / `acceptEdits` / `auto` / `dontAsk` /
  `bypassPermissions`. In `bypassPermissions` and `dontAsk` there are no permission prompts,
  so `WaitingForInput` simply never fires. Not a defect; just the ceiling on the headline
  feature.
- `agent_id` / `agent_type` — present when the hook fires **inside a subagent**. Subagent tool
  calls therefore publish states for the parent `session_id` too. Level semantics absorb this
  correctly (last write wins), but a fan-out of subagents can make the pose flicker; if that
  is annoying in practice, drop records carrying `agent_id`.
- `prompt_id` (v2.1.196+), and `last_assistant_message` on `Stop`. Unused for now.

Claude Code has over thirty hook events; the full list is in its hooks reference. Beyond the
table above, worth knowing: `PermissionDenied` fires only for auto-mode denials, never when you
deny a prompt yourself; nothing at all fires when you interrupt a turn; and the settings
schema rejects unknown keys in a hook entry, so remi's hooks can't carry a marker.

Hooks are configured in `~/.claude/settings.json` on **each machine Claude runs on**.
Getting them there is `remi-hook setup`, run *on* that machine — by the pet over ssh, or by
the user from a published `install.sh` for hosts the pet cannot reach. Plan §6.1.

---

## 6. Rejected alternatives — do not relitigate

| Rejected | Why |
|---|---|
| **Swift + AppKit** | Was the right call when this was macOS-only + Touch Bar. Cross-platform requirement killed it; user has never written Swift. |
| **Touch Bar version** | Touch Bar items are locked to 30 pt tall = 60×60 px. The 360×360 art loses all detail. Also macOS-only, last-gen hardware, needs private DFR APIs. Deferred indefinitely — possibly a later macOS-only extra. |
| **`ssh tail -F` on an append-only file** | Uses a *log* transport for a *state variable*. Unbounded file growth, rotation races, orphaned remote `tail` processes if the app crashes, buffering quirks. |
| **`ssh cat` polling a single overwritten file** | Not rejected — **promoted.** Zero infra, level-semantics native, needs `ControlMaster` multiplexing. It is now one of three first-class transports (local file watch / ssh poll / MQTT) in `remi-core`'s `source/`. See the plan §4.4. |
| **`paho-mqtt`** | Requires `cmake` on every build platform. |
| **Central custom HTTP service** | MQTT gives retained-state + LWT as protocol primitives; hand-rolling those is strictly more work. |
| **ntfy / Gotify** | Notification-shaped, not state-shaped. Possible *later* addition for phone push on `WaitingForInput`. |
| **Syncthing state file** | Least code, but sync latency isn't controllable. |
| **OSC escape sequences (OSC 9/777)** | Only works while the terminal is attached — dies exactly in the detached-zellij scenario that motivates the project. |
| **Prometheus / Netdata** | Correct architecture, absurd overkill for 70 bytes. |
| **Electron** | Would mean writing JS instead of Rust. |
| **Godot** | Decent for animated sprites, exports everywhere, but game-engine mental model and weak tray story. |
| **GTK4 direct / Qt via cxx-qt** | Painful build story on macOS + Windows. |

---

## 7. Linux — DEFERRED, and why

Linux (KDE Plasma) is explicitly **out of scope for v1**. The blocker is not the toolkit, it's
**Wayland**, and it would hit identically regardless of GUI framework:

- Wayland's `xdg-shell` has **no protocol request for a client to set its own position**, by
  design. A pet cannot restore itself to where the user left it.
- A client also cannot declare itself always-on-top; that's compositor policy.
- Transparency itself is *fine* — KWin always composites. Only positioning/stacking are blocked.
- Plasma 6 defaults to Wayland.

If/when Linux is revisited, options in order of preference: ship KWin window rules
(`Keep Above` + `No titlebar`, matched on window class — what most Linux pets do, user drags
her once); or `wlr-layer-shell` (KWin implements it, but almost no Rust toolkit exposes it —
you'd drop to `smithay-client-toolkit`); or target the X11 session (works trivially, but KDE
is winding X11 down).

macOS and Windows have none of these problems — both do transparent + always-on-top +
click-through + arbitrary positioning without complaint.

---

## 8. Dev environment ✅ (verified 2026-09-06)

**Machine:** MacBook Pro `Mac14,7` (M2, 13", 2022 — the last Touch Bar Mac), 8 GB RAM,
macOS 26.6.2.

| tool | version |
|---|---|
| rustc / cargo | 1.95.0 |
| rustup | 1.29.0 |
| node | 24.11.1 |
| npm / pnpm | 11.6.2 / 10.33.0 |
| Claude Code | 2.1.236 (homebrew cask) |
| OpenSSH | 10.3p1 |

**Not installed:** `cmake`, `pkg-config`, `Xcode.app` (Command Line Tools only, macOS 26.5 SDK),
`docker`, `mosquitto`, `syncthing`, `ntfy`.
Swift 6.3.3 is available via CLT but is no longer part of the plan.

**SSH hosts** (`~/.ssh/config`): `plume`, `theresa`, `lappland`, `whisperain` (all
`*.anything.moe`), plus `congestion`, `newcon`, `byte119`, `byte133`, `cadlinux`.
No `ControlMaster` configured — and the pet passes `ControlMaster=no` of its own accord, for a
reason worth reading in plan §4.4.

**The remote for M4 is `theresa`, not `plume`** ✅ probed 2026-09-14. All four `*.anything.moe`
boxes are Linux x86_64. `plume` is bare — no zellij, no node, no rust. `theresa` has
zellij 0.45.1, cargo 1.97.0 and git, so `remi-hook` can be built natively on it and the whole
cross-compilation question is deferred to M4b with nothing lost. ❓ **Claude Code is not
installed on any of them yet**, which is what M4's exit criterion still needs.

❓ **Windows dev/test machine availability is unknown** — needs confirming before Windows
support can actually be validated.

---

## 9. Build order

**Superseded by `docs/IMPLEMENTATION-PLAN.md` §9**, which sequences the milestones against the
current design (Spine as the v1 renderer, three transports, a local-first path that needs no
broker). This section is left as a pointer only so the two files cannot drift.

---

## 10. Open questions ❓

- Where exactly does Mosquitto run, and what auth/TLS setup? (user has domains + home server)
- ❓ **Which remote gets Claude Code**, and how `remi-hook` gets onto it before the installer of
  plan §6.1 exists. `theresa` is the candidate and can build it from source itself (§8).
- Topic scheme final form — is `remi/<host>/state` enough, or does it need per-session
  granularity for multiple concurrent Claude sessions on one host?
- ~~Click-through vs draggable~~ ✅ Resolved 2026-09-14: **draggable, and click-through is
  dropped entirely.** They are exclusive — a window that ignores the cursor cannot be dragged —
  and dragging is what a pet needs. Window *size* became the config choice instead (plan §5.1).
- Autostart on login (macOS `LaunchAgent`, Windows registry `Run` key / Startup folder).
- Window position persistence across restarts.
- ~~Does the repo get renamed?~~ ✅ Done — it is `remi-desktop`, with `origin` at
  `git.unlockableworld.com/unlockable/remi-desktop`.
- ❓ **That remote is self-hosted Forgejo/Gitea, but plan §7 and decision #26 specify GitHub
  Actions and a public GitHub release for `install.sh` + binaries.** Either mirror to GitHub
  for releases, or port the release matrix to Forgejo Actions and host the assets there.
  Gates M4b; costs nothing to leave open until then. Recorded as plan §12.8.
- Do Codex approval requests reach its rollout log? Unverified, and it decides whether Codex
  can ever show the waiting pose (§12, plan §12).

---

## 11. Spine as the render path — status 2026-09-08

**Resolved ✅.** The Spine asset (§2.2) contains every animation the GIFs do, and looks
better — confirmed by watching it in `tools/spine-viewer`, not inferred.

Verified empirically, not read off the JSON:

- Loads clean on the **official Spine 4.2 C runtime** via `rusty_spine` 0.8, all 80 physics
  constraints intact. A 4.1 runtime would reject the file outright (physics is 4.2-only).
- **`rusty_spine` builds without `cmake`** — it compiles spine-c through the `cc` crate.
  This is what disqualified `paho-mqtt` (§6), so it's worth stating: it does not apply here.
- The `rusty_spine` + macroquad viewer that runs today is also a working spike of the
  **egui fallback** in §3 — that pivot is now de-risked.

### What Spine buys over GIFs

1. **Resolution independence.** GIFs are locked at 360×360 and soft on Retina.
2. **Real transitions.** Spine's `AnimationState` blends between poses. The GIF plan is a CSS
   opacity crossfade between unrelated bitmaps, which always reads as a dissolve. The asset
   even ships explicit transition animations (`d_win`, and `a_win` as a pen pick-up).
3. **True 8-bit alpha.** GIF's 1-bit transparency fringes on a transparent always-on-top
   window — exactly this app's situation.
4. **Procedural secondary motion.** 80 physics constraints; hair and ribbons can react to
   the window being dragged. A GIF fundamentally cannot do this.
5. **Size.** ~700 KB atlas vs 5.1 MB of GIFs.

### What it still costs

- Gives up the §3 "animation is literally `<img src>`" simplicity: a WebGL canvas and a
  60 fps render loop in an always-on-top window, i.e. continuous GPU work on an 8 GB M2.
  Worth measuring battery impact before committing.
- The Rust viewer draws the single additive slot (`light`) with normal alpha, so that one
  effect looks flat there. The web player in `tools/spine-viewer/web/` handles it correctly.

### Decision ✅ — Spine is the v1 renderer

Decided 2026-09-08. Not a v1.1 swap: the pet ships with Spine from the start.
**Vindicated 2026-09-14** — the hard precondition below ("prove a transparent WebGL canvas
composites inside a transparent webview") was tested at plan M1 and holds, including the
8-bit alpha that motivated this whole section. `renderer/gif.js` is now a contingency nobody
expects to need rather than a live fallback.

The GIF-first ordering was only ever worth it if it saved work, and it doesn't. The
transparency milestone has to prove a **transparent WebGL canvas composites inside a
transparent webview** regardless — that is a meaningfully harder case than a transparent
`<img>` and can fail on its own — so once that's proven, a GIF renderer is a throwaway
plus a seam built to discard it.

The renderer still sits behind a thin interface (a three-function JS contract, plan §5.3),
and `renderer/gif.js` is kept as a ~40-line fallback behind that same contract in case the
WebGL canvas doesn't composite. `remi-core` stays UI-free either way.

The open cost is battery: a 60 fps GPU loop in an always-on-top window on an 8 GB M2. It gets
**measured** at plan milestone M2, with occlusion-pausing as the first mitigation. M1's probe
ran an unthrottled `requestAnimationFrame` loop and is a fair stand-in for that cost, but no
power measurement was taken from it — M2 still owes the number.

Superseded: an earlier reading of this asset claimed the animations were five costume
variants with no mapping to `PetState`. That was wrong — it was inferred from filename
prefixes (`A_`/`B_`/…, which are redrawn parts *per pose*), not from looking at the render.

---

## 12. More than one harness — status 2026-09-10

**Decided: v1 ships Claude Code and OpenCode. Codex is deferred, and the design is built to
take it later without a redesign.**

### Why this is cheaper than it looks

The level-semantics decision (§4) already did most of the work. A `SessionRecord` carries a
*pose* and a timestamp, never a hook name — so the registry, all three transports, the session
menu and the renderer have never known which harness produced a record. Supporting another
harness is confined entirely to the write path. Nothing downstream moves.

What was missing was the vocabulary in between. Today the hook→pose mapping lives in the
`settings.json` matcher, which is free and exactly right for Claude Code and does not
generalise: a harness that hands you one undifferentiated event stream has to do the mapping
in code. Three adapters each mapping straight to a pose means the policy "an edit means
`Writing`" is written down three times and drifts. Hence the neutral event vocabulary
and the single reducer in plan §3.5.

### Push beats pull, and it is not a style preference

A harness that *spawns* `remi-hook` (Claude Code's hooks; an OpenCode plugin) needs nothing
resident anywhere, so it works identically under all three transports. A harness we have to
*pull* from — tailing a log, holding a subscription — needs someone doing the pulling, which
exists under `local` and `ssh` but not under `mqtt`, where there is no pet on the remote. A
pull-only harness therefore drags in the resident `remi-agent` this brief deferred in §4.
That is the axis worth optimising, and it is why OpenCode should ship as a plugin even though
its SSE stream is the more obvious integration.

### Verified, not assumed (2026-09-10)

Both harnesses were inspected on the dev machine rather than recalled — `opencode serve`'s
live OpenAPI schema (1.16.2), and real Codex rollout logs in `~/.codex/sessions/`.

**OpenCode is the best-instrumented of the three.** It emits both approval edges
(`permission.v2.asked` / `permission.v2.replied`), which Claude Code does not, and it names
its tools directly (`read`, `edit`, `write`, `bash`, `grep`, `glob`, `task`, `webfetch`,
`skill`, `apply_patch`), so classification is a lookup rather than a guess. It also has a
literal `session.status` level of `{idle, busy, retry}`. Full mapping in plan §3.5.

**Codex is the difficult one**, on four independent counts:

- No hook system. Its only push is `notify` in `config.toml`, which fires on turn completion
  and nothing else — with `notify` alone Remi could only ever be `Proud` or `Idle`.
- Its real signal is an append-only rollout log per session, so any useful adapter is pull.
- Tool classification is lossy: its tool surface is shell-shaped. Every one of the 62 tool
  calls in the sampled session was `exec_command`; the only other writer is `apply_patch`.
  Read-versus-write beyond that means parsing command strings, which is a heuristic we should
  not build.
- **The headline feature may not be observable at all.** No approval request appeared in the
  sampled rollout — that session ran a permissive sandbox and was never asked anything — so
  whether approvals reach the log is unverified. Plan §12 records the ten-minute experiment
  that settles it.

Deferring Codex costs almost nothing because of the five structural obligations in plan §3.6
that v1 honours anyway: `harness` is part of session identity, the vocabulary and reducer are
their own module, `remi-hook signal` is the writer's entry point, `HarnessCaps` is declared
per adapter, and `remi-hook watch` is specified as "the state dir plus any enabled pull
adapters" even though v1 enables none. Those are the expensive-to-retrofit parts; the adapter
itself is one file.

One property worth remembering for when Codex comes back: because its rollout log is durable
and append-only, a puller that starts late reads back and catches up. Nothing is lost while
nobody is watching — the same retention guarantee the register gives us, which is why the
deferral is safe rather than merely convenient.

### A bug this analysis surfaced — and closed

Mapping a second harness exposed that the *first* one was wrong: nothing cleared
`WaitingForInput` after an approval was granted, so Remi held the waiting pose while Claude
was already working again.

**Resolved.** `PostToolUse`, matched on `*`, is the approval-cleared signal — it is the only
event Claude Code emits after a prompt is granted. It is in the mapping table in §5, in the
reducer rules in plan §3.5, and in the complete `settings.json` block in plan §6, which is
also what `remi-hook install --host` writes to a remote. Matching it narrowly (say
`Edit|Write`) reintroduces the bug for prompts raised by any other tool, so the matcher is
part of the fix, not a detail.

Finding it was the concrete return on defining the neutral vocabulary before writing a second
adapter: the bug is invisible while there is only one harness and only one place the mapping
lives.
