# Project

## Commands
- `./check.sh fast`: lint + typecheck (must pass after every edit; the PostToolUse hook from `ritual init --skills` enforces this once installed)
- `./check.sh` (full): lint + typecheck + tests

## Workflow
- Nontrivial features: plan mode first, then /plan-review before accepting the plan.
- Before planning, read `.ritual/architecture.md` if present (refresh with `ritual architect`) and align the plan with its extension seams.
- Implementation: /tdd, tests designed from the spec, written red before code.
- Before committing significant changes: /dual-review. Only confirmed findings get fixed silently.
