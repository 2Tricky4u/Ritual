#!/usr/bin/env bash
# PostToolUse hook (Edit|Write): run the project's fast checks and feed
# failures back to Claude. Projects without an executable ./check.sh opt out.
cd "${CLAUDE_PROJECT_DIR:-.}" 2>/dev/null || exit 0
[ -x ./check.sh ] || exit 0

out=$(./check.sh fast 2>&1)
status=$?
if [ $status -ne 0 ]; then
  {
    echo "./check.sh fast failed (exit $status). Fix before continuing:"
    echo "$out" | tail -60
  } >&2
  exit 2  # blocking feedback: stderr is fed back to Claude
fi
exit 0
