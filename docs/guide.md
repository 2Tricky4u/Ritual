# ritual — guide & tips

> One ritual: **plan → review → tests → implement → review → merge.**
> Two models keep each other honest; `check.sh` keeps both honest.

## Why this shape

Research verdict behind the workflow: *external feedback* (tests,
execution, checks) is the quality engine — not model debate. A second
model pays off in exactly three roles: **plan critic**, **independent
test designer**, and **second reviewer**. That is the pipeline.

## The pipeline

- **spec** — you write intent in `spec.md` (`⟨enter⟩` opens it in
  $EDITOR; or press `⟨s⟩` to **chat** it into shape — see below)
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
- From ANY terminal: `ritual ps` lists live daemons (chat edits too),
  `ritual attach <run-id>` streams one right there (`--kill` stops it).
- `x` cancels the running stage (kills the whole process group).
- Sidebar **needs-you** badge = a stage finished and wants a decision.
- `c` / `C` run `check.sh fast` / full — same script the hook runs.
- Everything is also a palette command: `:` then fuzzy-type
  (`rpl` → *run plan-review*).
- Something off? `ritual doctor` checks every prerequisite — agents,
  auth, MCP wiring, skills drift, hooks, check.sh, disk space
  (`--deep` also runs the fast checks).

## Tabs

- **1 live** — agent stream; greeter when idle
- **2 findings** — j/k select · f fix · d dismiss · v resolved ·
  enter/e editor · o nvim · Q quickfix
- **3 history** — past runs: cost, tokens, duration
- **4 plan** — rendered plan.md (falls back to spec)
- **5 guide** — this page

`tab` cycles; `j/k` scroll or select; `g` top; `G` follow the tail.
All keys are rebindable in `[keys]` (see config below).

## Chat to author the spec (or plan)

Press `⟨s⟩` (or `:` → *chat: edit spec/plan*) to open an interactive
chat: the **live document is on the left, the conversation on the
right**. Type an instruction (`⟨enter⟩` sends), and Claude edits the
file in place — you watch it change on the left as it happens.

- `⟨Tab⟩` cycles the **target**: the whole spec, each of its sections,
  then the plan. No plan yet? The target reads *plan (draft from spec)*
  and your first message drafts one FROM the spec. The left pane shows
  the whole document with the focused section highlighted in place.
- Each message acts on the document as it stands now, with your last
  few messages as context — so "make it 3 attempts, not 5" works. The
  file is the memory; no session state to manage.
- `⟨Ctrl+Z⟩` **undo** — swaps the document with its pre-edit snapshot
  (press again to redo). Works across restarts and for CLI chats too;
  single-level: each edit replaces the snapshot.
- `⟨Ctrl+X⟩` **cancel** an in-flight edit (kills the daemon, drops any
  queued messages).
- `⟨Alt+Enter⟩` inserts a newline (the input box grows); `⟨enter⟩`
  while an edit runs **queues** the message (up to 3, sent in order).
