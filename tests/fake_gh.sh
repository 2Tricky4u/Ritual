#!/usr/bin/env bash
# Fake GitHub CLI for token-free pr-comment tests. Records every invocation's
# argv and stdin under $FAKE_GH_LOG_DIR (default: cwd).
set -u
log_dir="${FAKE_GH_LOG_DIR:-.}"
mkdir -p "$log_dir"
printf '%s\n' "$*" >> "$log_dir/gh-args.log"

if [ "${1:-}" = "pr" ] && [ "${2:-}" = "view" ]; then
  case "$*" in
    *headRefOid*) echo '{"headRefOid":"abc123def456"}' ;;
    *number*) echo '{"number":7}' ;;
  esac
  exit 0
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "comment" ]; then
  cat >> "$log_dir/gh-stdin.log"
  exit 0
fi
if [ "${1:-}" = "api" ]; then
  exit 0
fi
exit 0
