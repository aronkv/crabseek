#!/usr/bin/env sh
# Removes seekr.
#
#   scripts/uninstall.sh            # remove the binary, keep login and settings
#   scripts/uninstall.sh --purge    # also remove login, settings, logs,
#                                   # download list and share cache
#
# Downloaded music is never touched.
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin/seekr"

if [ -e "$BIN" ]; then
    rm -f "$BIN"
    echo "removed $BIN"
else
    echo "no seekr binary at $BIN (set PREFIX if you installed elsewhere)"
fi

CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/seekr"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/seekr"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}/seekr"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/seekr"

if [ "${1:-}" != "--purge" ]; then
    echo "kept your login and settings in $CONFIG"
    echo "run 'scripts/uninstall.sh --purge' to remove them as well"
    exit 0
fi

echo
echo "This deletes (your downloaded music stays):"
for dir in "$CONFIG" "$DATA" "$STATE" "$CACHE"; do
    [ -e "$dir" ] && echo "  $dir"
done
printf "Continue? [y/N] "
read -r answer
case "$answer" in
    y | Y | yes | igen)
        rm -rf "$CONFIG" "$DATA" "$STATE" "$CACHE"
        echo "removed seekr's data"
        ;;
    *) echo "kept seekr's data" ;;
esac
