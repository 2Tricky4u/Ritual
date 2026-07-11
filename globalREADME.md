# Multi-LLM Coding Workbench

Everything here implements one idea, backed by research: **two models beat one only when their exchange is grounded in something objective** — tests, diffs, execution. Claude Code implements; OpenAI Codex critiques plans, designs tests, and independently reviews diffs; `./check.sh` is the ground truth; you are the judge.

This README is the "what can I do" map. Deep reference: [`multi-llm-playbook.md`](multi-llm-playbook.md) (evidence + configs) and [`ritual/README.md`](ritual/README.md) (the TUI).

---

## The loop (the main thing you do)

For any nontrivial feature, in any project:

| step | what happens | how to run it |
|---|---|---|
| 1. Spec | write WHAT, not HOW | `ritual run spec` (opens $EDITOR) or just write `.ritual/features/<branch>/spec.md` |
| 2. Plan | Claude plan mode designs the implementation | plan mode in `claude`, or `ritual run plan` |
| 3. Plan review | **Codex adversarially critiques the plan**, ≤2 debate rounds, disagreements come to you | `/plan-review` in claude, or `ritual run plan-review` |
| 4. Tests red | **Codex designs the test list from the spec** (it never sees the implementation plan), Claude writes them failing | `/tdd` in claude, or `ritual run tests-red` |
| 5. Implement | Claude codes until green; every edit auto-runs `./check.sh fast` (global hook) | just work in `claude`, or `ritual run implement` |
| 6. Dual review | **Claude's fresh-eyes reviewer subagent AND Codex review the same diff independently**; only findings confirmed by both (or by a repro) get fixed silently | `/dual-review [base]` in claude, or `ritual run dual-review` |
| 7. Merge | you review and merge | — |

The two review stages write machine-readable findings to `.ritual/findings/` — browsable, with a ◆both badge when both vendors agree (strongest signal a finding is real).

---

## What you can do, tool by tool

### In any Claude Code session (skills — installed user-level, work everywhere)

The cross-model workflow trio:
- `/plan-review [plan-file]` — cross-model critique of a plan before you accept it. Checklist: missing requirements, edge cases, hidden complexity, simpler alternative, risks, testability.
- `/tdd [feature]` — test-first implementation with Codex as the independent test designer.
- `/dual-review [base-ref]` — independent two-model diff review, confirm-before-fix.
- The `code-reviewer` subagent — read-only fresh-context reviewer; Claude uses it proactively after changes.

The tailored toolbelt (added 2026-07, modeled on the best-rated community skills, integrated with check.sh/.ritual conventions):
- `/brainstorm [idea]` — Socratic discovery BEFORE planning; converges on a spec (writes `.ritual` spec.md); hands off to plan mode.
- `/debug [symptom]` — systematic 4-phase root-cause debugging; fixing before understanding is forbidden; every fix ends with a regression test.
- `/commit`, `/pr [base]`, `/changelog [range]` — git delivery: Conventional Commits from the staged diff, evidence-based PR descriptions with risk/rollback, Keep-a-Changelog release notes.
- `/docs [target]` — project documentation with executed-before-documented examples; no marketing adjectives.
- `/document [what]` — deliverable .docx/.pdf via markdown + pandoc, verified artifacts.
- `/deps-audit` — supply-chain audit (cargo/npm/pip audit + licenses) with reachability triage; writes `.ritual` findings. Complements built-in `/security-review` (code-level).

### Talk to Codex directly (plugin commands)
- `/codex:review` — Codex reviews uncommitted changes / a branch / a commit.
- `/codex:adversarial-review` — the hostile version.
- `/codex:transfer` — hand the current task over to Codex.
- `/codex:status` — your Codex usage/limits.

### The `ritual` TUI (installed: `~/.local/bin/ritual`)
- `ritual` — full dashboard: pipeline per branch, live agent stream, findings browser (`e` = jump to file:line in nvim), run history with cost/tokens, auth + check widgets. Eldritch theme; `--theme tokyonight`, `--ascii` available.
- `ritual init` — make any project workflow-ready: `.ritual/`, stack-detected `check.sh` (Rust/Python/Node/mixed), CLAUDE.md snippet, gitignore entries.
- `ritual new "Title"` — start a feature on the current branch (seeds the spec template).
- `ritual status` / `findings [--json]` / `history` — scriptable, styled, pipe-clean.
- `ritual run <stage>` — any pipeline stage from the shell, headless ones streaming live.

### The safety net you don't have to think about
- **Global hook**: after every Edit/Write in any Claude session, `./check.sh fast` runs; failures feed straight back to Claude as blocking feedback. Projects opt in simply by having an executable `check.sh`.
- **Budget caps**: headless review runs carry `--max-budget-usd` (5/10 by default, per-project override in `.ritual/config.toml`).
- **Stalled run rescue**: every headless run stores its session id — `claude --resume <id>` takes it over interactively.

### Escalation (when a decision is genuinely contested)
pal-mcp-server's `consensus` tool runs stance-steered multi-model debate (one argues for, one against). Not installed — needs a (free-tier) Gemini key; recipe in the playbook. Use sparingly: the evidence says debate without grounding is the weakest pattern.

---

## One-time setup still pending

1. `codex login` — sign in with your ChatGPT account (type `! codex login` inside a Claude session). Until then, cross-model stages show red in ritual and refuse to run.
2. First real run: `cd ritual && ritual`, Enter on `plan-review` against a toy plan — verifies the whole bridge.
3. **Free disk space on /home** — it's at 100%; ritual builds were moved to `/var/tmp/ritual-target` but other things will break.

## What's where

```
~/Documents/project/
├── README.md                  ← you are here
├── multi-llm-playbook.md      ← evidence, configs, templates, anti-patterns
├── ritual/                    ← the TUI (Rust, v0.1, dogfoods its own workflow)
└── demo-tdd/                  ← throwaway sandbox used to verify the hook
~/.claude/
├── skills/{plan-review,tdd,dual-review}/   ← the cross-model skills
├── agents/code-reviewer.md                 ← read-only reviewer subagent
├── hooks/check-on-edit.sh                  ← the check.sh hook
└── settings.json                           ← hook wiring (backup: backups/settings.json.bak-2026-07-11)
```

## Anti-patterns (the research says don't)

- Open-ended model-to-model chat with no tests in the loop — debate is a *detector*, not a resolver.
- Letting the implementer design its own tests, or review its own diff in the same context.
- Auto-fixing findings only one model reported and nothing reproduced.
- Two agents writing the same branch. One writer; everyone else reads.
