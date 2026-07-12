#!/usr/bin/env bash
# Live end-to-end smoke test of the REAL installed `ritual` binary.
# Drives the whole lifecycle token-free via the fake-agent seam. Every step
# asserts exit code (and often an output substring). Prints a PASS/FAIL table.
set -u

RITUAL="$HOME/.local/bin/ritual"
FAKE="$HOME/Documents/project/ritual/tests/fake_agent.sh"
ROOT="$(mktemp -d)"
PROJ="$ROOT/proj"
PASS=0; FAIL=0; FAILED_STEPS=()

# --- fake helpers -----------------------------------------------------------
mkdir -p "$ROOT/bin"
# Fake $EDITOR: append meaningful content, exit 0.
cat > "$ROOT/bin/fake-editor" <<'EOF'
#!/usr/bin/env bash
printf '\nImplement the widget end to end.\n' >> "$1"
exit 0
EOF
# Fake "claude" for the interactive PLAN stage: write plan.md, exit 0.
cat > "$ROOT/bin/fake-plan" <<EOF
#!/usr/bin/env bash
# ritual runs this from the work_root; write the plan where plan-review looks.
mkdir -p "$PROJ/.ritual/features/main"
printf '# Plan\n\n1. do the thing\n' > "$PROJ/.ritual/features/main/plan.md"
exit 0
EOF
chmod +x "$ROOT/bin/fake-editor" "$ROOT/bin/fake-plan"

# --- assert helpers ---------------------------------------------------------
# ok "<name>" <expected_exit> <substring|-> -- <command...>
ok() {
  local name="$1" want="$2" needle="$3"; shift 3; [ "$1" = "--" ] && shift
  local out rc
  out="$("$@" 2>&1)"; rc=$?
  local why=""
  [ "$rc" != "$want" ] && why="exit $rc≠$want"
  if [ "$needle" != "-" ] && ! grep -qF -- "$needle" <<<"$out"; then
    why="${why:+$why; }missing '$needle'"
  fi
  if [ -z "$why" ]; then
    printf '  \033[32m✓\033[0m %s\n' "$name"; PASS=$((PASS+1))
  else
    printf '  \033[31m✗\033[0m %s  (%s)\n' "$name" "$why"; FAIL=$((FAIL+1))
    FAILED_STEPS+=("$name: $why")
    printf '%s\n' "$out" | sed 's/^/      | /' | head -6
  fi
}
# assert a file/glob exists
exists() { # <name> <path-or-glob>
  # shellcheck disable=SC2086
  if compgen -G "$2" >/dev/null; then
    printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS+1))
  else
    printf '  \033[31m✗\033[0m %s  (no match: %s)\n' "$1" "$2"; FAIL=$((FAIL+1))
    FAILED_STEPS+=("$1: missing $2")
  fi
}

cleanup() {
  git -C "$PROJ" worktree remove --force "$ROOT/proj-feat-parallel" 2>/dev/null
  rm -rf "$ROOT"
}
trap cleanup EXIT

export PATH="$ROOT/bin:$PATH"
run() { ( cd "$PROJ" && env "$@" ); }  # helper for readability (unused wrapper)

echo "── scaffold & feature ───────────────────────────────────────────────"
mkdir -p "$PROJ"
git -C "$PROJ" init -q -b main
printf '[package]\nname="x"\n' > "$PROJ/Cargo.toml"
ok "init scaffolds"            0 "check.sh"  -- bash -c "cd '$PROJ' && '$RITUAL' init"
exists "state.json written"   "$PROJ/.ritual/state.json"
exists "check.sh written"     "$PROJ/check.sh"
ok "status empty"             0 "no features yet" -- bash -c "cd '$PROJ' && '$RITUAL' status"
ok "new feature"              0 "-"         -- bash -c "cd '$PROJ' && '$RITUAL' new Widget Feature"
ok "status shows feature"     0 "Widget Feature" -- bash -c "cd '$PROJ' && '$RITUAL' status"
ok "status --json valid"      0 "\"current_branch\": \"main\"" -- bash -c "cd '$PROJ' && '$RITUAL' status --json"

