---
name: debug
description: Systematic root-cause debugging. Use when a bug, test failure, crash, or unexplained behavior needs fixing - especially when the cause is not yet understood. Four phases; fixing before understanding is forbidden.
argument-hint: "[symptom, failing test, or error message]"
---

# Systematic debugging

The failure mode this skill exists to prevent: patching the symptom before understanding the cause. You may not write a fix until phase 3 is complete.

## Phase 1 - Reproduce
Turn the symptom into a command that fails deterministically: a failing test, a script, a curl. If you cannot reproduce it, that IS the investigation - gather logs/inputs until you can. Record the exact command; it becomes the regression test later.

## Phase 2 - Isolate
Shrink the search space with evidence, not intuition:
- Bisect: inputs (minimize the failing case), code path (git bisect if a regression), data (which record breaks it).
- Instrument: add targeted logging/asserts at the boundaries you suspect; read the actual values, don't guess them.
- One variable at a time; write down what each probe ruled out.

## Phase 3 - Root cause
State the root cause in one sentence: "X happens because Y." Then try to disprove it: name one alternative explanation and show why the evidence excludes it. If you can't exclude it, return to phase 2. A cause you can't defend against one alternative is a hypothesis, not a diagnosis.

## Phase 4 - Fix + regression-proof
- Write the regression test FIRST (the phase-1 reproduction, minimized). It must fail before the fix and pass after.
- Fix the root cause, not the site of the symptom. If they differ, say so explicitly.
- Run `./check.sh` (full). Remove the phase-2 instrumentation.
- If the bug corresponds to a finding in `.ritual/findings/`, mention its id in the commit/summary and note the finding as addressed.

## Guardrails
- No "while I'm here" fixes - unrelated issues become notes to the user, not edits.
- If two phases pass without progress, stop and present what's known/ruled out rather than thrashing.
- A fix without a regression test is not done.
