# Shared by scripts/dev.sh, scripts/serve.sh, and scripts/update.sh (sourced).

# Git without the system or global configuration and credential helpers.
git() {
  GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0 \
    command git -c credential.helper= -c core.askPass= "$@"
}

# Writes $2 into file $1 in one step (a rename): a reader sees the whole old
# or the whole new content, also after a power loss, as the content is on the
# disk before the rename. Every file under builds/ is written so.
put() {
  printf '%s\n' "$2" >"$1.$$.tmp"
  sync
  mv -f "$1.$$.tmp" "$1"
}

# Sends $1 to the webhook without the service, and to the macOS desktop.
notify() {
  local notice_build
  notice_build="${serve_build:-${previous:-${build:-}}}"
  # An activation failure may leave no current build, and the previous
  # version may predate admin notify. The installed target can send offline.
  if [[ -n "${target:-}" && -x "$builds/$target/branvil" ]]; then notice_build="$target"; fi
  if [[ -z "$notice_build" ]]; then notice_build="$(cat "$builds/current" 2>/dev/null || true)"; fi
  # Queue before delivery: an unavailable webhook must not lose a terminal
  # failure that builds/attempted prevents the updater from repeating.
  if [[ -d "$builds" ]]; then
    local pending
    pending="$(mktemp "$builds/notify.XXXXXX.tmp")"
    put "${pending%.tmp}.notice" "$notice_build
$1"
    rm -f "$pending"
  else
    client "$notice_build" admin notify "$1" || true
  fi
  # A caller's one-notice marker is written only after the durable enqueue,
  # before the desktop notification can block or be cut off.
  if [[ $# -gt 1 ]]; then put "$2" "$1"; fi
  if command -v osascript >/dev/null 2>&1; then
    osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title "Branvil"' \
      -e 'end run' "$1" >/dev/null 2>&1 || true
  fi
}

# Each queued notice remembers a build, whose notify command never opens the
# database. Called periodically even while the service is stopped.
deliver_notices() {
  local pending notice_build message
  for pending in "$builds"/*.notice; do
    [[ -f "$pending" ]] || continue
    read -r notice_build <"$pending"
    message="$(tail -n +2 "$pending")"
    if client "$notice_build" admin notify "$message"; then rm -f "$pending"; fi
  done
}

# Runs build $1's branvil on the data directory, with its own credentials and
# URL even when the calling shell has those of another Branvil. Uses $builds
# and $data_dir.
client() {
  (
    unset BRANVIL_TOKEN BRANVIL_URL
    "$builds/$1/branvil" "${@:2}" --data-dir "$data_dir"
  )
}

