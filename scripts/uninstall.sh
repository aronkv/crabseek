#!/usr/bin/env sh
# Removes crabseek completely: the binary, your login and settings, the
# download list, the share cache and the logs. Downloaded music is never
# touched.
#
#   scripts/uninstall.sh        # asks once before deleting
#   scripts/uninstall.sh -y     # no question
set -eu

answer_given="${1:-}"
PREFIX="${PREFIX:-$HOME/.local}"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/crabseek"

# Stop it if it runs in the background, and drop the systemd user unit.
"$PREFIX/bin/crabseek" stop >/dev/null 2>&1 || true
UNIT="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/crabseek.service"
if [ -e "$UNIT" ] && command -v systemctl >/dev/null 2>&1; then
    systemctl --user disable --now crabseek >/dev/null 2>&1 || true
fi

# The targets become the positional parameters, so paths with spaces stay
# whole.
set --
for path in \
    "$PREFIX/bin/crabseek" \
    "$UNIT" \
    "$CONFIG" \
    "${XDG_DATA_HOME:-$HOME/.local/share}/crabseek" \
    "${XDG_STATE_HOME:-$HOME/.local/state}/crabseek" \
    "${XDG_CACHE_HOME:-$HOME/.cache}/crabseek"; do
    [ -e "$path" ] && set -- "$@" "$path"
done

if [ $# -eq 0 ]; then
    echo "crabseek is not installed (set PREFIX if you installed it elsewhere)"
    exit 0
fi

echo "This removes crabseek (your downloaded music stays):"
for path in "$@"; do
    echo "  $path"
done
if [ -e "$CONFIG/config.toml" ]; then
    echo
    echo "Your saved Soulseek password is deleted too. Soulseek has no password"
    echo "reset, so make sure you know it if you want to keep the account."
fi

if [ "$answer_given" != "-y" ] && [ "$answer_given" != "--yes" ]; then
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

rm -rf -- "$@"
echo "crabseek removed"
