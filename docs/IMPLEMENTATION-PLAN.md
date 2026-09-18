# Remi Desktop — Implementation Plan

> **Relationship to `PROJECT-BRIEF.md`:** the brief records *what was decided and why*,
> including rejected alternatives. This file records *what we build* — layout, module
> boundaries, type signatures, record formats, milestones. When a decision changes, this file
> is rewritten so it always describes one coherent design; superseded options move to the
> brief's §6 or disappear. Do not append "(was X, now Y)" notes here.
>
> Started 2026-09-08, consolidated 2026-09-09. Unresolved questions are collected in §12;
> everything else in this file is decided.

---

## 0. Decisions at a glance

| # | Question | Decision | Where |
|---|---|---|---|
| 1 | GUI stack | **Tauri v2 + Rust** — ✅ proven at M1, transparent WebGL composites. `egui`/`eframe` fallback unused | brief §3, §11 |
| 2 | Platforms | **macOS + Windows** for v1. Linux is not a priority; on Wayland it needs user-applied KWin rules | brief §7 |
| 3 | Renderer | **Spine from day one**, not a v1.1 swap — ✅ shipped at M2. `renderer/gif.js` kept behind the same contract, but its art is behind the `gif-fallback` cargo feature and off by default | §5.3, §5.5 |
| 4 | Frontend tooling | **No build step.** Static files, ES modules, `spine-webgl` vendored. Keeps node out of the Windows build | §5 |
| 5 | Crates | `remi-core` (UI-free, all the logic) · `remi-desktop` · `remi-hook` | §2 |
| 6 | Message semantics | **Transmit a level, not edges.** "Session S is in state X as of T" | brief §4 |
| 7 | IPC primitive | A **register**, not a channel — because a pet must see correct state for a session that started before it attached. File + atomic `rename()` | §3.1 |
| 8 | Where the register lives | `~/.local/state/remi/sessions/<harness>/<session>.json`, resolved from `$HOME` **only** — never from session env, or writer and reader can silently disagree | §3.2 |
| 9 | Not `/tmp` or `$XDG_RUNTIME_DIR` | `/run/user/<uid>` is destroyed when the last login session ends — exactly our detached-zellij case. `/tmp` is tmpfs on some distros and disk on others | §3.2 |
| 10 | Disk wear | Non-issue: ~0.2 GB/day against a 150–600 TBW rating. Writes are coalesced anyway (skip if same state and `ts` < 20 s old) | §3.3 |
| 11 | Write path | **One, always the same.** A hook writes the file and exits. No transport is ever in Claude's critical path | §3, §6 |
| 12 | Read paths | Three, one per transport: `local` · `ssh` · `mqtt`, feeding one registry. `local` is always on; the rest run only where the user has connected, and then concurrently | §4.4 |
| 12b | Which connections run | **Opt-in, one machine at a time.** Every host in `~/.ssh/config` is *offered* in the menu; none is connected to until the user presses Connect. Connecting remembers the host in the config, so it comes back on the next launch; Disconnect stops it and forgets it | §4.4, §5.2 |
| 13 | The `ssh` transport | Long-lived `ssh -T <host> remi-hook watch`, printing the state dir's whole listing as a JSON array on every change. **Not polling.** No `ControlMaster`, no edits to the user's `~/.ssh/config` | §4.4 |
| 14 | Why the system `ssh` binary | So `ProxyJump`, `IdentityFile`, agent and `known_hosts` apply for free. A Rust SSH client would mean configuring the tool separately | §4.4 |
| 15 | MQTT's role | **Optional.** A detached publisher alongside the file write, for hosts the laptop can't reach directly | §6, §8 |
| 16 | Session selection | **One session rendered at a time**, chosen from a right-click menu on Remi and an identical tray menu. Default "follow most recent". A pinned session stays selected after it ends, shown `Offline` | §4.3, §5.2 |
| 16b | Menu shape | This machine's sessions **flat and first**; every **watched** machine a **submenu** carrying its sessions and its Disconnect; every host nobody is watching one level further in, under **Connect to a host…**. A host is something to act on, not just a heading — and a watched one's row carries the counts, so a waiting session is visible without opening it | §5.2 |
| 17 | Why the tray duplicates the menu | Not the only way back any more — click-through is dropped. It is the way to the menu when the pet is covered or awkwardly placed, in an app with no Dock icon | §5.2 |
| 18 | Two clocks | `ts` (publisher) orders records. The receiver's monotonic clock times `Proud` decay and "last heard". Prevents clock skew distorting either | §4.2 |
| 19 | Timeouts | **No written pose times out.** Hooks write only on events, so a pending approval or a long tool call is silent, and a timeout would hide exactly the waiting pose. Only `Proud` decays, to `Idle` after 8 s — and a `Proud` the pet finds on attach rather than watches arrive starts already faded, since it is an edge and the pet has no idea how old it is. MQTT LWT is out too: a fire-and-forget hook can never trigger it | §4.3, brief §4 |
| 20 | Naming | `remi-desktop`, bundle id `moe.anything.remi`. `touchbar-remi` retired | §11 |
| 21 | Harness support | Adapters emit a **neutral event vocabulary**; one reducer turns those into poses. No adapter names a pose | §3.5 |
| 22 | Which harnesses in v1 | **Claude Code and OpenCode.** Codex deferred — pull-only, lossy tool classification, approvals unverified | §3.6, §11 |
| 23 | Push over pull | Prefer the harness spawning `remi-hook` (a hook, a plugin) over us tailing a log or holding a subscription. Push needs nothing resident and works under all three transports | §3.4 |
| 24 | Session identity | `(connection, harness, session)`. One box can run two harnesses at once, and the state dir, `watch` output and MQTT topic all name the harness | §3.2, §4.2, §4.3 |
| 25 | Installing on a target | **One `install.sh`**, published with the release. All configuration is `remi-hook setup`, run *on* the target. The pet pipes the script over ssh stdin; the `mqtt` case the user runs by hand | §6.1 |
| 26 | Distribution | **Public repo, GitHub Actions on a `v*` tag** — `remi-hook` for five targets, `install.sh` + checksums, and both GUI bundles. Unsigned for now | §7 |
| 27 | Ended sessions | Detected by absence from the connection's next snapshot — no message announces them. Shown `Offline` for 10 s, then dropped; a pinned one stays until the user selects something else | §4.3 |

---

## 1. The shape of the thing

A transparent always-on-top window showing Remi, animating to reflect what **one selected
Claude Code session** is doing. The user picks that session from a menu — the same gesture as
VS Code's remote picker — and the session may be running locally or on any reachable host.

Two ideas carry the whole design:

1. **Transmit a level, not edges.** Every message says "session S is in state X as of T".
   Dropped messages self-heal, ordering doesn't matter, the receiver is stateless. (Brief §4.)
2. **One write path, three read paths.** `remi-hook` always does exactly one thing — write a
   session-state file locally. How the pet *reads* those files (same machine / over SSH /
   forwarded via MQTT) is a swappable transport. This is what makes the app useful with zero
   infrastructure and still good with a broker.

Every path is the same two layers — a **register** holding the current value so a pet that
attaches late sees the truth, and a **notifier** so it isn't polling:

| | **local** | **ssh** | **server (MQTT)** |
|---|---|---|---|
| Register | the file, this machine | the file, on the remote | retained message on the broker |
| Notifier | `notify` (FSEvents / inotify / `ReadDirectoryChangesW`) | `remi-hook watch` on the remote, over ssh stdio | MQTT subscription |
| Pet does | watch the directory | one long-lived `ssh -T <host> remi-hook watch`, read one JSON array per line | subscribe `remi/+/+/+/state` |
| Extra writer work | none | none | detached child publishes `retain=true` |
| User must set up | nothing | nothing beyond already being able to `ssh <host>` | a broker |
| Use it when | Claude is on this laptop | Claude is on a box you can reach | the laptop **can't** reach the box directly, or you want many hosts without many ssh connections |

The three registers are different objects; the **record inside them is byte-identical**
(§4.2), and `Registry` cannot tell which one a record arrived through.

---

## 2. Repository layout

```
remi-desktop/
├─ Cargo.toml                  # workspace root
├─ install.sh                  # published as a release asset; the only installer (§6.1)
├─ .github/workflows/          # CI: test/clippy on PR, full release matrix on a v* tag (§7)
├─ docs/
│  ├─ PROJECT-BRIEF.md         # decisions + rationale (stable)
│  └─ IMPLEMENTATION-PLAN.md   # this file
├─ deploy/                     # mosquitto.conf, aclfile, launchd/systemd units
├─ assets/
│  ├─ *.gif                    # 6 usable GIFs, 360×360 (07 is broken — brief §2.1)
│  └─ spine-asset/             # skeleton + atlas + unpacked parts
├─ crates/
│  ├─ remi-core/               # pure lib, zero UI deps
│  ├─ remi-desktop/            # Tauri shell (the pet)
│  └─ remi-hook/               # CLI invoked by Claude Code hooks
└─ tools/
   └─ spine-viewer/            # kept: doubles as the egui-fallback spike
```

`tools/spine-viewer` stays **out of the workspace** (its own `Cargo.lock`, already the case)
so `macroquad` never enters the shipped dependency graph.

### Dependency direction

```
remi-hook  ──┐
             ├──> remi-core   (remi-core depends on neither)
remi-desktop ┘
```

Nothing in `remi-core` may reference `tauri`, `macroquad`, or any windowing crate. This is
the invariant that keeps the egui pivot (brief §3) cheap, and it is the one rule worth
enforcing in review.

### Dependency policy

`[workspace.dependencies]` is **not** how versions get deduplicated — one `Cargo.lock` already
collapses every semver-compatible requirement to a single build. It exists to stop two members
drifting onto *incompatible* requirements. So:

1. Used by two or more members → hoist it.
2. Used by one → leave it in that member. A root manifest listing every dependency in the repo
   stops being informative.
3. **Overrides rule 2:** hoist anything that appears in `remi-core`'s *public* API, even at one
   user, because then version agreement is correctness rather than tidiness.

The test for rule 3 is whether a third-party type or trait crosses the crate boundary.
`serde` does — `#[derive(Serialize)]` on `SessionRecord` implements *remi-core's* `Serialize`,
and a member on an incompatible serde sees the type as simply not serializable. `thiserror`
does not — `#[derive(Error)]` implements `std::error::Error`, which is in std. `tokio` will,
once the ssh and mqtt sources take the pet's channel as a parameter (§4.4), and so will
`tokio-util` if a `CancellationToken` travels beside it.

Note that features are additive and `default-features = false` cannot be overridden by a
member, so that decision has to be made in the workspace entry.

**`Cargo.lock` is committed**, including `tools/spine-viewer`'s. The old "commit for binaries,
not libraries" advice was about a library's lock being ignored by its consumers, which made
committing it a way to test against versions your users would never get. It does not apply
here — `remi-core` is never published, the deliverables are binaries, and `install.sh` puts one
of them on other people's machines. `--locked` in the release job (§7) is what makes a tag
rebuildable.

---

## 3. The write path — from harness event to session register

### 3.1 Why a file, and not "real" IPC

The instinct that this is an IPC problem is half right. It has an IPC *shape* — one process
tells another something — but the requirement that defines it is retention:

> The pet must show accurate status for a session it attached to **after** that session
> started, and after the pet itself was restarted. (Brief §1 — the whole point is staying
> correct while you are detached.)

That makes it a **register** (a durable current-value cell), not a **channel**. Every classic
IPC primitive is a channel: it delivers to whoever is listening *right now*, and if nobody is,
the value is gone. MQTT's `retain=true` exists precisely because the same problem shows up
there, and a retained message is a register bolted onto a channel.

So the primitive has to be a last-write-wins cell that:

- an **ephemeral writer** can update (a hook process lives ~5 ms and exits),
- **no reader needs to be running** for,
- survives a reader restart, and
- needs **no resident daemon** on the remote.

`write tmp + rename()` is exactly that, and POSIX/NTFS both make the rename atomic, so a
reader never observes a half-written record. It is not a workaround for lacking better IPC —
it is the primitive that matches the semantics.

