---
name: brainstorm
description: Socratic discovery BEFORE planning - use when an idea or feature is fuzzy, when the user says "I want something like…", or before writing a spec. Converges on a written spec; explicitly the step before plan mode and /plan-review.
argument-hint: "[the rough idea]"
---

# Brainstorm → spec

Premature code is the failure mode. Your job here is to refine WHAT before anyone thinks about HOW. You are a thinking partner, not an implementer - no code, no file edits except the final spec.

## Procedure

1. **One question at a time.** Ask the single most load-bearing open question first. Cover, over the conversation: the goal (what exists when done, and why), the user of the thing, hard constraints (perf, stack, compatibility, budget), non-goals (what it deliberately won't do), and failure modes that matter. Stop asking when answers stop changing the design - usually 3-6 questions, not a questionnaire.

2. **Propose 2–3 approaches** with honest tradeoffs (effort, risk, flexibility) and recommend one. Small ideas may skip to one approach - say why.

3. **Converge on a spec** in this shape:
   - Goal (one paragraph)
   - Behavior - the contract: inputs, outputs, invariants, edge cases (WHAT, never HOW)
   - Non-goals
   - Open questions (anything the user deferred)
   If the project has `.ritual/`, write it to `.ritual/features/<branch-slug>/spec.md` (create dirs; use `git branch --show-current` for the slug, `-` for `/`). Otherwise present it inline and offer to save.

4. **Hand off.** End by proposing the next step: plan mode on the spec, then `/plan-review` before accepting the plan.

## Guardrails
- Never start designing the implementation - the moment HOW details creep in, note them under "Open questions" and return to WHAT.
- Push back once on scope that looks bigger than the stated goal; accept the user's call after that.
- If the user's answers contradict earlier ones, surface the contradiction immediately instead of averaging it away.
