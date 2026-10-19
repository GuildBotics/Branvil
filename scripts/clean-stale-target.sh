#!/usr/bin/env bash
# Remove a Cargo target directory built under other build conditions.
#
#   scripts/clean-stale-target.sh <target-dir>         (before cargo build)
#   scripts/clean-stale-target.sh --mark <target-dir>  (after cargo build)
#
# Cargo names its outputs after the compiler and the profile settings, so when
# either changes it builds everything again under new names and leaves the old
# outputs behind. The first form runs `cargo clean` when the target directory
# exists and its record of the conditions (`rustc -vV` and the `[profile.*]`
# tables of Cargo.toml) is missing or differs from the current ones. The second
# form records the current conditions in an existing target directory.
#
# Neither form creates the target directory: Cargo marks only a directory it
# creates itself as a cache (CACHEDIR.TAG, and on macOS the Time Machine
# exclusion), so the build has to be the one that creates it.
set -euo pipefail

mark_only=false
if [[ "${1-}" == --mark ]]; then
  mark_only=true
  shift
fi
if [[ $# -ne 1 || -z "$1" ]]; then
  echo "Usage: $0 [--mark] <target-dir>" >&2
  exit 2
fi
target="$1"
manifest="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/Cargo.toml"
mark="$target/.build-conditions"

conditions="$(rustc -vV && awk '/^[[:space:]]*\[/ { profile = /^[[:space:]]*\[profile\./ } profile' "$manifest")"
if "$mark_only"; then
  if [[ -d "$target" ]]; then
    printf '%s\n' "$conditions" >"$mark"
  fi
elif [[ -e "$target" && ! (-f "$mark" && "$(<"$mark")" == "$conditions") ]]; then
  echo "Build conditions of $target changed or are unknown; running cargo clean." >&2
  cargo clean --manifest-path "$manifest" --target-dir "$target"
fi
