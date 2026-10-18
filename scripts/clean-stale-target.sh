#!/usr/bin/env bash
# Remove a Cargo target directory built under other build conditions.
#
#   scripts/clean-stale-target.sh <target-dir>
#
# Cargo names its outputs after the compiler and the profile settings, so when
# either changes it builds everything again under new names and leaves the old
# outputs behind. This keeps the conditions (`rustc -vV` and the `[profile.*]`
# tables of Cargo.toml) in the target directory and runs `cargo clean` when
# they differ from the current ones, or when the directory has no record.
set -euo pipefail

if [[ $# -ne 1 || -z "$1" ]]; then
  echo "Usage: $0 <target-dir>" >&2
  exit 2
fi
target="$1"
manifest="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/Cargo.toml"
mark="$target/.build-conditions"

conditions="$(rustc -vV && awk '/^[[:space:]]*\[/ { profile = /^[[:space:]]*\[profile\./ } profile' "$manifest")"
if [[ -f "$mark" && "$(<"$mark")" == "$conditions" ]]; then
  exit 0
fi
if [[ -e "$target" ]]; then
  echo "Build conditions of $target changed or are unknown; running cargo clean." >&2
  cargo clean --manifest-path "$manifest" --target-dir "$target"
fi
mkdir -p "$target"
printf '%s\n' "$conditions" >"$mark"
