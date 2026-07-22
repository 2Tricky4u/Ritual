# ritual

Rust + ratatui TUI that orchestrates the multi-LLM coding workflow (Claude Code + Codex). Hybrid UX: bare `ritual` = dashboard, subcommands = styled output.

## Commands
- `./check.sh fast`: fmt + clippy (must pass after every edit; global hook enforces)
- `./check.sh` (full): fmt + clippy + tests
- `cargo run -- status`: quick manual smoke

## Workflow
- Nontrivial features: plan mode first, then /plan-review before accepting the plan.
- Implementation: /tdd, tests designed from the spec, written red before code.
- Before committing significant changes: /dual-review.

## Conventions
- Parser code must be drift-tolerant: unknown JSON events become `AgentEvent::Raw`, never errors.
- Raw agent output is archived BEFORE parsing (`.ritual/runs/*.jsonl`), redacted first when redaction is on.
- All TUI state mutations flow through the single `AppMsg` channel; render never blocks.
- Terminal enter/leave only via `term.rs` guard; never call crossterm enable/disable elsewhere.
- Theme colors only via `theme.rs` semantic names, never raw hex in UI code.
