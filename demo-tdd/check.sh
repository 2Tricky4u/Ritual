#!/usr/bin/env bash
set -e
python3 -m py_compile app.py
[ "${1:-}" = fast ] && exit 0
python3 -m unittest discover -q
