---
name: coverage
description: Completeness gate - judge each `## Deliverables` checklist item in the plan against the actually-built tree (present? substantive, not a stub? meets its acceptance criterion?) and file one finding per gap. Read-only; decides whether a feature is genuinely done, not just tests-green.
argument-hint: "[path to plan.md - omit to use the feature's plan]"
---

# Completeness / coverage gate

Green tests do not mean the plan was built. This gate compares the plan's declared **deliverables** against what actually exists in the tree and reports every gap - the "reward-hacking gap" where an agent satisfies the visible tests without satisfying the intent.

You are strictly READ-ONLY: inspect and judge. Do NOT edit any file, do NOT run shell or git, do NOT tick checkboxes (ritual ticks them from your verdict). Judging your own work as done is the exact failure this gate exists to catch.

## Procedure

1. **Read the plan** at the argument path (or the feature's `plan.md`). Parse its `## Deliverables` checklist - each item is `- [ |x] <ID>: <description> - accept: <criterion> - route: <hint>`.
   - If there is NO `## Deliverables` section, emit exactly ONE gap finding (deliverable `"deliverables"`, `plan_step: "Deliverables"`) telling the author to add a concrete, pass/fail `## Deliverables` checklist covering every spec promise, then stop - there is nothing else to judge yet.
2. **Read the intent:** the spec (`spec.md` beside the plan) and, if present, `${RITUAL_INVARIANTS_FILE:-.ritual/invariants.md}`, so you judge against the real requirements and constraints.
3. **For EACH unchecked deliverable**, judge it against the tree (Glob/Grep/Read the relevant files):
   - **Present?** Does the artifact the deliverable describes actually exist?
   - **Substantive?** Is it real, not a stub or placeholder - a doc with headings but no content, a compose file with no services, a function that only `todo!()`s, an empty test? A stub is a GAP.
   - **Meets its acceptance criterion?** Judge the `accept:` clause literally.
   Present AND substantive AND meets-criterion = SATISFIED - and a SATISFIED verdict must cite its evidence: the specific file(s) (with lines where sensible) that meet the criterion. A "satisfied" you cannot anchor to files you actually read is leniency, not a verdict - treat it as a gap. Anything else is a GAP. Be strict: when unsure, it is a gap.
4. **Report** a short table: deliverable | verdict (satisfied / gap) | evidence (file:line for satisfied) | why.

## Machine-readable findings (ritual)

After the report, write ONE new file at `${RITUAL_FINDINGS_DIR:-.ritual/findings}/<UTC timestamp yyyymmddTHHMMSSZ>-coverage.json` (create the dir if needed), exactly this shape:

```json
{
  "ritual_findings": 1,
  "stage": "coverage",
  "branch": "<git branch --show-current, or empty>",
  "generated_at": "<ISO8601 UTC>",
  "satisfied": ["D2", "D5"],
  "findings": [
    {"id": 1, "severity": "major", "title": "<deliverable> not built: <one-line why>",
     "deliverable": "D3",
     "file": "stacks/media/compose.yml", "line": null, "plan_step": null,
     "scenario": "<what is missing vs the acceptance criterion>",
     "sources": ["coverage"], "verdict": "confirmed", "action": "pending"}
  ]
}
```

Routing (load-bearing - it decides which fixer builds the gap):
- **Code gap** (a file/dir must be created or filled): set `file` to the deliverable's `route:` path (or your best target path); leave `plan_step` null. Routes to the code-fix builder.
- **Plan gap** (the plan/spec itself is wrong or incomplete): set `plan_step` to the deliverable's `route:` `§Section` (or the section name); leave `file` null. Routes to the plan-fix editor.
- Always honor the deliverable's explicit `route:` over a guess.
- Put the deliverable id in `deliverable`. `satisfied` lists the ids you confirmed done (ritual ticks their boxes). Every verdict is `confirmed` (you verified against the tree) with `action: pending`.
- COVER EVERY unchecked deliverable: each must appear in `satisfied` (you confirmed it) OR as a gap finding (you flagged it). Ritual treats any unchecked deliverable that is in neither as an unverified gap and will re-drive it - silence is not "done". Do not omit or skim.

An empty `findings: []` with the full `satisfied` list means the project is COMPLETE. If `.ritual/` does not exist, skip this JSON section entirely.
