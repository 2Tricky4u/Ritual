#!/usr/bin/env bash
# Fake gitleaks for e2e tests: answers `version`, and for `dir <path>` writes
# a canned JSON report to the --report-path ritual passes. The canned finding
# points at <scan dir>/leaky.py so path-stripping is exercised.
# FAKE_GITLEAKS_EXIT overrides the exit code (default 2 = leaks found).
set -euo pipefail

if [ "${1:-}" = "version" ]; then
  echo "fake-gitleaks 0.0.0"
  exit 0
fi

scan_dir="${2:?fake_gitleaks: expected 'dir <path>'}"
report=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--report-path" ]; then report="$a"; fi
  prev="$a"
done
if [ -z "$report" ]; then
  echo "fake_gitleaks: no --report-path in argv" >&2
  exit 1
fi

code="${FAKE_GITLEAKS_EXIT:-2}"
if [ "$code" = "0" ]; then
  echo "[]" >"$report"
else
  cat >"$report" <<JSON
[
  {"RuleID": "generic-api-key", "Description": "Generic API Key",
   "File": "$scan_dir/leaky.py", "StartLine": 2,
   "Line": "api_key = \"REDACTED\"",
   "Fingerprint": "$scan_dir/leaky.py:generic-api-key:2",
   "Secret": "REDACTED", "Entropy": 3.7}
]
JSON
fi
exit "$code"
