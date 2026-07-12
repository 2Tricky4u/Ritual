#!/usr/bin/env bash
# Fake sandbox wrapper (stands in for `srt`): proves ritual actually wrapped
# the agent argv, then executes the wrapped command untouched.
set -euo pipefail
if [ -n "${FAKE_WRAPPER_LOG:-}" ]; then
  printf 'wrapped: %s\n' "$*" >>"$FAKE_WRAPPER_LOG"
fi
exec "$@"
