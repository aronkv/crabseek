#!/usr/bin/env sh
# Builds crabseek and installs it to $PREFIX/bin (default: ~/.local/bin).
#
#   scripts/install.sh              # install or update
#   PREFIX=/usr/local sudo -E scripts/install.sh
#   scripts/uninstall.sh [--purge]  # remove it again
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin/crabseek"
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--uninstall" ]; then
    exec scripts/uninstall.sh
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found – install Rust first (e.g. 'sudo pacman -S rustup && rustup default stable')" >&2
    exit 1
fi

cargo build --release --locked
install -Dm755 target/release/crabseek "$BIN"
echo "installed $BIN"

# The project used to be called seekr; remove that old binary, but only if
# it is really ours (another project ships a program called seekr).
OLD="$PREFIX/bin/seekr"
if [ -x "$OLD" ] && "$OLD" --help 2>/dev/null | grep -q "Soulseek"; then
    rm -f "$OLD"
    echo "removed the old $OLD (seekr is now called crabseek)"
fi

case ":$PATH:" in
    *":$PREFIX/bin:"*) echo "run it with: crabseek" ;;
    *)
        echo
        echo "note: $PREFIX/bin is not on your PATH yet. Add it, e.g.:"
        echo "  fish:      fish_add_path $PREFIX/bin"
        echo "  bash/zsh:  echo 'export PATH=\"$PREFIX/bin:\$PATH\"' >> ~/.bashrc"
        ;;
esac
