# Project

## Commands
- `./check.sh fast` — lint + typecheck (must pass after every edit; hook enforces)
- `./check.sh` — full: lint + typecheck + tests

## Workflow
- Nontrivial features: plan mode first, then /plan-review before accepting the plan.
- Implementation: /tdd — tests designed from the spec, written red before code.
- Before committing significant changes: /dual-review. Only confirmed findings get fixed silently.
