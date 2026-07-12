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

echo "── ps / attach ──────────────────────────────────────────────────────"
ok "ps with nothing live"     0 "no live runs" -- bash -c "cd '$PROJ' && '$RITUAL' ps"
RID_LAST="$(cd "$PROJ" && "$RITUAL" history --json | jq -r '.[0].run_id')"
ok "attach finished run"      0 "-"          -- bash -c "cd '$PROJ' && '$RITUAL' attach '$RID_LAST'"
ok "attach unknown errors"    1 "no such run" -- bash -c "cd '$PROJ' && '$RITUAL' attach nope"

echo "── clean (today-protection) ─────────────────────────────────────────"
# Every run so far is today-dated: clean must protect them all.
ok "clean dry-run"            0 "0 group(s) would delete" -- bash -c "cd '$PROJ' && '$RITUAL' clean --keep 0 --dry-run"
ok "clean keeps today's runs" 0 "started today" -- bash -c "cd '$PROJ' && '$RITUAL' clean --keep 0"
ok "verify-log still intact"  0 "chain intact" -- bash -c "cd '$PROJ' && '$RITUAL' verify-log"

echo "── workbench install + doctor ───────────────────────────────────────"
CLAUDE_HOME="$ROOT/claude-home"
ok "init --skills installs"   0 "workbench →" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_HOME='$CLAUDE_HOME' '$RITUAL' init --skills"
exists "spec skill installed" "$CLAUDE_HOME/skills/spec/SKILL.md"
printf '{"hooks":{"PostToolUse":[{"hooks":[{"command":"check-on-edit.sh"}]}]}}\n' > "$CLAUDE_HOME/settings.json"
ok "doctor healthy"           0 "0 failure(s)" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_HOME='$CLAUDE_HOME' RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' \
  '$RITUAL' doctor"

echo "── pr-comment (fake gh) ─────────────────────────────────────────────"
FAKE_GH="$HOME/Documents/project/ritual/tests/fake_gh.sh"
cat > "$PROJ/.ritual/findings/20260712T200000Z-dual-review.json" <<'EOF'
{"ritual_findings":1,"stage":"dual-review","branch":"main",
 "findings":[{"id":1,"severity":"major","title":"live driver finding",
              "file":"src/x.rs","line":1,"scenario":"s","sources":["claude"],
              "verdict":"confirmed","action":"pending"}]}
EOF
ok "pr-comment posts"         0 "posted summary comment on #7" -- bash -c "cd '$PROJ' && \
  RITUAL_GH_CMD='$FAKE_GH' FAKE_GH_LOG_DIR='$ROOT' '$RITUAL' pr-comment"
ok "body reached gh"          0 "live driver finding" -- grep "live driver finding" "$ROOT/gh-stdin.log"

if [ "${RITUAL_LIVE_SMOKE:-0}" = "1" ]; then
  echo "── LIVE doc-chat scoping smoke (real claude, ~cents) ────────────────"
  # Validates the dontAsk + Edit(//abs) permission syntax against the real
  # CLI: the /spec skill must succeed in editing ONLY the target doc.
  ok "live chat edits spec"   0 "spec updated" -- bash -c "cd '$PROJ' && \
    '$RITUAL' chat 'replace the Goal section body with exactly: smoke test goal' --section Goal --force"
  ok "live edit landed"       0 "smoke test goal" -- grep "smoke test goal" "$PROJ/.ritual/features/main/spec.md"
fi

echo "── tamper detection ─────────────────────────────────────────────────"
ARCHIVE="$(ls "$PROJ/.ritual/runs/"*plan-review.jsonl | head -1)"
printf 'tampered!\n' > "$ARCHIVE"
ok "verify-log detects tamper" 1 "CHAIN BROKEN" -- bash -c "cd '$PROJ' && '$RITUAL' verify-log"
# All runs are today-protected here, so clean must delete nothing even with
# --keep 0 (the broken-chain refusal path is covered by unit tests with aged
# runs — live runs are always today-dated).
ok "clean deletes nothing (broken chain + today)" 0 "0 group(s) deleted" -- bash -c "cd '$PROJ' && \
  '$RITUAL' clean --keep 0"

echo "── worktree parallelism ─────────────────────────────────────────────"
git -C "$PROJ" add -A
git -C "$PROJ" -c user.email=t@t -c user.name=t commit -qm init
ok "new --worktree"           0 "worktree:" -- bash -c "cd '$PROJ' && '$RITUAL' new Parallel --worktree feat/parallel"
exists "worktree checkout"    "$ROOT/proj-feat-parallel"
ok "status from worktree"     0 "\"current_branch\": \"feat/parallel\"" -- bash -c \
  "cd '$ROOT/proj-feat-parallel' && '$RITUAL' status --json"

