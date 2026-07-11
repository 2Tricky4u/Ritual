#!/usr/bin/env bash
set -e
if [ -f Cargo.toml ]; then
  cargo clippy --all-targets -- -D warnings
  [ "${1:-}" != fast ] && cargo test
fi
if [ -f pyproject.toml ]; then
  ruff check .
  [ "${1:-}" != fast ] && pytest -q
fi
if [ -f package.json ]; then
  npx eslint . --max-warnings 0
  [ "${1:-}" != fast ] && npx vitest run
fi
exit 0
