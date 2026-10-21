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

# Shows $1 as a macOS notification from Branvil; elsewhere it does nothing.
notify() {
  if command -v osascript >/dev/null 2>&1; then
    osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title "Branvil"' \
      -e 'end run' "$1" >/dev/null 2>&1 || true
  fi
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

