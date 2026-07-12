# ritual — guide & tips

> One ritual: **plan → review → tests → implement → review → merge.**
> Two models keep each other honest; `check.sh` keeps both honest.

## Why this shape

Research verdict behind the workflow: *external feedback* (tests,
execution, checks) is the quality engine — not model debate. A second
model pays off in exactly three roles: **plan critic**, **independent
test designer**, and **second reviewer**. That is the pipeline.

## The pipeline

- **spec** — you write intent in `spec.md` (`e` edits in $EDITOR)
- **plan** — Claude drafts `plan.md` from the spec
- **plan-review** — Codex attacks the plan; bounded 2-round debate;
  plan revised in place
- **tests-red** — Codex designs tests from the spec — written *red*,
  no implementation
- **implement** — Claude implements until `check.sh` is green
- **dual-review** — both models review the diff independently;
  findings merged

Cross-confirmed findings (both models agree, `◆ both`) are the strong
signal — treat them as blockers. Single-source minor findings are
suggestions.

## Running things

- `enter` runs the selected stage. Runs are **daemons**: quit the TUI,
  close the terminal — the run survives. Reopen `ritual` and it
  reattaches (resurrection), or press `a` to take the session over in
  interactive claude (`--resume`).
- `x` cancels the running stage (kills the whole process group).
- Sidebar **needs-you** badge = a stage finished and wants a decision.
- `c` / `C` run `check.sh fast` / full — same script the hook runs.
- Everything is also a palette command: `:` then fuzzy-type
  (`rpl` → *run plan-review*).

## Tabs

- **1 live** — agent stream; greeter when idle
- **2 findings** — j/k select · enter/e editor · o nvim · Q quickfix
- **3 history** — past runs: cost, tokens, duration
- **4 plan** — rendered plan.md (falls back to spec)
- **5 guide** — this page

`tab` cycles; `j/k` scroll or select; `g` top; `G` follow the tail.
All keys are rebindable in `[keys]` (see config below).

## Findings workflow

1. Run dual-review; findings land in `.ritual/findings/*.json`.
2. Tab 2: severity pills (crit/major/minor), `◆ both` = cross-model.
3. `Q` sends all locations to nvim's quickfix; `o` opens the selected
   one in your **running** nvim (auto-discovers the server socket);
   `e` uses $EDITOR.
4. Fix, re-run `C`, then re-review or accept.

In CI: `ritual run dual-review --ci` writes JUnit XML to `.ritual/ci/`
and exits nonzero on blocking findings.

## Money

- Per-run caps: `budget_plan_review_usd` (default $5),
  `budget_dual_review_usd` ($10) — passed to claude as a hard budget.
- Daily ceiling: `budget_daily_usd` — refuses new runs past it;
  `--force` overrides once. Statusline meter shows spend vs cap.
- `ritual history` = the ledger (`--json` for scripts).

## Safety & provenance

- **Redaction** (on by default): secrets are scrubbed *before* any
  byte hits the archive — vendor keys, JWTs, PEM blocks, assignments,
  high-entropy tokens. Archives are safe to commit.
- **Hash chain**: every run links to the previous one;
  `ritual verify-log` proves nobody edited history.
- **Repro bundles**: `ritual repro <run-id>` shows the exact model,
  CLI versions, git sha and diffs them against your current env.

## Parallel features

```
ritual new --worktree feat/audio   # branch + worktree, shared .ritual
```

`[` / `]` cycle features in the sidebar; each runs stages in its own
worktree, state and history stay unified. The needs-you queue tells
you which feature wants attention next.

## nvim control

Auto-discovers your running nvim (newest `$XDG_RUNTIME_DIR/nvim.*.0`),
or pin one: `nvim_server = "/path/to/socket"` — or launch with
`nvim --listen`. `o` open file:line · `Q` findings → quickfix.

## CLI (scriptable, styled, `--json` where it counts)

- `ritual` — the dashboard
- `ritual init` — scaffold .ritual/, check.sh, CLAUDE.md
- `ritual status` — pipeline state (`--json`)
- `ritual run <stage>` — headless stage (`--force`, `--ci`)
- `ritual findings` / `history` — browse artifacts (`--json`)
- `ritual report [--pdf]` — feature report from all artifacts
- `ritual new [--worktree B]` — name/create a feature
- `ritual verify-log` — check the tamper-evident chain
- `ritual repro <run-id>` — reproducibility bundle + env diff
- `ritual bench <stage>` — N repeated runs, scored (`--golden`)
- `ritual export` — OTLP-JSON spans of all runs

## Config

Layered: defaults ← `~/.config/ritual/config.toml` ←
`.ritual/config.toml` ← env ← flags.

```toml
theme = "eldritch"            # or "tokyonight"
transparency = true           # terminal bg shows through
redaction = true
budget_daily_usd = 15.0
check_timeout_secs = 600
offline = false               # block runs (metered/plane mode)
nvim_server = ""              # empty = auto-discover

[keys]                        # rebind anything
check-full = "F"

[models]                      # route stages to models
plan-review = "opus"

[commands]                    # your own palette entries
"deploy preview" = "./scripts/preview.sh"
```

## Tips

- Small plans review better. One feature = one plan; split epics
  before plan-review, not after.
- Let plan-review change your plan. The debate is bounded (2 rounds)
  and detector-not-resolver — *you* arbitrate what it flags.
- Never let the implementer design its own tests — that is the whole
  point of tests-red running on the other model.
- Trust `◆ both` findings even when they look pedantic. Live stat:
  the first real run's cross-confirmed critical was a genuine
  path-traversal bug.
- `check.sh fast` must stay under ~10s — it runs on every edit via
  the hook. Push slow suites to the full variant.
- Archives are the truth: `.ritual/runs/*.jsonl` is raw agent output,
  kept verbatim (post-redaction) even when parsing fails.
- If a run looks stuck, quit the TUI and reopen — reattach is free.
  `a` (takeover) turns any headless run into an interactive session.
- Worktrees + `offline = false` on hotel wifi: queue specs and plans
  locally, fire reviews when you're back on a real connection.
- `NO_COLOR=1 ritual status` / `--ascii` for logs and plain terminals
  — every state is readable without color.
