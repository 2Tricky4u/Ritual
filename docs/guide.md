# ritual: guide & tips

> One ritual: **plan → review → tests → implement → review → merge.**
> Two models keep each other honest; `check.sh` keeps both honest.

## Why this shape

Research verdict behind the workflow: *external feedback* (tests,
execution, checks) is the quality engine, not model debate. A second
model pays off in exactly three roles: **plan critic**, **independent
test designer**, and **second reviewer**. That is the pipeline.

## The pipeline

- **spec**: you write intent in `spec.md` (`⟨enter⟩` opens it in
  $EDITOR; or press `⟨s⟩` to **chat** it into shape, see below)
- **plan**: Claude drafts `plan.md` from the spec
- **plan-review**: Codex attacks the plan; bounded 2-round debate;
  plan revised in place
- **tests-red**: Codex designs tests from the spec, written *red*,
  no implementation
- **implement**: Claude implements until `check.sh` is green
- **dual-review**: both models review the diff independently;
  findings merged
- **coverage**: a read-only LLM-as-Judge checks every `## Deliverables`
  item against the actually-built tree and files a gap per miss

Cross-confirmed findings (both models agree, `◆ both`) are the strong
signal: treat them as blockers. Single-source minor findings are
suggestions.

## Running things

- `enter` runs the selected stage. Runs are **daemons**: quit the TUI,
  close the terminal, and the run survives. Reopen `ritual` and it
  reattaches (resurrection), or press `a` to take the session over in
  interactive claude (`--resume`).
- From ANY terminal: `ritual ps` lists live daemons (chat edits too),
  `ritual attach <run-id>` streams one right there (`--kill` stops it).
- `x` cancels the running stage (kills the whole process group).
- Sidebar **needs-you** badge = a stage finished and wants a decision.
- `c` / `C` run `check.sh fast` / full, the same script the hook runs.
- Everything is also a palette command: `:` then fuzzy-type
  (`rpl` → *run plan-review*).
- Something off? `ritual doctor` checks every prerequisite: agents,
  auth, MCP wiring, skills drift, hooks, check.sh, disk space
  (`--deep` also runs the fast checks).

## Tabs

- **1 live**: agent stream; greeter when idle
- **2 findings**: j/k select · enter details · f fix · d dismiss(+reason) ·
  F queue/apply claude answers · m queue manual · t auto-triage ·
  u revert batch · v resolved · `/` filter · e editor · o nvim ·
  Q quickfix/manual pass
- **3 history** shows past runs: cost, tokens, duration · `/` filter
- **4 plan**: rendered plan.md (falls back to spec)
- **5 guide**: this page

