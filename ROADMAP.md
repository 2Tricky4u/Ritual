# ritual — roadmap

> **Status (v0.2, 2026-07-12): everything below through v0.4 is SHIPPED**, except the
> in-app version-check note (deferred — `ritual --version` covers it) and full
> session-state persistence (the load-bearing part — daemonized runs + reattach —
> shipped; scroll/tab restoration was judged not worth the complexity).
> This file is kept as the design rationale for what was built.

Feature candidates researched July 2026 across three sources: what makes professional TUIs beloved (lazygit, k9s, yazi, zellij, atuin…), what AI-coding orchestrators ship (Claude Squad, vibe-kanban, Plandex, aider, goose…), and what research engineers / embedded technicians / ops actually need (MLflow, 21 CFR Part 11 audit trails, Zephyr HIL pipelines, gitleaks…). Ranked by value ÷ effort **for this codebase** — most items build directly on existing pieces (raw run archives, run meta, check.sh abstraction, config layering).

## v0.2 — high value, low effort (quality-of-life + safety)

1. **Secret redaction in archives** — regex pass (gitleaks-style patterns: keys, tokens, PEM blocks) over every line before it hits `.ritual/runs/*.jsonl`, findings, and reports. The audit trail must be safe to commit/share. *(borrows: gitleaks --redact, CI secret masking; universal need)*
2. **`ritual report [feature|run-id]`** — generate a Markdown run report: spec → plan → findings (accepted/unresolved) → diffs summary → costs/tokens → outcome; `--pdf` via pandoc when available. PR descriptions for programmers, lab-notebook pages for researchers, postmortems for ops. *(data already sits in runs/ + findings/ + state.json)*
3. **Budgets + spend alerts** — `budget_daily_usd` in config; status-bar shows today's spend (already computed), turns orange at 75%, red over; refuse new headless runs over budget with `--force` escape. *(borrows: Claude Enterprise spend limits; meta already has cost)*
4. **`--json` + exit codes everywhere** — `status --json`, `history --json`, `findings --json` (exists); exit 1 when confirmed critical findings exist → scriptable gates. *(borrows: k9s/atuin scriptability conventions)*
5. **Desktop notifications** — `notify-send` on stage done / needs-attention / failed (user already runs a notify-done.sh hook; same pattern). *(borrows: vibe-kanban "needs you" alerts)*
6. **Custom keybindings** — `[keys]` table in config.toml mapping action names to keys. *(the single most universal pro-TUI feature)*
7. **Command palette (`:`)** — fuzzy-invoke any action ("run plan-review", "open findings", "report"). *(borrows: k9s command mode, Textual Ctrl+P)*

## v0.3 — differentiators

8. **Parallel features in git worktrees** — `ritual new --worktree <branch>` creates a worktree; dashboard already keys state by branch-slug, so multiple features run stages concurrently without clobbering; sidebar lists all in-flight features with a "needs you" queue. *(borrows: Claude Squad/Conductor — THE orchestrator feature of 2026)*
9. **CI mode** — `ritual run dual-review --ci`: no TTY, JUnit-XML from findings (`<testcase>` per finding, failures = confirmed critical/major), artifacts uploadable; ships a GitHub Actions recipe. *(borrows: GitHub Agentic Workflows, JUnit conventions)*
10. **Reproducibility bundle per run** — meta gains git commit, dirty-diff hash, CLI versions (`claude --version`, `codex --version`), skill file hashes, config snapshot; `ritual repro <run-id>` prints it and diffs against current env. *(borrows: MLflow run snapshots; researchers' #1 ask)*
11. **One-key takeover (`a`)** — attach interactively to a stalled/finished headless run via stored `session_id` → suspend TUI → `claude --resume <id>`. *(plumbing exists; borrows: Nimbalyst session resume)*
12. **Per-stage model routing** — `[models] plan_review = "opus"` style config appending `--model` per stage; cheap model for mechanical stages. *(borrows: goose routing, Plandex model packs)*
13. **Hardware-in-the-loop check profile** — documented `check.sh` template for embedded: build → flash → capture serial → assert (Zephyr Twister/pytest-style), plus a `check_timeout_secs` config so a hung board can't wedge the pipeline. *(technicians; check.sh abstraction already supports it — this is a template + timeout)*
14. **Tamper-evident audit chain** — each run meta stores `sha256(prev_meta_hash + jsonl_hash)`; `ritual verify-log` walks the chain. Cheap now, impossible to retrofit honestly later. *(borrows: 21 CFR Part 11 / WORM audit trails; researchers + regulated shops)*

## v0.4 — bigger bets (decide after v0.3 usage)

15. **Offline / local-model backend** — the `RITUAL_CLAUDE_CMD` seam already allows any CLI; add a tested recipe for an Ollama-backed agent CLI + `offline = true` config that disables cloud preflights. *(air-gapped labs, embedded shops)*
16. **Eval harness** — `ritual bench`: run the pipeline N times against fixture tasks (fake-agent or live), score findings-precision and check-pass-rate; compare model configs. *(borrows: aider's benchmark culture)*
17. **Session persistence/resurrection** — serialize dashboard state (stream buffer, scroll, tab) so `ritual` reattaches to in-flight runs after a crash/reboot. *(borrows: zellij resurrection; needs runs to be daemonized first — largest architectural change)*
18. **Plugin hooks** — lazygit-style custom commands: user-defined actions in config with template context (`{{run_id}}`, `{{finding.file}}`), output to popup/log. Stop short of a full Lua/WASM plugin system. *(borrows: lazygit custom commands, k9s plugins)*
19. **OpenTelemetry export** — emit spans per stage/run for ops teams with existing observability. *(ops; low solo value — last)*

## Non-goals (researched, rejected)

- **Full multi-agent kanban GUI** — ritual is a solo-power-user TUI; vibe-kanban exists.
- **Voice input** — belongs in the OS/dictation layer (Wispr/Spokenly), not the TUI.
- **E2E-encrypted sync** — atuin-grade infra for marginal solo value.
- **Open-ended agent-to-agent chat modes** — contradicts the evidence the whole workflow is built on.

## Polish debt (do alongside anything)

- Accessibility: state must never be color-only (icons already differ per state — keep it that way); document `--ascii` + `NO_COLOR`; avoid redraw storms.
- A VHS `.tape` demo script for the README (charmbracelet/vhs) — reproducible GIFs, CI-rendered.
- In-app "new version" note when the installed binary is older than the repo tag.
