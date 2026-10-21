#!/usr/bin/env bash
# Update the Branvil that scripts/serve.sh runs from this checkout to origin/main.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/update.sh [--auto] [--data-dir PATH] [--backup-dir PATH]

Update the Branvil that scripts/serve.sh runs from this checkout to origin/main,
which must be this Branvil (it fetches as the owner while the service runs):
  1. back up the running service into the backup directory as
     branvil-update-<UTC time>, and remove all but the newest three of them;
  2. switch the checkout to main and fast-forward it to origin/main;
  3. install that commit as builds/<commit> (scripts/dev.sh --install);
  4. stop the branvil serve that has the data directory open;
  5. run branvil admin upgrade and admin check with the new build;
  6. name the new build in builds/current, which scripts/serve.sh starts, wait
     until it answers, and remove the builds other than the new and previous.
  --auto             Update only when origin/main is neither the running build
                     nor the last commit an update gave up on
                     (builds/attempted), and only once its checks in this
                     Branvil have all succeeded.
  --data-dir PATH    Data directory (default: BRANVIL_DATA_DIR or <repo>/data).
  --backup-dir PATH  Backup directory (default: ~/branvil-backups).
                     Relative paths are resolved from the repository root.
  -h, --help         Show this help.

It refuses to start when the service is not running from builds/current (no
backup can be taken), when another update runs, or when the checkout has
uncommitted changes, and does nothing when origin/main is the running build.
When a step fails before the new build answers, Branvil runs the previous
build again when the data directory was not upgraded and stays stopped when it
was; the script prints and notifies where it stopped.
EOF
}

scripts=
builds=
data_dir=
auto=false
stage=
target=
previous=
# Set when this update has stopped the service (builds/hold), when admin
# upgrade changed the database, when the update decided to install $target,
# and when a TERM or Ctrl-C ended it.
held=false
applied=false
decided=false
interrupted=false
# While upgrading and checking, a TERM or Ctrl-C waits in $pending.
deferring=false
pending=
stopped=
copy=
backup=

# Git without the system or global configuration and credential helpers.
git() {
  GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0 \
    command git -c credential.helper= -c core.askPass= "$@"
}

# Fetches origin as the member $1 with the token in file $2: the token goes
# through GIT_ASKPASS and the environment, never a URL or an argument.
fetch_as() {
  local token askpass status=0
  token="$(<"$2")"
  askpass="$(mktemp "${TMPDIR:-/tmp}/branvil-askpass.XXXXXX")"
  cat >"$askpass" <<'EOF'
#!/bin/sh
case "$1" in
  Username*) printf '%s\n' "$BRANVIL_GIT_USER" ;;
  *) printf '%s\n' "$BRANVIL_GIT_TOKEN" ;;
esac
EOF
  chmod 700 "$askpass"
  GIT_ASKPASS="$askpass" BRANVIL_GIT_USER="$1" BRANVIL_GIT_TOKEN="$token" \
    git fetch origin || status=$?
  rm -f "$askpass"
  return "$status"
}

# Prints ready, running, or failed: the checks of commit $1 in the Branvil of
# origin, read with the owner's token in file $2 (passed in the environment).
checks_of() {
  local pattern='^(https?://[^/]+)/git/([^/]+/[^/]+)\.git$' url
  url="$(git remote get-url origin)"
  if [[ ! "$url" =~ $pattern ]]; then
    echo "origin ($url) is not a repository URL of a Branvil." >&2
    return 1
  fi
  BRANVIL_CHECKS_URL="${BASH_REMATCH[1]}/api/v1/repos/${BASH_REMATCH[2]//\//%2F}/commits/$1/checks" \
    BRANVIL_CHECKS_TOKEN="$(<"$2")" node --input-type=module -e '
      const answer = await fetch(process.env.BRANVIL_CHECKS_URL, {
        headers: { authorization: `Bearer ${process.env.BRANVIL_CHECKS_TOKEN}` },
      }).catch(() => null);
      let state = "running"; // not reachable now: asked again in the next round
      if (answer) {
        // A 404 too: the commit has just been fetched from there.
        if (!answer.ok) throw new Error(`reading the checks: HTTP ${answer.status}`);
        const { checks, checks_expected } = await answer.json();
        if (checks.some((c) => c.status !== "completed")) state = "running";
        else if (checks.length) state = checks.every((c) => c.conclusion === "success") ? "ready" : "failed";
        else state = checks_expected ? "running" : "ready";
      }
      console.log(state);
    '
}

