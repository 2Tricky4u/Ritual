#!/usr/bin/env bash
# Fake agent for token-free testing. Replays a fixture as if it were
# `claude -p ... --output-format stream-json`. Ignores all arguments.
#
#   RITUAL_CLAUDE_CMD="tests/fake_agent.sh" ritual run plan-review ...
#
# Env:
#   FAKE_AGENT_FIXTURE  fixture to replay (default: claude_toolrich.jsonl)
#   FAKE_AGENT_DELAY    seconds between lines (default: 0.05)
#   FAKE_AGENT_EXIT     exit code (default: 0)
#   FAKE_AGENT_FINDINGS if set, write a canned findings file there before exiting
set -u
dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fixture="${FAKE_AGENT_FIXTURE:-$dir/fixtures/claude_toolrich.jsonl}"
delay="${FAKE_AGENT_DELAY:-0.05}"

while IFS= read -r line; do
  printf '%s\n' "$line"
  sleep "$delay"
done < "$fixture"

if [ -n "${FAKE_AGENT_FINDINGS:-}" ]; then
  mkdir -p "$(dirname "$FAKE_AGENT_FINDINGS")"
  cat > "$FAKE_AGENT_FINDINGS" <<'EOF'
{
  "ritual_findings": 1,
  "stage": "dual-review",
  "branch": "test-branch",
  "generated_at": "2026-07-11T00:00:00Z",
  "source_models": {"claude": "claude-test", "codex": "codex-test"},
  "findings": [
    {"id": 1, "severity": "critical", "title": "Canned test finding", "file": "src/main.rs", "line": 1,
     "plan_step": null, "scenario": "fake agent scenario", "sources": ["claude", "codex"],
     "verdict": "confirmed", "action": "pending"}
  ]
}
EOF
fi

exit "${FAKE_AGENT_EXIT:-0}"
