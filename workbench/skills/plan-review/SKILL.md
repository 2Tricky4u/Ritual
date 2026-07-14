---
name: plan-review
description: Cross-model adversarial review of an implementation plan before executing it. Use after plan mode produces a plan, or when the user asks to critique/review/stress-test a plan. OpenAI Codex critiques the plan, debate is bounded to 2 rounds, output is a revised plan plus unresolved disagreements for the human.
argument-hint: "[path to plan file - omit to use the current plan in context]"
---

# Cross-model plan review

You are running a bounded adversarial review of an implementation plan, using OpenAI Codex (a different vendor's model) as the critic. The value comes from decorrelated errors: Codex has different blind spots than you. The debate is a **detector, not a resolver** - unresolved disagreements go to the user, never silently dropped or silently "won".

## Procedure

1. **Get the plan.** If an argument path was given, read that file. Otherwise use the most recent plan in this conversation. If neither exists, ask the user for it.

2. **Round 1 - Codex critique.** Call the `codex` MCP tool (server `codex`). Send the full plan plus one paragraph of project context, with this instruction:

   > Review this implementation plan as an adversarial senior engineer. Find real problems only - do not invent findings; if the plan is sound, say so. Check specifically for: (a) requirements from the user's request the plan misses, (b) unhandled edge cases and failure modes, (c) steps with hidden complexity or underestimated effort, (d) a materially simpler alternative that achieves the same goal, (e) risks - security, data loss, performance, migration/rollback, (f) testability - how will each step be verified, and which tests are missing. Output numbered findings, each with: severity (critical/major/minor), the plan step it targets, and a concrete failure scenario. No style opinions.

3. **Triage each finding - accept or rebut.**
   - **Accept** if the finding is correct or plausibly correct and cheap to address: revise the plan.
   - **Rebut** only with a specific reason (finding rests on a wrong assumption, contradicts an explicit user constraint, or is out of scope). Collect all rebuttals into ONE message.
   - Drop pure style/preference findings without debate.

4. **Round 2 - final exchange.** Send the consolidated rebuttals via `codex-reply` and get Codex's final response. **HARD STOP after this round** - evidence shows debate beyond ~2 rounds does not improve outcomes, it only burns tokens. Do not continue the exchange even if disagreement remains.

5. **Output** (all three sections, in this order):
   - **Revised plan** - the updated plan with accepted findings incorporated.
   - **Accepted findings** - each finding and what changed because of it.
   - **Unresolved disagreements** - for each: Codex's position and yours, 1–2 sentences each, no verdict. The user decides. A `critical`-severity finding may never be dropped silently - it either changes the plan or appears here.
   - *Optional escalation:* if a critical/major disagreement remains AND the `mcp__pal__consensus` tool is available in this session, you may run /consensus on that ONE item and append its verdict paragraph under the disagreement - clearly labeled as a third-model opinion. Never escalate more than one item per review; without the tool, skip silently.

## Invariants (ritual)

If `${RITUAL_INVARIANTS_FILE:-.ritual/invariants.md}` exists and contains bullets, Read it before reviewing. Every bullet is a non-negotiable acceptance criterion: any plan step that violates one becomes a finding of severity major or higher, citing the bullet. Never weaken or reinterpret an invariant to let a plan pass.

## Guardrails

- Never auto-accept a finding that expands scope beyond the user's request - list it under unresolved disagreements instead.
- If the `codex` tool fails with an auth error, tell the user to run `! codex login` and stop.
- If the `codex` tool fails because the MODEL is unavailable (model-not-found / unsupported model - NOT an auth error), retry the same call ONCE with `model: "gpt-5.5"` (verified fallback) and note the downgrade in your report. Codex's default is deliberately unpinned so it tracks the newest model (gpt-5.6 today); this keeps the cross-model gate alive when that default isn't available on the account.
- If Codex returns zero findings, say so plainly - do not fabricate rigor.

## Machine-readable findings (ritual)

If the project working directory contains a `.ritual/` directory: after producing the output above, ALSO write the findings to a NEW file (never modify an existing one) at `${RITUAL_FINDINGS_DIR:-.ritual/findings}/<UTC timestamp yyyymmddTHHMMSSZ>-plan-review.json`, creating the directory if needed, with exactly this JSON shape:

```json
{
  "ritual_findings": 1,
  "stage": "plan-review",
  "branch": "<git branch --show-current, or empty>",
  "generated_at": "<ISO8601 UTC>",
  "source_models": {"claude": "<your model id>", "codex": "<codex model if known>"},
  "findings": [
    {"id": 1, "severity": "critical|major|minor", "title": "<one-sentence finding>",
     "file": null, "line": null, "plan_step": "<the plan step it targets>",
     "snippet": "<verbatim plan/source excerpt when one anchors the finding, else omit>",
     "scenario": "<concrete failure scenario>", "sources": ["codex"],
     "verdict": "accepted|rebutted|unresolved", "action": "<what changed in the plan, or 'none'>"}
  ]
}
```

Keep `title` under 80 characters; when a finding targets existing code, set `file`+`line` to the exact line and copy `snippet` verbatim (anchored findings get acted on). Use `null` for unknown fields. Include every finding from the exchange (accepted, rebutted, and unresolved). An empty `findings: []` file is valid when Codex found nothing. This section must not change the human-visible output or the procedure above. If `.ritual/` does not exist, skip this section entirely.
