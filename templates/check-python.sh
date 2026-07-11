#!/usr/bin/env bash
set -e
ruff check .
ruff format --check .
[ "${1:-}" = fast ] && exit 0
pyright .
pytest -q
