# ritual

A fast, eldritch-themed TUI that drives a **multi-LLM coding workflow**: Claude Code implements, OpenAI Codex adversarially reviews plans, designs tests, and second-reviews diffs — every loop grounded in your project's `./check.sh`.

```
╭ feature title ─────────────╮╭ ritual ──────────────────────────────────────╮
│ ▸ ! feat-auth              ││ live · findings · history · plan             │
│    main                    ││ 󰚩 claude-fable-5                              │
│ ───────────────────────────││ ▸ mcp__codex__codex {"prompt":"critique …"}   │
│ ▸  spec                   ││   ↳ Finding 1 (major): missing rollback step │
│   ⠹ plan-review            ││  $0.31 4 turns 92.3s                         │
│   ○ tests-red              ││                                               │
│                            ││                                               │
│   feat/auth               ││                                               │
│ 󰚩 claude ok (max)          ││                                               │
│ 󰚩 codex ok · bridge ok     ││                                               │
│  check green              ││                                               │
╰────────────────────────────╯╰───────────────────────────────────────────────╯
 enter run · : commands · j/k move · ? help          $1.42/$5.00  ⠹ plan-review
```

## Features (v0.5)

**Quality gates** — `ritual mutants` mutates only your diff (cargo-mutants) and turns every mutant the tests failed to kill into an anchored finding (a proven test gap, advisory); `ritual secrets` gitleaks-scans exactly what changed (incl. untracked files) and its critical findings **block until dismissed or fingerprinted** — auto-run before every dual-review. `.ritual/invariants.md` is the project constitution: every bullet becomes an acceptance criterion re-injected into each review stage. `ritual lessons` distills your f/d dispositions into review memory the critic reads first — it stops re-flagging what you already dismissed.

**Attempts + resilience** — `fallback_model` keeps headless runs alive through provider overloads; `[retry] models` offers *retry `<stage>` with `<model>`* in the palette for failed stages (`run --model` on the CLI); the sidebar shows `×N` attempts and history/reports grow a model column. `ritual costs` breaks spend down per stage with cache-hit economics.

**Ensemble + audit** — optional CodeRabbit CLI third reviewer (findings land unconfirmed; dual-review verifies → three-source signal). Optional `[sandbox]` wrapper spawns every headless run under Anthropic's `srt` (or any prefix) from the single spawn chokepoint. `ritual export` now carries OTel GenAI-semconv attributes, and `--audit-trail` emits IETF draft-sharif agent-audit-trail records (JCS-canonical, SHA-256-chained). Findings carry verbatim source **snippets**; chat undo is a 10-deep stack (`Alt+Z` redo) and reopening chat reattaches to a still-running edit.

## Features (v0.4)

**The pipeline** — per-branch stages `spec → plan → plan-review → tests-red → implement → dual-review`; one-key launch; headless stages stream live; interactive stages hand you a real attached `claude` session and resume the TUI on exit.

**Chat to author the spec/plan** (`s`) — a split view with the live document on the left (focused section highlighted in place) and a conversation on the right: type an instruction and Claude edits `spec.md` (or `plan.md`) in place while you watch, scopable to the whole doc or one `##` section (`Tab` to switch — a missing plan is *drafted from the spec*). `Ctrl+Z`/`Alt+Z` undo/redo (persisted 10-deep stack), `Ctrl+X` cancel, `Alt+Enter` multi-line, messages queue while an edit runs. The agent is hard-scoped at the permission layer: it can read the project but write only the targeted document. Also headless: `ritual chat "<msg>" [--plan] [--section …]`.

**Findings lifecycle** — on the findings tab, `f` marks fixed and `d` dismisses (write-through to the JSON; `v` shows/hides resolved). The exit-code/CI contract follows: a confirmed critical blocks until resolved. `ritual pr-comment [N] [--inline]` posts the open findings to the branch's GitHub PR, redacted.

