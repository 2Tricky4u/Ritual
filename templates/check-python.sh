#!/usr/bin/env bash
set -e
ruff check .
ruff format --check .
# fast = lint + typecheck (the snippet's promise); tests only on the full run.
pyright .
[ "${1:-}" = fast ] && exit 0
pytest -q
