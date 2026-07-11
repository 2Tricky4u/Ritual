#!/usr/bin/env bash
set -e
npx eslint . --max-warnings 0
[ "${1:-}" = fast ] && exit 0
npx tsc --noEmit
npx vitest run
