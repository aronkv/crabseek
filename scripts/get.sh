#!/usr/bin/env sh
# Installs the latest crabseek release (prebuilt, x86_64 Linux) into
# ~/.local/bin, after checking its SHA-256 sum.
#
#   curl -fsSL https://raw.githubusercontent.com/aronkv/crabseek/main/scripts/get.sh | sh
#
# Options (after `sh -s --`): --version 0.1.1, --uninstall.
# Environment: PREFIX (default ~/.local).
set -eu

REPO="aronkv/crabseek"
PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin/crabseek"
VERSION=""
UNINSTALL=""

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2#v}"; shift 2 ;;
        --uninstall) UNINSTALL=1; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

say() { printf '%s\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "$1 is required"; }

need curl
need tar
need sha256sum

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if [ -n "$UNINSTALL" ]; then
    # The uninstaller asks once; read the answer from the terminal, since
    # stdin is this script when it comes through a pipe.
    curl -fsSL "https://raw.githubusercontent.com/$REPO/main/scripts/uninstall.sh" -o "$TMP/uninstall.sh"
    PREFIX="$PREFIX" sh "$TMP/uninstall.sh" </dev/tty
    exit 0
fi

ARCH="$(uname -m)"
[ "$(uname -s)" = Linux ] || fail "prebuilt binaries are for Linux only; build from source instead"
[ "$ARCH" = x86_64 ] || fail "no prebuilt binary for $ARCH yet; build from source: https://github.com/$REPO#installation"

# The binary needs glibc 2.35 or newer.
GLIBC="$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}')"
if [ -n "$GLIBC" ]; then
    major="${GLIBC%%.*}"; minor="${GLIBC#*.}"; minor="${minor%%.*}"
    if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 35 ]; }; then
        fail "glibc $GLIBC is too old (2.35+ needed); build from source instead"
    fi
fi

if [ -z "$VERSION" ]; then
    VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)"
    [ -n "$VERSION" ] || fail "could not find the latest release"
fi

NAME="crabseek-$VERSION-x86_64-linux"
URL="https://github.com/$REPO/releases/download/v$VERSION"
say "downloading crabseek $VERSION..."
curl -fsSL "$URL/$NAME.tar.gz" -o "$TMP/$NAME.tar.gz" || fail "download failed: $URL/$NAME.tar.gz"
curl -fsSL "$URL/$NAME.tar.gz.sha256" -o "$TMP/$NAME.tar.gz.sha256" || fail "checksum download failed"
(cd "$TMP" && sha256sum -c --quiet "$NAME.tar.gz.sha256") || fail "checksum mismatch – not installing"
tar -xzf "$TMP/$NAME.tar.gz" -C "$TMP"

mkdir -p "$PREFIX/bin"
install -m755 "$TMP/$NAME/crabseek" "$BIN"
say "installed $BIN"

# A systemd user unit for background mode at login (installed, not enabled).
if [ "$PREFIX" = "$HOME/.local" ] && command -v systemctl >/dev/null 2>&1 &&
    [ -f "$TMP/$NAME/crabseek.service.in" ]; then
    UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    mkdir -p "$UNIT_DIR"
    sed "s|@BIN@|$BIN|g" "$TMP/$NAME/crabseek.service.in" > "$UNIT_DIR/crabseek.service"
    systemctl --user daemon-reload 2>/dev/null || true
fi

"$BIN" --version >/dev/null 2>&1 || fail "the installed binary does not run on this system"

case ":$PATH:" in
    *":$PREFIX/bin:"*) say "run it with: crabseek" ;;
    *)
        say ""
        say "note: $PREFIX/bin is not on your PATH yet. Add it, e.g.:"
        say "  fish:      fish_add_path $PREFIX/bin"
        say "  bash/zsh:  echo 'export PATH=\"$PREFIX/bin:\$PATH\"' >> ~/.bashrc"
        ;;
esac
