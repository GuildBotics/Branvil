#!/usr/bin/env bash
# Update the Branvil that scripts/dev.sh runs from this checkout to origin/main.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/update.sh [--data-dir PATH] [--backup-dir PATH] [--no-open]

Update the Branvil that scripts/dev.sh runs from this checkout to origin/main:
  1. back up the running service into the backup directory as
     branvil-update-<UTC time>, and remove all but the newest three of them;
  2. switch the checkout to main and fast-forward it to origin/main, which
     must be this Branvil: it fetches as the owner while the service runs;
  3. stop the branvil serve that has the data directory open;
  4. build as scripts/dev.sh does, then branvil admin upgrade and admin check;
  5. start it again with scripts/dev.sh and wait until it answers.
  --data-dir PATH    Data directory (default: BRANVIL_DATA_DIR or <repo>/data).
  --backup-dir PATH  Backup directory (default: ~/branvil-backups).
                     Relative paths are resolved from the repository root.
  --no-open          Passed to scripts/dev.sh.
  -h, --help         Show this help.

It refuses to start when the service is not running (no backup can be taken)
or when the checkout has uncommitted changes. When a step fails, it prints
where it stopped and how to go back.
EOF
}

data_dir=
stage=
running=true
previous_head=
backup=
copy=

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

# Runs at an exit until the updated service answers.
report() {
  local status=$?
  if (( status == 0 )); then return; fi
  {
    echo
    echo "Update stopped while $stage (exit status $status)."
    if "$running"; then
      echo "Branvil is still running the previous build."
    else
      echo "Branvil is stopped."
    fi
    if [[ -n "$previous_head" ]]; then
      echo "Checkout before the update: $previous_head (git switch --detach $previous_head, then scripts/dev.sh --data-dir $data_dir)"
    fi
    if [[ -n "$backup" ]]; then
      echo "Backup taken before the update: $backup (branvil restore --from it into a new data directory)"
    fi
    if [[ -n "$copy" ]]; then
      echo "Database before the upgrade: $copy (put it in place of branvil.redb before starting the previous build)"
    fi
    echo "See \"Upgrading\" and \"Restore\" in docs/backup-restore.md."
  } >&2
}

# The whole script is read before the checkout changes under it.
main() {
  local repo_root backup_root
  repo_root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
  data_dir="${BRANVIL_DATA_DIR:-$repo_root/data}"
  backup_root="$HOME/branvil-backups"
  local dev_args=()
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
      --no-open) dev_args+=(--no-open); shift ;;
      -h|--help) usage; exit 0 ;;
      *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
  done

  cd "$repo_root"
  if [[ "$backup_root" != /* ]]; then backup_root="$repo_root/$backup_root"; fi
  for command in cargo npm node git lsof; do
    if ! command -v "$command" >/dev/null 2>&1; then
      echo "Required command not found: $command (see README requirements)." >&2
      exit 1
    fi
  done
  local binary="$repo_root/target/debug/branvil"
  # The processes of branvil that have the database open: the service.
  serving() { lsof -t -a -c branvil -- "$data_dir/branvil.redb" 2>/dev/null || true; }
  # The service of this data directory, with its credentials and URL even when
  # the calling shell has those of another Branvil (as in scripts/dev.sh).
  client() {
    (
      unset BRANVIL_TOKEN BRANVIL_URL
      "$binary" "$@" --data-dir "$data_dir"
    )
  }
  if [[ -z "$(serving)" ]]; then
    echo "Branvil is not running with $data_dir; a backup needs the running service. Start it with scripts/dev.sh, then run this again." >&2
    exit 1
  fi
  if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    echo "The checkout has uncommitted changes; commit or stash them, then run this again." >&2
    git status --short --untracked-files=no >&2
    exit 1
  fi
  trap report EXIT

  stage="backing up the running service"
  local target="$backup_root/branvil-update-$(date -u +%Y%m%dT%H%M%SZ)"
  client backup --to "$target"
  backup="$target"

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
  previous_head="$(git rev-parse HEAD)"
  local owner
  owner="$(client member list | node -e '
    const items = JSON.parse(require("fs").readFileSync(0, "utf8")).items;
    console.log(items.find((m) => m.kind === "owner").name);
  ')"
  fetch_as "$owner" "$data_dir/secrets/owner.token"
  git switch main
  git merge --ff-only origin/main

  stage="stopping the service"
  # Looked up again: the service may have been restarted since the preflight.
  local pids
  pids="$(serving)"
  # Jobs running in CI are interrupted and queued again once at the next start.
  # shellcheck disable=SC2086
  kill -TERM $pids 2>/dev/null || true
  local deadline=$((SECONDS + 60)) pid
  for pid in $pids; do
    while kill -0 "$pid" 2>/dev/null; do
      if (( SECONDS >= deadline )); then
        echo "Branvil (pid $pid) did not stop within 60 seconds." >&2
        exit 1
      fi
      sleep 0.2
    done
  done
  running=false

  stage="building"
  "$repo_root/scripts/dev.sh" --build-only
  stage="upgrading the data directory"
  local upgraded
  upgraded="$("$binary" admin upgrade --data-dir "$data_dir")"
  echo "$upgraded"
  copy="$(sed -n 's/^the database as it was: //p' <<<"$upgraded")"
  stage="checking the data directory"
  "$binary" admin check --data-dir "$data_dir"

  stage="starting the service"
  echo "Updated to $(git rev-parse --short HEAD). Backup taken before the update: $backup"
  # A background job ignores Ctrl-C; pass it on as the TERM dev.sh stops on.
  local dev=
  trap 'kill -TERM "$dev" 2>/dev/null || true; wait "$dev" || true; exit 130' INT
  trap 'kill -TERM "$dev" 2>/dev/null || true; wait "$dev" || true; exit 143' TERM
  "$repo_root/scripts/dev.sh" --data-dir "$data_dir" ${dev_args[@]+"${dev_args[@]}"} &
  dev=$!
  until client member list >/dev/null 2>&1; do
    if ! kill -0 "$dev" 2>/dev/null; then
      wait "$dev" || exit
      exit 1
    fi
    sleep 0.5
  done
  trap - EXIT
  echo "Update finished: Branvil answers with the new build."
  wait "$dev"
}

main "$@"
