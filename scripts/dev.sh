#!/usr/bin/env bash
# Build the GUI and server, initialize local data once, and open the browser.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/dev.sh [--data-dir PATH] [--owner NAME] [--no-open]

Build and run Branvil until Ctrl-C. Open the GUI after the server is ready.
  --data-dir PATH  Data directory (default: BRANVIL_DATA_DIR or <repo>/data).
                  Relative paths are resolved from the repository root.
  --owner NAME     Owner name for first initialization (default: owner).
  --no-open        Start the server without opening a browser.
  -h, --help       Show this help.

Existing data and branvil.toml are reused. Restart to pick up code changes.
EOF
}

repo_root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
data_dir="${BRANVIL_DATA_DIR:-$repo_root/data}"
owner=owner
open_browser=true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --data-dir|--owner)
      if [[ $# -lt 2 || -z "$2" || "$2" == --* ]]; then
        echo "Missing value for $1" >&2
        exit 2
      fi
      if [[ "$1" == --data-dir ]]; then data_dir="$2"; else owner="$2"; fi
      shift 2
      ;;
    --no-open) open_browser=false; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

cd "$repo_root"
for command in cargo npm git; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "Required command not found: $command (see README requirements)." >&2
    exit 1
  fi
done

if [[ ! -f web/node_modules/.package-lock.json || web/package-lock.json -nt web/node_modules/.package-lock.json ]]; then
  npm --prefix web ci
fi
npm --prefix web run build
target="$repo_root/target"
"$repo_root/scripts/clean-stale-target.sh" "$target"
# Mark even a failed build: its outputs were made under the current conditions.
build_status=0
cargo build --target-dir "$target" || build_status=$?
"$repo_root/scripts/clean-stale-target.sh" --mark "$target"
if (( build_status != 0 )); then exit "$build_status"; fi
binary="$target/debug/branvil"

if [[ ! -f "$data_dir/branvil.redb" ]]; then
  "$binary" init --data-dir "$data_dir" --owner "$owner"
fi

startup_log="$(mktemp "${TMPDIR:-/tmp}/branvil-dev.XXXXXX")"
server_pid=
cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -f "$startup_log"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# The service flushes this line only after startup recovery is complete.
# Watching our child's output avoids mistaking another server for this one.
"$binary" serve --data-dir "$data_dir" >"$startup_log" &
server_pid=$!
deadline=$((SECONDS + 60))
until grep -q '^branvil listening on ' "$startup_log"; do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$startup_log"
    wait "$server_pid" || exit $?
    echo "Branvil exited before it was ready." >&2
    exit 1
  fi
  if (( SECONDS >= deadline )); then
    echo "Branvil did not finish starting within 60 seconds." >&2
    exit 1
  fi
  sleep 0.2
done
cat "$startup_log"
echo "Branvil is running. Press Ctrl-C to stop."

if "$open_browser"; then
  # Use this data directory's owner token and configured URL even when the
  # calling shell has credentials for a different Branvil instance.
  (
    unset BRANVIL_TOKEN BRANVIL_URL
    if ! "$binary" open --data-dir "$data_dir"; then
      echo "Could not open the browser; the server is still running. Open this single-use link within 60 seconds:" >&2
      "$binary" open --print --data-dir "$data_dir" >&2 || true
    fi
  )
fi

wait "$server_pid"
