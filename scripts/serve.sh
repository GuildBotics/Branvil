#!/usr/bin/env bash
# Run the Branvil built from this checkout and keep it up to date with main.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/serve.sh [--data-dir PATH] [--interval SECONDS] [--plist]

Run the installed build that builds/current names, and keep it up to date with
origin/main. Meant to run as a launchd user agent (--plist); it is the only
process that starts or stops branvil, and TERM stops it all. Failure notices
also go to the configured webhook without needing the service. Failed webhook
deliveries are queued under builds/ and retried each interval, including while
Branvil is stopped.
  --data-dir PATH     Data directory (default: BRANVIL_DATA_DIR or <repo>/data).
                      Relative paths are resolved from the repository root.
  --interval SECONDS  Time between update checks, the first at the start
                      (default: 300).
  --plist             Print a launchd user agent that runs this script with
                      these options and the current PATH, then exit.
  -h, --help          Show this help.

It runs the build named in builds/current and stays stopped without it. To
run another build, write its commit there in one step:
  printf '%s\n' <commit> >builds/current.new && mv builds/current.new builds/current
and, when it is older than origin/main, first write origin/main's commit into
builds/attempted the same way, so that the next update round leaves it. See
"Running the service from this checkout" in README.md and "Upgrading" in
docs/backup-restore.md.
EOF
}

log() { echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*"; }

# The whole script is read before an update changes the checkout under it.
main() {
  local repo_root data_dir interval=300 plist=false args=("$@")
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

  . "$repo_root/scripts/lib.sh"
  local builds="$repo_root/builds" serve_pid= serve_build= update_pid= build status
  # Its times are counted in ticks of the loop below, a second each
  # (BRANVIL_SERVE_TICK, in seconds, makes them shorter for the tests).
  local tick="${BRANVIL_SERVE_TICK:-1}" now=0 started=0 quick=0 next_update=0 slow= message
  # The update being started (builds/update, written by scripts/update.sh):
  # its new build, the build it replaces, and, once it has failed, the step
  # that failed.
  local target previous failure next_notice=0
  record() {
    target= previous= failure=
    if [[ -e "$builds/update" ]]; then read -r target previous failure <"$builds/update"; fi
  }
  # Whether the serve started last has said that it is ready.
  ready() { grep -q '^branvil listening on ' "$builds/serve.out" 2>/dev/null; }
  # Stops the serve: TERM, and KILL when it has not ended 30 ticks later.
  # A serve that says it waits for its running CI jobs (ci.stop_wait_secs at
  # most) is waited for until it ends; one that had to interrupt jobs then
  # (exit status 3) is told.
  stopped() {
    local i=0 status=0
    if [[ -z "$serve_pid" ]]; then return; fi
    kill -TERM "$serve_pid" 2>/dev/null || true
    while kill -0 "$serve_pid" 2>/dev/null; do
      if (( i == 30 )) && ! grep -q '^branvil stopping: ' "$builds/serve.out" 2>/dev/null; then
        log "branvil serve of $serve_build did not stop within 30 seconds of TERM; killing it"
        kill -KILL "$serve_pid" 2>/dev/null || true
        break
      fi
      sleep "$tick"
      i=$((i + 1))
    done
    wait "$serve_pid" || status=$?
    if grep -q '^branvil stopping: ' "$builds/serve.out" 2>/dev/null; then
      log "branvil serve of $serve_build stopped $i seconds after TERM; it waited for its running CI jobs"
    fi
    if (( status == 3 )); then
      message="branvil serve of $serve_build stopped while CI jobs still ran, ci.stop_wait_secs after TERM: they were interrupted, and queued again in their place unless interrupted before."
      log "$message"
      notify "$message"
    fi
    serve_pid=
  }
  # TERM (launchctl bootout) stops an update and the service, the service at
  # once (INT: TERM would wait for its running CI jobs); set before anything
  # starts.
  stop() {
    local pid
    if [[ -n "$serve_pid" ]]; then kill -INT "$serve_pid" 2>/dev/null || true; fi
    if [[ -n "$update_pid" ]]; then kill -TERM "$update_pid" 2>/dev/null || true; fi
    for pid in $serve_pid $update_pid; do wait "$pid" 2>/dev/null || true; done
    exit 143
  }
  trap stop TERM INT
  cd "$repo_root"
  # Logs and notifies $1, once: a conclusion done again after a kill finds it
  # in builds/notified.
  tell() {
    log "$1"
    if [[ "$(cat "$builds/notified" 2>/dev/null)" != "$1" ]]; then
      notify "$1" "$builds/notified"
    fi
  }
  # Removes what a cut off rehearsal, install, write, or upgrade left: the
  # rehearsal's copy, builds/*.new, builds/*.tmp, and the data directory's
  # partial database copy. Runs only while nothing else does.
  tidy() {
    rm -rf -- "$builds/rehearsal" "$builds"/*.new "$builds"/*.tmp \
      "$data_dir"/branvil-schema-*.redb.partial
  }
  # Concludes the update that failed while $failure: back to the build it
  # replaced when that build can open the database as it is, stopped
  # otherwise. Done again from builds/update when it was cut off.
  give_up() {
    message="The update to $target stopped while $failure"
    put "$builds/attempted" "$target"
    if client "$previous" admin upgrade; then
      put "$builds/current" "$previous"
      message+="; Branvil runs $previous again."
    else
      rm -f "$builds/current"
      message+=", after the data directory was upgraded; Branvil is stopped. The log names the copy of the database as it was."
    fi
    tell "$message"
    rm -f "$builds/update"
  }
  # Build $1 could not start, while $2.
  failed() {
    record
    if [[ "$target" == "$1" ]]; then
      failure="$2"
      put "$builds/update" "$target $previous $failure"
      give_up
    else
      # Told before builds/current goes, so a cut off start is told again
      # rather than never (the same failure can come back after a fix).
      message="$1 stopped while $2; Branvil is stopped. See \"Upgrading\" in docs/backup-restore.md."
      log "$message"
      notify "$message"
      rm -f "$builds/current"
      quick=0
    fi
  }
  # Starts build $1: its admin upgrade, which answers 3 while the database
  # was upgraded and has not passed a check since (the database keeps that,
  # whatever cut off a start before), then its admin check, then branvil serve.
  start() {
    local status=0
    log "starting $1"
    client "$1" admin upgrade || status=$?
    if (( status != 0 && status != 3 )); then
      failed "$1" "upgrading the data directory"
      return
    fi
    if (( status == 3 )) && ! client "$1" admin check; then
      failed "$1" "checking the data directory"
      return
    fi
    if [[ "$1" != "$serve_build" ]]; then quick=0; fi
    # Its standard output carries the line that says it is ready. The last
    # serve's goes first: the child empties the file only once it runs.
    rm -f "$builds/serve.out"
    "$builds/$1/branvil" serve --data-dir "$data_dir" >"$builds/serve.out" &
    serve_pid=$!
    serve_build="$1"
    started=$now
    slow=
  }

  # builds/ appears complete, with its builds/current, or not at all: a start
  # without it (after a failed or interrupted first install) installs again.
  if [[ ! -e "$builds" ]]; then
    rm -rf -- "$builds.new"
    build="$(git rev-parse HEAD 2>/dev/null || true)"
    log "installing ${build:-HEAD}, the checkout's HEAD"
    if [[ -n "$build" ]] && "$repo_root/scripts/dev.sh" --install "$builds.new/$build"; then
      put "$builds.new/current" "$build"
      mv "$builds.new" "$builds"
    else
      message="Could not install the checkout's HEAD ${build}; see the log, then start scripts/serve.sh again."
      log "$message"
      notify "$message"
    fi
  fi
  tidy

  while true; do
    if (( now >= next_notice )); then
      next_notice=$((now + interval))
      deliver_notices
    fi
    record
    if [[ -n "$failure" ]]; then give_up; record; fi
    if [[ -n "$serve_pid" ]] && ! kill -0 "$serve_pid" 2>/dev/null; then
      status=0
      wait "$serve_pid" || status=$?
      serve_pid=
      log "branvil serve of $serve_build exited with status $status"
      if (( now - started < 10 )); then quick=$((quick + 1)); else quick=0; fi
      if [[ "$target" == "$serve_build" ]]; then
        failed "$serve_build" "starting the new build"
      elif (( quick >= 3 )); then
        failed "$serve_build" "starting (it exited within 10 seconds 3 times)"
      fi
      record
    fi
    if [[ -n "$update_pid" ]] && ! kill -0 "$update_pid" 2>/dev/null; then
      wait "$update_pid" || true
      update_pid=
    fi
    # scripts/update.sh recorded an update (also before a restart): name its
    # build, stop the service, and go on with the checkout's scripts/serve.sh,
    # which may have changed with it.
    if [[ -z "$update_pid" && -n "$target" && -z "$failure" \
      && "$(cat "$builds/current" 2>/dev/null || true)" != "$target" ]]; then
      put "$builds/current" "$target"
      log "updating from $previous to $target"
      stopped
      exec "$BASH" "$repo_root/scripts/serve.sh" ${args[@]+"${args[@]}"}
    fi
    # The new build is ready: the update is done.
    if [[ -n "$serve_pid" && -n "$target" && "$target" == "$serve_build" ]]; then
      if ready; then
        rm -f "$builds/update"
        # Keep pending notices deliverable before retiring their sender build.
        local pending
        for pending in "$builds"/*.notice; do
          [[ -f "$pending" ]] || continue
          put "$pending" "$target
$(tail -n +2 "$pending")"
        done
        # The replaced build stays, to go back to with the database copy.
        for build in "$builds"/*/; do
          build="${build%/}"
          if [[ "$build" != "$builds/$target" && "$build" != "$builds/$previous" ]]; then rm -rf -- "$build"; fi
        done
        log "Update finished: $target is ready."
      elif [[ -z "$slow" ]] && (( now - started >= 1800 )); then
        slow=1
        message="$target has not said that it is ready 30 minutes after its start; still waiting."
        log "$message"
        notify "$message"
      fi
    fi
    build="$(cat "$builds/current" 2>/dev/null || true)"
    # What runs follows builds/current.
    if [[ -n "$serve_pid" && "$build" != "$serve_build" ]]; then
      log "builds/current names ${build:-no build}; stopping branvil serve of $serve_build"
      stopped
    fi
    if [[ -z "$serve_pid" && -n "$build" ]]; then start "$build"; fi
    # An update round at the start and every interval, one at a time, while
    # the service is ready (it takes the backup).
    if [[ -z "$update_pid" && -n "$serve_pid" && ! -e "$builds/update" ]] \
      && (( now >= next_update )) && ready; then
      next_update=$((now + interval))
      "$BASH" "$repo_root/scripts/update.sh" --data-dir "$data_dir" &
      update_pid=$!
    fi
    sleep "$tick"
    now=$((now + 1))
  done
}

main "$@"; exit