`tab` cycles; `j/k` scroll or select; `g` top; `G` bottom (Live: follow
the tail). `i` opens the stage-detail overlay: status, staleness ("spec
changed after this ran"), blockers, and the suggested next action.
`/` opens a live filter on the findings/history lists (type to narrow,
Enter to keep it and navigate, Esc to clear; it drops when you leave
the tab). The statusline carries a spend sparkline of recent runs, and
pasting multi-line text into the chat input keeps its newlines instead
of submitting at the first one.
All keys are rebindable in `[keys]` (see config below).

## Chat to author the spec (or plan)

Press `⟨s⟩` (or `:` → *chat: edit spec/plan*) to open an interactive
chat: the **live document is on the left, the conversation on the
right**. Type an instruction (`⟨enter⟩` sends), and Claude edits the
file in place; you watch it change on the left as it happens.

- `⟨Tab⟩` cycles the **target**: the whole spec, each of its sections,
  then the plan. No plan yet? The target reads *plan (draft from spec)*
  and your first message drafts one FROM the spec. The left pane shows
  the whole document with the focused section highlighted in place.
- Each message acts on the document as it stands now, with your last
  few messages as context, so "make it 3 attempts, not 5" works. The
  file is the memory; no session state to manage.
- `⟨Ctrl+Z⟩` **undo** walks back through the last 10 edits (persisted:
  survives restarts, covers CLI chats too); `⟨Alt+Z⟩` **redo** walks
  forward again. A new edit invalidates the redo branch, like any editor.
- `⟨Ctrl+X⟩` **cancel** an in-flight edit (kills the daemon, drops any
  queued messages).
- Closed the TUI mid-edit? Press `⟨s⟩` again: if the daemonized edit is
  still running, the chat **reattaches**: transcript replayed, completion
  lands normally.
- `⟨Alt+Enter⟩` inserts a newline (the input box grows); `⟨enter⟩`
  while an edit runs **queues** the message (up to 3, sent in order).
- `⟨↑⟩`/`⟨↓⟩` scroll the transcript, `⟨esc⟩` closes (a running edit
  finishes on its own; it's a daemon like any other run).
- From a script: `ritual chat "tighten the goal to one sentence"`,
  `--section "Behavior…"` to scope it, `--plan` to target the plan.

The spec stage flips to **done** when the document gains real content.
Runs cost `budget_doc_chat_usd` at most (default $0.50/message), and
the agent is **hard-scoped**: it can read the project but write only
the one document you targeted (enforced at the permission layer).

## Findings workflow

1. Run dual-review; findings land in `.ritual/findings/*.json`. Other
   emitters drop files in the same dir and show up in the same list:
   `ritual mutants` (test gaps), `ritual secrets` (gitleaks hits), and
   the optional CodeRabbit reviewer.
2. Tab 2: severity pills (crit/major/minor), `◆ both` = cross-model.
   The selected finding shows its **snippet**: the 1-3 verbatim source
   lines the reviewer anchored it to. `⟨enter⟩` opens the **detail
   overlay**: full scenario, snippet, sources, verdict, and the actions.
3. `o` opens the selected finding in your **running** nvim
   (auto-discovers the server socket); `e` uses $EDITOR. Plan-review
   findings anchor to their plan step: `o`/`e` jump into plan.md at the
   referenced step. If a later edit moves the plan under a finding, it
   gets an **`⚓` anchor-lost** marker instead of silently mis-anchoring.
4. **Answer every finding first, then correct the plan ONCE.** Fixing
   findings one at a time mutates the plan under the remaining ones -
   anchors rot and n runs cost n×. Triage instead: `⟨F⟩` queues a plan
   finding for claude (**⚑A**, toggle), `⟨m⟩` queues any finding as
   yours to fix (**⚑M**), `⟨d⟩` dismisses - with an optional one-line
   **reason** (Enter on empty = plain dismiss) that feeds the review
   memory. The statusline counts your queue (`⚑N`).
   **Every row wears its state on the right**: `⚑A queued` / `⚑M
   manual` / `✗ declined` / `✓ fixed` / `∅ dismissed` - or, while
   untriaged, a dim **ghost of the recommended decision** (`→⚑A` `→⚑M`
   `→✓` archive `→∅` dismiss `→you`). One touch applies them all:
   `⟨t⟩` shows the counts (archive = the review already fixed it and
   recorded HOW - the prose moves into `reason`, never lost; withdrawn/
   refuted → dismissed; confirmed plan/code → queued ⚑A/⚑M; "need you"
   is never auto-applied) and `y` writes the dispositions. `t` never
   touches the plan - that stays behind `F`-apply.
5. **Apply**: `⟨F⟩` on a queued finding (or `:` → "findings: apply
   answers") confirms and spawns **one** headless run answering ALL
   queued findings against a single plan snapshot: it reads the whole
   plan, spec, and invariants, edits ONLY the queued findings' sections,
   and must end with a per-finding `ANSWERS:` verdict block. The union
   of those sections is enforced **mechanically** - a leaked edit
   auto-reverts wholesale, queue intact. Per finding: `FIXED`
   auto-marks; `DECLINED <reason>` returns it to triage with the reason
   shown (an unchanged plan defeats any FIXED claim). `⟨u⟩` reverts the
   whole applied batch atomically - plan restored, its fixed findings
   reopened AND requeued. One run = one `plan-fix` row in `ritual
   costs`, capped by `budget_finding_fix_usd` (per run, not per
   finding).
6. **Let claude fix code findings too**: a confirmed dual-review code
   finding (`file:line`) can go to the LLM just like a plan finding -
   `⟨F⟩` queues it (⚑A), or `⟨A⟩` queues EVERY confirmed code finding on
   the feature at once ("fix all"). `⟨F⟩`/apply then runs ONE headless
   pass that fixes them all, and verifies against the **global context**:
   it runs `./check.sh` (full), then an independent, strictly read-only
   **re-review** confirms each finding is resolved and nothing regressed.
   ritual detects the change by **content hash**, so the re-review sees the
   real edits even when the code lives in a directory this git repo does
   not track; a fix that changes nothing observable - or that moves HEAD (a
   stray commit/reset) - **fails closed**. Accept is **per finding**: each
   finding confirmed resolved is marked fixed, the rest stay queued with the
   reviewer's reason (a reported REGRESSION fails the whole batch, since the
   one diff can't be split). **Pass or fail, the attempt is LEFT in your
   working tree** - ritual never deletes the work; git is the undo. A
   failure **names why** (and offers `ritual attach <id>`); review with
   `git diff`, keep the good parts, or discard with `git restore .` /
   `git stash`. Press `⟨x⟩` to cancel an in-flight fix (the attempt stays in
   the tree; a cancelled plan-fix is reverted instead). Prefer to fix by
   hand? `⟨m⟩` flags a finding ⚑M and `Q` sends the manual queue to nvim's
   quickfix; work through them and `⟨f⟩` each.
7. Fix code findings, re-run `C`, then **close the loop**: `⟨f⟩` marks
   the selected finding fixed, `⟨d⟩` dismisses it (either toggles back
   on re-press), writing into the findings JSON. Resolved findings
   recede from the list; `⟨v⟩` shows/hides them (`ritual findings
   --all` on the CLI).
8. On a GitHub project, `ritual pr-comment` posts the open findings to
   the branch's PR (redacted; `--inline` adds file:line review comments).

A failed plan-fix **names its reason** in the statusline and the desktop
notification: a budget kill says which knob to raise (e.g.
`budget_finding_fix_usd`), tool-lock denials name the tool and file, and
`ritual attach <run-id>` replays the full transcript for anything deeper.

## Completeness: green tests are not "done"

A structural test suite proves what EXISTS is correct, never that the whole
plan was BUILT - so an LLM can green the tests at 40% and stop (the
"reward-hacking gap"). ritual closes it with a **coverage gate**:

- Your plan carries a **`## Deliverables`** checklist, one item per concrete
  deliverable: `- [ ] D1: <desc> - accept: <measurable pass/fail criterion> -
  route: <path or §Section>`. The `plan` stage is prompted to write it and
  `plan-review` flags a missing or vague one. IDs (`D1`) are stable, independent
  of step numbers.
- The **`coverage`** stage (last in the pipeline) is an LLM-as-Judge: read-only,
  it checks each deliverable against the actual tree - present? substantive (not
  a stub)? meets its acceptance criterion? - and files a **gap finding** per
  miss (which routes into the normal fix loop). ritual - not the agent - ticks
  the satisfied boxes. The stage is `Done` only at **zero gaps**.
- **`ritual complete`** drives it: judge coverage, auto-build each gap (code
  gaps via code-fix, plan gaps via plan-fix) in bounded rounds, re-judge, and
  loop until the judge is clean - a deliverable that keeps failing is marked
  STUCK (after `complete_max_attempts_per_item` tries) so the rest still
  progress. Bounded by `budget_complete_usd`, `complete_max_rounds`, and
  `complete_round_scope` (a few gaps per round so each pass actually finishes).
- **`ritual complete --check`** is the token-free CI gate: exit 0 only when
  coverage is clean AND `check.sh` is green AND no confirmed finding is open.
  "Done" means all three, never just tests-green. Completeness is judged
  **deterministically** from the latest coverage report plus the plan's
  checklist - a plan with no real `## Deliverables` can never be "complete", and
  a coverage run that produced no report is never read as "clean". Each run
  supersedes the prior coverage report (only the newest is kept).
- The judge must account for **every** unchecked deliverable: any it neither
  confirms (`satisfied`) nor flags (a gap) is treated as an unverified gap and
  re-driven - a partial or lazy judgment cannot read as "clean".
- Coverage evidence is scoped **per branch** (a stamped `branch` on each
  findings file), so another feature's coverage or open findings never affect
  this one, and one feature's run never supersedes another's report.
- `--check` is token-free, so it trusts the last coverage judgment for the
  *code* it saw; it re-checks the plan's deliverable coverage, `check.sh`, and
  open findings live, but does not re-judge code. After changing code, re-run
  `ritual complete` to re-judge - a stale judgment is not detected by `--check`
  alone.

The exit-code contract follows the lifecycle: a confirmed critical
blocks scripts/CI **until you mark it fixed or dismissed**. In CI:
`ritual run dual-review --ci` writes JUnit XML to `.ritual/ci/` and
exits nonzero on unresolved blocking findings.

Your dispositions feed back into reviews: `ritual lessons` (auto-run
before every dual-review) distills them into `.ritual/lessons.md`:
dismissed findings become a "known noise, do not re-flag" list the
reviewer reads first, fixed ones mark where real bugs actually lived.
The critic stops re-reporting what you already threw out.

## Invariants (the project constitution)

`ritual init` scaffolds `.ritual/invariants.md`. Fill it with the
non-negotiables, one bullet each ("parsers never panic on unknown
input", "state mutations flow through AppMsg"). Once it has real
bullets, every review stage receives it and treats each bullet as an
acceptance criterion: violations become major+ findings, /tdd derives
tests from the invariants a change touches, and the chat agent refuses
to write spec/plan content that contradicts one. Re-injected per stage,
so a standing constraint can never silently fall out of context.
`doctor` shows whether it's active. Commit it. Worktrees still resolve
the shared main-root `.ritual`.

## Quality gates

**Mutation gate (`ritual mutants`).** After implement goes green:
mutates only the code your diff touched (`cargo mutants --in-diff`),
runs the tests, and records every mutant the suite FAILED to kill as a
major finding with the mutated code as its snippet: proof of a test
gap, file:line-anchored. Advisory by design (major never blocks CI);
adjudicate with `f`/`d`. Baseline-red trees are refused with advice.
`[mutants] cmd` swaps the runner (TS/JS: point it at Stryker with a
wrapper that emits the same outcomes.json; recipe out of scope here).

**Secrets gate (`ritual secrets`).** Scans exactly what changed
(tracked modifications + untracked files, the "agent wrote a .env"
surface) with one `gitleaks dir` pass over a staged copy, so hits are
file:line-anchored and `.gitleaksignore` fingerprints keep matching.
Hits are critical/confirmed findings → they **block** until dismissed
or fingerprinted (the finding carries a paste-ready fingerprint). Runs
automatically before every dual-review when gitleaks is installed
(`pacman -S gitleaks`); silently skipped otherwise.

**Whole-project audit (`ritual audit`, optional).** Everything above
reviews a *change*; the audit reviews the *system*: cross-flow and
inter-stage contracts, exactly where per-diff review is blind by
construction. `ritual audit --discover` enumerates your project's
flows/techs into `.ritual/audit-lanes.md` (edit it - it's yours); a
plain `ritual audit` then runs one focused review lane per flow IN
PARALLEL - read-only by prompt contract (each lane's only write target
is its report file), blind to each other (they see the other lanes'
names only - independent reviewers with decorrelated blind spots), plus an
always-on `global-overview` lane for the contracts between flows. A
judge then adversarially refutes every candidate, requires an
independent Codex verdict per survivor (a judge should never grade its
own vendor's work), and writes standard findings (stage `audit`) that
triage exactly like review findings (`t`, `A`, `F`). Each lane is capped
at `budget_audit_usd`; the judge's cap scales with the reports it
adjudicates (`budget_audit_usd × (1 + lanes)`) - all ceilings, not
spends. Run it at milestones - pre-release, after big merges - not per
commit; dual-review stays the everyday gate.

## Third reviewer (CodeRabbit, optional)

`[coderabbit] enabled = true` runs the CodeRabbit CLI before each
dual-review (`coderabbit auth login` once; free tier = 3 reviews/hour;
**cloud-backed: your diff leaves the machine**). Its comments land as
single-source *unconfirmed* findings that never block; the dual-review
skill verifies or refutes each one and only then adds `coderabbit` to a
finding's sources: three sources is the strongest signal there is.

## Sandboxing headless runs (optional)

```toml
[sandbox]
enabled = true
wrapper = "srt --settings /home/you/.config/ritual/srt-settings.json"
```

Every headless agent run gets spawned as `<wrapper> <agent argv>`:
pipeline, chat, bench, and resumed daemons alike, because there is a
single spawn chokepoint. The wrapper is supervisor-owned config the
agent can't edit, and it's recorded in each run's meta for repro.
Recipe for Anthropic's sandbox-runtime: `npm i -g
@anthropic-ai/sandbox-runtime`, `pacman -S bubblewrap socat ripgrep`,
start from `docs/srt-settings.example.json` (allow-lists your project,
target dir, and the agent vendors' domains; denies ~/.ssh). Caveat:
file-watchers that scan outside the sandbox trip violations. Interactive
stages are never wrapped, because they own your terminal.

## Retry with another model

```toml
fallback_model = "claude-sonnet-5"   # overload? switch, don't die
[retry]
models = ["claude-opus-4-8", "claude-sonnet-5"]
```

`fallback_model` rides every headless claude run as `--fallback-model`:
a retryable API error hours into a review switches models instead of
failing the run. `[retry] models` adds palette entries (*retry
dual-review with claude-opus-4-8*) that appear only when a headless
stage failed or needs attention; `ritual run <stage> --model <m>` is
the CLI form. The pipeline sidebar shows `×N` once a stage has multiple
attempts, and history/report grow a model column so attempts compare.

## Money

- Per-run caps: `budget_plan_review_usd` (default $5),
  `budget_dual_review_usd` ($10), passed to claude as a hard budget.
- Daily ceiling: `budget_daily_usd` refuses new runs past it;
  `--force` overrides once. Statusline meter shows spend vs cap.
- `ritual history` = the ledger (`--json` for scripts); the footer now
  shows today's **cache-hit rate**.
- `ritual costs` = the analytics: today / 7 days / all-time totals, a
  per-stage table sorted by spend with per-stage cache economics, and
  the daily-budget gauge (`--json` for scripts).

## Safety & provenance

- **Redaction** (on by default): secrets are scrubbed *before* any
  byte hits the archive: vendor keys, JWTs, PEM blocks, assignments,
  high-entropy tokens. Archives are safe to commit.
- **Hash chain**: every run links to the previous one;
  `ritual verify-log` proves nobody edited history.
- **Repro bundles**: `ritual repro <run-id>` shows the exact model,
  CLI versions, git sha and diffs them against your current env.
- **Pruning without breaking the chain**: `ritual clean` (default:
  keep the newest 50) deletes old run artifacts but never touches live
  runs, state-referenced runs, or today's runs (the budget ledger).
  Pruned chained runs are attested by a **checkpoint** (a rolling
  genesis, like a git shallow clone), so `verify-log` stays intact:
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
or pin one: `nvim_server = "/path/to/socket"`, or launch with
`nvim --listen`. `o` open file:line · `Q` findings → quickfix.

## CLI (scriptable, styled, `--json` where it counts)

- `ritual`: the dashboard
- `ritual init`: scaffold .ritual/, check.sh, CLAUDE.md
  (`--skills` also installs the vendored workbench into `~/.claude`:
  all 14 skills, the code-reviewer agent, both hooks; one clone
  reproduces the whole setup)
- `ritual doctor`: check every prerequisite (`--deep` runs checks)
- `ritual status`: pipeline state (`--json`)
- `ritual run <stage>`: headless stage (`--force`, `--ci`)
- `ritual chat <msg>`: edit spec/plan (`--plan`, `--section`)
- `ritual ps` / `attach <run-id>`: live daemons; follow or `--kill`
- `ritual findings` / `history`: browse artifacts (`--json`, `--all`)
- `ritual pr-comment [N]`: findings → GitHub PR (`--inline`)
- `ritual report [--pdf]`: feature report from all artifacts
- `ritual new [--worktree B]`: name/create a feature
- `ritual reset-plan [--force]`: re-plan from the spec - delete plan.md, reset
  the plan..coverage stages, clear THIS feature's plan-review/coverage findings
  (exact branch match, so it never touches another feature's) + the plan undo
  stack (spec + code untouched). Dry-run without `--force`; palette `reset-plan`
  in the TUI (confirm y/n)
- `ritual clean`: prune old runs safely (`--keep N`, `--dry-run`)
- `ritual verify-log`: check the tamper-evident chain
- `ritual repro <run-id>`: reproducibility bundle + env diff
- `ritual bench <stage>`: N repeated runs, scored + spread stats
  (`--golden` adds recall and cost-per-hit)
- `ritual costs`: per-stage, cache-aware spend analytics (`--json`)
- `ritual lessons`: regenerate the review memory (`--stdout`)
- `ritual mutants`: mutation-kill gate over the diff (`--base`)
- `ritual secrets`: gitleaks over changed files; exits 1 on leaks
- `ritual skills diff`: vendored workbench vs installed skills
- `ritual export`: OTLP-JSON spans of all runs, with OTel GenAI
  semconv attributes (`--audit-trail` emits IETF
  draft-sharif-agent-audit-trail records instead: JCS-canonical,
  SHA-256 hash-chained JSONL, the compliance-shaped view of the
  same history the chain already protects)

## Settings editor (S)

`S` opens an in-TUI editor over the practical config knobs: budgets,
model/effort routing, theme/icons/transparency, notifications,
redaction, offline, base ref, check timeout. Each row shows the
EFFECTIVE value after layering plus its source - `(default)`,
`(user)` = `~/.config/ritual/`, `(project)` = `.ritual/config.toml`,
`(flag)` = a CLI flag shadows it this session.

- `enter` on a toggle/choice flips or cycles it in place; on a
  number/text row it opens an inline edit line (prefilled, validated -
  a bad value keeps the prompt open with the error).
- Empty input on an optional key CLEARS it from the project file so
  the layer below shows through; optional choices (per-stage effort)
  unset by cycling past their last value.
- Every change writes the PROJECT config with your comments and
  formatting preserved, then live-applies (theme included). Writes are
  transactional: if the reloaded config were invalid, the file is
  restored byte-for-byte.
- Command seams (`claude_cmd`, …), `[keys]`, `[commands]`, and the
  sub-tool tables (`[mutants]`, `[secrets]`, …) stay file-only on
  purpose.
- Worktrees share one `.ritual/`: sibling instances pick a change up
  on their next launch. `--theme`/`--ascii` win for the session; the
  written value takes over on the next flagless launch.

## Config

Layered: defaults ← `~/.config/ritual/config.toml` ←
`.ritual/config.toml` ← env ← flags.

```toml
theme = "eldritch"            # or "tokyonight"
transparency = true           # terminal bg shows through
redaction = true
budget_daily_usd = 15.0
budget_doc_chat_usd = 0.50    # per spec/plan chat message
budget_finding_fix_usd = 1.0  # per F-apply plan-fix batch run
budget_code_fix_usd = 5.0     # per code-fix batch run (fix + re-review)
check_timeout_secs = 600
offline = false               # block runs (metered/plane mode)
nvim_server = ""              # empty = auto-discover
fallback_model = ""           # overload fallback for headless claude runs

[keys]                        # rebind anything
check-full = "W"

[models]                      # route stages to models
plan-review = "opus"

[effort]                      # per-stage reasoning effort
plan = "xhigh"
plan-fix = "high"             # the F fix runs

[retry]                       # palette offers for failed stages
models = []

[mutants]                     # mutation-kill gate
cmd = "cargo mutants"
timeout_secs = 300
enabled = false               # advisory flag for doctor/guide hints

[secrets]                     # gitleaks gate (auto before dual-review)
enabled = true

[sandbox]                     # wrap headless runs (srt recipe above)
enabled = false
wrapper = ""

[coderabbit]                  # third reviewer (cloud-backed, off by default)
enabled = false

[commands]                    # your own palette entries
"deploy preview" = "./scripts/preview.sh"

[consensus]                   # third-model arbitration (off by default)
enabled = false
```

## Consensus tier (optional third model)

For a genuinely contested plan-review disagreement, a third vendor can
arbitrate: one stance argues for, one against, and the verdict lands
under the disagreement, clearly labeled as an opinion, not truth.
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
  and detector-not-resolver: *you* arbitrate what it flags.
- Never let the implementer design its own tests. That is the whole
  point of tests-red running on the other model.
- Trust `◆ both` findings even when they look pedantic. Live stat:
  the first real run's cross-confirmed critical was a genuine
  path-traversal bug.
- `check.sh fast` must stay under ~10s because it runs on every edit via
  the hook. Push slow suites to the full variant.
- Archives are the truth: `.ritual/runs/*.jsonl` is raw agent output,
  kept verbatim (post-redaction) even when parsing fails.
- If a run looks stuck, quit the TUI and reopen. Reattach is free.
  `a` (takeover) turns any headless run into an interactive session.
- Worktrees + `offline = true` on hotel wifi: agent runs are blocked, so
  you can queue specs and edit plans locally, then flip it back and fire
  the reviews on a real connection.
- `NO_COLOR=1 ritual status` / `--ascii` for logs and plain terminals:
  every state is readable without color.

## A full run, start to finish

A concrete walkthrough of one feature, touching every part of the tool.
Keys are shown as `⟨key⟩`. The sidebar (left) always shows three
sections: FEATURES, PIPELINE, AGENTS; the main pane (right) is the
five tabs.

**0. Open ritual.** Run `ritual init` once in your repo (scaffolds
`.ritual/`, `check.sh`, `CLAUDE.md`), then just `ritual`. You land on
the **live** tab (`⟨1⟩`) showing the greeter. Bottom line is the
powerline statusline: branch, today's spend vs budget, check state.

**1. Name the feature.** In another shell: `ritual new "Audio engine"`.
For parallel work that shouldn't touch your current branch, use a
worktree: `ritual new --worktree feat/audio` (own checkout, shared
`.ritual`). Back in the TUI, `⟨r⟩` refreshes; the feature shows in the
FEATURES section. `⟨[⟩` / `⟨]⟩` cycle features: needs-you ones sort
first, flagged with a yellow ``.

**2. Write the spec.** The PIPELINE section lists the seven stages with
one highlighted. On the greeter, `⟨j⟩`/`⟨k⟩` move that highlight;
land on `spec` and press `⟨enter⟩`. ritual opens `spec.md` in your
`$EDITOR` (the TUI hands over the terminal, then takes it back on
exit). Write what you want built, `:wq`. The stage flips to **done**
if you wrote real content, stays pending if you only left comments.
*Prefer to talk it out?* Press `⟨s⟩` instead for the chat (see "Chat
to author the spec" above). Describe the feature and Claude drafts
the spec live, section by section.

**3. Draft the plan.** Highlight `plan`, `⟨enter⟩` → an interactive
Claude session opens (plan mode). When it saves `plan.md` and exits,
the stage goes done. Read the result on the **plan** tab (`⟨4⟩`):
it's rendered markdown; `⟨j⟩`/`⟨k⟩` scroll, `⟨g⟩` jumps to top.

**4. Cross-review the plan.** The fastest way to run any stage from
anywhere is the command palette: `⟨:⟩`, type `run plan-review`,
`⟨enter⟩` (fuzzy, `rpl` works). Claude and Codex now debate the plan.
This is a **daemon**: the **live** tab (`⟨1⟩`) streams both models;
the statusline budget meter ticks up. You can quit ritual entirely
(`⟨q⟩`) and reopen later; it reattaches to the running daemon. Press
`⟨a⟩` to take the session over in interactive Claude (`--resume`).
`⟨x⟩` cancels. When it finishes you get a desktop notification and the
stage shows **needs-you** (a human decides).

**5. Triage findings.** Switch to the **findings** tab (`⟨2⟩`). Each
finding is a severity pill (crit/major/minor); a green **◆ both**
badge means *both* models flagged it. Treat those as blockers.
`⟨j⟩`/`⟨k⟩` select. Then either `⟨o⟩` (open the file:line in your
already-running nvim), `⟨Q⟩` (push *all* findings to nvim's quickfix
list, `:cnext` through them), or `⟨e⟩` (open in `$EDITOR`). Edit
`plan.md` to address them; re-run `plan-review` if the plan changed
materially.

**6. Tests first.** Palette → `run tests-red`: Codex designs tests
from the *spec* (not from your plan, and never the model that will
implement), written **failing**. Press `⟨c⟩` to run `check.sh fast`:
red, as expected. This is the whole point: the test author and the
implementer are different models. ritual pins this session to an id it
owns (`--session-id`), stored under the feature, so the handoff below is
deterministic.

**7. Implement.** Palette → `run implement`: ritual **resumes the exact
tests-red session** (`--resume <that id>`), so the same conversation that
wrote the failing tests now makes them pass. This is pinned by id - a
Claude session you have open in another terminal can't hijack the handoff
(the old `--continue` grabbed "the most recent conversation in the
directory"). Because an interactive `claude --resume` can't be handed an
opening message, ritual **copies a ready-to-paste implement instruction to
your clipboard** and shows a short overlay: press `⟨enter⟩` to open the
session and paste to start (`⟨c⟩` re-copies; `⟨esc⟩` cancels). If no session
is pinned yet, the overlay leads into the `--resume` picker so you choose the
right one. As Claude edits, the global PostToolUse
hook auto-runs `check.sh` and feeds failures back; the check segment in the
statusline goes green/red, and the stage completes when `check.sh` passes.
`⟨a⟩` takeover also reattaches to these pinned sessions now.

**8. Final review.** Palette → `run dual-review`: both models review
the actual diff independently, findings merged into tab `⟨2⟩` again.
Fix every **◆ both**, re-run `⟨C⟩`, then accept. In CI you'd run this
headless instead: `ritual run dual-review --ci` writes JUnit XML and
exits nonzero on blocking findings.

**9. Wrap up and prove it.** From the shell (or the palette's custom
commands):
- `ritual report --pdf`: a shareable report from every artifact
  (redacted, safe to commit).
- **history** tab (`⟨3⟩`) or `ritual history`: cost, tokens, duration
  per run; the statusline shows today's total vs your `budget_daily_usd`.
- `ritual verify-log` proves the tamper-evident hash chain over all
  runs is intact.
- `ritual repro <run-id>`: the exact models, CLI versions, and git sha
  a run used, diffed against your current environment.
- `ritual export`: OTLP-JSON spans for a tracing backend.
- `ritual bench plan-review --runs 5 --golden expected.json`: measure
  a stage's quality/recall across repeats when tuning models or prompts.

**Throughout:** secrets are redacted before anything is archived;
runs survive the TUI dying; the daily budget refuses new runs past the
ceiling (`--force` overrides once); `offline = true` blocks agent calls
for metered connections. Nothing here needs the cloud except the agent
calls themselves: archives, findings, reports, and the chain are all
local files.

That's the loop: **spec → plan → plan-review → tests-red → implement →
dual-review**, with two models keeping each other honest and
`check.sh` keeping both honest.
