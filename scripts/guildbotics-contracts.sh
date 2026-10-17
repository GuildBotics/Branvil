#!/bin/sh
# Run GuildBotics' port contracts (code host and tickets) for the `branvil`
# provider against a real `branvil serve` (debug build) instead of the
# Python double. See integration/guildbotics/branvil_live.py.
#
#   scripts/guildbotics-contracts.sh /path/to/GuildBotics [pytest args...]
#
# The GuildBotics checkout must contain the `branvil` provider and have its
# environment synced (`uv sync --extra test --extra dev`). Every selected
# contract must pass; a skipped contract fails the run.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
guildbotics=${1:?usage: $0 /path/to/GuildBotics [pytest args...]}
shift
guildbotics=$(cd "$guildbotics" && pwd)

cargo build --quiet --manifest-path "$here/Cargo.toml"

cd "$guildbotics"
BRANVIL_LIVE=1 \
BRANVIL_BIN="$here/target/debug/branvil" \
PYTHONPATH="$here/integration/guildbotics${PYTHONPATH:+:$PYTHONPATH}" \
  exec uv run --no-sync python -m pytest -p branvil_live -p no:cacheprovider \
    tests/guildbotics/integrations/contracts/test_code_hosting_contract.py \
    tests/guildbotics/integrations/contracts/test_ticket_service_contract.py \
    -k branvil -rs "$@"