**Run control from anywhere** — `ritual ps` lists live daemons, `ritual attach <run-id>` streams one into any terminal (`--kill` stops it). `ritual doctor [--deep]` checks agents, auth, MCP wiring, skills drift, hooks, check.sh, and disk pressure. `ritual clean [--keep N] [--dry-run]` prunes old run artifacts — live/state-referenced/today's runs protected, pruned chained runs attested by a tamper-evident **checkpoint** so `verify-log` never breaks.

**Reproducible workbench** — the whole multi-LLM setup (13 skills incl. `/spec` and the optional `/consensus`, the code-reviewer agent, both hooks) is vendored in `workbench/` and installed by `ritual init --skills`. An optional third-model **consensus tier** (`[consensus] enabled`, pal MCP + Gemini) lets plan-review escalate one contested finding for arbitration.

**Runs are daemons** — every headless run detaches (`setsid`) and survives the TUI, the terminal, and reboots of your session. The raw event stream is archived to `.ritual/runs/*.jsonl` *before* parsing; the TUI is just a tailer. Restart `ritual` and it reattaches to live runs and reconciles anything that finished while you were away. Cancel kills the whole process group.

**Parallel features in git worktrees** — `ritual new "Title" --worktree feat/x` creates a worktree sharing ONE `.ritual/` state in the main repo. The sidebar lists all features with a needs-you queue (`!` badge, attention-first ordering); `[` `]` cycle features; runs execute in the right checkout automatically.

**Safety + money** — gitleaks-style **secret redaction** on every archived line, stream, and report (vendor key shapes, PEM blocks, assignments, entropy tokens; `redaction = false` to opt out). **Daily budgets** (`budget_daily_usd`) with a status-bar meter and run refusal (`--force` overrides). Desktop **notifications** on stage completion.

**Provenance** — every run records a **reproducibility bundle** (git commit, dirty-diff hash, claude/codex versions, skill-file hashes, config snapshot; `ritual repro <run-id>` diffs it against your current env) and a **tamper-evident hash chain** (`ritual verify-log` walks it and reports the first break).

**CI mode** — `ritual run dual-review --ci` writes JUnit XML to `.ritual/ci/` (confirmed critical/major findings = failures) and exits nonzero. Findings browsing is scriptable: `--json` everywhere, `ritual findings` exits 1 on confirmed criticals.

**Keyboard-first** — every action is rebindable (`[keys]` table), and the `:` command palette fuzzy-matches all actions, per-stage runs, and your own `[commands]` templates (lazygit-style, with `{{branch}}`, `{{run_id}}`, `{{finding.file}}`, `{{finding.line}}`).

**One-key takeover** — `a` reattaches the selected stage's recorded session interactively (`claude --resume <session-id>`).

**nvim remote control** — ritual drives your *running* nvim (no suspend, no nested editors): `o` opens the selected finding at file:line in it, `Q` pushes all located findings into its quickfix list (`:copen` included). Discovery: `$NVIM` → newest `$XDG_RUNTIME_DIR/nvim.*.0` socket → `nvim_server` config; the sidebar shows ` nvim ok` when one is found. Falls back to attached `$EDITOR` when nvim isn't running.

**Bench + export** — `ritual bench plan-review --runs 5 [--golden expected.json]` scores repeated runs (findings, cross-confirmation, golden recall, cost) for model/prompt comparison; `ritual export` emits OTLP-JSON spans from run history for any OpenTelemetry collector.

**Reports** — `ritual report [--pdf]`: one Markdown document per feature (pipeline state, spec, plan, findings table, runs, spend), redacted, pandoc-converted when available.

## Install

```sh
git clone <this repo> && cd ritual
cargo install --path . --root ~/.local   # → ~/.local/bin/ritual
```