echo "── interactive stages (spec, plan) ──────────────────────────────────"
ok "spec via \$EDITOR"        0 "spec done" -- bash -c "cd '$PROJ' && EDITOR='$ROOT/bin/fake-editor' '$RITUAL' run spec"
ok "plan writes plan.md"      0 "-"         -- bash -c "cd '$PROJ' && RITUAL_CLAUDE_CMD='$ROOT/bin/fake-plan' '$RITUAL' run plan"
exists "plan.md present"      "$PROJ/.ritual/features/main/plan.md"

echo "── spec/plan chat (headless doc edit) ───────────────────────────────"
ok "chat edits the spec"      0 "spec updated" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  FAKE_AGENT_SPEC_EDIT='.ritual/features/main/spec.md' \
  '$RITUAL' chat 'add a low-latency requirement' --section Goal"
ok "chat targets the plan"    0 "plan updated" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  FAKE_AGENT_SPEC_EDIT='.ritual/features/main/plan.md' \
  '$RITUAL' chat 'add a rollback step' --plan"

echo "── headless plan-review (daemonized) ────────────────────────────────"
ok "plan-review ok"           0 "plan-review ok" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  FAKE_AGENT_FINDINGS='.ritual/findings/20260712T130000Z-plan-review.json' \
  '$RITUAL' run plan-review"
exists "raw archive (.jsonl)" "$PROJ/.ritual/runs/"*plan-review.jsonl
exists "meta.json"            "$PROJ/.ritual/runs/"*plan-review.meta.json
exists "findings file"        "$PROJ/.ritual/findings/"*plan-review.json
# critical + confirmed finding -> findings exits 1 (scriptability contract)
ok "findings exit 1 on crit"  1 "Canned test finding" -- bash -c "cd '$PROJ' && '$RITUAL' findings"
ok "findings --json"          1 "ritual_findings" -- bash -c "cd '$PROJ' && '$RITUAL' findings --json"

echo "── history / report / provenance ────────────────────────────────────"
ok "history lists run"        0 "plan-review" -- bash -c "cd '$PROJ' && '$RITUAL' history"
ok "history --json array"     0 "run_id"    -- bash -c "cd '$PROJ' && '$RITUAL' history --json"
ok "report markdown"          0 "report:"   -- bash -c "cd '$PROJ' && '$RITUAL' report"
# report --pdf: exit 0 either way — produces a PDF if an engine works, else
# gracefully keeps the markdown. Here we only require the graceful contract.
ok "report --pdf graceful"    0 "report:"   -- bash -c "cd '$PROJ' && '$RITUAL' report --pdf"
if compgen -G "$PROJ/.ritual/reports/*.pdf" >/dev/null; then
  printf '  \033[32m✓\033[0m pdf produced (engine available)\n'; PASS=$((PASS+1))
else
  printf '  \033[33m•\033[0m pdf skipped (no working PDF engine in this env — markdown kept)\n'
fi
ok "verify-log intact"        0 "chain intact" -- bash -c "cd '$PROJ' && '$RITUAL' verify-log"

RID="$(cd "$PROJ" && "$RITUAL" history --json | jq -r '.[0].run_id')"
ok "repro shows bundle"       0 "git_commit" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' '$RITUAL' repro '$RID'"
ok "repro unknown errors"     1 "no run"    -- bash -c "cd '$PROJ' && '$RITUAL' repro nope"

echo "── export (OTLP) ────────────────────────────────────────────────────"
ok "export stdout"            0 "ritual:plan-review" -- bash -c "cd '$PROJ' && '$RITUAL' export"
ok "export --out file"        0 "span(s) exported"   -- bash -c "cd '$PROJ' && '$RITUAL' export --out '$ROOT/spans.jsonl'"
exists "spans file written"   "$ROOT/spans.jsonl"

echo "── bench ────────────────────────────────────────────────────────────"
ok "bench 2 runs scorecard"   0 "ok-rate 100%" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  '$RITUAL' bench plan-review --runs 2"
ok "bench rejects interactive" 1 "only supports headless" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' '$RITUAL' bench spec --runs 1"

echo "── CI mode (JUnit) ──────────────────────────────────────────────────"
# New findings file -> blocking critical -> nonzero exit + JUnit XML.
ok "ci fails on blocking"     1 "blocking finding" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  FAKE_AGENT_FINDINGS='.ritual/findings/20260712T140000Z-plan-review.json' \
  '$RITUAL' run plan-review --ci"
