#!/usr/bin/env bash
# Prepare an update of the Branvil that scripts/serve.sh runs to origin/main.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/update.sh --data-dir PATH   (run by scripts/serve.sh only)

scripts/serve.sh runs this at its start and every interval. When origin/main
(which must be this Branvil) is neither the running build nor the last commit
an update gave up on (builds/attempted), and its checks in this Branvil have
all succeeded, it backs the running service up into ~/branvil-backups,
fast-forwards the checkout's main, installs the commit as builds/<commit>, and
rehearses the upgrade on a copy restored from that backup; then it records
the update in builds/update, which scripts/serve.sh carries out. See "Upgrading" in docs/backup-restore.md.
EOF
}

builds=
data_dir=
stage=
target=
previous=
decided=false
interrupted=false
backup=

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

# Runs at the exit, which no TERM cuts short.
finish() {
  trap '' INT TERM
  local status=$1 message
  rm -rf -- "$builds/rehearsal"
  if (( status == 0 )); then
    rm -f "$builds/notified"
    return
  fi
  message="The update to ${target:-origin/main} stopped while $stage (exit status $status); Branvil still runs ${previous:-no build}."
  echo >&2
  echo "$message" >&2
  # Interrupted, it concluded nothing; the next round runs it again.
  if "$interrupted"; then return; fi
  if "$decided"; then
    put "$builds/attempted" "$target"
    echo "The update does not try $target again (builds/attempted)." >&2
  fi
  if [[ -n "$backup" ]]; then echo "Backup taken before the update: $backup" >&2; fi
  # A round that fails as the one before notifies once.
  if [[ "$(cat "$builds/notified" 2>/dev/null)" == "$message" ]]; then return; fi
  put "$builds/notified" "$message"
  notify "$message"
}

# The whole script is read before the checkout changes under it.
main() {
  local repo_root
  repo_root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
  if [[ $# -ne 2 || "$1" != --data-dir ]]; then
    usage >&2
    exit 2
  fi
  data_dir="$2"
  cd "$repo_root"
  . "$repo_root/scripts/lib.sh"
  builds="$repo_root/builds"
  previous="$(cat "$builds/current" 2>/dev/null || true)"
  # A TERM (from scripts/serve.sh) ends the update after the running step.
  trap 'interrupted=true; exit 143' INT TERM
  trap 'finish $?' EXIT

  # A launchd agent printed from a shell without these fails here every
  # round, so this is told like any failure before the decision.
  stage="checking the required commands"
  for command in cargo npm node git; do
    if ! type -P "$command" >/dev/null; then
      echo "Required command not found: $command (see README requirements)." >&2
      exit 1
    fi
  done

  stage="checking the checkout"
  if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
    echo "The checkout has uncommitted changes; commit or stash them." >&2
    git status --short --untracked-files=no >&2
    exit 1
  fi

  stage="reaching the running service"
  local owner token="$data_dir/secrets/owner.token"
  owner="$(client "$previous" member list | node -e '
    const items = JSON.parse(require("fs").readFileSync(0, "utf8")).items;
    console.log(items.find((m) => m.kind === "owner").name);
  ')"
  stage="fetching origin"
  fetch_as "$owner" "$token"
  target="$(git rev-parse origin/main)"
  if [[ "$target" == "$previous" || "$target" == "$(cat "$builds/attempted" 2>/dev/null)" ]]; then
    exit 0
  fi
  stage="checking the checkout"
  if ! git merge-base --is-ancestor main "$target"; then
    echo "The checkout's main has commits that origin/main ($target) does not have; reset or move them." >&2
    exit 1
  fi
  stage="reading the checks of $target"
  local checks
  checks="$(checks_of "$target" "$token")"
  if [[ "$checks" == running ]]; then
    echo "The checks of $target have not finished; trying again later."
    exit 0
  fi
  if [[ "$checks" == failed ]]; then
    put "$builds/attempted" "$target"
    echo "The checks of $target did not all succeed; it is not installed."
    exit 0
  fi
  echo "Updating Branvil from $previous to $target"

  stage="backing up the running service"
  local made=
  # A new name, also after a run that was killed within the same second.
  while [[ -z "$made" || -e "$made" ]]; do
    if [[ -n "$made" ]]; then sleep 1; fi
    made="$HOME/branvil-backups/branvil-update-$(date -u +%Y%m%dT%H%M%SZ)"
  done
  client "$previous" backup --to "$made"
  backup="$made"
  # From here on, a failure is the commit's: it is not tried again.
  decided=true
  rm -f "$builds/notified"

  stage="removing old backups"
  # Only complete backups (the manifest is written last) count as generations.
  local stamp='[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]T[0-9][0-9][0-9][0-9][0-9][0-9]Z'
  local backups=("$HOME"/branvil-backups/branvil-update-$stamp)
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
  "$repo_root/scripts/dev.sh" --install "$builds/$target"

  # The running build restores the backup into builds/rehearsal, and the new
  # build upgrades and checks that copy, while the service runs.
  stage="rehearsing the upgrade on a copy of the backup"
  local status=0
  rm -rf -- "$builds/rehearsal"
  ( data_dir="$builds/rehearsal"; client "$previous" restore --from "$backup" )
  ( data_dir="$builds/rehearsal"; client "$target" admin upgrade ) || status=$?
  if (( status != 0 && status != 3 )); then exit "$status"; fi
  ( data_dir="$builds/rehearsal"; client "$target" admin check )
  # The update, which scripts/serve.sh carries out.
  put "$builds/update" "$target $previous"
}

main "$@"; exit