# Runs build $1's branvil against this data directory, with its credentials
# and URL even when the calling shell has those of another Branvil.
client() {
  (
    unset BRANVIL_TOKEN BRANVIL_URL
    "$builds/$1/branvil" "${@:2}" --data-dir "$data_dir"
  )
}

# The processes of branvil that have the database open: the service.
serving() { lsof -t -a -c branvil -- "$data_dir/branvil.redb" 2>/dev/null || true; }

# Stops the service; its pids are added to $stopped.
stop_service() {
  local pids
  pids="$(serving)"
  stopped+=" $pids"
  # Jobs running in CI are interrupted and queued again once at the next start.
  # shellcheck disable=SC2086
  kill -TERM $pids 2>/dev/null || true
  local deadline=$((SECONDS + 60)) pid
  for pid in $pids; do
    while kill -0 "$pid" 2>/dev/null; do
      if (( SECONDS >= deadline )); then
        echo "Branvil (pid $pid) did not stop within 60 seconds." >&2
        return 1
      fi
      sleep 0.2
    done
  done
}

# Names build $1 in builds/current in one step: a rename replaces the file,
# so scripts/serve.sh reads either the old or the new commit.
switch_to() {
  printf '%s\n' "$1" >"$builds/current.new"
  mv -f "$builds/current.new" "$builds/current"
}

# Ends the update with status $1 for a TERM or Ctrl-C, after the running step
# (bash runs a trap once the command running in the foreground has returned).
interrupt() {
  if "$deferring"; then
    pending="$1"
    return
  fi
  interrupted=true
  exit "$1"
}

# Lets scripts/serve.sh start builds/current (build $1) again.
release() {
  rm -f "$builds/$1/exited" "$builds/hold"
}

# Waits until build $1, which scripts/serve.sh starts, answers. scripts/serve.sh
# writes the pid of a serve that exited into builds/<commit>/exited; the ones
# stopped here do not count.
answers() {
  local deadline=$((SECONDS + 60))
  until client "$1" member list >/dev/null 2>&1; do
    if [[ -e "$builds/$1/exited" && " $stopped " != *" $(<"$builds/$1/exited") "* ]]; then
      echo "builds/$1 exited at its start; see the log of scripts/serve.sh." >&2
      return 1
    fi
    if (( SECONDS >= deadline )); then
      echo "builds/$1 did not answer within 60 seconds; is scripts/serve.sh running?" >&2
      return 1
    fi
    sleep 0.5
  done
}

