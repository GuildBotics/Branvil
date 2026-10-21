#!/usr/bin/env bash
# Run the Branvil built from this checkout and keep it up to date with main.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/serve.sh [--data-dir PATH] [--interval SECONDS] [--plist]

Run branvil serve of the installed build that builds/current names, start it
again whenever it exits, and every interval run scripts/update.sh --auto, which
updates to origin/main once that commit's checks have succeeded. Meant to run
as a launchd user agent (--plist); TERM stops the service and, after its
running step, an update.
  --data-dir PATH     Data directory (default: BRANVIL_DATA_DIR or <repo>/data).
                      Relative paths are resolved from the repository root.
  --interval SECONDS  Time between update checks (default: 300).
  --plist             Print a launchd user agent that runs this script with
                      these options and the current PATH, then exit.
  -h, --help          Show this help.

When builds/ does not exist, it first installs the checkout's HEAD there
(scripts/dev.sh --install) and runs that. Without
builds/current Branvil stays stopped; writing a commit into it
(echo <commit> >builds/current) starts that build, and naming another build
there stops the running one and starts that. A build that exits within
10 seconds of its start 3 times in a row is stopped by removing the file.
EOF
}

log() { echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*"; }

# Whether file $1 holds the pid of a running process.
alive() {
  local pid
  pid="$(cat "$1" 2>/dev/null || true)"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

# The whole script is read before an update changes the checkout under it.
main() {
  local repo_root data_dir interval=300 plist=false
  repo_root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
  data_dir="${BRANVIL_DATA_DIR:-$repo_root/data}"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --data-dir|--interval)
        if [[ $# -lt 2 || -z "$2" || "$2" == --* ]]; then
          echo "Missing value for $1" >&2
          exit 2
        fi
        if [[ "$1" == --data-dir ]]; then data_dir="$2"; else interval="$2"; fi
        shift 2
        ;;
      --plist) plist=true; shift ;;
      -h|--help) usage; exit 0 ;;
      *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
  done
  if [[ ! "$interval" =~ ^[1-9][0-9]*$ ]]; then
    echo "--interval takes a whole number of seconds" >&2
    exit 2
  fi
  if [[ "$data_dir" != /* ]]; then data_dir="$repo_root/$data_dir"; fi

  if "$plist"; then
    xml() { sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g' <<<"$1"; }
    local repo data path logs
    repo="$(xml "$repo_root")"
    data="$(xml "$data_dir")"
    path="$(xml "$PATH")"
    logs="$(xml "$HOME/Library/Logs/branvil.log")"
    cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.guildbotics.branvil</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>$repo/scripts/serve.sh</string>
    <string>--data-dir</string>
    <string>$data</string>
    <string>--interval</string>
    <string>$interval</string>
  </array>
  <key>WorkingDirectory</key><string>$repo</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>$path</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ExitTimeOut</key><integer>120</integer>
  <key>StandardOutPath</key><string>$logs</string>
  <key>StandardErrorPath</key><string>$logs</string>
</dict>
</plist>
EOF
    exit 0
  fi

  local builds="$repo_root/builds" notify="$repo_root/scripts/notify.sh"
  local serve_pid= serve_build= update_pid= build status started=0 quick=0 message stopping=
  # TERM (launchctl bootout) stops the service and an update; set before
  # anything starts.
  stop() {
    local pid
    for pid in $serve_pid $update_pid; do kill -TERM "$pid" 2>/dev/null || true; done
    for pid in $serve_pid $update_pid; do wait "$pid" 2>/dev/null || true; done
    exit 143
  }
  trap stop TERM INT
  cd "$repo_root"

  if [[ ! -e "$builds" ]]; then
    build="$(GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null git rev-parse HEAD 2>/dev/null || true)"
    log "installing ${build:-HEAD}, the checkout's HEAD"
    if [[ -n "$build" ]] && "$repo_root/scripts/dev.sh" --install "$builds/$build"; then
      echo "$build" >"$builds/current"
    else
      # Without builds/, the next start tries again.
      rm -rf -- "$builds"
      message="Could not install the checkout's HEAD ${build}; see the log, then start scripts/serve.sh again."
      log "$message"
      "$notify" "$message"
    fi
  fi

  local next_update=$((SECONDS + interval))
  while true; do
    if [[ -n "$serve_pid" ]] && ! kill -0 "$serve_pid" 2>/dev/null; then
      status=0
      wait "$serve_pid" || status=$?
      log "branvil serve of $serve_build exited with status $status"
      # Tells scripts/update.sh, which waits for the build to answer, that it exited.
      { echo "$serve_pid" >"$builds/$serve_build/exited"; } 2>/dev/null || true
      serve_pid=
      if (( SECONDS - started < 10 )); then quick=$((quick + 1)); else quick=0; fi
      # During an update, the update decides what runs.
      if (( quick >= 3 )) && ! alive "$builds/updating" \
        && [[ "$(cat "$builds/current" 2>/dev/null || true)" == "$serve_build" ]]; then
        rm -f "$builds/current"
        quick=0
        message="branvil serve of $serve_build exited at its start 3 times; Branvil is stopped. See the log, then echo <commit> >$builds/current to start a build."
        log "$message"
        "$notify" "$message"
      fi
    fi
    if [[ -n "$update_pid" ]] && ! kill -0 "$update_pid" 2>/dev/null; then
      wait "$update_pid" || true
      update_pid=
    fi
    build="$(cat "$builds/current" 2>/dev/null || true)"
    # What runs follows builds/current.
    if [[ -n "$serve_pid" && "$build" != "$serve_build" && "$stopping" != "$serve_pid" ]]; then
      log "builds/current names ${build:-no build}; stopping branvil serve of $serve_build"
      kill -TERM "$serve_pid" 2>/dev/null || true
      stopping="$serve_pid"
    fi
    # An update that stopped the service holds it stopped while it lives, and
    # nothing starts while a branvil (an admin command, say) has the database open.
    if [[ -z "$serve_pid" && -n "$build" ]] && ! alive "$builds/hold" \
      && [[ -z "$(lsof -t -a -c branvil -- "$data_dir/branvil.redb" 2>/dev/null || true)" ]]; then
      if [[ "$build" != "$serve_build" ]]; then quick=0; fi
      serve_build="$build"
      started=$SECONDS
      log "starting branvil serve of $serve_build"
      "$builds/$serve_build/branvil" serve --data-dir "$data_dir" &
      serve_pid=$!
    fi
    if [[ -z "$update_pid" && -n "$build" ]] && (( SECONDS >= next_update )); then
      next_update=$((SECONDS + interval))
      "$BASH" "$repo_root/scripts/update.sh" --auto --data-dir "$data_dir" &
      update_pid=$!
    fi
    sleep 1
  done
}

main "$@"; exit
