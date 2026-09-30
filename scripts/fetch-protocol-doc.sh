#!/usr/bin/env sh
# Downloads the Nicotine+ Soulseek protocol documentation to docs/ for
# local reference while developing. It is GPL-3.0 licensed and therefore
# not committed to this MIT repository.
set -eu
cd "$(dirname "$0")/.."
mkdir -p docs
curl -fsSL -o docs/SLSKPROTOCOL.md \
    https://raw.githubusercontent.com/nicotine-plus/nicotine-plus/master/doc/SLSKPROTOCOL.md
echo "saved docs/SLSKPROTOCOL.md"
