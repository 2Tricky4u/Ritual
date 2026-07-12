#!/usr/bin/env bash
# Fake agent for token-free testing. Replays a fixture as if it were
# `claude -p ... --output-format stream-json`. Ignores all arguments.
#
#   RITUAL_CLAUDE_CMD="tests/fake_agent.sh" ritual run plan-review ...
#
# Env:
#   FAKE_AGENT_FIXTURE    fixture to replay (default: claude_toolrich.jsonl)
#   FAKE_AGENT_DELAY      seconds between lines (default: 0.05)
#   FAKE_AGENT_EXIT       exit code (default: 0)
#   FAKE_AGENT_FINDINGS   if set, write a canned findings file there before exiting
#   FAKE_AGENT_SESSION_ID if set, prepend a system/init line carrying this id
#   FAKE_AGENT_TRUNCATE   if set, stop after the first 2 fixture lines (no
#                         result event — an interrupted stream)
set -u
# `<fake> login status` = the codex auth preflight: always "logged in".
if [ "${1:-}" = "login" ] && [ "${2:-}" = "status" ]; then
  echo "Logged in using ChatGPT (fake)"
  exit 0
fi
# `<fake> --version` = provenance probe: answer instantly.
if [ "${1:-}" = "--version" ]; then
  echo "fake-agent 0.0.0"
  exit 0
fi
# `<fake> auth status` = the claude auth probe (doctor / sidebar).
if [ "${1:-}" = "auth" ] && [ "${2:-}" = "status" ]; then
  echo '{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"fake"}'
  exit 0
fi
# `<fake> mcp list` = MCP registry probe.
if [ "${1:-}" = "mcp" ] && [ "${2:-}" = "list" ]; then
  echo "codex: codex mcp-server - ✓ connected"
  exit 0
fi
dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fixture="${FAKE_AGENT_FIXTURE:-$dir/fixtures/claude_toolrich.jsonl}"
delay="${FAKE_AGENT_DELAY:-0.05}"

if [ -n "${FAKE_AGENT_SESSION_ID:-}" ]; then
  printf '{"type":"system","subtype":"init","session_id":"%s","model":"fake-model"}\n' \
    "$FAKE_AGENT_SESSION_ID"
fi

n=0
while IFS= read -r line; do
  printf '%s\n' "$line"
  sleep "$delay"
  n=$((n + 1))
  if [ -n "${FAKE_AGENT_TRUNCATE:-}" ] && [ "$n" -ge 2 ]; then
    exit "${FAKE_AGENT_EXIT:-0}"   # died mid-stream: no result event
  fi
done < "$fixture"

# Simulate the /spec skill editing a document in place (appends a marker line).
if [ -n "${FAKE_AGENT_SPEC_EDIT:-}" ]; then
  mkdir -p "$(dirname "$FAKE_AGENT_SPEC_EDIT")"
  printf 'A concrete change applied by the fake agent.\n' >> "$FAKE_AGENT_SPEC_EDIT"
fi

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