# Runs at an exit until the new build answers. builds/current always names a
# build that can open the database as it is, or is missing because Branvil
# stays stopped: an upgrade that changed the database moved it to the new
# build at once, and until then it names the previous build.
report() {
  local status=$1 message
  if (( status == 0 )); then
    rm -f "$builds/notified"
    return
  fi
  message="The update to ${target:-origin/main} stopped while $stage (exit status $status)."
  if ! "$held"; then
    message+=" Branvil still runs $previous."
  elif "$applied" && "$interrupted"; then
    release "$target"
    message+=" The data directory was upgraded; scripts/serve.sh runs $target."
  elif "$applied"; then
    # scripts/serve.sh stops whatever runs once builds/current names nothing.
    rm -f "$builds/current"
    release "$target"
    message+=" The data directory was upgraded; Branvil is stopped."
  else
    # scripts/serve.sh stops the new build, if it runs, and starts this one.
    switch_to "$previous"
    release "$previous"
    if "$interrupted"; then
      message+=" The data directory was not upgraded; scripts/serve.sh runs $previous again."
    elif answers "$previous"; then
      message+=" The data directory was not upgraded; Branvil runs $previous again."
    else
      message+=" The data directory was not upgraded, but $previous does not answer either."
    fi
  fi
  # A run that was interrupted (or killed) concluded nothing and is repeated.
  if "$decided" && ! "$interrupted"; then
    echo "$target" >"$builds/attempted"
  fi
  {
    echo
    echo "$message"
    if [[ -n "$backup" ]]; then
      echo "Backup taken before the update: $backup (branvil restore --from it into a new data directory)"
    fi
    if "$applied" && ! "$interrupted"; then
      echo "Database before the upgrade: ${copy:-not reported; see the output of admin upgrade above}"
      echo "To run $previous again, put that copy in place of branvil.redb and run: echo $previous >$builds/current"
      echo "To start the new build as it is: echo $target >$builds/current"
    fi
    if "$decided" && ! "$interrupted"; then
      echo "scripts/update.sh --auto does not try $target again (builds/attempted)."
    fi
    echo "See \"Upgrading\" in docs/backup-restore.md."
  } >&2
  # Before the decision, a round that fails as the one before notifies once.
  if "$auto" && ! "$decided"; then
    if [[ "$(cat "$builds/notified" 2>/dev/null)" == "$message" ]]; then return; fi
    echo "$message" >"$builds/notified"
  fi
  "$scripts/notify.sh" "$message"
}

# Runs at the exit, which no TERM or Ctrl-C cuts short.
finish() {
  trap '' INT TERM
  report "$1"
  rm -f "$builds/updating"
}

