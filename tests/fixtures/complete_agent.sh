#!/usr/bin/env bash
# Fake agent for the `ritual complete` loop e2e (token-free). It branches on the
# prompt and uses the TREE as state, so the loop converges naturally:
#   - /coverage prompt  -> writes a coverage findings file: a gap for D1 while
#                          `media.txt` is missing, else `satisfied:["D1"]`.
#   - code-fix prompt   -> builds `media.txt` (unless COMPLETE_AGENT_STUCK=1,
#                          simulating a deliverable it cannot satisfy).
# Always emits one valid stream-json result so the run's meta is ok.
set -u

# Auth / probe shims (mirror fake_agent.sh).
if [ "${1:-}" = "login" ] && [ "${2:-}" = "status" ]; then echo "Logged in (fake)"; exit 0; fi
if [ "${1:-}" = "--version" ]; then echo "fake-agent 0.0.0"; exit 0; fi
if [ "${1:-}" = "auth" ] && [ "${2:-}" = "status" ]; then
  echo '{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"fake"}'; exit 0
fi
if [ "${1:-}" = "mcp" ] && [ "${2:-}" = "list" ]; then echo "codex: - ✓ connected"; exit 0; fi

prompt="$*"

# A valid result event so the run counts as ok.
printf '{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"fake","total_cost_usd":0.01}\n'

fdir="${RITUAL_FINDINGS_DIR:-.ritual/findings}"
ts="$(date -u +%Y%m%dT%H%M%S%N)Z"

case "$prompt" in
  */coverage*)
    if [ -n "${COMPLETE_AGENT_NO_REPORT:-}" ]; then
      exit 0   # simulate a judge that produced no coverage report this round
    fi
    mkdir -p "$fdir"
    if [ -f "media.txt" ]; then
      printf '{"ritual_findings":1,"stage":"coverage","satisfied":["D1"],"findings":[]}\n' \
        > "$fdir/${ts}-coverage.json"
    else
      printf '{"ritual_findings":1,"stage":"coverage","satisfied":[],"findings":[{"id":1,"severity":"major","title":"media file missing","deliverable":"D1","file":"media.txt","plan_step":null,"scenario":"media.txt not built","sources":["coverage"],"verdict":"confirmed","action":"pending"}]}\n' \
        > "$fdir/${ts}-coverage.json"
    fi
    ;;
  *"Fix these code review findings"*)
    if [ -z "${COMPLETE_AGENT_STUCK:-}" ]; then
      printf 'built by the fix agent\n' > media.txt
    fi
    ;;
esac

exit 0
