---
name: dual-review
description: Independent two-model review of the current diff - Claude's code-reviewer subagent and OpenAI Codex review the same changes without seeing each other's findings, then results are merged and only confirmed findings are fixed. Use before committing significant changes.
argument-hint: "[base ref, e.g. main - the whole feature (committed + uncommitted) is reviewed; omit to review uncommitted changes only]"
---

# Dual-model review gate

Two reviewers from different vendors have decorrelated blind spots. The independence is the point: **neither reviewer may see the other's findings before producing its own.** Agreement is strong evidence a finding is real; a single-model finding is a hypothesis that needs verification before acting.

## Procedure

1. **Scope the diff.** Uncommitted changes by default. If a base ref argument was given, review the WORKING TREE against the merge-base - `BASE=$(git merge-base <base> HEAD) && git diff $BASE` - so uncommitted implementation is reviewed too (committed-only `<base>...HEAD` silently skips everything not yet committed), **plus** untracked files: list them with `git ls-files --others --exclude-standard` and read each one directly. If `merge-base` fails (unborn HEAD, unknown base), fall back to `git diff HEAD` plus the untracked files.

2. **Run both reviews independently - in parallel, same input:**
   - **Claude side:** launch the `code-reviewer` subagent on the diff.
   - **Codex side:** call the `codex` MCP tool with the diff and instructions equivalent to the code-reviewer's (real defects only, file:line + severity + concrete failure scenario, no style nits, verify before reporting). `/codex:review` is the interactive equivalent if the user prefers running it themselves.
   - Do not include either reviewer's output in the other's prompt.

3. **Merge and dedup** by file/line/defect (same defect described differently = one finding).

4. **Triage each unique finding:**
   - **Confirmed** = reported by BOTH models, or verifiable by reproduction (write/run a failing test, run the command, trace the code yourself). Fix confirmed findings now - unless the fix expands scope, in which case ask the user first.
   - **Unconfirmed** (one model, not reproducible cheaply): do NOT auto-fix - reviewers hallucinate defects too. Present to the user with your own one-line assessment.

5. **After fixes:** re-run `./check.sh` (or the test suite); confirm nothing regressed.

6. **Report** a table: finding | source (both / claude / codex) | verdict (confirmed / unconfirmed / refuted) | action taken. If both reviewers found nothing, say so in one line.

## Invariants (ritual)

If `${RITUAL_INVARIANTS_FILE:-.ritual/invariants.md}` exists and contains bullets, Read it before reviewing. Every bullet is a non-negotiable acceptance criterion: any diff hunk that violates one becomes a finding of severity major or higher, citing the bullet. Never weaken or reinterpret an invariant to let a review pass.

## Review memory (ritual)

If `.ritual/lessons.md` exists, Read it before reviewing. Items under "Known noise" were already reviewed and dismissed by a human - do not re-report them unless the evidence is materially new. Use "Confirmed real-bug areas" to direct extra scrutiny at the code regions where real bugs actually lived.

## Third reviewer (ritual)

If a fresh `*-coderabbit.json` file exists in `${RITUAL_FINDINGS_DIR:-.ritual/findings}` (generated within the last hour), Read it: those are single-source, unconfirmed comments from an independent third reviewer. Verify or refute each against the actual diff - for ones you confirm, add `"coderabbit"` to your own finding's `sources` (three sources = strongest signal); ignore the rest silently. Never copy them unverified.

## Guardrails

- If the `codex` tool fails with an auth error, tell the user to run `! codex login`; offer the single-model review rather than silently degrading.
- If the `codex` tool fails because the MODEL is unavailable (model-not-found / unsupported model - NOT an auth error), retry the same call ONCE with `model: "gpt-5.5"` (verified fallback) and note the downgrade in your report. Codex's default is deliberately unpinned so it tracks the newest model (gpt-5.6 today); this keeps the cross-model gate alive when that default isn't available on the account.
- Severity comes from the failure scenario, not from reviewer confidence.

## Machine-readable findings (ritual)

If the project working directory contains a `.ritual/` directory: after the report above, ALSO write the merged findings to a NEW file (never modify an existing one) at `${RITUAL_FINDINGS_DIR:-.ritual/findings}/<UTC timestamp yyyymmddTHHMMSSZ>-dual-review.json`, creating the directory if needed, with exactly this JSON shape:

```json
{
  "ritual_findings": 1,
  "stage": "dual-review",
  "branch": "<git branch --show-current, or empty>",
  "generated_at": "<ISO8601 UTC>",
  "source_models": {"claude": "<your model id>", "codex": "<codex model if known>"},
  "findings": [
    {"id": 1, "severity": "critical|major|minor", "title": "<one-sentence defect>",
     "file": "src/foo.rs", "line": 42, "plan_step": null,
     "snippet": "<1-3 verbatim source lines at the finding>",
     "scenario": "<concrete inputs/state -> wrong outcome>",
     "sources": ["claude", "codex"],
     "verdict": "confirmed|unconfirmed|refuted", "action": "fixed|pending|skipped"}
  ]
}
```

`sources` lists which reviewer(s) reported it (both = cross-confirmed). Use `null` for unknown fields. An empty `findings: []` file is valid when both reviewers found nothing. This section must not change the human-visible report or the procedure above. If `.ritual/` does not exist, skip this section entirely.

Anchoring rules (findings that follow them get acted on; vague ones get ignored): `file` + `line` must point at the exact defective line, not the enclosing function; `snippet` is 1-3 verbatim source lines copied from that location (never paraphrased); `title` stays under 80 characters.