Prereqs (verified against: Claude Code 2.1.205, Codex CLI 0.144.1, July 2026):
- Claude Code with the multi-LLM skills (`~/.claude/skills/{plan-review,tdd,dual-review}` — see `multi-llm-playbook.md`)
- Codex CLI (`npm i -g @openai/codex`), authenticated: `codex login`
- The codex MCP bridge: `claude mcp add --scope user codex -- codex mcp-server`

## Use

```sh
cd your-project
ritual init                      # .ritual/, stack-detected check.sh, CLAUDE.md
ritual new "My feature"          # or: ritual new "Big thing" --worktree feat/big
ritual                           # the dashboard
```

Scriptable: `status|findings|history [--json]`, `run <stage> [--force] [--ci]`, `report [--pdf]`, `repro <run-id>`, `verify-log`, `bench <stage> --runs N`, `export [--out f]`.

## Configuration (`.ritual/config.toml` or `~/.config/ritual/config.toml`)

```toml
theme = "eldritch"            # or tokyonight; --ascii for no Nerd Font
base_ref = "main"
budget_daily_usd = 5.0        # omit for no ceiling
budget_plan_review_usd = 5.0  # per-run --max-budget-usd caps
budget_dual_review_usd = 10.0
redaction = true
notifications = true
check_timeout_secs = 600      # hung build / dead HIL board can't wedge the loop
offline = false               # true = skip all cloud auth preflights
# nvim_server = "/run/user/1000/nvim.12345.0"   # explicit socket (auto-discovered otherwise)

[keys]                        # rebind any action
check-full = "F"

[models]                      # per-stage model routing
plan-review = "opus"

[commands]                    # palette-invocable templates
blame = "git log --oneline -3 -- {{finding.file}}"
```

## Recipes

**GitHub Actions gate**
```yaml
- name: dual review gate
  run: |
    ritual run dual-review --ci ${{ github.base_ref }}
- uses: mikepenz/action-junit-report@v4
  if: always()
  with: { report_paths: ".ritual/ci/*.xml" }
```

**Air-gapped / local models** — set `offline = true` and point the seam at any local agent CLI: `claude_cmd = "my-ollama-agent"` (or env `RITUAL_CLAUDE_CMD`). Everything that matters — archives, findings, reports, chain — is local files; nothing requires cloud auth.

**Embedded / hardware-in-the-loop** — use `templates/check-hil.sh` (build → flash → capture serial → assert) as your project's check.sh and set `check_timeout_secs` low enough that a dead board fails fast.

## Keys (defaults — all rebindable)

`enter` run/open · `s` chat: edit spec/plan · `:` palette · `j/k` move · `tab` `1-5` panes (`5` = in-app guide & tips) · `f/d/v` finding fix/dismiss/show-resolved · `[` `]` features · `a` takeover · `o` open in nvim · `Q` findings → quickfix · `c/C` check fast/full · `x` cancel · `e` editor · `r` refresh · `g/G` scroll/follow · `?` help · `q` quit — in chat: `Ctrl+Z` undo · `Ctrl+X` cancel · `Alt+Enter` newline

## Design notes

- **Drift-tolerant parsing**: unknown stream-json events render dimmed (`Raw`), never crash; field names verified against live captures in `tests/fixtures/`.
- **Zero-token testing**: `RITUAL_CLAUDE_CMD`/`RITUAL_CODEX_CMD` swap in `tests/fake_agent.sh`; the entire pipeline (including daemon survival) is E2E-tested without an API call.
- **Accessibility**: state is never color-only (every status has a distinct glyph); `--ascii` replaces Nerd Font icons; `NO_COLOR` and piped output disable color.
- **Terminal safety**: one guard owns raw-mode transitions; a panic hook restores your shell; Ctrl-C in a child can't kill the TUI.

## Development

`./check.sh` = fmt + clippy -D warnings + tests (63 tests). Builds land in `/var/tmp/ritual-target` (see `.cargo/config.toml`). The repo dogfoods its own workflow (`CLAUDE.md`); `demo.tape` renders the README demo via [vhs](https://github.com/charmbracelet/vhs).