# The whole script is read before the checkout changes under it.
main() {
  local repo_root backup_root
  repo_root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
  data_dir="${BRANVIL_DATA_DIR:-$repo_root/data}"
  backup_root="$HOME/branvil-backups"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --data-dir|--backup-dir)
        if [[ $# -lt 2 || -z "$2" || "$2" == --* ]]; then
          echo "Missing value for $1" >&2
          exit 2
        fi
        if [[ "$1" == --data-dir ]]; then data_dir="$2"; else backup_root="$2"; fi
        shift 2
        ;;
      --auto) auto=true; shift ;;
      -h|--help) usage; exit 0 ;;
      *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
  done
  # As branvil names the database in its output.
  data_dir="${data_dir%/}"

  cd "$repo_root"
  if [[ "$backup_root" != /* ]]; then backup_root="$repo_root/$backup_root"; fi
  for command in cargo npm node git lsof; do
    if ! command -v "$command" >/dev/null 2>&1; then
      echo "Required command not found: $command (see README requirements)." >&2
      exit 1
    fi
  done
  scripts="$repo_root/scripts"
  builds="$repo_root/builds"
  previous="$(cat "$builds/current" 2>/dev/null || true)"
  if [[ -z "$previous" || -z "$(serving)" ]]; then
    echo "Branvil is not running from $builds/current with $data_dir; a backup needs the running service. Start it with scripts/serve.sh, then run this again." >&2
    exit 1
  fi
  # One update at a time; the lock holds the pid of the update that took it.
  local lock="$builds/updating"
  if ! (set -o noclobber; echo "$$" >"$lock") 2>/dev/null; then
    if kill -0 "$(cat "$lock")" 2>/dev/null; then
      echo "Another update is running (pid $(<"$lock"))." >&2
      if "$auto"; then exit 0; fi
      exit 1
    fi
    echo "$$" >"$lock"
  fi
  trap 'interrupt 130' INT
  trap 'interrupt 143' TERM
  trap 'finish $?' EXIT

  stage="checking the checkout"
  if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    echo "The checkout has uncommitted changes; commit or stash them, then run this again." >&2
    git status --short --untracked-files=no >&2
    exit 1
  fi

  stage="fetching origin"
  local owner token="$data_dir/secrets/owner.token"
  owner="$(client "$previous" member list | node -e '
    const items = JSON.parse(require("fs").readFileSync(0, "utf8")).items;
    console.log(items.find((m) => m.kind === "owner").name);
  ')"
  fetch_as "$owner" "$token"
  target="$(git rev-parse origin/main)"
  if [[ "$target" == "$previous" ]]; then
    if ! "$auto"; then echo "Branvil already runs $target; nothing to update."; fi
    exit 0
  fi
  if "$auto"; then
    if [[ "$target" == "$(cat "$builds/attempted" 2>/dev/null)" ]]; then exit 0; fi
    stage="reading the checks of $target"
    local checks
    checks="$(checks_of "$target" "$token")"
    if [[ "$checks" == running ]]; then
      echo "The checks of $target have not finished; trying again later."
      exit 0
    fi
    if [[ "$checks" == failed ]]; then
      echo "$target" >"$builds/attempted"
      echo "The checks of $target did not all succeed; it is not installed."
      exit 0
    fi
  fi
  decided=true
  rm -f "$builds/notified"
  echo "Updating Branvil from $previous to $target"

  stage="backing up the running service"
  local made
  made="$backup_root/branvil-update-$(date -u +%Y%m%dT%H%M%SZ)"
  client "$previous" backup --to "$made"
  backup="$made"

  stage="removing old backups"
  # Only complete backups (the manifest is written last) count as generations.
  local stamp='[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]T[0-9][0-9][0-9][0-9][0-9][0-9]Z'
  local backups=("$backup_root"/branvil-update-$stamp)
  local kept=0 i dir
  for (( i = ${#backups[@]} - 1; i >= 0; i-- )); do
    dir="${backups[i]}"
    if [[ ! -d "$dir" || -L "$dir" ]]; then continue; fi
    if [[ -f "$dir/manifest.json" ]] && (( kept < 3 )); then
      kept=$((kept + 1))
      continue
    fi
    rm -rf -- "$dir"
    echo "Removed old backup $dir"
  done

  stage="updating the checkout to origin/main"
  git switch main
  git merge --ff-only "$target"

  stage="installing builds/$target"
  rm -rf -- "${builds:?}/$target"
  "$scripts/dev.sh" --install "$builds/$target"

  stage="stopping the service"
  # While this process lives, scripts/serve.sh starts nothing.
  echo "$$" >"$builds/hold"
  held=true
  stop_service

  stage="upgrading the data directory"
  # Not interrupted until builds/current matches what admin upgrade and
  # admin check made of the database.
  deferring=true
  local upgraded status=0 rest
  upgraded="$(client "$target" admin upgrade)" || status=$?
  echo "$upgraded"
  # A failed upgrade leaves the database as it was, and so does one that
  # reports exactly this line; anything else counts as having changed it.
  if (( status != 0 )); then exit "$status"; fi
  rest="${upgraded#"$data_dir/branvil.redb already has schema version "}"
  if [[ "$rest" == "$upgraded" || ! "$rest" =~ ^[0-9]+$ ]]; then
    applied=true
    switch_to "$target"
    copy="$(sed -n 's/^the database as it was: //p' <<<"$upgraded")"
  fi
  stage="checking the data directory"
  client "$target" admin check
  deferring=false
  if [[ -n "$pending" ]]; then interrupt "$pending"; fi

  stage="starting the new build"
  switch_to "$target"
  release "$target"
  answers "$target"
  trap 'rm -f "$builds/updating"' EXIT
  # The previous build stays, to go back to with the database copy.
  for dir in "$builds"/*; do
    if [[ -d "$dir" && "$dir" != "$builds/$target" && "$dir" != "$builds/$previous" ]]; then
      rm -rf -- "$dir"
    fi
  done
  echo "Update finished: Branvil answers with $target. Backup taken before the update: $backup"
}

main "$@"; exit
