# Remi

A desktop pet who shows what your coding agent is doing.

She sits on your desktop — transparent, always on top — and animates to match one agent
session: thinking, reading, writing, replying, proud when a turn lands. The one that matters:
when the agent is **blocked waiting for your approval**, she stands there waiting, and you
notice without watching the terminal.

**Claude Code and Codex are supported.** See [Harnesses](#harnesses) for setup and event coverage.

The session can be on this machine or on any host you already `ssh` to, and she keeps showing
the right thing **while you are detached from it** — which is the entire point. A hook on the
remote writes a state file; nothing has to stay connected for the state to stay true.

## Install

Grab the latest release. In short:

| | |
|---|---|
| macOS 11+ | `Remi-*-macos-universal.app.tar.gz` → `/Applications`, then `xattr -dr com.apple.quarantine /Applications/Remi.app` |
| Windows 10/11 | `Remi-*-windows-x86_64.msi` or `-setup.exe` |
| Linux x86_64 | `Remi-*-linux-x86_64.deb` or `.AppImage` — X11 / XWayland recommended |
| any machine running an agent | `curl -fsSL .../releases/latest/download/install.sh \| sh` |

Nothing is code-signed yet, so macOS and Windows will warn on first launch. The release notes carry
the exact incantations.

Remi has **no Dock icon**. Use the tray icon or right-click her for the session menu — pick a
session, connect to a host, resize, hide, quit.

### Linux

The release workflow builds `.deb` and AppImage packages for Linux x86_64. Install the `.deb`
with `sudo apt install ./Remi-*-linux-x86_64.deb`, or make the AppImage executable and run it.
For a checkout whose changes have not been released yet, build locally using the commands below.

Remi prefers **X11 / XWayland**, which allows position restoration and always-on-top requests.
GTK falls back to native Wayland if X11 is unavailable. An explicit `GDK_BACKEND` overrides
this choice, for example `GDK_BACKEND=wayland remi-desktop` or `GDK_BACKEND=x11 remi-desktop`.
Install/enable XWayland in a Wayland session for the full desktop-pet behavior.

Native Wayland still leaves positioning, stacking and taskbar visibility to the compositor.
On KDE Plasma, add a window rule (System Settings → Window Management → Window Rules)
matching Remi's window class:

| property | setting |
|---|---|
| Position | Remember |
| Keep above other windows | Apply initially · Yes |
| Skip taskbar / pager / switcher | Apply initially · Yes |

The tray needs AppIndicator (`libayatana-appindicator3-1` on Debian/Ubuntu,
`libappindicator-gtk3` on Arch). KDE Plasma shows it natively; GNOME needs its AppIndicator
extension. Right-clicking the pet also opens the menu.

`Remi-*-linux-x86_64.deb` and `.AppImage` are the pet as macOS and Windows ship her: Spine only.
WebKitGTK does not always composite the transparent WebGL canvas that renderer draws into, and a
pet that fails that way is invisible rather than merely ugly — she says so in a message on the
window, but there is nothing to see. If that happens, install the `-gif-fallback` package
instead: the same build plus 8.5 MiB of GIF art the renderer falls back to. The feature is off
by default, so a local build has no GIF art unless you pass `--features gif-fallback` yourself.

### Codex setup

On each machine running Codex, install the hook from this checkout and configure it:

```sh
cargo install --path crates/remi-hook --locked
~/.cargo/bin/remi-hook setup --harness codex --check
cargo run -p remi-desktop
```

Setup merges Remi's hooks into `$CODEX_HOME/hooks.json` (default `~/.codex/hooks.json`),
preserving your other hooks and a `.bak`. It does not edit `config.toml` or replace `notify`.
Restart Codex, then use **`/hooks` in the CLI to review and trust the Remi hooks**. This is
[Codex's hook trust requirement](https://learn.chatgpt.com/docs/hooks#review-and-trust-hooks);
setup/check verify configuration, not runtime trust. Use a Codex version with lifecycle hooks
(the local integration was developed with CLI 0.155.1); older notify-only versions need updating.
Desktop/IDE clients must use a Codex runtime that loads these hooks and the same `CODEX_HOME`.

```sh
remi-hook check --harness codex
remi-hook uninstall --harness codex
```

After the next release, the download installer also accepts
`sh install.sh --harness codex --check`. Claude Code remains the default when `--harness` is
omitted, and both adapters can be installed together.

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
| **Codex** | supported through lifecycle hooks: `remi-hook setup --harness codex --check`. See [Codex setup](#codex-setup). |

Codex maps `UserPromptSubmit` to thinking, read tools to viewing, `apply_patch` to writing,
`PermissionRequest` / `request_user_input` to waiting, `PostToolUse` to thinking, `Stop` to
proud, `Interrupt` to idle, and `SessionEnd` to removal. Shell commands keep the thinking pose:
their names alone do not tell us whether they read or edit files. Codex does not currently
provide a reply-stream hook, so its replies keep the preceding pose until `Stop`. Hosted tools
without lifecycle hooks and work happening before hook installation cannot be observed.

Adding a harness should cost one file under `harness/` plus a config variant. If it ever costs
more than that, the neutral vocabulary is wrong and wants fixing rather than working around.

`docs/IMPLEMENTATION-PLAN.md` is the real design document; `docs/PROJECT-BRIEF.md` records what
was decided and why, including the alternatives that were rejected.

## Building from source

Linux needs Rust, a C/C++ build toolchain, GTK 3, WebKitGTK 4.1 and AppIndicator development
packages. On Debian/Ubuntu:

```sh
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev patchelf libxdo-dev
```

On Arch: `sudo pacman -S --needed base-devel rust pkgconf webkit2gtk-4.1 libappindicator-gtk3 librsvg patchelf`.

```sh
cargo test --workspace          # all three crates
cargo run -p remi-desktop       # the pet, straight from cargo — no node, no tauri CLI needed
cargo build -p remi-hook --release
```

The frontend has **no build step**: static files and ES modules under `crates/remi-desktop/ui/`,
with `spine-webgl` vendored. `build.rs` stages the Spine art into `ui/assets/` on every build.

Bundling the app needs the Tauri CLI (`cargo install tauri-cli --version "^2.11"`), then
`cargo tauri build` from `crates/remi-desktop` (on Linux, what the release builds:
`cargo tauri build --bundles deb,appimage --features gif-fallback`).
On macOS that also merges `Info.plist`, which is
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
