#!/bin/sh
# Check that GuildBotics' `branvil` provider and this Branvil still work
# together: tickets and their relations, a push with a pull request and its
# merge, and checks deciding readiness, each against its own temporary
# `branvil serve` (debug build). See integration/guildbotics/test_smoke.py.
#
#   scripts/guildbotics-smoke.sh /path/to/GuildBotics [pytest args...]
#
# The GuildBotics checkout must have its environment synced
# (`uv sync --extra test --extra dev`). It prints the commits of both
# checkouts, then one line per path.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
guildbotics=${1:?usage: $0 /path/to/GuildBotics [pytest args...]}
shift
guildbotics=$(cd "$guildbotics" && pwd)

echo "Branvil $(git -C "$here" rev-parse HEAD)"
echo "GuildBotics $(git -C "$guildbotics" rev-parse HEAD)"
cargo build --quiet --manifest-path "$here/Cargo.toml"
target=$(cargo metadata --quiet --format-version 1 --no-deps --manifest-path "$here/Cargo.toml" |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

# A short traceback: the long one prints the arguments of the failing calls,
# the services' tokens among them.
cd "$here"
BRANVIL_BIN="$target/debug/branvil" \
  exec "$guildbotics/.venv/bin/python" -I -m pytest -p no:cacheprovider -v --tb=short \
    -o timeout=300 integration/guildbotics/test_smoke.py "$@"