| alternative | why not here |
|---|---|
| **Unix socket / named pipe** | Needs a listener. On the remote there is never a pet to listen to, so the hook would have to spawn and supervise a daemon (`remi-agent`, deferred in brief §4) — and that daemon would still need somewhere to keep current state for a pet that attaches later. A channel, when we need a register. |
| **Loopback TCP/HTTP** | Same listener problem, plus a port to allocate and firewall prompts on macOS. |
| **D-Bus** | Linux-only in practice; macOS and Windows are v1. |
| **macOS `notify(3)` / Windows named events** | Push-only, carry no payload, retain nothing, and don't cross an SSH boundary. |
| **mmap + seqlock in `/dev/shm`** | The "clever file". Real technique (Prometheus client libs do it) but it buys throughput we do not need at ~5–30 writes/minute, in exchange for a fixed record layout and hand-rolled memory ordering. |
| **SQLite** | Genuinely reasonable — WAL mode, concurrent readers, real queries. But it is still a file, gives no change notification across processes (so we'd watch it anyway), and adds a dependency to a crate whose entire data model is "a handful of small JSON records". Revisit only if we ever want history. |

The one real cost of files — no push notification — is solved by *adding* a layer, not by
replacing the primitive: `notify` (inotify / FSEvents / `ReadDirectoryChangesW`) turns the
directory into a push source with no daemon anywhere. If FSEvents coalescing turns out to add
visible lag, the contained fix is a doorbell: the hook additionally `connect()`s to a unix
socket the pet may or may not be listening on (a failed connect to a missing socket costs
microseconds). File stays the register; socket becomes an optional accelerator. **Measured at
M3, not decided now.**

### 3.2 Layout

Every `remi-hook` invocation, on whatever machine Claude runs on, writes one file:

```
$REMI_STATE_DIR                     # override, checked first (tests)
$HOME/.local/state/remi/sessions/<harness>/<session_id>.json
```

**The same path on every OS**, and — more importantly — **resolved from `$HOME` alone, not
from session environment**. `remi-hook` resolves it identically whether it was launched by
Claude Code in an interactive login shell or by `ssh host remi-hook watch` non-interactively.
An env-dependent path would let the writer and reader disagree silently, which is the worst
failure this design can have.

#### Why not `/tmp`, `$XDG_RUNTIME_DIR`, or another ephemeral location

They are semantically the *better* fit — this is runtime state, not durable state — and on
Linux they are tmpfs, so they would cost the disk nothing. The disqualifier is lifetime:

- **`$XDG_RUNTIME_DIR`** (`/run/user/<uid>`) is destroyed by `systemd-logind` when the user's
  **last login session ends**, unless `loginctl enable-linger` is set. Our headline use case is
  Claude running in a *detached zellij on a server you have logged out of* — precisely the
  case where this directory disappears out from under a live session. It is also unset in some
  non-interactive SSH environments, which trips the writer/reader-disagreement failure above.
- **`/tmp`** is tmpfs on some distros and disk on others (and always disk on macOS), so its
  behaviour is inconsistent across exactly the machines we target. It is world-writable, which
  is the wrong permission for a file naming your working directories, and `systemd-tmpfiles`
  ages files out on its own schedule.

`~/.local/state` has none of these problems and the write cost it avoids is negligible
(§3.3), so it wins on the only axis that turned out to matter.

`$XDG_STATE_HOME` is deliberately **not** consulted, even though `~/.local/state` is its
default. It is usually unset, and when a user does set it, it is typically in an interactive
shell rc that a non-interactive `ssh host remi-hook watch` never sources — so honouring it is
exactly how the writer and the reader end up in different directories.

One directory per harness, named by its id (`claude-code`, `opencode`), holding one file per
agent session, named by the harness's **full** session id. Harness ids are lowercase letters,
digits and `-`; session ids are letters, digits, `-` and `_`; so neither can escape the state
dir. Each file holds exactly the record from §4.2, written atomically inside its harness
directory (`<id>.json.<pid>.tmp` → `rename`). The pid is in the temp name because two hooks
for the same session can run at once (parallel tool calls), and a shared temp name would let
one clobber the other's half-written file.

The harness directory is for people rather than for the code: `ls` shows which harness each
session belongs to, and the menu can offer a harness before its sessions when there are many.
Readers skip a file whose record names a different harness or session than its path, so the
path can always be trusted when debugging.

Lifecycle:

- `SessionStart` / any state hook → create or overwrite.
- `SessionEnd` → unlink. The local watcher and `remi-hook watch` both see the removal, so no
  `offline` record needs writing first.
- Every invocation also prunes files, in every harness directory, not modified for 24 h, so
  a crashed session cannot litter the directory forever. A `readdir` of a few directories
  holding a handful of entries — cheap enough to do unconditionally.

Because this file is the source of truth, **no transport is in Claude Code's critical path.**
A hook invocation is one small atomic write and an exit; a broker being down, slow, or absent
cannot make Claude feel slow.

### 3.3 Write volume and disk wear

Not a concern, by roughly three orders of magnitude. Arithmetic, not measurement:

- ~30 writes/min sustained, 8 h/day ≈ 14 k writes/day.
- Each is ~200 B logical but costs a filesystem block plus journal/metadata; call it 16 KB of
  NAND after write amplification. The file is **rewritten, never appended**, so it does not
  grow and the allocator reuses the same blocks.
- ≈ 0.2 GB/day, ≈ 80 GB/year. Consumer NVMe endurance ratings are 150–600 TBW.

That is centuries against the drive's rating, and it is well under what a browser or macOS's
own unified logging writes in the same day. The coalescing rule below cuts it several-fold
again, for free.

#### Coalescing (do this anyway)

`remi-hook signal` **skips the write entirely** if the new record would equal the existing one
in everything but `ts` (state, `_resume`, `cwd`, `title`, …) *and* the existing `ts` is newer
than 20 s. Consecutive `Read`/`Grep`/`Glob` calls all map to `Viewing`, so a burst of tool
calls would otherwise rewrite the same value dozens of times. Comparing the whole record, not
just `state`, is what keeps a newly learned title or a changed `_resume` from being skipped.

The 20 s ceiling keeps `ts` honest: the pet orders sessions by it and shows it as "last
heard", so it never lags a busy session by more than 20 s. The check costs one small read of a
file that is certain to be in page cache.

### 3.4 How an event gets out of a harness

Everything above describes the register and the transports that read it. This section
describes the other end: what causes a write in the first place. It is the **only** part of
the system that knows which agent harness is running, and the reason the rest of the design
does not is that `SessionRecord` (§4.2) carries a *pose*, not a hook name. `Registry`, all
three sources, the menu and the renderer are already harness-agnostic and stay that
way.

```
╔══ MACHINE WHERE THE AGENT RUNS ═══ (laptop, or plume, or any box) ══╗
║                                                                    ║
║   ┌────────────────┐                                               ║
║   │    harness     │   Claude Code · OpenCode · (Codex, later)     ║
║   └───────┬────────┘                                               ║
║           │  STEP 1 — get the event out of the harness.            ║
║           │  THE ONLY PART THAT DIFFERS PER HARNESS. Push or pull. ║
║           ▼                                                        ║
║   ┌────────────────┐                                               ║
║   │   remi-hook    │   the only thing that ever writes.            ║
║   └───────┬────────┘   Identical binary on every machine.          ║
║           │  STEP 2 — write a temp file, rename it over the old.   ║
║           ▼                                                        ║
║   ┌──────────────────────────────────────────────────────┐         ║
║   │  ~/.local/state/remi/sessions/<harness>/<id>.json    │         ║
║   │  one file per agent session, ~200 B, overwritten in  │         ║
║   │  place. Holds the CURRENT pose only (§3.1, §3.2).    │         ║
║   └───────┬──────────────────────────────────────────────┘         ║
╚═══════════╪════════════════════════════════════════════════════════╝
            │  STEP 3 — the pet reads it. Three ways (§4.4), and the
            │  record is byte-identical in all three:
   ┌────────┴──────────┬──────────────────────┬──────────────────────┐
   │  local            │  ssh                 │  mqtt                │
   │  read the file,   │  `ssh <host>         │  remi-hook also      │
   │  `notify` says    │  remi-hook watch`,   │  publishes retained; │
   │  when it changed  │  read JSON lines     │  we subscribe        │
   └────────┬──────────┴──────────────────────┴──────────────────────┘
            ▼  STEP 4
   ┌──────────────────────────────────────────────────────────┐
   │  remi-core, on the laptop, inside the pet                │
   │  · one table keyed by (connection, harness, session)     │
   │  · builds the menu · picks ONE session                   │
   │  · applies `Proud` decay (§4.3)                          │
   └───────────────────────┬──────────────────────────────────┘
                           ▼  STEP 5 — one pose name
   ┌──────────────────────────────────────────────────────────┐
   │  webview → Spine renderer → plays the animation (§5.3)   │
   └──────────────────────────────────────────────────────────┘
```

#### Push and pull, and why push wins

Step 1 comes in two shapes, and the difference is not cosmetic:

| shape | who drives | mechanism | needs something resident? |
|---|---|---|---|
| **push** | the harness | it spawns `remi-hook signal …`, which writes and exits in ~5 ms | **no** |
| **pull** | us | we tail a log or hold a subscription open, then write | **yes** — someone has to be doing it |

Push is strictly better here and is the default we design for. A pushing harness needs no
resident process anywhere, so it behaves identically under all three transports, including
`mqtt` — where there is no pet on the remote to do any pulling. A pulling harness only works
under `local` (the pet itself does the tailing) and `ssh` (`remi-hook watch` is already
running there for the transport); making it work under `mqtt` would require the resident
`remi-agent` that brief §4 deferred.

Per harness:

- **Claude Code — push.** Hooks in `~/.claude/settings.json` spawn `remi-hook signal …`
  on each event. This is the shape everything else is measured against.
- **OpenCode — push, via a plugin.** It also exposes a pull path (`opencode serve`, then
  `GET /api/event`, an SSE stream), but a ~20-line JS plugin that shells out to `remi-hook`
  turns it into the Claude Code shape: no port to discover, no connection to keep alive, and
  it works over `mqtt`. The SSE adapter stays specified as the fallback for anyone who will
  not install a plugin.
- **Codex — pull only, and therefore deferred.** See §3.6.

### 3.5 The neutral event vocabulary

Adapters do **not** map their harness's events straight to a `PetState`. If they did, the
policy "an edit means `Writing`" would be written down once per harness and would drift.
Instead every adapter produces one of nine neutral events, and one reducer turns those into
poses:

```
  harness's own event  ──▶  neutral event  ──▶  PetState  ──▶  Spine animation
        (adapter)                (reducer, one implementation, in remi-core)
```

```rust
// remi-core/src/signal.rs
pub enum Signal {
    TurnStart,
    ReadStart,
    EditStart,
    Reply,          // reply text is streaming to the user; may repeat for one reply
    ToolEnd,
    ApprovalAsked,
    ApprovalAnswered,
    TurnEnd,
    SessionEnd,
}

// Everything else the harness reports about the session travels beside the signal.
pub struct SessionContext { session, harness, ts, cwd, title }

impl Signal {
    pub fn next_update(self, previous: Option<&SessionRecord>, context: SessionContext) -> Update;
}
```

The variants mirror `remi-hook signal`'s event names one to one, and carry no payload: no pose
depends on a tool's name or on whether it succeeded. The tool's class is the event itself
(`ReadStart` / `EditStart`); add a variant only if a pose ever needs another class.

There is no session-started signal. No pose fits a session that has not had a prompt yet
(`Idle` is never written), and Claude Code's `SessionStart` also fires on compaction, mid-turn,
where writing anything would clobber the real pose. A session appears in the menu from its
first prompt.

#### The reducer needs one field of memory

Most signals map to a pose with no context. `ApprovalAnswered` does not: on a harness that
reports the answer before the guarded tool runs, Remi must return to `Writing` after you
approve an edit, and the approval event does not say what was being approved. So the reducer carries exactly one field — the pose it was in when the prompt
arrived — and because a push-shaped writer is a 5 ms process that then dies, that field rides
in the same JSON file under a writer-private key:

```json
{"v":1,"session":"a1b2c3d4","state":"waiting_for_input","ts":1757150400,"_resume":"writing"}
```

Readers ignore unknown fields (§4.2), so this costs the pet nothing. The hook already reads
the file for the coalescing check (§3.3), so it costs the writer nothing either.

The rules, in full, because "back to `_resume`" is ambiguous otherwise:

| incoming | `_resume` | new pose | new `_resume` |
|---|---|---|---|
| `ReadStart` | any | `Viewing` | cleared |
| `EditStart` | any | `Writing` | cleared |
| `Reply` | any | `Replying` | cleared |
| `ApprovalAsked` | empty | `WaitingForInput` | **current pose**, stored |
| `ApprovalAsked` | set | `WaitingForInput` | kept |
| `ApprovalAnswered` | set | the stored pose | cleared |
| `ApprovalAnswered` | empty | `Thinking` | cleared |
| `ToolEnd` | any | `Thinking` | cleared |
| `TurnStart` | any | `Thinking` | cleared |
| `TurnEnd` | any | `Proud` | cleared |
| `SessionEnd` | any | — | file removed |

`ToolEnd` always means `Thinking`, prompt or no prompt. A finished tool is not being read or
written any more; the agent is deciding what to do next. That one rule covers both harness
shapes. With only a rising approval edge (Claude Code, whose `PostToolUse` fires *after* the
tool has run), tool completion is what clears the waiting pose. With both edges (OpenCode),
`ApprovalAnswered` arrives *before* the tool runs and restores the stored pose, and the tool's
own completion then moves on to `Thinking`. Restoring the stored pose on `ToolEnd` instead
would leave Claude Code showing `Writing` after an approved edit had already finished.

A second `ApprovalAsked` while `_resume` is set keeps the stored value, so a second prompt
during one tool call cannot overwrite the pose we have to get back to. `WaitingForInput` itself
is never stored, or answering could not clear it.

The cross-harness test in §10 asserts that the same work, expressed in either harness's
events, passes through the same poses and ends in the same one.

#### Claude Code

Hook name plus the matcher configured in `~/.claude/settings.json` (§6):

| hook + matcher | neutral event | PetState | Spine |
|---|---|---|---|
| `UserPromptSubmit` | `TurnStart` | `Thinking` | `b` |
| `PreToolUse` · `Read\|Grep\|Glob` | `ReadStart` | `Viewing` | `a` |
| `PreToolUse` · `Edit\|Write` | `EditStart` | `Writing` | `d` |
| `MessageDisplay` | `Reply` | `Replying` | not chosen yet (M2) |
| `PermissionRequest` · `*` | `ApprovalAsked` | `WaitingForInput` | `e` |
| `Notification` · `permission_prompt\|agent_needs_input\|elicitation_dialog\|elicitation_url_dialog` | `ApprovalAsked` | `WaitingForInput` | `e` |
| `PostToolUse` · `*` | `ToolEnd` | `Thinking` (clears a prompt) | `b` |
| `PostToolUseFailure` · `*` | `ToolEnd` | `Thinking` | `b` |
| `Stop` | `TurnEnd` | `Proud` → `Idle` after 8 s | `c` |
| `StopFailure` | `TurnEnd` | `Proud` → `Idle` after 8 s | `c` |
| `SessionEnd` | `SessionEnd` | file removed; pet shows `Offline` | — |

⚠️ **`PostToolUse` is not optional.** Claude Code has no "approval granted" hook, so without
it nothing publishes after you approve a prompt: Remi holds the waiting pose while Claude is
already working, until the next tool call or the end of the turn. That is a false positive on
the one pose this project exists for. `PostToolUse` is the approval-cleared signal. It runs only
for tools that succeed, so `PostToolUseFailure` sends the same `ToolEnd` for tools that fail.

**`MessageDisplay` is what separates `Replying` from `Thinking`.** It runs for each batch of
reply text as it streams, never for Claude's hidden thinking or for messages that only call
tools. It is synchronous — Claude Code holds each batch on screen until the hook returns — so
it has a 2 s timeout; `remi-hook signal` measured about 3 ms, and after the first batch the
unchanged-record rule (§3.3) skips the write. Its `final` field is ignored: the pose stays
`Replying` until the next event, which keeps the event name on the command line rather than in
stdin.

**`PermissionRequest` makes the waiting pose immediate.** The `permission_prompt` notification
fires only after a prompt has waited about six seconds, and each keystroke restarts that wait.
The notification stays as a backup, because a sandboxed command's network prompt produces only
the notification; the second `ApprovalAsked` is harmless (§3.5 rules).

**`StopFailure` runs instead of `Stop`** when a turn ends on an API error such as a rate limit.
Since no pose times out, leaving it out would hold the pose where the error found it.

Two gaps no Claude Code hook can close: denying a prompt fires nothing — the agent's reply
(`Reply`) or the end of the turn clears the pose — and interrupting a turn fires nothing, so the
pose stays until the next prompt.

#### OpenCode

Event names taken from the live API schema of `opencode serve` (verified against 1.16.2 on
2026-09-10), and identical whether they arrive via the plugin or the SSE fallback:

| event | neutral event | PetState | Spine |
|---|---|---|---|
| `session.next.prompted` | `TurnStart` | `Thinking` | `b` |
| `session.next.tool.called` · `read grep glob bash webfetch websearch` | `ReadStart` | `Viewing` | `a` |
| `session.next.tool.called` · `edit write apply_patch` | `EditStart` | `Writing` | `d` |
| `session.next.tool.success` / `.failed` | `ToolEnd` | `Thinking` | `b` |
| `permission.v2.asked` · `question.asked` | `ApprovalAsked` | `WaitingForInput` | `e` |
| `permission.v2.replied` | `ApprovalAnswered` | back to `_resume` | — |
| `session.idle` | `TurnEnd` | `Proud` → `Idle` | `c` |
| `session.deleted` | `SessionEnd` | file removed; pet shows `Offline` | — |

`Reply` has no OpenCode mapping yet; which of its events carries streamed reply text is to be
checked at M8.

OpenCode is the **best-instrumented** of the three: it is the only one with both approval
edges, so `ApprovalAnswered` is a real event there rather than an inference from `PostToolUse`.
It also names its tools directly (`read`, `edit`, `write`, `bash`, `grep`, `glob`, `task`,
`webfetch`, `todowrite`, `websearch`, `skill`, `apply_patch`), so classification is a lookup.

`Session.directory` supplies the `cwd` label; session ids are `ses_…`.

### 3.6 Harness support in v1

**v1 ships Claude Code and OpenCode. Codex is deferred.** The design must stay structurally
able to take Codex (and anything else) later; it is not built now.

Why Codex is the hard one — from its rollout logs, read on the dev machine 2026-09-10:

- **No hook system.** Its only push is `notify` in `config.toml`, which fires on turn
  completion and nothing else. With `notify` alone Remi can only ever be `Proud` or `Idle`.
- **Its real signal is an append-only log**, one file per session at
  `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, carrying `session_meta`
  (id, `cwd`, git branch), `turn_context` (`approval_policy`, sandbox), `event_msg/task_started`,
  `event_msg/task_complete`, `response_item/function_call` and `function_call_output`. That is
  a **pull** adapter, with the resident-process cost in §3.4.
- **Tool classification is lossy.** Codex's tool surface is shell-shaped: every one of the 62
  tool calls in the sampled session was `exec_command`, and the only other writer is
  `apply_patch`. `apply_patch` → `Writing` and `exec_command` → `Viewing` is the whole mapping;
  parsing `rg`/`cat`/`sed` out of the command string is a heuristic we should not build.
- **The headline pose may not be observable at all.** No approval request appeared in the
  sampled rollout — that session ran a permissive sandbox and was never asked anything — so
  whether approvals reach the log is unverified (§12).

One genuinely nice property, for when we do come back to it: because the log is durable and
append-only, a puller that starts late reads back through the file and catches up. Nothing is
lost while nobody is watching, which is the same retention guarantee the register gives us.

#### What "structurally compatible" obliges us to do now

These are the parts that are expensive to retrofit, so they land in v1 even though only two
harnesses use them:

1. **`harness` is part of session identity**, not a display afterthought — it is in
   `SessionRecord` (§4.2), in the state dir's layout (§3.2) and in `SessionKey` (§4.3). One
   box can run two harnesses at once, and the menu has to say which.
2. **The neutral vocabulary and the reducer exist as their own module** (§3.5), even with two
   adapters. A third harness must be a new file under `harness/` and nothing else.
3. **`remi-hook signal` is the writer's entry point**, not `remi-hook state` (§6). A pushing
   harness only ever names a neutral event; only the reducer names a pose.
4. **`HarnessCaps` is declared per adapter** and printed by `remi-hook check` (§6). Support
   is not uniform — Codex-with-`notify` cannot produce `WaitingForInput` — and the user should
   be able to see why a pose never fires on a given host rather than filing it as a bug.
5. **`remi-hook watch` is specified as "the state dir, plus any enabled pull adapters"** even
   though v1 enables none. That is the seam a Codex adapter plugs into, and writing it into
   the contract now costs nothing.

## 4. `remi-core`

The whole of the domain lives here, testable without a display, a broker, or a network.

```
crates/remi-core/src/
├─ lib.rs
├─ state.rs        # PetState
├─ signal.rs       # Signal, ToolClass, Reducer — the neutral vocabulary (§3.5)
├─ record.rs       # SessionRecord, SessionId — the on-disk / over-the-network contract
├─ store.rs        # the state dir: locate, atomic write, read, list, remove, prune
├─ registry.rs     # updates + clock -> menu + rendered state.  Pure. The heart of the crate.
├─ config.rs       # Config load/save
├─ harness/        # how the register gets WRITTEN — one file per harness
│  ├─ mod.rs       # enum Harness, HookInput: which session an event belongs to (+ HarnessCaps, later)
│  ├─ claude.rs    # stdin JSON -> HookInput {session, cwd, transcript}   (push)
│  └─ opencode.rs  # not yet: the plugin passes flags, stdin is never read; SSE fallback later
└─ source/         # how the PET READS — one file per transport
   ├─ mod.rs        # ConnectionId, SessionUpdate
   ├─ local.rs      # StateDirWatch: the state dir's whole listing, again on every change
   ├─ ssh.rs        # stream from `ssh <host> remi-hook watch`, with Stop and reconnection
   ├─ ssh_config.rs # the hosts ~/.ssh/config names, so the menu can offer them
   └─ mqtt.rs       # subscribe to remi/+/+/+/state (rumqttc)
```

`harness/` and `source/` are the two independent axes, and nothing crosses between them: an
adapter produces `Signal`s and never names a transport, a source produces `SessionRecord`s and
never names a harness. Adding a harness touches only `harness/`; adding a transport touches
only `source/`.

### 4.1 `state.rs`

```rust
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PetState { Thinking, Viewing, Writing, Replying, WaitingForInput, Proud, Idle, Offline }
```

`Idle` is never published — it is synthesised locally by the registry from `Proud` decay.
Keeping it in the same enum means the renderer has exactly one input type.

A state names what the agent is doing, not which animation plays, so it is not one-to-one with
the Spine asset. `Replying` has no animation of its own yet and borrows `Writing`'s, `Offline`
borrows `Idle`'s and is told apart by being drawn grey, and several
states may end up sharing one; that mapping is the renderer's (§5.3). The distinction lives in
the record anyway, because the reducer runs in `remi-hook` on every remote: telling two
activities apart later would mean upgrading every host, while changing which animation a state
plays is a change to the pet alone. So `Writing` and `Replying` stay separate even if M2 gives
them the same animation.

### 4.2 `record.rs` — the contract

The same JSON is the file body and the MQTT payload. One schema, one parser, one test suite.

```json
{"v":1,"session":"1a0e02ad-5127-41ae-b140-139fed68bc31","harness":"claude-code","state":"thinking",
 "ts":1757150400,"title":"Creating bin from crate libs","cwd":"remi-desktop","started":1757150100}
```

| field | type | required | notes |
|---|---|---|---|
| `v` | u8 | yes | schema version; a reader ignores records with a `v` it doesn't know |
| `session` | string | yes | the harness's own session id, **full length**; letters, digits, `-`, `_`. The pet may shorten it for display when the prefix is unambiguous |
| `harness` | string | yes | `claude-code` \| `opencode` \| `codex`. Part of identity, names the session file's directory, and a menu label |
| `state` | string | yes | snake_case `PetState` |
| `ts` | i64 | yes | unix seconds, **publisher's clock** |
| `title` | string | no | the session's own title, truncated to 80 chars; absent until the harness has one. Harness placeholders (OpenCode's `New session - <ts>`) are dropped by the adapter, never written |
| `cwd` | string | no | basename only, for the menu label. Never a full path. |
| `started` | i64 | no | first-seen ts, for sorting the menu |
| `_resume` | string | no | **writer-private** (§3.5): the pose to return to when an approval is answered. The pet never reads it |

Unknown fields are ignored on parse (forward compatible). Payload stays under ~300 bytes.

**There is no `host` field.** The machine a session runs on is named by the pet, the way VS
Code names a remote — never by the writer, which cannot know what the user calls it:

- `local` connection → `local`.
- `ssh` connection → the `Host` alias from the connection config (what the user types after
  `ssh`).
- `mqtt` connection → the name the user gave `remi-hook setup --forward … --name <name>` on
  that machine, carried as the `<name>` segment of the topic (§6). One broker serves many
  machines, so this is the one transport where the name has to travel with the record.

**Two clocks, used for two different things** — this is worth stating because getting it
wrong produces bugs that only appear across machines:

- `ts` (publisher's clock) decides **ordering**: last-write-wins between two records for the
  same session.
- The receiver's own monotonic clock, stamped when a record *arrives*, times **`Proud` decay**
  and the menu's "last heard".

So clock skew between the pet's laptop and a remote host can neither cut `Proud` short nor
make a live session look hours old.

### 4.3 `registry.rs` — the only real logic

Pure. No I/O, no async, no clock of its own. Feed it updates with the time they arrived; ask it
two questions with the time now.

```rust
pub struct SessionKey {
    pub connection: ConnectionId,  // config-assigned, e.g. "local", "plume"
    pub harness:    HarnessId,     // one box can run two harnesses at once
    pub session:    SessionId,
}

pub enum Selection { Auto, Pinned(SessionKey) }            // Auto is the default

pub struct Registry {
    sessions:    BTreeMap<SessionKey, StoredSession>,      // record, received, ended
    connections: BTreeMap<ConnectionId, ConnectionStatus>, // Up | Lost { reason }
    selection:   Selection,
}

impl Registry {
    pub fn apply(&mut self, connection: ConnectionId, update: SessionUpdate, now: Instant);
    pub fn select(&mut self, selection: Selection);
    pub fn selection(&self) -> &Selection;

    /// Connection → harness → sessions, newest first. Drives the context menu.
    pub fn menu(&self, now: Instant) -> Vec<ConnectionMenu>;

    /// What to draw. `None` means Remi shows `Offline`.
    pub fn current(&self, now: Instant) -> Option<SessionEntry>;
}

// One session as the UI shows it, computed on every call, because its pose changes with time.
pub struct SessionEntry { key, label, pose, last_heard: Duration, record }
```

The registry stores only facts — each session's record, when it arrived, and when it ended —
and works out everything it shows from those and `now`, so time passing needs no timer inside.

**`apply`:**

- `Upsert` stores the record unless its `ts` is older than the stored one. A tie is taken,
  since `ts` is whole seconds. `Snapshot` replaces the connection's sessions wholesale.
- Hearing an identical record again keeps its arrival time, so a reconnect's resent snapshot
  restarts neither `Proud` nor "last heard".
- A session missing from its connection's next snapshot, or `Removed` over MQTT, has **ended**.
  Nothing on the wire announces it.
- Anything but `ConnectionLost` marks the connection up. A lost connection keeps its sessions.

**Pose**, per session: ended → `Offline`; `Proud` older than 8 s, **or `Proud` the pet did not
witness arrive** → `Idle`; otherwise the state as published.

**`Proud` is the one pose that is really an edge**, and the second rule above is what that costs.
Every other pose is a level that stays true however old it is; `Proud` means *a turn just ended*,
so its eight seconds are timed from when the record arrived. That only stands in for "when the
turn ended" if the pet was there to receive it. A session file left at `proud` — Claude finished a
turn, nobody typed anything since — would otherwise be given a fresh eight seconds every time the
pet launched, congratulating Claude for work it finished hours ago.

So the registry marks each stored session `witnessed`, and a `Proud` that is not is shown `Idle`
from the start. Not witnessed means **delivered while attaching**: the connection was not already
`Up` when the update arrived. That covers a launch's first snapshot and also a reconnect, since
anything written while a source was away is backlog for the same reason. A record resent unchanged
keeps whether it was witnessed, alongside the arrival time it already kept — so a drop and
reconnect cannot retract a `Proud` the pet did see.

This needs no clock comparison and so does not spend the two-clock guarantee (§4.2, decision #18):
`witnessed` is about whether the pet was listening, not about how old anything is.

**No other pose times out.** Hooks write only on events, so a session blocked on an approval
prompt, or running a long tool, is silent for as long as that lasts — a timeout would turn
exactly the waiting pose into `Idle`. A session that dies without ending keeps its last pose:
Auto moves on as soon as any other session is newer, the menu shows how long ago it was last
heard from, and the 24 h prune (§3.2) removes its file.

**Ended sessions** stay listed as `Offline` for 10 s, then are dropped. A **pinned** session is
the exception: it stays, `Offline`, until the user selects something else, because a session
the user chose should never vanish from under them. That is why the registry holds the
selection: whether an ended session may be dropped depends on it.

**Auto** shows the session with the newest `ts` across all connections, ties going to the last
key. It is a deterministic choice, **not** a priority merge — the aggregation problem the brief
§4 removed stays removed, because only ever one session is rendered. When that session ends,
Auto shows it `Offline` for the same 10 s, then moves on to the next newest. Auto is the default
so the pet does something sensible before the user has picked anything. A pin the pet has never
heard of — restored from config for a session that ended while the pet was closed, or on a host
that is not connected — shows `Offline` until the session appears (decided 2026-09-15, replacing
a fallback to Auto): quietly showing some other session would hide that the pin points at nothing,
and `Offline` tells the user to go back to Auto or pick another.

**The menu** lists connections by name, each one's harnesses by id, and each harness's sessions
newest first, so rows don't jump around between rebuilds. A connection with no sessions is still
listed. Labels are chosen here: `title`, else `cwd`, else the session id.

This file has a table-driven unit-test suite over (updates, elapsed) → expected menu and
expected state. It is also what makes a transport swap harmless.

### 4.4 `source/mod.rs` — the transport abstraction

```rust
pub enum SessionUpdate {
    Upsert(SessionRecord),
    Removed { harness: HarnessId, session: SessionId },
    Snapshot(Vec<SessionRecord>),   // full replacement, sent on (re)attach
    ConnectionUp,
    ConnectionLost { reason: String },
}
```

There is no `SessionSource` trait. The desktop matches on each connection's configured kind and
starts that transport's own entry point; a trait would exist only to keep different sources in
one list, which a three-armed `match` already does. Every source funnels into one channel of
`(ConnectionId, SessionUpdate)`, and one task in the desktop owns the `Registry`: it calls
`apply` for each message, ticks at 1 Hz so `Proud` and ended sessions age without a message, and
is the only place that reads the clock. Adding a transport means adding a file here and a config
variant — nothing else in the app moves.

A trait stays a **might-do**. Revisit it once `ssh` and `mqtt` both exist: if their `run` loops
duplicate the same lifecycle — connect, `ConnectionUp`, deliver, `ConnectionLost`, back off,
retry, stop on cancel — pull that shared part out, as a plain function first, and as a trait
only if it genuinely needs to call back into each source.

`local` is `StateDirWatch`. It is synchronous — `notify` delivers events on a thread of its own —
so the desktop runs it on a thread and forwards each listing as a `Snapshot`. It never interprets
file events: any event, after 50 ms for the burst to settle, means "list the directory again",
and only a listing that differs from the last one is reported. Each platform's watcher merges,
reorders and names events differently; a fresh listing is the same everywhere.

| source | mechanism | latency | what the user must set up |
|---|---|---|---|
| `local` | `notify` watch on the state dir; the whole listing on every change | instant | nothing |
| `ssh` | long-lived `ssh <host> remi-hook watch`, a JSON array per change on stdout | instant | nothing beyond being able to `ssh <host>` already |
| `mqtt` | subscribe `remi/+/+/+/state`, retained | instant | a broker |

**Connections are opt-in, and each is the user's decision.** `local` is always on — it needs no
configuring and costs nothing. Every host in the user's `~/.ssh/config` is *listed* in the menu
from the first launch (`source/ssh_config.rs` reads the names, and only the names), but none is
connected to until the user presses Connect on it. Starting a source per host at launch would
spend the pet's first ten seconds reporting `ConnectionLost` for machines nobody asked about,
and would hold an ssh connection open to each of them all day.

Connecting writes the host into the pet's config, which *is* the list of sources started at
launch — so a fresh config watches this machine and nothing else, and everything beyond that is
there because the user asked for it once. Disconnect stops the source and takes it back out. A
connection that drops on its own is *not* disconnected: it keeps retrying, because a remote going
quiet for a while is the case this project exists for.

Whatever is connected runs **concurrently** — that is what populates a menu spanning several
machines. Only the *selected* session is rendered.

#### The `ssh` source is a stream, not a poll

We spawn **one long-lived child process** per configured host:

```
ssh -T -o BatchMode=yes <host> '$HOME/.local/bin/remi-hook watch'
```

`remi-hook watch` on the remote runs the same `StateDirWatch` the local source does, and prints
the state dir's **whole listing** — one JSON array, exactly what `remi-hook snapshot` prints —
once at start and again after every change. The pet applies each line as a `Snapshot`. There is
no delta format: a session that ended is simply missing from the next line. This is the
i3bar/swaybar status protocol, and structurally what VS Code Remote does — ship a small helper
over the ssh channel and read its stdout.

Consequences worth naming:

- **No polling**, so no `active_interval` / `idle_interval` knobs and no tuning.
- **No protocol to invent or version.** Each line is a list of records, and each record carries
  its own format version.
- **Resending everything costs nothing:** about 200 B per session, at ~30 changes a minute.
- **No `ControlMaster` needed**, because there is only ever one connection per host. If we
  ever do need multiplexing, we pass `-o ControlPath=<our own state dir>/cm-%C` on the command
  line — we never write to the user's `~/.ssh/config`.
- Reconnect on exit with exponential backoff (1 s → 30 s), emitting `Connecting` /
  `ConnectionLost` so the menu can say what a host is doing rather than silently dropping it.
  The backoff resets only after a connection that actually delivered a line, so a host that
  fails instantly is not retried every second for ever.
- `BatchMode=yes` so a host needing an interactive passphrase fails fast and visibly instead
  of hanging on a prompt nobody can see.

Four things in `source/ssh.rs` are not in the happy path and are each load-bearing:

⚠️ **Keepalives** (`ServerAliveInterval=15`, `ServerAliveCountMax=3`, `ConnectTimeout=10`).
Without them a host that drops off the network mid-connection leaves the reader blocked in a
kernel read that never completes: the pet keeps showing that machine's last pose for ever and
never retries, which reads exactly like "nothing is happening there" — the one thing this
project must not get wrong.

⚠️ **`ControlMaster=no`.** We honour the user's `~/.ssh/config`, and a `ControlMaster auto` in
it makes an ssh that finds no master fork a *background* one — which inherits our stdout and
outlives the connection. Killing our own child would then leave the pipe open and the reader
stuck on it for ever, leaking a thread per disconnect. Refusing to *be* a master does not stop
us using one that already exists, and §4.4 wants only one connection per host anyway. This was
found by a test that hung, not by reasoning.

⚠️ **The child's stderr is drained continuously**, on a thread of its own, keeping the last two
lines. ssh writes banners, MOTDs and warnings there, and a pipe nobody reads fills up and blocks
the process writing to it — so a remote with a chatty login would *hang* the connection instead
of failing it. Those kept lines are also the failure reason the menu shows, because ssh's own
words (`Permission denied (publickey)`, `Connection refused`, `Host key verification failed`)
name the cause where its exit status is 255 for all of them. Exit 127 is special-cased to
"remi-hook is not installed there", which is the first thing that happens against a new host.

⚠️ **Stopping has to reach a thread blocked on a read**, which no flag can do: `ssh::Stop` holds
the child, and stopping kills it so the read ends at the closed pipe. The same call cuts short a
wait between retries. The disconnection is then reported by the watch thread *as it ends*, not by
whoever called stop — otherwise it could overtake a snapshot that thread had already read, and
put the connection back up with sessions nobody is listening for.

We shell out to the system `ssh` binary rather than using `russh`/`libssh2` **specifically so
the user's existing config applies for free** — `ProxyJump`, `IdentityFile`, agent forwarding,
`known_hosts`, everything. A Rust SSH client would mean reimplementing `ssh_config` parsing
and would be the thing that forced users to configure the tool separately. OpenSSH ships with
macOS and with Windows 10 1803+.

⚠️ **On Windows every child is spawned with `CREATE_NO_WINDOW`** (`spawn::without_a_window`,
called from `ssh_command`). `ssh.exe` is a console program and the pet is a GUI one with no
console to lend it, so Windows gives the child a console of its own — and a console has a
window. Without the flag, a terminal flashes up on every connection *and every retry*, which
against a host that is switched off is twice a minute, for ever. The symptom appears only in a
**release** build: a debug build is not `windows_subsystem = "windows"`, owns a console, and the
child inherits it, so this is a fix that can look verified against the wrong binary. It is a
named helper rather than a `cfg` at the call site because §6.1's remote install will spawn
console programs too. Verified on Windows 2026-09-15; `ControlMaster=no` was checked there at
the same time and is accepted silently, so the option stays unconditional.

We invoke `remi-hook` by **absolute path rather than relying on `$PATH`**: `~/.local/bin` is
frequently missing from a non-interactive ssh shell's `PATH`. Getting the binary there in the
first place is §6.1.

#### Failure modes

These differ more than the happy paths do, and the last row is the one that bites.

| | `local` | `ssh` | `mqtt` |
|---|---|---|---|
| Pet not running | file keeps updating; pet catches up on launch | same; `watch` dies with the connection and is respawned | broker holds the retained message |
| Remote unreachable | — | `ConnectionLost` → host greys out, backoff 1 s → 30 s | last retained state still shown |
| Broker down | — | — | detached publisher fails silently; **the file is still correct**, so falling back to `ssh` loses nothing |
| Machine crashes mid-session | last pose kept, menu shows when it was last heard from; pruned at 24 h | same | last pose kept, and **never deleted** |

That last cell is the one real asymmetry between the transports. File registers clean
themselves up via `unlink`; MQTT's does not, which is why `SessionEnd` must publish an empty
retained payload to delete it (§6). Skip that and ended sessions haunt the menu of every pet
that ever reconnects.

---

## 5. `remi-desktop`

`✅` exists as of M2; everything else is still to be written.

```
crates/remi-desktop/
├─ Cargo.toml           ✅  tauri 2.11.5 (feature `macos-private-api`), tauri-build 2.6.3;
│                             feature `gif-fallback` (off) gates the GIF art — §5.5
├─ build.rs             ✅  tauri_build::build() + asset staging (§5.5)
├─ tauri.conf.json      ✅
├─ capabilities/
│  └─ default.json      ✅  REQUIRED — see §5.1; without it the window cannot be dragged
├─ icons/icon.png       ✅  placeholder (sips'd from 03pride.gif); real art before M4b
├─ src/
│  ├─ main.rs           ✅  wiring only: managed state, the three commands, the menu handler
│  ├─ config.rs         ✅  config.toml load/save, debounced by a writer thread
│  ├─ window.rs         ✅  size and position persistence (the rest is tauri.conf.json)
│  ├─ menu.rs           ✅  the session menu, built from Registry::menu — shared by
│  │                          the window's right-click and, later, the tray
│  ├─ tray.rs               icon + the same menu + quit
│  └─ bridge.rs         ✅  Registry -> webview events, and the menu's snapshot
└─ ui/                  ✅  frontendDist; plain static files, no build step
   ├─ index.html        ✅  #root[data-tauri-drag-region]; the renderer appends its own element
   ├─ app.js            ✅  picks a renderer, mounts it, feeds it states; M2 debug keys
   ├─ renderer/spine.js ✅  the v1 renderer
   ├─ renderer/gif.js   ✅  the fallback, same contract — art gated by `gif-fallback`
   ├─ vendor/spine-webgl.js ✅  4.2.120 IIFE build; provenance in vendor/README.md
   └─ assets/           ✅  staged by build.rs — gitignored, never edited by hand
```

`gen/` and `ui/assets/` are gitignored by `crates/remi-desktop/.gitignore`.

⚠️ **Everything under `ui/` is embedded in the executable** by `generate_context!` at compile time
— paths and contents both, nothing is read from disk at runtime. Two consequences: a frontend edit
needs a `cargo build` to take effect (it does trigger one), and staging a file into `ui/assets/` is
the same decision as shipping it. The second is why §5.5 has a feature gate.

### 5.1 Window configuration

`tauri.conf.json > app.windows[0]`: `transparent: true`, `decorations: false`,
`alwaysOnTop: true`, `shadow: false`, `resizable: false`, `skipTaskbar: true`.

**Click-through is dropped, 2026-09-14.** Brief §10 left it undecided between click-through and
draggable; draggable won, and the two are exclusive — a window that ignores the cursor cannot be
dragged. Dragging is the behaviour a pet needs, so there is no toggle and no config key. The
window's size is a config choice instead (§5.4): `small`, `medium` or `large`, defaulting to
`medium`, because the art's native 360 px turned out to be larger than anyone wants sitting on top
of their screen.

⚠️ **macOS requires `app.macOSPrivateApi: true`** for a genuinely transparent window. That
flag makes the app ineligible for the App Store — irrelevant here (private, personal use) but
it must be set explicitly or transparency silently under-delivers. ✅ Confirmed at M1 — it is
set in `tauri.conf.json` and transparency behaves.

Dragging: `data-tauri-drag-region` on the root element, plus two things that are not
optional and not discoverable from the failure:

⚠️ **`core:window:default` does NOT grant `allow-start-dragging`** — it is read-only window
info (`is-visible`, `outer-position`, `theme`, …). The drag region works by invoking a
`start_dragging` IPC command, so `capabilities/default.json` must list
`core:window:allow-start-dragging` *explicitly*, alongside `core:default`. Omitting the
`capabilities/` directory entirely produces `gen/schemas/capabilities.json == {}` — zero
permissions — and the window is simply immovable with no error anywhere. Verified 2026-09-14.

⚠️ **`generate_context!` panics at compile time if `icons/icon.png` is missing**, even when
`bundle.icon` is `[]`. A placeholder is in the tree; replace it with real art before M4b.

Anything covering the drag region needs `pointer-events: none` — the renderer canvas is
full-bleed, so without it the root element never sees the mousedown.

### 5.2 The session menu

Right-clicking Remi opens a context menu — the user's model is VS Code's remote picker:

```
  ✓ local · Creating bin from crate libs — thinking, 2s
    local · 3f9a1c2e — waiting, just now
  ─────────────────────────────────────────────
    theresa — 2 sessions, 1 waiting           ▸   ✓ dotfiles — waiting, just now
    whisperain — connecting…                  ▸       remi-desktop — writing, 3s
    lappland — disconnected: Permission den…  ▸       ────────────────
    Connect to a host…                        ▸       Disconnect
        └─ plume · bastion · lab-7                    (every host nobody is watching)
  ─────────────────────────────────────────────
    Follow most recent — local · dotfiles       (Selection::Auto; names the session it follows)
  ─────────────────────────────────────────────
    Size                     ▸  Small · Medium · Large
    Settings…                                   (not built yet)
    Quit Remi
```

Each session is labelled `<name>`, where the name is the first of these the record has: `title`,
then `cwd`, then the raw `session` id. The record carries all three as-is and **the pet** picks,
so changing the display rule never requires upgrading `remi-hook` on the remotes. Names are cut
at 44 characters: `title` allows 80, and a menu that wide covers a good part of the screen. A
session listed flat is prefixed with its connection (`local · dotfiles`); one inside a machine's
submenu is not, because the row it opened from already said which machine it is on.

Built fresh from `Registry::menu()` on every popup, not kept and mutated — a native menu cannot
change while it is open, and every session row is time-dependent. Each row shows its pose and how
long ago its session was last heard from, so one that died without ending is recognisable. A
session that ended reads `offline` for 10 s and then leaves the menu — unless it is pinned, in
which case it stays until another session or Auto is chosen (§4.3).

**This machine's sessions are flat and first.** It is always connected, needs no configuring, and
is where most sessions are; one level reads faster than two for the rows the user actually picks
from. The harness is named in a row only where a connection is running more than one, which is the
only case where it tells two rows apart (§3.6's first obligation is about identity, not about
always printing it).

**Every *watched* machine is a submenu**, listing its sessions and then what can be done about
the connection — Cancel while connecting, or Disconnect. A host is something to act on and not
merely a heading, which is what earns the extra level. Each is built from the same
`Registry::menu` snapshot, so a machine with no sessions, one still connecting, and one whose
source has dropped each stay visible with a reason rather than silently vanishing.

**Machines nobody is watching are one level further in**, under a single **Connect to a host…**
row (decided 2026-09-14, replacing a top level that listed every host in `~/.ssh/config`). The top
level is for what is *happening*, and a host nobody is connected to is not that; nine offered hosts
sitting above two real ones buries the sessions the menu exists for. Picking one connects to it and
it moves up to a row of its own, saying `connecting…` and then carrying its sessions — the same
transition Disconnect runs backwards. Where a machine is listed is read straight off its status:
`Disconnected` is exactly "nobody is watching it", and `Connecting` and `Lost` stay up top because
the user asked for them and what they are doing is news. The row is kept even when there is nothing
under it — where hosts come from is otherwise invisible, since an unread `~/.ssh/config` and an
empty one look identical from a menu that just omits the row — and then holds one disabled line
saying so.

⚠️ **What nesting costs is the glance that says which machine wants attention**, so the host's own
row pays it back: `theresa — 2 sessions, 1 waiting`. Those are counts, not a merged state — the
pet renders one session and only one, and no priority rule exists anywhere in the system (brief
§4). The waiting count is there because an agent blocked on the user, on a machine whose submenu
is closed, is the exact thing this project exists to make noticeable.

The selected session is ticked, and clicking a row pins it; **Follow most recent** is ticked
instead under `Selection::Auto`, and is how the user gets back to it. While ticked it also names
the session it is following (`Follow most recent — local · dotfiles`), since no session row can
carry that: a tick there means pinned. Selection persists to config
as either `auto` or a pinned `(connection, harness, session)`. A pin on a session belonging to a
machine that is then disconnected is *kept*, not thrown away: Remi shows `Offline` for a pinned
session it has not heard of, so reconnecting brings the selection back with it. Size applies immediately — the
renderer refits itself to whatever the window is — and persists the same way; a hand-written pixel
count in the config ticks none of the three, which is the truth.

Two mechanics that are not obvious and not discoverable from the failure:

⚠️ **A webview cannot draw a native menu**, so the gesture is JavaScript's and the menu is Rust's:
`ui/app.js` catches `contextmenu`, calls `preventDefault` (which is what suppresses the webview's
own menu) and invokes the `context_menu` command. Tauri's drag script only acts on button 0, so a
right-click never starts a drag.

⚠️ **`context_menu` must be `#[tauri::command(async)]`.** Creating a menu marshals to the main
thread and blocks until it answers, and on macOS the popup then runs a nested event loop there
until the menu is dismissed — so on the main thread it deadlocks rather than failing. `(async)`
puts it on a worker thread. The same constraint is why it takes `AppHandle` by value instead of
`tauri::State`: an off-thread command cannot borrow from the request.

**The tray will carry the identical menu.** Its original justification — that a click-through
window cannot receive a right-click — died with click-through (§5.1). What is left is smaller but
real: the app has no Dock icon (`LSUIElement`), so if the pet is dragged somewhere awkward or a
full-screen app covers it, the tray is the only way to reach the menu or quit. That makes it a
convenience rather than the only way back, which is why it comes after the context menu rather
than with it.

**Installing to a host from the menu is postponed** (decided 2026-09-14). Connect assumes
`remi-hook` is already on the machine and says `remi-hook is not installed there` when it is not,
which is a clear enough answer to act on. The install dialog of §6.1 — pick a target and a
harness, press Install — is still the intended shape, and Disconnect will never run
`remi-hook uninstall`: removing a *host* may offer that, a disconnect never does (§6.2).

### 5.3 The renderer seam

Rendering happens in the webview, so the "thin interface" brief §11 asks for is a JS contract,
not a Rust trait — Rust never learns which renderer is active:

```js
// renderer/*.js each export:
export async function mount(rootEl);        // load assets, take over the canvas/img
export function setState(petState, opts);   // opts: { transition: bool }
export function dispose();
```

**Spine is the v1 renderer** (`renderer/spine.js`, `spine-webgl` vendored). It gives true
8-bit alpha on a transparent window, resolution independence, real `AnimationState` blending,
and the asset's own transition animations (`d_win`, `a_win`) — all things the GIF path
structurally cannot do (brief §11). `renderer/gif.js` is kept as a ~40-line fallback behind
the same contract, for the case where a transparent WebGL canvas turns out not to composite; its
art is gated (§5.5), so it mounts with a message naming the feature rather than showing broken
images in a build that does not carry it.

Animation mapping (brief §2.2): `a`→Idle, `a_win`→Viewing, `b`→Thinking, `c`→Proud,
`d`→Writing, `e`→WaitingForInput. **`a` and `a_win` are swapped** relative to the asset's first
reading, decided 2026-09-14 by watching the pet idle: `a_win` is the pen pick-up and loops like
being patted on the head, which is tolerable in a pose that flashes past during a read and wrong in
the one Remi holds whenever nothing is happening. Pen-in-hand also reads better as "on task".
**`d`→Replying as well** — decided 2026-09-14 after watching
`d_win`, the former candidate: Replying is Claude putting words on the screen, which is near enough
to writing that a separate pose is a distinction without a difference. `light`, `d_win` and `0` are
now all unplayed.

**Framing is the union of the played animations**, sampled at load, not per-animation.
Per-animation framing makes Remi change size on every state change — `c` throws both arms up. The
union covers only what this renderer actually plays, which is worth real pixels: `d_win` reached
~45 world units further left than anything else, so dropping it from the map made Remi ~13% larger
on screen, and `light` would cost another ~15% for a pose nothing maps to.

⚠️ **Measure the framing on a throwaway skeleton, never on the one being drawn.** Posing a skeleton
leaves slot attachments behind, and an animation only resets the slots it keys. Measuring on the
render skeleton left it wearing the last sampled pose's attachments, so the first state to play
inherited whichever of them it did not key itself — observed as a missing mouth on startup that
fixed itself on the first state change.

⚠️ **The atlas is straight alpha** (`leimi.atlas` carries no `pma` flag), so `drawSkeleton` takes
`premultipliedAlpha: false` while the *canvas* is created `premultipliedAlpha: true`. These are not
in conflict: the batcher blends colour `(SRC_ALPHA, ONE_MINUS_SRC_ALPHA)` and alpha separately
`(ONE, ONE_MINUS_SRC_ALPHA)`, so the framebuffer ends up premultiplied, which is what M1 proved
composites. Setting both to the same value is the tempting wrong answer.

The 60 fps loop is real GPU work in an always-on-top window on an 8 GB M2. Mitigations, in
order: pause the loop when the window is fully occluded; drop to a
lower tick rate for static-ish states. (Pausing on `Offline` was the third, and is gone — see the
2026-09-15 note in §9.) Battery impact gets **measured** at M2, not argued
about.

Rust → webview: one Tauri event `pet://state` carrying
`{ state, host, cwd, session }`. Emitted on every registry change **and** on a 1 Hz
tick, so `Proud` decays without a message arriving.

### 5.4 Config

`toml`, in the platform config dir via `directories` (macOS
`~/Library/Application Support/moe.anything.remi/config.toml`).

```toml
renderer  = "spine"            # "spine" | "gif"
size      = "medium"           # "small" (180) | "medium" (260) | "large" (360), or a pixel count
selection = "auto"             # or: selection = { connection = "plume", session = "a1b2c3d4" }

[window]
x = 1620
y = 820
hidden = false                 # the pet put away; she is still reachable from the menu bar

[[connections]]
name = "local"
kind = "local"

[[connections]]
name = "theresa"
kind = "ssh"                   # `host = "…"` only when the ssh name differs from the connection's

[[connections]]
name = "home"
kind = "mqtt"
url      = "mqtts://mqtt.anything.moe:8883"
username = "remi-pet"
# password: OS keychain, never this file
```

A connection is a *source*, not a host — the same physical machine reached two ways is two
connections, and the registry deduplicates nothing, deliberately. If that turns out to be
confusing in practice it is a display concern, not a data-model one.

⚠️ **`connections` is the list of sources started at launch, and nothing else.** It is not the
list of machines the menu offers — that is this list plus every host in `~/.ssh/config` (§4.4).
The default is `local` alone; pressing Connect on a host appends it here and Disconnect removes
it, which is the whole of how a host is remembered across a restart. Editing it by hand works
the same way.

Config is written on change (selection, window move — debounced ~1 s), not only on quit, so a
crash doesn't lose the window position.

### 5.5 Asset staging (`build.rs`)

The Spine files are named `Q蕾米.json` / `Q版蕾米.zip`. Non-ASCII in a URL the webview fetches
is an avoidable class of bug. `build.rs` copies and **renames** into `ui/assets/`:

```
assets/spine-asset/Q蕾米.json  -> ui/assets/remi.json
assets/spine-asset/leimi.atlas -> ui/assets/remi.atlas   (rewrite the page-image line)
assets/spine-asset/leimi.png   -> ui/assets/remi.png
assets/0*.gif                  -> ui/assets/gif/        only with --features gif-fallback
```

The `.atlas` file names its page image on line 1, so the rename requires rewriting that line,
not just the filename.

**The GIF art is behind the `gif-fallback` cargo feature, default off.** Measured 2026-09-14: it is
8.5 MiB, 26% of the binary, for a render path M1 and M2 between them showed macOS does not need.
`ui/renderer/gif.js` ships either way — it is 40 lines, and a seam with one implementation is not a
seam — so turning the feature on is a rebuild, not a port. The case it is still held for is M6:
WebView2 on DWM is a different compositor path from WKWebView, and it is the one place the
fallback's original justification is unresolved. Delete both once M6 passes.

Two traps this staging has already hit, neither of which announces itself:

⚠️ **`cfg!(feature = "gif-fallback")` is always false in a build script** — build scripts are
compiled without their own crate's features. `CARGO_FEATURE_GIF_FALLBACK` in the environment is the
only thing cargo actually sets, and the macro version silently disables the feature forever.

⚠️ **`ui/assets/` must be declared `rerun-if-changed` even though it is the script's *output*.**
Cargo caches one build-script result per feature set, so turning the feature back off finds a fresh
no-feature fingerprint, skips the script, and embeds the art the previous build left on disk — the
feature appears to do nothing. Watching the destination is what makes that mtime invalidate the
cache. Verified by toggling the feature both ways and measuring the binary, not by reasoning.

---

## 6. `remi-hook`

One small binary, deployed to every machine an agent runs on — including the laptop, for the
`local` connection. Claude Code delivers hook input as JSON on stdin and blocks on the process;
the OpenCode plugin invokes the same binary the same way.

```sh
remi-hook signal turn-start --harness claude-code      # a neutral event (§3.5) — the normal entry point
remi-hook signal edit-start --harness claude-code      # read-start / edit-start: the tool's class is the event
remi-hook signal reply --harness claude-code           # reply text is streaming to the user
remi-hook signal approval-asked --harness claude-code
remi-hook signal session-end --harness claude-code     # unlink the session's file
remi-hook signal turn-start --harness opencode --session ses_… --title "…"   # flags override stdin
remi-hook state writing [--session <id>]    # escape hatch: name a pose directly, no reducer
remi-hook watch                             # the snapshot, then again on every change, until stdin closes (ssh source)
remi-hook snapshot                          # one JSON array of live records, then exit
remi-hook check                             # this program's path, the state dir, each harness's hooks; non-zero if anything is wrong
remi-hook setup   --harness claude-code     # add remi's hooks to this machine's harness config; idempotent, keeps a .bak
remi-hook uninstall                         # remove exactly the hooks setup added
```

`remi-hook signal <event>` is what harnesses call. It reads stdin only to pick up the session
id and `cwd` (tolerating stdin being empty or unparseable, and not reading it at all when it
is a terminal or the harness is OpenCode, whose plugin passes flags and may leave stdin open),
applies the reducer from §3.5 —
which is the only thing in the system that names a pose — and writes the file (§3). `--harness`
is required: it decides how stdin is read, and `setup` writes it into the harness config so
nobody types it. `--session` and `--title` override what stdin says; the OpenCode plugin passes
them, and they make testing by hand easy. The folder name has no flag: it comes from stdin's
`cwd`, else the directory the hook was started in.

`remi-hook state <pose>` stays as the escape hatch: it skips the reducer and writes the pose
verbatim, to the session named by `--session` (default `manual`, so a test pose never
overwrites a real agent session). No event ever ends that session: clear it with
`remi-hook signal session-end --harness claude-code --session manual`, or leave it to the
24 h prune (§3.2).
Useful for testing the pet end to end, and for any harness that can run a
command but whose events we have not modelled. It cannot express "return to what you were
doing", so it is not what an adapter should use.

`remi-hook check` prints this program's path, the state dir with its session count, and for
each harness whether every hook `setup` would write is in place — listing the missing ones, and
remi hooks `setup` wouldn't write, such as those from an older version or naming another copy of
`remi-hook`. It exits non-zero on any problem. Printing each adapter's `HarnessCaps` (§3.6) is
still to come: support is not uniform across harnesses, and a pose that never fires should be
legible as a capability gap rather than a bug.

`remi-hook watch` is what the pet runs over ssh (§4.4). It watches **the state dir, plus any
enabled pull adapters** — v1 enables none, but that is the seam a Codex adapter plugs into
(§3.6), and writing it into the contract now costs nothing. It prints the whole listing as one
JSON array per line, flushing each, first at start and then whenever the listing changes (§4.4).
It exits when stdin closes, when stdout stops accepting writes, or on SIGTERM, so a dropped ssh
connection reaps it. It creates the state dir if no session has written yet, and never prunes.

If — and only if — an MQTT forwarder is configured on that machine, the same invocation also
spawns a **detached** child that publishes the record to `remi/<name>/<harness>/<session>/state` (`<name>`
chosen at setup, §4.2) with
`retain=true`, QoS 1. Detached so the TLS handshake never blocks the hook; unordered delivery
is harmless because `ts` decides ordering. On `offline` the child additionally publishes an
empty retained payload to that topic, which is how MQTT deletes a retained message — without
it, ended sessions haunt the menu of every pet that reconnects.

Exit code is **always 0**. A non-zero exit from a `PreToolUse` hook can block the tool call;
a pet must never be able to stop Claude working.

Configured in `~/.claude/settings.json` on each machine — or `$CLAUDE_CONFIG_DIR/settings.json`
when that is set, as Claude Code itself does — by `remi-hook setup`, which writes this block
with the absolute path of the `remi-hook` it ran as (shortened to `remi-hook` here):

```json
{ "hooks": {
  "UserPromptSubmit": [ { "hooks": [ { "type": "command", "command": "remi-hook signal turn-start --harness claude-code", "timeout": 5 } ] } ],
  "PreToolUse": [
    { "matcher": "Read|Grep|Glob", "hooks": [ { "type": "command", "command": "remi-hook signal read-start --harness claude-code", "timeout": 5 } ] },
    { "matcher": "Edit|Write",     "hooks": [ { "type": "command", "command": "remi-hook signal edit-start --harness claude-code", "timeout": 5 } ] }
  ],
  "PostToolUse":        [ { "matcher": "*", "hooks": [ { "type": "command", "command": "remi-hook signal tool-end --harness claude-code", "timeout": 5 } ] } ],
  "PostToolUseFailure": [ { "matcher": "*", "hooks": [ { "type": "command", "command": "remi-hook signal tool-end --harness claude-code", "timeout": 5 } ] } ],
  "PermissionRequest":  [ { "matcher": "*", "hooks": [ { "type": "command", "command": "remi-hook signal approval-asked --harness claude-code", "timeout": 5 } ] } ],
  "Notification": [ { "matcher": "permission_prompt|agent_needs_input|elicitation_dialog|elicitation_url_dialog",
                      "hooks": [ { "type": "command", "command": "remi-hook signal approval-asked --harness claude-code", "timeout": 5 } ] } ],
  "MessageDisplay": [ { "hooks": [ { "type": "command", "command": "remi-hook signal reply --harness claude-code", "timeout": 2 } ] } ],
  "Stop":           [ { "hooks": [ { "type": "command", "command": "remi-hook signal turn-end --harness claude-code", "timeout": 5 } ] } ],
  "StopFailure":    [ { "hooks": [ { "type": "command", "command": "remi-hook signal turn-end --harness claude-code", "timeout": 5 } ] } ],
  "SessionEnd":     [ { "hooks": [ { "type": "command", "command": "remi-hook signal session-end --harness claude-code", "timeout": 5 } ] } ]
} }
```

This is the complete v1 Claude Code configuration — every row of the Claude Code table in §3.5.

**How `setup` edits the file.** The file is the user's, so:

- remi's hooks are recognised by their command — a program named `remi-hook` running `signal` —
  not by a marker. Claude Code's settings schema forbids unknown keys in a hook entry, and a
  file that fails validation opens a "Settings Error" dialog at session start. The same rule
  adopts hooks written by hand before `setup` existed.
- `setup` removes every remi hook, and any group or event list only they were in, then appends
  one group per row above. Everything else — other keys, the user's own hooks, even in a group
  shared with remi's — stays as it was, keys in their original order.
- If the result equals the file as read, nothing is written, so running `setup` again is a
  no-op. Otherwise the new file is written beside the old one and renamed over it, the old
  contents are kept as `settings.json.bak`, the file keeps its permissions, and a symlinked
  settings file is written through so the link survives.
- A file that isn't valid JSON, or whose `hooks` isn't shaped as documented, is refused and
  left untouched.
- `uninstall` is the removal half on its own.

Two matchers carry load and neither is optional:

⚠️ **`Notification` must be matched on notification type.** Bare `Notification` also fires on
`auth_success`, `idle_prompt` and `quota_auto_resume_*`, any of which would put Remi in the
waiting pose for no reason. Full type list and payload notes in brief §5.

⚠️ **`PostToolUse` must match `*`, i.e. every tool, not just the ones that can prompt.** It is
the only event Claude Code emits after an approval is granted, so it is what clears the
waiting pose (§3.5); scoping it to `Edit|Write` would leave the pose stuck whenever a prompt
came from a tool outside that set. Firing it on reads too is harmless — see the reducer rules
in §3.5, where a `ToolEnd` with no approval outstanding just means Claude is deciding what
to do next.

### 6.1 Installation — one script, three ways to run it

The bar to clear: **the user should not have to configure anything they haven't already.**
Adding a host to the pet means typing a name they already `ssh` to.

All configuration is `remi-hook`'s own job, done on the machine it will run on:

```sh
remi-hook setup --harness claude-code    # merge the block above into ~/.claude/settings.json
                --harness opencode       # install the plugin instead (§3.4)
                --forward mqtts://…      # optional: write forwarder config, prompt for the password
                --name plume             #   with --forward: what pets call this machine (topic segment)
                --check                  # then run check and print what a pet will see
```

Nothing ever reaches into a remote's `settings.json` over an ssh heredoc. The laptop's only
jobs are *getting the bytes there* and *invoking a command* — which is what lets one code path
serve all three transports:

| case | who runs it | how |
|---|---|---|
| `local` | the pet | runs its **bundled** `remi-hook setup` directly — the binary ships inside the app bundle, nothing is downloaded and no shell is required |
| `ssh` | the pet | `ssh <host> sh -s -- <flags> < install.sh` — the script goes over stdin, so nothing is ever written to the remote's disk |
| `mqtt` | the user | `curl -fsSL https://…/install.sh \| sh -s -- --forward mqtts://…`, after ssh-ing in themselves |

`install.sh` (published with the release, §7) is the whole download half: resolve `uname -sm`,
fetch the matching static `remi-hook` into `~/.local/bin/`, verify its checksum, then exec
`remi-hook setup` with whatever flags it was passed. A machine that already has the binary
skips straight to `setup`.

The `mqtt` case is manual **because it has to be**: §8 sells MQTT as the transport for hosts
the laptop cannot reach directly, and a host the laptop cannot reach is a host it cannot
install to either. It also needs broker credentials, which belong typed into a prompt on the
machine that will use them rather than pushed from the laptop.

In the GUI this is one dialog: pick a target (this machine, or a host from `~/.ssh/config`),
pick a harness, press Install, read the `--check` output. `remi-hook install --host <name>` is
the same thing for scripting. `install.sh` assumes a POSIX remote; a *Windows* remote is out of
scope for v1, since the remote is the headless-server case (brief §1).

Three things this must get right:

- **Static `linux-musl` builds**, so there is no glibc version to match against the remote.
- **Merge, don't overwrite** `settings.json` — the user has their own hooks — and **tag the
  entries we add**, so uninstall and upgrade are exact edits rather than fuzzy matches against
  a file we don't own.
- **Absolute paths, not `$PATH`.** `~/.local/bin` is frequently absent from a non-interactive
  ssh shell's `PATH`, and whether `bash` sources `~/.bashrc` under `sshd` is distro-dependent.
  `setup` writes the resolved absolute path into `settings.json`, and the ssh source spawns
  `ssh -T <host> '$HOME/.local/bin/remi-hook watch'`. That deletes the entire "installed but
  silent" failure class, which is otherwise the most likely way M4 fails.

**Version skew** is the price of the manual path — the pet cannot silently upgrade a host it
cannot reach. Every record carries its format version `v` (§4.2). When the ssh source meets a
record with a `v` it does not understand, it reports the connection lost with *re-run the
installer* as the reason, rather than misparsing records. It parses each record in a line on
its own, so one unknown record cannot throw away the rest.

### 6.2 Uninstalling

Two different things get called cleanup here, and only one of them should ever be automatic.

- **The installer leaves nothing behind.** It is piped over ssh stdin and never lands on the
  remote's disk, so there is no temp file to reap.
- **The harness config stays until the user removes the host**, which runs `remi-hook
  uninstall` over ssh (`--purge` also removes the binary and the state dir). It is explicitly
  **not** removed when an ssh connection drops. The premise of the project is that the remote
  keeps recording state while the laptop is away (brief §1); tearing the hooks out on
  disconnect would blind the pet during exactly the detached-zellij window it exists for, and
  would race a session mid-tool-call. Disconnect is a fact about the laptop, not about the
  remote.

---

## 7. Building and releasing

Both halves ship from GitHub Actions on a `v*` tag (`.github/workflows/release.yml`; §12.9
settled this on 2026-09-15). The repo is public and the artifacts are public downloads —
`install.sh` is a plain `curl | sh` against a release asset, which only works if they are. That
premise is what `assets/README.md` still has to clear.

**Asset names are a contract, not a convenience.** `install.sh` resolves `uname -sm` straight
to `remi-hook-{linux-x86_64,linux-aarch64,macos-universal}` and fetches them through GitHub's
`releases/latest/download/<name>` alias, so those three names carry no version and renaming one
silently breaks every `curl | sh` in the field. The GUI bundles do carry the tag in their
names — nothing resolves them programmatically, and a human downloading one wants to see which
build it is.

### What gets built

| artifact | targets | runner |
|---|---|---|
| `remi-hook` | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` | `ubuntu-latest` + `cross` |
| `remi-hook` | `x86_64-apple-darwin` + `aarch64-apple-darwin`, `lipo`'d into one universal binary | `macos-14` |
| `remi-hook` | `x86_64-pc-windows-msvc` | `windows-latest` |
| `remi-desktop` | universal `.app` in a `.tar.gz`; `.msi` + NSIS `.exe` | `macos-14`, `windows-latest` |
| `remi-desktop` on Linux | **not shipped.** It compiles and runs there (2026-09-15), but the pet is not a development focus on Linux; positioning, stacking and taskbar hiding rely on KWin rules the user applies by hand (brief §7) | — |
| `install.sh` + `SHASUMS256.txt` | — | attached to the release |

`remi-hook` needs the macOS and Windows targets even though remotes are Linux: the pet bundles
it for its own platform to serve the `local` connection (§6.1). Conversely Linux `remi-hook` is
first-class even though the *pet* is macOS/Windows only (brief §7) — it is the write path, and
the remote is almost always Linux.

### Rules the workflow has to hold

- **The musl builds are verified static in CI** (`ldd` must report *not a dynamic executable*).
  The whole install path rests on this and it regresses silently.
- **`install.sh` verifies `sha256`** against the published `SHASUMS256.txt` before moving
  anything into `~/.local/bin`. It is a `curl | sh`; it should behave like one that respects
  the user.
- **`install.sh` defaults to the latest release**, honours `REMI_VERSION` to pin one, and
  honours `REMI_HOOK_BIN=<path>` to install a locally built binary instead of downloading —
  which is what makes M3 and M4 testable before any release exists.
- **The tag may be a pre-release; the bundle version may not.** `tauri.conf.json` no longer
  carries a `version` at all — it inherits Cargo's, which stays numeric (`0.1.0`) — because WiX
  rejects any pre-release identifier that is not itself numeric, so a `-beta.1` in the *product*
  version fails the msi build outright. `v0.1.0-beta.1` names the release; `0.1.0` names the
  product. A hyphen in the tag is also what makes the workflow pass `--prerelease`.
- **`tauri.conf.json` lists `["app", "msi", "nsis"]` on every platform.** The bundler filters
  package types the host cannot produce rather than erroring on them, so one config serves both
  runners and neither job needs to know what the other builds.
- **`remi-hook --version` prints `CARGO_PKG_VERSION`.** Compatibility itself is gated by the
  record-format `v` (§4.2), bumped independently; that is what the skew check in §6.1 reads.
- **PR builds run `cargo test` + `cargo clippy` on all three crates and build `remi-hook` for
  every target**, but do *not* build the Tauri bundles. Those are slow and only a tag needs them.
- **No `.dmg` — ship the `.app` in a `.tar.gz`** (`tauri build --bundles app`). The `.app`
  bundle itself is non-negotiable: `LSUIElement` (tray-only, no Dock icon) and
  `NSHighResolutionCapable` live in its `Info.plist`. The *dmg* is only a container, and it
  earns nothing while we are unsigned — quarantine propagates out of a dmg exactly as it does
  out of an archive, and Tauri's dmg step drives AppleScript/`hdiutil` to style the window,
  which is the flakiest thing in a headless macOS job. It is also the wrong artifact if the pet
  ever grows the Tauri updater, which consumes `.app.tar.gz`. Revisit when signing lands: a
  stapled notarization ticket on a dmg is what makes first launch clean with no network.

### Signing — deferred, and it will be visible

Unsigned macOS bundles are quarantined by Gatekeeper; unsigned Windows installers draw a
SmartScreen warning. Ad-hoc signing (`codesign -s -`) keeps the app runnable on the machine
that built it and does nothing for a downloaded `.dmg`. A paid Apple Developer ID is the only
real fix; until someone decides to buy one, the release notes carry the
`xattr -dr com.apple.quarantine` incantation — and on macOS 15+ the old right-click→Open
bypass is gone, so the alternative is System Settings → Privacy & Security → Open Anyway.
Signing is also the trigger to reconsider the `.dmg`. §12.

---

## 8. Broker (optional — only for the `mqtt` connection)

Mosquitto on the home server. TLS on 8883 with a real cert for the existing domain;
per-client username/password; ACLs so a machine's hook may only write `remi/<its-host>/#`
and the pet may only read. `deploy/` holds `mosquitto.conf` + `aclfile`.

Not required for a working pet — `local` and `ssh` both need nothing the user does not already have.
MQTT buys instant push from hosts you don't want to poll, and works when the pet can't reach
the host directly.

---

## 9. Milestones

Each has an exit criterion that is *observed*, not reasoned about. The order is chosen so
something end-to-end works before any infrastructure exists.

| # | Milestone | Exit criterion |
|---|---|---|
| **M0** | Workspace scaffold; `remi-core` with `PetState`, `SessionRecord`, `Signal`/`Reducer`, `Registry` + tests | `cargo test -p remi-core` green |
| **M1** | Transparent, borderless, always-on-top Tauri window on macOS, **with a WebGL canvas compositing in it** | Screenshot: pet over a text editor, no chrome, no black box behind the canvas |
| **M2** | Spine renderer: all 7 states switchable from a debug key, with blended transitions | Cycling states looks right ✅. Battery measurement **dropped** from the exit criterion — see below |
| **M3** | `remi-hook signal` + Claude Code adapter + `local` source + session menu, config and window position persist | Run Claude in another terminal on the laptop; pet tracks it. Approve a permission prompt and the waiting pose **clears**. Restart restores position and selection. ✅ |
| **M4** | `ssh` source against a real remote | Detach zellij, run Claude on the remote, pet tracks it; menu lists both machines' sessions. ✅ met against `congestion`, 2026-09-15 |
| **M4b** | CI: a `v*` tag publishes `remi-hook` for all five targets, `install.sh` + `SHASUMS256.txt`, and both GUI bundles | `curl …/install.sh \| sh` on a fresh remote installs, and `remi-hook setup --check` passes there |
| **M5** | Mosquitto deployed; `mqtt` source + hook forwarder | `mosquitto_sub` sees retained messages; pet tracks `plume` with the ssh source disabled |
| **M6** | Windows build + validation | M1 and M3 criteria, on Windows. ✅ met 2026-09-15 |
| **M7** | Autostart on login, both platforms | Reboot, pet is there |
| **M8** | OpenCode adapter (plugin, SSE fallback) | Run OpenCode and Claude Code on the same box; menu lists both, labelled by harness; each drives the right pose |

**Where we are — 2026-09-14.** **M0 and M1 are met; M2 is met apart from the battery number;
M3 is most of the way there.**
`remi-core` has `state`,
`record`, `store`, `signal` (the reducer), `harness` (Claude Code stdin parsing), `registry`,
and `source::local` (`StateDirWatch`), all with unit tests. `remi-hook` implements `signal`,
`state`, `snapshot` and `watch`, and `signal` has run live under real Claude Code hooks on the
Linux dev box. Session titles are not read yet.

**M3 is met, 2026-09-14** — observed on the dev Mac against a real Claude Code session, waiting
pose and all. The tray is **postponed**: its whole job is reaching the menu when the pet is
covered or awkwardly placed, and right-clicking the pet already reaches it.

**M4 is met, 2026-09-15** — the pet was seen following a real Claude Code session on
`congestion` over ssh. **M6 is met the same day**: `remi-desktop` was compiled and run on
Windows and on Linux as well as macOS. The `theresa`/`plume` probing below is superseded —
`congestion` is the host the transport was actually proven against.

**M4's code was written 2026-09-14**, and the description of it still holds.
`source/ssh.rs` watches one host — spawn, read a listing per line, explain the failure,
reconnect with backoff, and stop on demand — with twelve tests that drive it against a local
`sh` instead of a network. `source/ssh_config.rs` reads the host *names* out of the user's own
`~/.ssh/config`, so a machine the user can already reach is one the pet offers. The registry
grew `Connecting` and `Disconnected` statuses and `declare`, the menu grew a submenu per machine
with Connect/Disconnect, and the config's `connections` list became "what to start at launch".
What M4 still owes is the exit criterion: `remi-hook` built and installed on `theresa`, Claude
Code running there, and the pet seen to follow it.

⚠️ **Superseded by the `congestion` run above — kept for the probing method.** Probed 2026-09-14: `plume` has no zellij, no
node and no rust, while `theresa` has zellij 0.45.1, cargo 1.97.0 and git — so `remi-hook` can
be built natively *on* it and M4 needs no cross-compilation, no musl toolchain and no
`install.sh`. Neither machine has Claude Code on it yet. The real ssh invocation was tried
against `theresa` and returns 127 with the shell's own "no such file or directory", which is the
path `explain` turns into *remi-hook is not installed there*.

**The tray, and the earlier note that M3 had everything but it, 2026-09-14.** The pet follows local Claude Code sessions
end to end: `source::local` feeds a registry thread, which emits `pet://state`, and the window
remembers its size and position. Right-clicking Remi now opens the session menu (§5.2) — pick a
session, follow the most recent one, change size, quit — and both choices persist to
`config.toml`.

One thing about the tray is worth keeping, because it is not visible from the outside: **a tray
menu cannot be attached to the icon here.** An attached menu is opened by the OS, which shows
whatever was attached last, and every row of this one is time-dependent — so it would need
rebuilding on a timer for the life of the process. There is no rebuild-before-it-opens hook
either: Tauri dispatches tray events through the event-loop proxy (`app.rs`), so the handler runs
*after* the OS has already opened an attached menu. The tray therefore has to pop the menu itself
on the click event, off the main thread, with the pet focused first — a menu popped by an app
that is not frontmost misbehaves on both platforms.

**M1 passed on the M2 MacBook, 2026-09-14** — observed, not reasoned about. `remi-desktop`
is a real Tauri 2.11.5 shell (`tauri-build` 2.6.3, no `cmake`, no node, no `cargo-tauri`
CLI — `cargo run -p remi-desktop` is the whole dev loop). A WebGL2 canvas clearing to
`rgba(0,0,0,0)` composites correctly inside the transparent webview: **no opaque box behind
it**, and a premultiplied alpha ramp fades cleanly to nothing at the edges, so **true 8-bit
alpha survives the compositor** — the property GIF's 1-bit transparency structurally cannot
give us (brief §11). Borderless, shadowless, always-on-top and draggable all confirmed by
eye. The probe lived at `ui/m1-webgl.js`; it was deleted when `renderer/spine.js` landed.

**M2 renders, 2026-09-14** — watched on the M2 MacBook, one frame at a time. `renderer/spine.js`
draws the real skeleton through the vendored 4.2.120 runtime: all seven states switch from the
digit keys, transitions blend bone-for-bone rather than dissolving, and the alpha at the hair and
ribbon edges is clean on a transparent always-on-top window. The startup missing-mouth bug (§5.3)
was found by eye here and fixed.

**The battery measurement is deliberately skipped, 2026-09-14.** It was M2's second exit criterion
and it is being dropped, not deferred-and-forgotten: a working GUI that tracks local Claude is worth
more than a power number for a pet nobody is running yet, and the number is only actionable once
there is something to run all day. The loop is an unthrottled `requestAnimationFrame` with the one
mitigation that needs no help from Rust — stop on `visibilitychange`, and stop entirely once
`Offline` has finished fading out. Occlusion-pausing (covered, not hidden) is a Tauri event and is
the first thing to reach for if it turns out to matter. Revisit after M3, when the pet is actually
in front of someone all day.

**`Offline` draws a resting Remi, greyed — it does not draw nothing, 2026-09-15.** Reported from
use: ending a Claude Code session left Remi frozen on screen. `renderer/spine.js` handled
`Offline` with `setEmptyAnimation`, on this document's assumption that an empty track draws
nothing. It does not: mixing to an empty animation returns the skeleton to its **setup** pose,
and this asset's setup pose is a fully visible Remi — 51 of 199 slots carry a setup attachment,
and animation `0` is that pose and is empty. The loop then stopped exactly as specified and left
the still frame up for as long as no session was running.

Drawing nothing was reachable — fade `skeleton.color.a` to 0 — and was **rejected**, because the
tray is postponed (§5.2) and the session menu is therefore reachable only by right-clicking Remi:
a pet that vanishes when the last session ends is a pet with no way back to its own menu. So
`Offline` plays `a`, the empty-handed rest, and the grey `[data-live="false"]` treatment was
widened to `[data-state="offline"]` as well — one CSS rule, both renderers, the §5.3 seam still
three functions. The cost is the loop: `Offline` animates, so it no longer stops for it, and
`visibilitychange` is the one mitigation left. **Revisit when the tray lands** — with a tray there
is a way back, and fading out becomes the better answer as well as the cheaper one.

**This closes the stack question.** §0.1 and brief §3's `egui` fallback, and §0.3's
`renderer/gif.js` fallback, are no longer on the critical path — neither was needed. M2 is
unblocked: port the `tools/spine-viewer` render loop into `renderer/spine.js` and measure
idle battery cost.

Later, unsequenced: `tray.rs` (above); Codex adapter (§3.6); a shared source lifecycle, or a
source trait, once `ssh` and `mqtt` exist (§4.4); installing `remi-hook` to a host from the menu
(§5.2, §6.1); returning a Claude Code session to idle after Ctrl+C/Esc, which fires no hook,
by spotting the `[Request interrupted by user…]` line in its transcript from `StateDirWatch`;
local IME indicator; phone push on `WaitingForInput`; `remi-agent` with a
persistent connection for real LWT; `07other-to-view` re-fetch or re-export.

**M8 is deliberately last and deliberately cheap.** It is the test of whether §3.5 actually
holds: if adding OpenCode costs more than one file under `harness/` plus a config variant, the
neutral vocabulary is wrong and it is better to find that out on the harness we have verified
than on the one we have not.

**M1 is the gate.** It is second because it is the only step that can invalidate the stack
choice, and it must include the WebGL case — a transparent canvas inside a transparent webview
is a harder problem than a transparent `<img>`, and Spine is now the v1 renderer, not a v1.1
aspiration.

**M3 is the first genuinely useful build**, and it needs no broker, no server, and no network.

**M4b gates M5, not M4.** The `mqtt` transport is the one the user installs by hand, so it is
the first milestone that needs a public `install.sh` to exist. Everything before it uses
`REMI_HOOK_BIN=` against a locally built binary (§7).

---

## 10. Testing

- `remi-core`: real unit tests. `registry.rs` table-driven (`Proud` decay, a `Proud` found on
  attach or during an outage starting already faded while a resent one does not, no timeout on any other pose, auto-resolution,
  ended sessions shown `Offline` and then dropped, a pinned session staying after it ends, two connections reporting the same host, **one host running
  two harnesses**). `record.rs` round-trip + forward-compat (unknown field, unknown `v`).
  `signal.rs` table-driven over (sequence of `Signal`s) → expected poses — in particular that
  an approval answered mid-edit returns to `Writing` and not to `Thinking`, and that the same
  sequence expressed in Claude Code events and in OpenCode events produces the same poses.
- `remi-hook`: a temp-dir test for the write/prune/unlink lifecycle; a `snapshot` golden test.
  Plus a manual check that a hook cannot block Claude — point the forwarder at an unreachable
  broker and confirm Claude is unaffected.
- `remi-desktop`: no automated UI tests. The milestone exit criteria are the tests.

---

## 11. Not in v1

Linux (brief §7) · Touch Bar (brief §6) · rendering more than one session at once ·
real LWT (brief §4) · local IME indicator · phone push · **Codex** (§3.6).

Codex is deferred, not rejected: the five obligations in §3.6 exist so it stays a new file
under `harness/` rather than a redesign.

Naming: repo and product are **remi-desktop**; bundle id `moe.anything.remi`. The old
`touchbar-remi` name is retired.

---

## 12. Open decisions

1. **Does the pet ever show more than the selected session?** Currently no — one session is
   rendered and the rest live in the menu. A small badge ("2 other sessions waiting") is the
   obvious middle ground, and would want a rule for what counts as worth surfacing. Half of this
   now exists in the menu: a machine's row carries how many of its sessions are waiting (§5.2).
   What is still open is whether anything reaches the *pet* itself.
2. **Does the local source need a doorbell socket?** File + `notify` is the v1 answer (§3.1).
   If FSEvents coalescing on macOS adds visible lag at M3, the fix is an optional unix-socket
   poke alongside the write. Measure before adding.
3. **Windows dev/test machine** — ✅ M6 met 2026-09-15; the first run found the console-window
   bug above and it is fixed. **The release-build check that `CREATE_NO_WINDOW` actually works
   was run and passed, 2026-09-15**: spawning `ssh` opened no console window, and the pet took
   no taskbar button. That was the only verification available for it — `Command` exposes no
   getter for creation flags, so there is nothing to assert in a test. What remains open is
   *regression* coverage rather than a first run: the `ssh` source's tests are Unix-only (they
   drive `sh`), and this check will silently stop being performed the moment nobody remembers
   to do it by hand. `.github/workflows/ci.yml` now at least runs `cargo test` on a
   Windows runner, which is what keeps the Unix-only gap from widening silently.
4. **Where Mosquitto runs**, and cert/auth specifics (brief §10). Gates M5 only.
5. **Do Codex approval requests reach the rollout log?** Unverified — the sampled session ran
   a permissive sandbox and was never asked anything (§3.6). Ten-minute experiment when Codex
   comes back on the table: set `approval_policy = "untrusted"`, trigger a prompt, grep the
   new rollout file. If they do not appear, Codex ships without `WaitingForInput` and that
   should be known before the adapter is written, not after.
6. **Code signing.** Deferred (§7). An Apple Developer ID is the only thing that removes the
   Gatekeeper quarantine from a downloaded `.dmg`, and it costs money; Windows SmartScreen
   wants an EV cert or download reputation. Until then the release notes carry the
   `xattr` incantation, which is a real tax on anyone who is not the author.
7. **How does `remi-hook` get onto a host, before §6.1's installer exists?** By hand, and for
   `theresa` that means building it there (it has cargo). The pet says *remi-hook is not
   installed there* when it is missing, which is enough to act on. Installing from the menu is
   postponed rather than dropped (§5.2).
8. **Does `install.sh` need to handle a non-`~/.local/bin` layout?** It assumes a writable
   `~/.local/bin` and `$HOME`. A remote where that is not true (a locked-down shared box)
   would need `--prefix`. Cheap to add; not worth guessing at before someone hits it.
9. **Where does the release actually build and publish?** ✅ **Resolved 2026-09-15: GitHub.**
   The project migrates off self-hosted Forgejo, so §7's matrix applies verbatim —
   `.github/workflows/release.yml` builds it and `install.sh` resolves
   `github.com/<owner>/remi-desktop/releases/latest/download/…`.

   The art question behind it is **half** resolved, 2026-09-15. Provenance is now recorded
   (`assets/README.md`): the character is *Zenless Zone Zero*'s, whose fan-content terms allow
   non-commercial use, and the GIF/Spine animation is 森哈_Yeah's on bilibili. A non-affiliation
   disclaimer ships in the repo, the README and the release notes. What is **not** settled is
   the stated term itself — *仅供个人使用* is personal-use-only, which is stricter than
   non-commercial, and publishing binaries with the art embedded in them is not personal use by
   any reading. Ask 森哈_Yeah before the first public release; a "yes, go ahead" in a bilibili
   reply is worth more than any wording we choose here.