echo "── v0.5: mutation + secrets gates ───────────────────────────────────"
FAKE_MUTANTS="$HOME/Documents/project/ritual/tests/fake_mutants.sh"
FAKE_GITLEAKS="$HOME/Documents/project/ritual/tests/fake_gitleaks.sh"
ok "mutants: empty diff no-op" 0 "nothing to mutate" -- bash -c "cd '$PROJ' && \
  RITUAL_MUTANTS_CMD='$FAKE_MUTANTS' '$RITUAL' mutants"
printf '\n# touched\n' >> "$PROJ/Cargo.toml"
ok "mutants: survivors -> findings" 0 "1 caught, 1 missed" -- bash -c "cd '$PROJ' && \
  RITUAL_MUTANTS_CMD='$FAKE_MUTANTS' '$RITUAL' mutants"
exists "mutants findings file" "$PROJ/.ritual/findings/"*-mutants.json
printf 'x = 1\napi_key = "h"\n' > "$PROJ/leaky.py"
ok "secrets: leaks block"     1 ".gitleaksignore" -- bash -c "cd '$PROJ' && \
  RITUAL_GITLEAKS_CMD='$FAKE_GITLEAKS' '$RITUAL' secrets"
exists "secrets findings file" "$PROJ/.ritual/findings/"*-secrets.json
ok "secrets fingerprint recorded" 0 "leaky.py:generic-api-key" -- \
  grep -r "leaky.py:generic-api-key" "$PROJ/.ritual/findings/"
rm -f "$PROJ/leaky.py"

echo "── v0.5: lessons, costs, retry, invariants ──────────────────────────"
# Dismiss the mutants finding -> it becomes review memory.
sed -i 's/"action": "pending"/"action": "dismissed"/' "$PROJ/.ritual/findings/"*-mutants.json
ok "lessons distill dispositions" 0 "Known noise" -- bash -c "cd '$PROJ' && '$RITUAL' lessons --stdout"
ok "costs rolls up per stage" 0 "plan-review" -- bash -c "cd '$PROJ' && '$RITUAL' costs --json"
ok "run --model overrides"    0 "plan-review ok" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  FAKE_AGENT_FINDINGS='.ritual/findings/20260712T210000Z-plan-review.json' \
  '$RITUAL' run plan-review --model fake-model-x"
ok "override reached the argv" 0 "fake-model-x" -- bash -c "cd '$PROJ' && \
  '$RITUAL' history --json | jq -r '.[0].argv | join(\" \")'"
printf '# Invariants\n- parsers never panic\n' > "$PROJ/.ritual/invariants.md"
ok "doctor sees the constitution" 0 "enforced by review stages" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_HOME='$CLAUDE_HOME' RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' \
  '$RITUAL' doctor"

echo "── v0.5: sandbox wrapper + coderabbit ───────────────────────────────"
FAKE_WRAP="$HOME/Documents/project/ritual/tests/fake_wrapper.sh"
FAKE_CR="$HOME/Documents/project/ritual/tests/fake_coderabbit.sh"
printf '[sandbox]\nenabled = true\nwrapper = "%s"\n' "$FAKE_WRAP" > "$PROJ/.ritual/config.toml"
ok "sandboxed run completes"  0 "plan-review ok" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  FAKE_WRAPPER_LOG='$ROOT/wrapper.log' \
  FAKE_AGENT_FINDINGS='.ritual/findings/20260712T220000Z-plan-review.json' \
  '$RITUAL' run plan-review"
ok "wrapper actually wrapped" 0 "wrapped:" -- cat "$ROOT/wrapper.log"
printf '[coderabbit]\nenabled = true\n' > "$PROJ/.ritual/config.toml"
ok "coderabbit lands findings" 0 "coderabbit review →" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' RITUAL_CODERABBIT_CMD='$FAKE_CR' \
  FAKE_AGENT_DELAY=0 FAKE_AGENT_FINDINGS='.ritual/findings/20260712T230000Z-dual-review.json' \
  '$RITUAL' run dual-review"
exists "coderabbit findings file" "$PROJ/.ritual/findings/"*-coderabbit.json
rm -f "$PROJ/.ritual/config.toml"