exists "junit xml written"    "$PROJ/.ritual/ci/"*.xml

echo "── failure & budget paths ───────────────────────────────────────────"
ok "agent failure -> error"   1 "failed"    -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 FAKE_AGENT_EXIT=3 \
  '$RITUAL' run plan-review"
sleep 0.5  # let the daemon fully finalize before the next daemon run
printf 'budget_daily_usd = 0.001\n' > "$PROJ/.ritual/config.toml"
ok "budget ceiling blocks"    1 "daily budget reached" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  '$RITUAL' run plan-review"
ok "--force overrides budget" 0 "plan-review ok" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  FAKE_AGENT_FINDINGS='.ritual/findings/20260712T150000Z-plan-review.json' \
  '$RITUAL' run plan-review --force"
rm -f "$PROJ/.ritual/config.toml"
ok "bad keybinding rejected"  1 "unknown action" -- bash -c "cd '$PROJ' && \
  printf '[keys]\n\"nope\" = \"s\"\n' > '$PROJ/.ritual/config.toml' && '$RITUAL' status"
rm -f "$PROJ/.ritual/config.toml"
ok "unknown stage errors"     1 "unknown stage" -- bash -c "cd '$PROJ' && '$RITUAL' run shoggoth"

echo "── tamper detection ─────────────────────────────────────────────────"
ARCHIVE="$(ls "$PROJ/.ritual/runs/"*plan-review.jsonl | head -1)"
printf 'tampered!\n' > "$ARCHIVE"
ok "verify-log detects tamper" 1 "CHAIN BROKEN" -- bash -c "cd '$PROJ' && '$RITUAL' verify-log"

echo "── worktree parallelism ─────────────────────────────────────────────"
git -C "$PROJ" add -A
git -C "$PROJ" -c user.email=t@t -c user.name=t commit -qm init
ok "new --worktree"           0 "worktree:" -- bash -c "cd '$PROJ' && '$RITUAL' new Parallel --worktree feat/parallel"
exists "worktree checkout"    "$ROOT/proj-feat-parallel"
ok "status from worktree"     0 "\"current_branch\": \"feat/parallel\"" -- bash -c \
  "cd '$ROOT/proj-feat-parallel' && '$RITUAL' status --json"

echo "── daemon survives launcher death ───────────────────────────────────"
# Slow agent; kill the launcher mid-run; the detached daemon must still
# write its meta. Detect by a *new* meta appearing (run_id is time-based, so
# don't key off the findings filename).
metas_before=$(ls "$PROJ/.ritual/runs/"*.meta.json 2>/dev/null | wc -l)
( cd "$PROJ" && RITUAL_CLAUDE_CMD="$FAKE" RITUAL_CODEX_CMD="$FAKE" FAKE_AGENT_DELAY=0.3 \
  FAKE_AGENT_FINDINGS=".ritual/findings/daemon-survival.json" \
  "$RITUAL" run plan-review >/dev/null 2>&1 ) &
LAUNCHER=$!
sleep 1.2
kill -9 "$LAUNCHER" 2>/dev/null; wait "$LAUNCHER" 2>/dev/null
# Wait for the meta count to grow (the daemon finalized on its own).
survived=1
for _ in $(seq 1 60); do   # up to 30s under load
  now=$(ls "$PROJ/.ritual/runs/"*.meta.json 2>/dev/null | wc -l)
  if [ "$now" -gt "$metas_before" ]; then survived=0; break; fi
  sleep 0.5
done
if [ "$survived" = 0 ]; then
  printf '  \033[32m✓\033[0m daemon survived launcher kill\n'; PASS=$((PASS+1))
else
  printf '  \033[31m✗\033[0m daemon survived launcher kill  (no meta)\n'; FAIL=$((FAIL+1))
  FAILED_STEPS+=("daemon survival: no meta")
fi

echo
echo "═════════════════════════════════════════════════════════════════════"
printf 'E2E: \033[32m%d passed\033[0m, ' "$PASS"
if [ "$FAIL" = 0 ]; then printf '\033[32m0 failed\033[0m\n'; else printf '\033[31m%d failed\033[0m\n' "$FAIL"; fi
for s in "${FAILED_STEPS[@]:-}"; do [ -n "$s" ] && printf '   ✗ %s\n' "$s"; done
exit "$FAIL"
