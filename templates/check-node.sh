#!/usr/bin/env bash
set -e
npx eslint . --max-warnings 0
# fast = lint + typecheck (the snippet's promise); tests only on the full run.
npx tsc --noEmit
[ "${1:-}" = fast ] && exit 0
npx vitest run
