---
name: commit
description: Write a Conventional Commit from the actual staged diff and commit it. Use when the user asks to commit, or when work is done and needs committing.
argument-hint: "[optional scope or context hint]"
---

# Commit

The message describes the STAGED diff — never what you remember doing.

## Procedure

1. `git status --short` and `git diff --cached --stat`. If nothing is staged, show unstaged changes and ask what to stage — never `git add -A` on your own initiative.
2. Read the staged diff (`git diff --cached`). Group what it actually contains.
3. Compose:
   - **Type** from the dominant change: feat / fix / refactor / test / docs / chore / perf / build.
   - **Scope** from the touched area (crate, module, subsystem) when it clarifies: `fix(runner): …`
   - **Subject**: imperative, ≤72 chars, says what changes.
   - **Body** (only when the diff doesn't speak for itself): WHY, constraints, tradeoffs — not a line-by-line narration.
   - Breaking change → `!` and a `BREAKING CHANGE:` footer.
4. If the staged diff contains unrelated clusters, propose splitting into separate commits and offer the split.
5. Commit. Show the final message.

## Guardrails
- Never invent ticket/issue numbers or Refs footers — include them only if the user or branch name provides them.
- Mixed staged+secret-looking content (keys, tokens, .env): stop and flag before committing.
- Follow the repo's existing convention if `git log` shows one that differs from Conventional Commits (match the house style, note the deviation).
- Keep any commit-trailer conventions this environment requires.