- `⟨↑⟩`/`⟨↓⟩` scroll the transcript, `⟨esc⟩` closes (a running edit
  finishes on its own — it's a daemon like any other run).
- From a script: `ritual chat "tighten the goal to one sentence"`,
  `--section "Behavior…"` to scope it, `--plan` to target the plan.

The spec stage flips to **done** when the document gains real content.
Runs cost `budget_doc_chat_usd` at most (default $0.50/message), and
the agent is **hard-scoped**: it can read the project but write only
the one document you targeted (enforced at the permission layer).

## Findings workflow

1. Run dual-review; findings land in `.ritual/findings/*.json`.
2. Tab 2: severity pills (crit/major/minor), `◆ both` = cross-model.
3. `Q` sends all locations to nvim's quickfix; `o` opens the selected
   one in your **running** nvim (auto-discovers the server socket);
   `e` uses $EDITOR.
4. Fix, re-run `C`, then **close the loop**: `⟨f⟩` marks the selected
   finding fixed, `⟨d⟩` dismisses it (either toggles back on re-press),
   writing into the findings JSON. Resolved findings recede from the
   list; `⟨v⟩` shows/hides them (`ritual findings --all` on the CLI).
5. On a GitHub project, `ritual pr-comment` posts the open findings to
   the branch's PR (redacted; `--inline` adds file:line review comments).

The exit-code contract follows the lifecycle: a confirmed critical
blocks scripts/CI **until you mark it fixed or dismissed**. In CI:
`ritual run dual-review --ci` writes JUnit XML to `.ritual/ci/` and
exits nonzero on unresolved blocking findings.

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
- **Pruning without breaking the chain**: `ritual clean` (default:
  keep the newest 50) deletes old run artifacts but never touches live
  runs, state-referenced runs, or today's runs (the budget ledger).
  Pruned chained runs are attested by a **checkpoint** — a rolling
  genesis, like a git shallow clone — so `verify-log` stays intact:
  `chain intact: checkpoint(2026-07-12, 34 pruned) + 16 run(s)
  verified`. Tampering with the checkpoint breaks verification like
  tampering with any run. `--dry-run` previews.

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
  (`--skills` also installs the vendored workbench into `~/.claude`:
  all 13 skills, the code-reviewer agent, both hooks — one clone
  reproduces the whole setup)
- `ritual doctor` — check every prerequisite (`--deep` runs checks)
- `ritual status` — pipeline state (`--json`)
- `ritual run <stage>` — headless stage (`--force`, `--ci`)
- `ritual chat <msg>` — edit spec/plan (`--plan`, `--section`)
- `ritual ps` / `attach <run-id>` — live daemons; follow or `--kill`
- `ritual findings` / `history` — browse artifacts (`--json`, `--all`)
- `ritual pr-comment [N]` — findings → GitHub PR (`--inline`)
- `ritual report [--pdf]` — feature report from all artifacts
- `ritual new [--worktree B]` — name/create a feature
- `ritual clean` — prune old runs safely (`--keep N`, `--dry-run`)
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
budget_doc_chat_usd = 0.50    # per spec/plan chat message
check_timeout_secs = 600
offline = false               # block runs (metered/plane mode)
nvim_server = ""              # empty = auto-discover

[keys]                        # rebind anything
check-full = "F"

[models]                      # route stages to models
plan-review = "opus"

[commands]                    # your own palette entries
"deploy preview" = "./scripts/preview.sh"

[consensus]                   # third-model arbitration (off by default)
enabled = false
```

## Consensus tier (optional third model)

For a genuinely contested plan-review disagreement, a third vendor can
arbitrate: one stance argues for, one against, and the verdict lands
under the disagreement — clearly labeled as an opinion, not truth.
Use sparingly: the evidence says ungrounded debate is the weakest
pattern; prefer a discriminating test when one exists.

Setup (once): get a free-tier Gemini key (aistudio.google.com), then

```
claude mcp add --scope user pal --env GEMINI_API_KEY=<key> -- \
  uvx --from git+https://github.com/BeehiveInnovations/pal-mcp-server.git pal-mcp-server
```

and set `[consensus] enabled = true`. plan-review then may escalate at
most ONE unresolved critical/major item per review via the `/consensus`
skill. `ritual doctor` shows the pal server's status.

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

## A full run, start to finish

A concrete walkthrough of one feature, touching every part of the tool.
Keys are shown as `⟨key⟩`. The sidebar (left) always shows three
sections — FEATURES, PIPELINE, AGENTS; the main pane (right) is the
five tabs.

**0. Open ritual.** Run `ritual init` once in your repo (scaffolds
`.ritual/`, `check.sh`, `CLAUDE.md`), then just `ritual`. You land on
the **live** tab (`⟨1⟩`) showing the greeter. Bottom line is the
powerline statusline: branch, today's spend vs budget, check state.

**1. Name the feature.** In another shell: `ritual new "Audio engine"`.
For parallel work that shouldn't touch your current branch, use a
worktree: `ritual new --worktree feat/audio` (own checkout, shared
`.ritual`). Back in the TUI, `⟨r⟩` refreshes; the feature shows in the
FEATURES section. `⟨[⟩` / `⟨]⟩` cycle features — needs-you ones sort
first, flagged with a yellow ``.

**2. Write the spec.** The PIPELINE section lists the six stages with
one highlighted. On the greeter, `⟨j⟩`/`⟨k⟩` move that highlight;
land on `spec` and press `⟨enter⟩`. ritual opens `spec.md` in your
`$EDITOR` (the TUI hands over the terminal, then takes it back on
exit). Write what you want built, `:wq`. The stage flips to **done**
if you wrote real content, stays pending if you only left comments.
*Prefer to talk it out?* Press `⟨s⟩` instead for the chat (see "Chat
to author the spec" above) — describe the feature and Claude drafts
the spec live, section by section.

**3. Draft the plan.** Highlight `plan`, `⟨enter⟩` → an interactive
Claude session opens (plan mode). When it saves `plan.md` and exits,
the stage goes done. Read the result on the **plan** tab (`⟨4⟩`) —
it's rendered markdown; `⟨j⟩`/`⟨k⟩` scroll, `⟨g⟩` jumps to top.

**4. Cross-review the plan.** The fastest way to run any stage from
anywhere is the command palette: `⟨:⟩`, type `run plan-review`,
`⟨enter⟩` (fuzzy — `rpl` works). Claude and Codex now debate the plan.
This is a **daemon**: the **live** tab (`⟨1⟩`) streams both models;
the statusline budget meter ticks up. You can quit ritual entirely
(`⟨q⟩`) and reopen later — it reattaches to the running daemon. Press
`⟨a⟩` to take the session over in interactive Claude (`--resume`).
`⟨x⟩` cancels. When it finishes you get a desktop notification and the
stage shows **needs-you** (a human decides).

**5. Triage findings.** Switch to the **findings** tab (`⟨2⟩`). Each
finding is a severity pill (crit/major/minor); a green **◆ both**
badge means *both* models flagged it — treat those as blockers.
`⟨j⟩`/`⟨k⟩` select. Then either `⟨o⟩` (open the file:line in your
already-running nvim), `⟨Q⟩` (push *all* findings to nvim's quickfix
list — `:cnext` through them), or `⟨e⟩` (open in `$EDITOR`). Edit
`plan.md` to address them; re-run `plan-review` if the plan changed
materially.

**6. Tests first.** Palette → `run tests-red`: Codex designs tests
from the *spec* (not from your plan, and never the model that will
implement), written **failing**. Press `⟨c⟩` to run `check.sh fast` —
red, as expected. This is the whole point: the test author and the
implementer are different models.

**7. Implement.** Palette → `run implement`: Claude codes until the
suite is green. As it edits, the global PostToolUse hook auto-runs
`check.sh` and feeds failures back. You can also press `⟨c⟩` (fast) or
`⟨C⟩` (full) yourself; the check segment in the statusline goes
green/red. The stage completes when `check.sh` passes.

**8. Final review.** Palette → `run dual-review`: both models review
the actual diff independently, findings merged into tab `⟨2⟩` again.
Fix every **◆ both**, re-run `⟨C⟩`, then accept. In CI you'd run this
headless instead: `ritual run dual-review --ci` writes JUnit XML and
exits nonzero on blocking findings.

**9. Wrap up and prove it.** From the shell (or the palette's custom
commands):
- `ritual report --pdf` — a shareable report from every artifact
  (redacted, safe to commit).
- **history** tab (`⟨3⟩`) or `ritual history` — cost, tokens, duration
  per run; the statusline shows today's total vs your `budget_daily_usd`.
- `ritual verify-log` — proves the tamper-evident hash chain over all
  runs is intact.
- `ritual repro <run-id>` — the exact models, CLI versions, and git sha
  a run used, diffed against your current environment.
- `ritual export` — OTLP-JSON spans for a tracing backend.
- `ritual bench plan-review --runs 5 --golden expected.json` — measure
  a stage's quality/recall across repeats when tuning models or prompts.

**Throughout:** secrets are redacted before anything is archived;
runs survive the TUI dying; the daily budget refuses new runs past the
ceiling (`--force` overrides once); `offline = true` blocks agent calls
for metered connections. Nothing here needs the cloud except the agent
calls themselves — archives, findings, reports, and the chain are all
local files.

That's the loop: **spec → plan → plan-review → tests-red → implement →
dual-review**, with two models keeping each other honest and
`check.sh` keeping both honest.
