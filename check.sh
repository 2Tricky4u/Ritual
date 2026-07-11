#!/usr/bin/env bash
set -e
cargo fmt --check
cargo clippy --all-targets -- -D warnings
[ "${1:-}" = fast ] && exit 0
cargo test
