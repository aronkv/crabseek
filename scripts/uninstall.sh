#!/usr/bin/env sh
# Removes crabseek completely: the binary, your login and settings, the
# download list, the share cache and the logs. Downloaded music is never
# touched.
#
#   scripts/uninstall.sh        # asks once before deleting
#   scripts/uninstall.sh -y     # no question
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/crabseek"

targets=""
for path in \
    "$PREFIX/bin/crabseek" \
    "$CONFIG" \
    "${XDG_DATA_HOME:-$HOME/.local/share}/crabseek" \
    "${XDG_STATE_HOME:-$HOME/.local/state}/crabseek" \
    "${XDG_CACHE_HOME:-$HOME/.cache}/crabseek"; do
    [ -e "$path" ] && targets="$targets $path"
done

if [ -z "$targets" ]; then
    echo "crabseek is not installed (set PREFIX if you installed it elsewhere)"
    exit 0
fi

echo "This removes crabseek (your downloaded music stays):"
for path in $targets; do
    echo "  $path"
done
if [ -e "$CONFIG/config.toml" ]; then
    echo
    echo "Your saved Soulseek password is deleted too. Soulseek has no password"
    echo "reset, so make sure you know it if you want to keep the account."
fi

if [ "${1:-}" != "-y" ] && [ "${1:-}" != "--yes" ]; then
    printf "Remove everything? [y/N] "
    read -r answer
    case "$answer" in
        y | Y | yes | igen | i) ;;
        *)
            echo "nothing removed"
            exit 0
            ;;
    esac
fi

for path in $targets; do
    rm -rf "$path"
done
echo "crabseek removed"
