#!/bin/sh
# The end-to-end flow: ticket -> candidate -> push -> pull request -> real CI
# (microVM) -> the owner's review and merge in the GUI -> ticket done. See
# integration/guildbotics/e2e_flow.py.
#
#   scripts/e2e-flow.sh /path/to/GuildBotics [--keep]
#
# Needs: a GuildBotics checkout with the `branvil` provider and its synced
# environment (.venv); msb 0.6.17 and the alpine image its pipeline names in
# Branvil's image cache (`branvil runner pull ...`, see docs/ci.md); web/node_modules
# with Playwright; Google Chrome (or BRANVIL_E2E_CHANNEL= for Playwright's
# Chromium). It starts its own service on a free loopback port and removes
# its temporary directory afterwards.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
guildbotics=${1:?usage: $0 /path/to/GuildBotics [--keep]}
shift
guildbotics=$(cd "$guildbotics" && pwd)

if [ ! -f "$here/web/dist/index.html" ]; then
  (cd "$here/web" && npm run build)
fi
cargo build --quiet --manifest-path "$here/Cargo.toml"

exec "$guildbotics/.venv/bin/python" -I "$here/integration/guildbotics/e2e_flow.py" \
  --bin "$here/target/debug/branvil" --web "$here/web" "$@"
