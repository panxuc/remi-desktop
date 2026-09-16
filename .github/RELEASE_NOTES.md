Remi is a desktop pet that shows what your coding agent is doing — on this machine, or on a
server you reach over ssh, **including while you are detached from the session**. She thinks,
reads, writes, and stands there unmistakably waiting when the agent is blocked on your approval.

Remi is harness-neutral by design: adapters report neutral events and one reducer turns those
into poses. **Claude Code is the harness supported in this beta**; OpenCode is next.

## Install the pet

**macOS** (11+, Intel and Apple Silicon) — download `Remi-*-macos-universal.app.tar.gz`,
unpack it, and move `Remi.app` to `/Applications`. The build is **not signed**, so Gatekeeper
will refuse it on first launch. Clear the quarantine flag:

```sh
xattr -dr com.apple.quarantine /Applications/Remi.app
```

On macOS 15 and later the old right-click → Open bypass is gone; the alternative to the command
above is System Settings → Privacy & Security → Open Anyway.

Remi has no Dock icon and no menu bar — **right-click her** for the session menu, where you pick
a session, connect to a host, change her size, and quit.

**Windows 10/11** — download either `Remi-*-windows-x86_64.msi` or
`Remi-*-windows-x86_64-setup.exe`. Also unsigned, so SmartScreen will warn: *More info* →
*Run anyway*.

**Linux** — no pet yet. Wayland has no protocol for a window to position itself or stay on top,
which is the whole premise of a desktop pet. The hook below is fully supported on Linux, which
is what matters for the machines the agent actually runs on.

## Teach a machine to talk to her

Every machine running an agent — including your laptop — needs `remi-hook`, which writes what
the agent is doing to a small state file. One command, on that machine:

```sh
curl -fsSL https://github.com/un-lock-able/remi-desktop/releases/latest/download/install.sh | sh
```

It downloads the right binary for the machine, verifies it against `SHASUMS256.txt`, installs it
to `~/.local/bin/remi-hook`, and configures a harness on it — Claude Code by default, merging
into `~/.claude/settings.json` alongside your own hooks and keeping a `.bak`. Pass
`--harness <name>` to pick another one. Then it prints what the pet will see. Run it again any
time to check:

```sh
~/.local/bin/remi-hook check
```

To undo everything it did: `remi-hook uninstall`, or `remi-hook uninstall --purge` to remove the
binary and state directory too.

## Known limits in this beta

- **Nothing is code-signed.** See the quarantine and SmartScreen notes above.
- **The pet does not install the hook for you.** Even for your own machine, run the script above.
- **No autostart.** Add Remi to your login items yourself.
- **No tray icon.** Right-clicking Remi is the only route to her menu, so she never fully
  disappears — when no session is running she rests on screen, greyed out.
- **Claude Code is the only harness with an adapter.** `--harness opencode` is accepted by the
  CLI but its plugin is not written yet.

## Credits

The character is from [*Zenless Zone Zero*](https://zenless.hoyoverse.com/); the GIF and Spine
animations are by [森哈_Yeah](https://space.bilibili.com/2021405481) on bilibili. The art is
**personal, non-commercial use only** and is not covered by this project's MIT license, which
applies to the code.

Remi is an unofficial fan project — **not affiliated with, endorsed, or sponsored by miHoYo /
HoYoverse / COGNOSPHERE**. It sells nothing and accepts no payment.
