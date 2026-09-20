#!/bin/sh
#
# install.sh — put `remi-hook` on this machine and configure a harness to call it.
#
# This is the download half of plan §6.1; `remi-hook setup` is the configuration half, and it
# always runs *on* the machine it configures. Nothing here reaches into a settings file.
#
#   curl -fsSL https://github.com/un-lock-able/remi-desktop/releases/latest/download/install.sh \
#     | sh -s -- --harness claude-code --check
#   ssh <host> sh -s -- --harness codex --check < install.sh
#
# Every argument is forwarded verbatim to `remi-hook setup`, so this script never has to learn
# what that command's flags mean. `--harness` is one of them and has no default here either:
# which agent a machine runs is the one thing an installer must not guess.
#
# Environment:
#   REMI_VERSION   release tag to install (default: the latest release)
#   REMI_HOOK_BIN  path to a locally built remi-hook to install instead of downloading; this is
#                  what makes the install path testable before any release exists
#   REMI_REPO      owner/name on GitHub, if you forked it
#   REMI_PREFIX    directory to install into (default: $HOME/.local/bin)
#
set -eu

REPO="${REMI_REPO:-un-lock-able/remi-desktop}"
PREFIX="${REMI_PREFIX:-$HOME/.local/bin}"
TARGET="$PREFIX/remi-hook"

die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*" >&2; }

# ---------------------------------------------------------------- which build do we want

# Only the POSIX remotes are resolvable here by construction: a Windows host cannot run this
# script, and plan §6.1 puts Windows remotes out of scope — the remote is the headless-server
# case. macOS appears because the *laptop* also needs a hook for its own `local` connection.
asset_name() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Darwin) echo "remi-hook-macos-universal" ;;
        Linux)
            case "$arch" in
                x86_64 | amd64) echo "remi-hook-linux-x86_64" ;;
                aarch64 | arm64) echo "remi-hook-linux-aarch64" ;;
                *) die "no prebuilt remi-hook for Linux $arch — build it from source with cargo" ;;
            esac
            ;;
        *) die "no prebuilt remi-hook for $os — build it from source with cargo" ;;
    esac
}

# `latest/download/<asset>` is GitHub's own alias and saves an API call, which also keeps this
# script working against a rate-limited or unauthenticated network.
release_url() {
    if [ -n "${REMI_VERSION:-}" ]; then
        echo "https://github.com/$REPO/releases/download/$REMI_VERSION/$1"
    else
        echo "https://github.com/$REPO/releases/latest/download/$1"
    fi
}

# ---------------------------------------------------------------- fetching and verifying

fetch() {
    # $1 url, $2 destination. curl on almost everything, wget on the minimal images that have
    # only that.
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2" || return 1
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1" || return 1
    else
        die "neither curl nor wget is installed"
    fi
}

sha256_of() {
    # coreutils on Linux, BSD `shasum` on macOS. Both print "<hash>  <path>".
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "neither sha256sum nor shasum is installed — cannot verify the download"
    fi
}

# A `curl | sh` that skips verification is asking the user to trust the network as well as the
# publisher. This one refuses to install a binary whose hash is not in the release's
# SHASUMS256.txt, rather than warning and continuing.
verify() {
    # $1 downloaded file, $2 asset name, $3 sums file
    expected="$(grep -E "[[:space:]]\*?$2\$" "$3" | cut -d' ' -f1 | head -n1)"
    [ -n "$expected" ] || die "$2 is not listed in SHASUMS256.txt — wrong release, or a partial upload"
    actual="$(sha256_of "$1")"
    [ "$expected" = "$actual" ] || die "checksum mismatch for $2: expected $expected, got $actual"
}

# ---------------------------------------------------------------- install

mkdir -p "$PREFIX" || die "cannot create $PREFIX"

if [ -n "${REMI_HOOK_BIN:-}" ]; then
    [ -f "$REMI_HOOK_BIN" ] || die "REMI_HOOK_BIN=$REMI_HOOK_BIN does not exist"
    say "installing remi-hook from $REMI_HOOK_BIN"
    cp "$REMI_HOOK_BIN" "$TARGET.tmp"
else
    asset="$(asset_name)"
    tmp="$(mktemp -d)"
    # The temp dir goes away whatever happens, including the die() paths above and below.
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "downloading $asset"
    fetch "$(release_url "$asset")" "$tmp/$asset" \
        || die "could not download $asset — check REMI_VERSION, or that the release has that asset"
    fetch "$(release_url SHASUMS256.txt)" "$tmp/SHASUMS256.txt" \
        || die "could not download SHASUMS256.txt"
    verify "$tmp/$asset" "$asset" "$tmp/SHASUMS256.txt"

    cp "$tmp/$asset" "$TARGET.tmp"
fi

chmod 755 "$TARGET.tmp"
# rename(2), so a hook that fires mid-install runs either the old binary or the new one and
# never a half-written file.
mv "$TARGET.tmp" "$TARGET"
say "installed $TARGET"

# ---------------------------------------------------------------- configure

# The binary is in place by now; only the harness configuration is left, and that needs to be
# told which harness. Saying so is better than configuring the wrong agent silently.
if [ "$#" -eq 0 ]; then
    say "installed, but no harness was configured."
    die "pass --harness claude-code or --harness codex, e.g. \`sh -s -- --harness claude-code --check\`"
fi

# By absolute path, never by name: `~/.local/bin` is frequently missing from a non-interactive
# ssh shell's PATH, which is the whole "installed but silent" failure class (plan §6.1).
exec "$TARGET" setup "$@"
