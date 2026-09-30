#!/usr/bin/env sh
# Builds seekr and installs it to $PREFIX/bin (default: ~/.local/bin).
#
#   scripts/install.sh              # install or update
#   PREFIX=/usr/local sudo -E scripts/install.sh
#   scripts/uninstall.sh [--purge]  # remove it again
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin/seekr"
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--uninstall" ]; then
    exec scripts/uninstall.sh
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found – install Rust first (e.g. 'sudo pacman -S rustup && rustup default stable')" >&2
    exit 1
fi

cargo build --release --locked
install -Dm755 target/release/seekr "$BIN"
echo "installed $BIN"

case ":$PATH:" in
    *":$PREFIX/bin:"*) echo "run it with: seekr" ;;
    *)
        echo
        echo "note: $PREFIX/bin is not on your PATH yet. Add it, e.g.:"
        echo "  fish:      fish_add_path $PREFIX/bin"
        echo "  bash/zsh:  echo 'export PATH=\"$PREFIX/bin:\$PATH\"' >> ~/.bashrc"
        ;;
esac