echo "── v0.5: skills diff + audit trail ──────────────────────────────────"
ok "skills diff identical"    0 "identical" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_HOME='$CLAUDE_HOME' '$RITUAL' skills diff"
printf '\nLOCAL TWEAK\n' >> "$CLAUDE_HOME/skills/tdd/SKILL.md"
ok "skills diff flags drift"  0 "differs at line" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_HOME='$CLAUDE_HOME' '$RITUAL' skills diff"
cat > "$ROOT/bin/verify-audit" <<'EOF'
#!/usr/bin/env python3
import json, hashlib, sys
lines = [l.rstrip("\n") for l in open(sys.argv[1]) if l.strip()]
prev = None
for i, l in enumerate(lines):
    r = json.loads(l)
    if i == 0:
        assert r["prev_hash"] is None, "genesis must have null prev_hash"
    else:
        expect = hashlib.sha256(prev.encode()).hexdigest()
        assert r["prev_hash"] == expect, f"broken link at record {i}"
    for k in ["record_id","timestamp","agent_id","session_id","action_type","outcome","trust_level"]:
        assert r[k], f"missing {k}"
    prev = l
print(f"chain ok ({len(lines)} records)")
EOF
chmod +x "$ROOT/bin/verify-audit"
ok "audit-trail exports"      0 "audit record(s) exported" -- bash -c "cd '$PROJ' && \
  '$RITUAL' export --audit-trail --out '$ROOT/audit.jsonl'"
ok "audit chain re-verifies"  0 "chain ok" -- "$ROOT/bin/verify-audit" "$ROOT/audit.jsonl"

echo "── v0.5.1: aged clean, live attach --kill, undo stack ───────────────"
# Aged UNCHAINED runs actually get PRUNED (today's stay protected) — the
# real deletion path, not just today-protection.
for i in 1 2; do
  printf '{"run_id":"20260101T00000%sZ-old","stage":"plan-review","ok":true,"started_at":"2026-01-01T00:00:0%sZ"}\n' "$i" "$i" \
    > "$PROJ/.ritual/runs/20260101T00000${i}Z-old.meta.json"
  printf 'line\n' > "$PROJ/.ritual/runs/20260101T00000${i}Z-old.jsonl"
done
ok "clean prunes aged runs"   0 "2 group(s) deleted" -- bash -c "cd '$PROJ' && '$RITUAL' clean --keep 0"
ok "aged meta actually gone"  1 "-" -- test -f "$PROJ/.ritual/runs/20260101T000001Z-old.meta.json"

# A live slow run shows in ps and dies to attach --kill.
( cd "$PROJ" && RITUAL_CLAUDE_CMD="$FAKE" RITUAL_CODEX_CMD="$FAKE" FAKE_AGENT_DELAY=2 \
  "$RITUAL" run plan-review >/dev/null 2>&1 ) &
SLOW_LAUNCHER=$!
sleep 1.5
LIVE_ID="$(cd "$PROJ" && "$RITUAL" ps 2>/dev/null | awk 'NR==2{print $1}')"
ok "ps lists the live run"    0 "plan-review" -- bash -c "cd '$PROJ' && '$RITUAL' ps"
ok "attach --kill stops it"   0 "killed" -- bash -c "cd '$PROJ' && '$RITUAL' attach '$LIVE_ID' --kill"
wait "$SLOW_LAUNCHER" 2>/dev/null
sleep 0.5

# A second chat edit deepens the persisted undo stack (v0.5.1: stack, not swap).
ok "chat edit deepens undo"   0 "-" -- bash -c "cd '$PROJ' && \
  RITUAL_CLAUDE_CMD='$FAKE' RITUAL_CODEX_CMD='$FAKE' FAKE_AGENT_DELAY=0 \
  FAKE_AGENT_SPEC_EDIT='.ritual/features/main/spec.md' \
  '$RITUAL' chat 'tighten the goal once more'"
UNDO_EXPECT=2
[ "${RITUAL_LIVE_SMOKE:-0}" = "1" ] && UNDO_EXPECT=3 # the live smoke chats once more
ok "undo stack depth $UNDO_EXPECT" 0 "$UNDO_EXPECT" -- bash -c "ls '$PROJ/.ritual/features/main/.undo/spec' | wc -l"

# audit-trail zero-run guard in a completely fresh project.
FRESH="$ROOT/fresh"
mkdir -p "$FRESH" && git -C "$FRESH" init -q -b main
( cd "$FRESH" && "$RITUAL" init >/dev/null 2>&1 )
ok "audit-trail on empty history" 0 "0 audit record(s)" -- bash -c "cd '$FRESH' && '$RITUAL' export --audit-trail 2>&1"

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
