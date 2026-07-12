#!/usr/bin/env bash
# Fake CodeRabbit CLI: answers --version; `review --agent ...` prints a canned
# agent-mode JSON review to stdout. FAKE_CODERABBIT_EXIT forces a failure.
set -euo pipefail

if [ "${1:-}" = "--version" ]; then
  echo "fake-coderabbit 0.0.0"
  exit 0
fi

if [ "${FAKE_CODERABBIT_EXIT:-0}" != "0" ]; then
  echo "rate limit exceeded (3 reviews/hour)" >&2
  exit "$FAKE_CODERABBIT_EXIT"
fi

cat <<'JSON'
{
  "review": {"id": "cr-fake-1", "status": "completed"},
  "comments": [
    {"file": "src/lib.rs", "line": 5, "severity": "high",
     "comment": "Canned rabbit finding: unchecked index may panic",
     "code_snippet": "let x = v[i];"}
  ]
}
JSON
