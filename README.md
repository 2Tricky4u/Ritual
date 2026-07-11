# ritual

A fast, eldritch-themed TUI that drives a **multi-LLM coding workflow**: Claude Code implements, OpenAI Codex adversarially reviews plans, designs tests, and second-reviews diffs — with every loop grounded in your project's `./check.sh`.

```
╭ feature title ─────────────╮╭ ritual ──────────────────────────────────────╮
│ ▸  spec                   ││ live · findings · history · plan             │
│    plan                   ││ 󰚩 claude-fable-5                              │
│   ⠹ plan-review            ││ ▸ mcp__codex__codex {"prompt":"critique …"}   │
│   ○ tests-red              ││   ↳ Finding 1 (major): missing rollback step │
│   ○ implement              ││  $0.31 4 turns 92.3s                         │
│   ○ dual-review            ││                                               │
│                            ││                                               │
│   main                    ││                                               │
│ 󰚩 claude ok (max)          ││                                               │
│ 󰚩 codex ok                 ││                                               │
│ 󰚩 bridge ok                ││                                               │
│  check green              ││                                               │
╰────────────────────────────╯╰───────────────────────────────────────────────╯
 enter run · j/k move · tab panes · c check · e edit · x cancel · ? help · q
```

## What it does

- **Pipeline dashboard** — per-branch stages `spec → plan → plan-review → tests-red → implement → dual-review`, one-key launch, state in `.ritual/state.json`.
- **Headless stage runs** — `plan-review` / `dual-review` run `claude -p "/skill …" --output-format stream-json` and stream live into the TUI (or styled into your terminal via `ritual run <stage>`). Raw event streams are archived verbatim in `.ritual/runs/` before parsing — schema drift can never lose data.
- **Interactive handoff** — `plan`, `tests-red`, `implement` suspend the TUI and hand you a real attached `claude` session; the dashboard resumes when you exit.
- **Findings browser** — plan-review/dual-review skills write machine-readable findings to `.ritual/findings/`; browse by severity with a ◆both / ◇single cross-model badge, `e` jumps to file:line in `$EDITOR`.
- **check.sh watcher** — file changes rerun `./check.sh fast`; failures show in a red pane. Pauses automatically while an agent owns the project.
- **Run history** — tokens, cost, duration, session id per run (`claude --resume <id>` to take over a stalled headless run).
- **Auth widgets** — claude subscription, codex CLI auth, MCP bridge health.

## Install

```sh
git clone <this repo> && cd ritual
cargo install --path . --root ~/.local   # → ~/.local/bin/ritual
```

Prereqs (versions this was built and verified against, July 2026):
- Claude Code **2.1.205** with the multi-LLM skills installed (`~/.claude/skills/{plan-review,tdd,dual-review}`, `code-reviewer` agent — see `~/Documents/project/multi-llm-playbook.md`)
- Codex CLI **0.144.1** (`npm i -g @openai/codex`), authenticated: `codex login`
- The `codex` MCP server registered: `claude mcp add --scope user codex -- codex mcp-server`

## Use

```sh
cd your-project
ritual init          # scaffold .ritual/, check.sh (stack auto-detected), CLAUDE.md
ritual new "My feature"
ritual               # the dashboard
```

Or scriptable, no TUI:

```sh
ritual status
ritual run plan-review [plan.md]
ritual run dual-review [base-ref]
ritual findings [--json]
ritual history
```

Flags: `--theme eldritch|tokyonight`, `--ascii` (no Nerd Font). Per-project config in `.ritual/config.toml`:

```toml
theme = "eldritch"
base_ref = "main"
budget_plan_review_usd = 5.0
budget_dual_review_usd = 10.0
```

## Keys

| key | action |
|---|---|
| `enter` | run selected stage / open finding |
| `j/k` | navigate stages, findings, stream |
| `tab`, `1-4` | switch pane (live/findings/history/plan) |
| `c` / `C` | run check.sh fast / full |
| `x` | cancel a running stage |
| `e` | open finding in `$EDITOR` |
| `r` | refresh auth + artifacts |
| `g` / `G` | scroll top / follow |
| `?` | help, `q` quit |

## Design notes

- **Drift-tolerant parsing**: every unrecognized stream-json event becomes a dimmed `Raw` line, never a crash. Field names verified against live captures in `tests/fixtures/`.
- **Zero-token testing**: `RITUAL_CLAUDE_CMD`/`RITUAL_CODEX_CMD` swap the real CLIs for `tests/fake_agent.sh`, which replays fixtures — the whole pipeline is E2E-tested without an API call.
- **Findings are immutable per-run files** written by the skills (never merged JSON — LLM read-modify-write is the least reliable operation there is).
- **Terminal safety**: one guard (`term.rs`) owns raw-mode transitions; a panic hook restores your shell; SIGINT in a child can't kill the TUI.

## Manual test checklist (needs a real terminal)

- [ ] `ritual` → Enter on `plan` → full interactive claude session → exit → dashboard resumes cleanly (×3)
- [ ] resize the terminal while suspended → resume redraws correctly
- [ ] Ctrl-C inside the child kills the child, not ritual
- [ ] first real `plan-review` run end-to-end after `codex login`

## Development

`./check.sh` = fmt + clippy -D warnings + tests. Builds land in `/var/tmp/ritual-target` (see `.cargo/config.toml` — adjust if your `/home` has room). The repo dogfoods its own workflow: see `CLAUDE.md`.
