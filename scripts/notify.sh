#!/usr/bin/env bash
# Show $1 as a macOS notification from Branvil; elsewhere do nothing.
# scripts/serve.sh and scripts/update.sh notify through it.
if command -v osascript >/dev/null 2>&1; then
  osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title "Branvil"' \
    -e 'end run' "$1" >/dev/null 2>&1 || true
fi
