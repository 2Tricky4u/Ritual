#!/usr/bin/env bash
# Fake cargo-mutants for e2e tests: answers --version, records argv, and
# writes a canned outcomes.json under the --output dir ritual passes.
# FAKE_MUTANTS_EXIT overrides the exit code (default 2 = missed mutants).
set -euo pipefail

if [ "${1:-}" = "--version" ]; then
  echo "fake-mutants 0.0.0"
  exit 0
fi

if [ -n "${FAKE_MUTANTS_ARGV_LOG:-}" ]; then
  printf '%s\n' "$@" >"$FAKE_MUTANTS_ARGV_LOG"
fi

out=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--output" ]; then out="$a"; fi
  prev="$a"
done
if [ -z "$out" ]; then
  echo "fake_mutants: no --output dir in argv" >&2
  exit 1
fi

mkdir -p "$out/mutants.out"
cat >"$out/mutants.out/outcomes.json" <<'JSON'
{
  "cargo_mutants_version": "27.1.0",
  "outcomes": [
    {"scenario": "Baseline", "summary": "Success"},
    {"scenario": {"Mutant": {"package": "x", "file": "src/lib.rs",
        "function": {"function_name": "canned_fn", "return_type": "-> bool"},
        "span": {"start": {"line": 3, "column": 1}, "end": {"line": 3, "column": 9}},
        "genre": "FnValue", "replacement": "true"}},
     "summary": "MissedMutant"},
    {"scenario": {"Mutant": {"file": "src/lib.rs",
        "function": {"function_name": "other_fn"},
        "span": {"start": {"line": 9}}, "genre": "FnValue", "replacement": "0"}},
     "summary": "CaughtMutant"}
  ],
  "summary": {"total_mutants": 2}
}
JSON

exit "${FAKE_MUTANTS_EXIT:-2}"
