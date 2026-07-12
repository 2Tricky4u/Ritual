---
name: pr
description: Write a pull-request title and description from the real branch diff. Use when the user asks for a PR, PR description, or to open a pull request.
argument-hint: "[base ref, defaults to main]"
---

# Pull request

The description is evidence-based: it reflects `git diff <base>...HEAD` and the commit history, not memory.

## Procedure

1. Determine base (`main` unless given), then read `git log <base>..HEAD --oneline` and `git diff <base>...HEAD --stat`; read the full diff for anything non-obvious.
2. Compose:
   - **Title**: imperative, ≤72 chars, the user-visible outcome.
   - **Summary**: 2-4 sentences — what changed and why, written for a reviewer who hasn't read the code.
   - **Changes**: grouped bullets by area, not per-file noise.
   - **Test evidence**: what was actually run (`./check.sh` output summary, test counts, manual verification steps performed). Never claim untested things work.
   - **Risk & rollback**: what could break, how to revert (single revert? config flag?).
   - If `.ritual/findings/` has findings resolved by this branch, list their ids/titles under "Findings addressed".
3. Output the title + body in a fenced block ready to paste. Run `gh pr create` ONLY if the user asks — then show the URL.

## Guardrails
- If the diff includes changes the commits don't explain, ask before ascribing intent.
- Keep any PR-body trailer conventions this environment requires.
- Draft PRs for work-in-progress branches (failing check.sh) — and say the check state honestly.
