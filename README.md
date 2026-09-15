# Remi

A desktop pet who shows what your coding agent is doing.

She sits on your desktop — transparent, always on top — and animates to match one agent
session: thinking, reading, writing, replying, proud when a turn lands. The one that matters:
when the agent is **blocked waiting for your approval**, she stands there waiting, and you
notice without watching the terminal.

**Claude Code is the harness supported today.** Remi is not built around it, though — see
[Harnesses](#harnesses).

The session can be on this machine or on any host you already `ssh` to, and she keeps showing
the right thing **while you are detached from it** — which is the entire point. A hook on the
remote writes a state file; nothing has to stay connected for the state to stay true.

## Install

Grab the latest release. In short:

| | |
|---|---|
| macOS 11+ | `Remi-*-macos-universal.app.tar.gz` → `/Applications`, then `xattr -dr com.apple.quarantine /Applications/Remi.app` |
| Windows 10/11 | `Remi-*-windows-x86_64.msi` or `-setup.exe` |
| Linux | no pet yet — see below |
| any machine running an agent | `curl -fsSL .../releases/latest/download/install.sh \| sh` |

Nothing is code-signed yet, so both platforms will warn on first launch. The release notes carry
the exact incantations.

Remi has **no Dock icon and no tray**: right-click her for the session menu — pick a session,
connect to a host, resize, quit.

### Why there is no Linux pet

Not the toolkit. Wayland's `xdg-shell` has no request for a client to set its own position, by
design, and a client cannot declare itself always-on-top either — that is compositor policy. A
pet that cannot restore itself to where you left it is not a pet. `remi-hook` is fully supported
on Linux, which is where the agent usually runs.

## How it works

```
 harness event  ──▶  remi-hook signal  ──▶  ~/.local/state/remi/sessions/<harness>/<id>.json
                                                              │
                              ┌───────────────────────────────┴───────────────┐
                        local │ file watch                              ssh   │ ssh -T host remi-hook watch
                              └───────────────────▶  remi-desktop  ◀──────────┘
```

Two ideas carry the design:

- **Transmit a level, not edges.** Every record says "session S is in state X as of T". Dropped
  messages self-heal, ordering does not matter, the receiver is stateless.
- **One write path, many read paths.** `remi-hook` only ever writes a file and exits. No
  transport is ever in the agent's critical path, and a hook can never block or slow it down.
- **Adapters emit events, not poses.** A harness adapter says `edit-start` or
  `approval-asked` — neutral facts about what happened. One reducer turns those into poses, and
  it is the only thing in the system that knows Remi has poses at all.

## Harnesses

The state directory is keyed by `(harness, session)` and every record names its harness, so one
machine can run two of them at once and Remi keeps them apart — the menu labels each session by
which harness it came from.

| harness | status |
|---|---|
| **Claude Code** | supported. `remi-hook setup --harness claude-code` merges Remi's hooks into your `settings.json`, keeping your own hooks and a `.bak`. |
| **OpenCode** | next. It gets a plugin rather than hooks; the event vocabulary is already the shared one. |
| **Codex** | under investigation. It has no hook system — its only push fires on turn completion — so following it means tailing its rollout log, and whether approval requests even appear there is unverified. `docs/IMPLEMENTATION-PLAN.md` §3.6 has the findings. |

Adding a harness should cost one file under `harness/` plus a config variant. If it ever costs
more than that, the neutral vocabulary is wrong and wants fixing rather than working around.

`docs/IMPLEMENTATION-PLAN.md` is the real design document; `docs/PROJECT-BRIEF.md` records what
was decided and why, including the alternatives that were rejected.

## Building from source

```sh
cargo test --workspace          # all three crates
cargo run -p remi-desktop       # the pet, straight from cargo — no node, no tauri CLI needed
cargo build -p remi-hook --release
```

The frontend has **no build step**: static files and ES modules under `crates/remi-desktop/ui/`,
with `spine-webgl` vendored. `build.rs` stages the Spine art into `ui/assets/` on every build.

Bundling the app needs the Tauri CLI (`cargo install tauri-cli --version "^2.11"`), then
`cargo tauri build` from `crates/remi-desktop`. On macOS that also merges `Info.plist`, which is
what makes the bundle Dock-less — `cargo run` never does, so the dev loop always has a Dock icon.

## License

The **code** is MIT — see `LICENSE`. Reuse it freely; MIT's one condition is that the copyright
notice travels with it, so keep that in anything you build from this.

The **character art** is not mine and is **not covered by that license**. The character is from
[*Zenless Zone Zero*](https://zenless.hoyoverse.com/); the GIFs and the Spine skeleton are by
[森哈_Yeah](https://space.bilibili.com/2021405481) on bilibili. It is **personal, non-commercial
use only** — see `assets/README.md`. A fork that ships the art is redistributing someone else's
work, not mine, and MIT does not carry that permission along with the code.

Remi is an unofficial fan project, **not affiliated with or endorsed by miHoYo / HoYoverse**.
It sells nothing and accepts no payment.
