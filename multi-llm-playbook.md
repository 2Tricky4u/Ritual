# Multi-LLM Coding Playbook

A research-backed workflow combining Claude Code (orchestrator + implementer) with OpenAI Codex (adversarial critic, test designer, second reviewer). Built July 2026 from a deep-research pass over 28 sources with 22 adversarially-verified claims (appendix at the bottom).

## The core insight

**External feedback is the engine — not a second model's opinion.** LLMs cannot reliably self-correct without an outside signal, and can get *worse* when they try (DeepMind, arXiv 2310.01798). Extra pipeline complexity adds almost nothing beyond a simple generate → execute → refine loop (arXiv 2604.21950), and open-ended multi-agent debate fails to beat simple single-model baselines (arXiv 2502.08788).

So a second model is used only where it measurably pays:
1. **As an adversarial critic of plans** — cheap, early, decorrelated blind spots.
2. **As an independent test designer** — tests from a non-implementing model are more objective (AgentCoder: test accuracy 67% → 88%, arXiv 2312.13010).
3. **As an independent second reviewer of diffs** — agreement between vendors is strong evidence a finding is real.

And every loop terminates in something objective: tests, types, linters — never in "the models agreed."

## The loop

```
┌───────────────┐   ┌──────────────────┐   ┌──────────────────┐
│  Plan mode    │──▶│  /plan-review     │──▶│  human approves  │
│  (Claude)     │   │  Codex critiques, │   │  the plan        │
└───────────────┘   │  ≤2 rounds        │   └────────┬─────────┘
                    └──────────────────┘            │
┌───────────────────────────────────────────────────▼─────────┐
│  /tdd — Codex designs tests from the spec (not the impl),   │
│  Claude writes them RED, then implements until GREEN.        │
│  check.sh hook lints/typechecks on every edit (exit-2       │
│  failures feed straight back to Claude).                     │
└───────────────────────────────────┬──────────────────────────┘
                                    │
                    ┌───────────────▼──────────────┐
                    │  /dual-review — code-reviewer │
                    │  subagent + Codex review the │
                    │  diff INDEPENDENTLY; only     │
                    │  confirmed findings get fixed │
                    └───────────────┬──────────────┘
                                    │
                          human reviews & merges
```

Rules that make it work:
- **One writer at a time.** Claude implements; Codex never edits the same branch.
- **Reviewers get fresh context.** The code-reviewer subagent and Codex never see the implementer's reasoning or each other's findings.
- **Debate is bounded to 2 rounds.** Disagreements go to the human, not another round.
- **Nothing merges on model agreement alone** — tests and the human are the arbiters.

## What's installed where

| Piece | Location | Purpose |
|---|---|---|
| Codex CLI (v0.144.1) | `~/.local/bin/codex` | The OpenAI side. Auth: `codex login` (ChatGPT subscription — no API key needed) |
| `codex` MCP server | user scope (`claude mcp list`) | Exposes `codex()` / `codex-reply()` tools to Claude in every project |
| Codex plugin | `codex@openai-codex` | `/codex:review`, `/codex:adversarial-review`, `/codex:transfer`, `/codex:rescue`, `/codex:status`, `/codex:setup` |
| `/plan-review` skill | `~/.claude/skills/plan-review/` | Bounded cross-model plan critique |
| `/tdd` skill | `~/.claude/skills/tdd/` | Cross-model test-first implementation |
| `/dual-review` skill | `~/.claude/skills/dual-review/` | Independent two-model diff review |
| `/brainstorm` `/debug` `/commit` `/pr` `/changelog` `/docs` `/document` `/deps-audit` | `~/.claude/skills/*` | Tailored toolbelt (2026-07): pre-spec discovery, root-cause debugging, git delivery, verified docs, pandoc deliverables, supply-chain audit. ~11 skills total — at the community-recommended context-tax ceiling; disable what you don't use |
| `code-reviewer` subagent | `~/.claude/agents/code-reviewer.md` | Read-only fresh-eyes reviewer (no Edit/Write) |
| check-on-edit hook | `~/.claude/hooks/check-on-edit.sh` + `PostToolUse` in `~/.claude/settings.json` | Runs `./check.sh fast` after every Edit/Write; failures block with feedback |

Everything is user-level: it works in every project. A project opts into the edit-time checks simply by having an executable `./check.sh`.

## check.sh templates

The hook calls `./check.sh fast` after every edit (keep it under ~10s: lint + typecheck). Bare `./check.sh` = full run including tests, used by `/tdd` and `/dual-review`.

**Python**
```bash
#!/usr/bin/env bash
set -e
ruff check . && ruff format --check .
[ "${1:-}" = fast ] && exit 0
pyright .
pytest -q
```

**Rust**
```bash
#!/usr/bin/env bash
set -e
cargo fmt --check
cargo clippy --all-targets -- -D warnings
[ "${1:-}" = fast ] && exit 0
cargo test
```

**JavaScript / TypeScript**
```bash
#!/usr/bin/env bash
set -e
npx eslint . --max-warnings 0
[ "${1:-}" = fast ] && exit 0
npx tsc --noEmit
npx vitest run
```

**Mixed / monorepo** — dispatch on what exists:
```bash
#!/usr/bin/env bash
set -e
[ -f Cargo.toml ]    && { cargo clippy --all-targets -- -D warnings; [ "${1:-}" != fast ] && cargo test; }
[ -f pyproject.toml ] && { ruff check .; [ "${1:-}" != fast ] && pytest -q; }
[ -f package.json ]  && { npx eslint . --max-warnings 0; [ "${1:-}" != fast ] && npx vitest run; }
exit 0
```

`chmod +x check.sh` — the hook skips projects where it isn't executable.

## CLAUDE.md snippet for any project

Drop this into a project's `CLAUDE.md` so every session uses the loop:

```markdown
## Workflow
- Nontrivial features: plan mode first, then /plan-review before accepting the plan.
- Implementation: /tdd — tests are designed from the spec and written red before code.
- ./check.sh fast must pass after every edit (hook enforces); full ./check.sh before review.
- Before committing significant changes: /dual-review. Only confirmed findings get fixed silently.
```

## Escalation tier: pal-mcp-server (optional, not installed)

For genuinely contested design decisions, [pal-mcp-server](https://github.com/BeehiveInnovations/pal-mcp-server) (ex-Zen MCP) adds `consensus` — stance-steered multi-model debate (one model argues for, one against) — and `clink`, which bridges whole CLIs (Codex, Gemini) with shared conversation context.

To install later:
1. Install uv: `curl -LsSf https://astral.sh/uv/install.sh | sh`
2. Get a free-tier Gemini API key (aistudio.google.com) — cheapest unlock, adds a third vendor.
3. `claude mcp add --scope user pal --env GEMINI_API_KEY=<key> -- uvx --from git+https://github.com/BeehiveInnovations/pal-mcp-server.git pal-mcp-server`

Use it for: contested architecture choices (`consensus` with opposing stances), or generating a hard function from 2–3 models and keeping whichever passes the most tests (ensembles beat every single model — EnsLLM 90.2% vs 83.5% on HumanEval — but cost N×, so reserve them).

## Anti-patterns (evidence says don't)

- **Unbounded model-to-model chat with no tests in the loop.** Multi-agent debate does not beat simple baselines (arXiv 2502.08788). Two rounds, then a human.
- **Self-review in the implementer's context.** Intrinsic self-correction doesn't work (arXiv 2310.01798). Reviewers always get fresh context.
- **Two writers on one branch.** Merge conflicts plus diffused responsibility. One implementer; everyone else read-only.
- **Auto-fixing unconfirmed critic findings.** Reviewers hallucinate defects. Confirmed = both models or a reproduction.
- **Adding pipeline stages instead of better feedback.** Topology adds ~nothing (arXiv 2604.21950). If quality is lacking, improve check.sh and the tests, not the org chart.
- **Letting the implementer design its own tests.** It inherits its own misunderstandings (AgentCoder, arXiv 2312.13010).

## Maintenance

- Tool names drift fast: `codex mcp-server` spelling, plugin command names, and pal's repo name were all verified July 2026 — re-verify when things break after an update.
- `~/.claude/settings.json` backup from before the hook merge: `~/.claude/backups/settings.json.bak-2026-07-11`.
- Codex usage draws on the ChatGPT subscription's Codex limits — `/codex:status` shows usage.

## Evidence appendix

| # | Claim (verified 2–3 independent votes) | Source |
|---|---|---|
| 1 | LLMs can't self-correct reasoning without external feedback; performance can degrade | [arXiv 2310.01798](https://arxiv.org/abs/2310.01798) (DeepMind) |
| 2 | Test-driven agentic workflow: 88.8% SWE-bench Lite (+27.8pts over next best), 94.3% Verified | [arXiv 2510.23761](https://arxiv.org/pdf/2510.23761) (TDFlow) |
| 3 | TDD infrastructure: +34–48pts generation quality on WebGen-Bench | [arXiv 2605.17242](https://arxiv.org/html/2605.17242) |
| 4 | Separate test-designer agent: test accuracy 67.1% → 87.8% vs single-conversation | [arXiv 2312.13010](https://arxiv.org/html/2312.13010v1) (AgentCoder) |
| 5 | Architect/editor split improved every model pair; o1-mini +10.3pts with separate editor | [aider architect mode](https://aider.chat/2024/09/26/architect.html) |
| 6 | Multi-agent debate fails to reliably beat CoT/self-consistency baselines | [arXiv 2502.08788](https://arxiv.org/abs/2502.08788) |
| 7 | Pipeline topology beyond generate→execute→refine adds no significant gain | [arXiv 2604.21950](https://arxiv.org/pdf/2604.21950) |
| 8 | Cross-model ensemble 90.2% HumanEval / 50.2% LiveCodeBench vs best single model 83.5% / 43.4% | [arXiv 2503.15838](https://arxiv.org/pdf/2503.15838) (EnsLLM) |
| 9 | Reflexion (self-reflection grounded in test feedback): 91% pass@1 HumanEval | [arXiv 2303.11366](https://arxiv.org/pdf/2303.11366) |
| 10 | Sub-agent decomposition reduces long-context burden, beats monolithic agents on repair | [arXiv 2510.23761](https://arxiv.org/pdf/2510.23761) |
| 11 | Claude Code subagents: isolated context windows, own prompts/tools/permissions | [docs](https://code.claude.com/docs/en/sub-agents) |
| 12 | Zen/PAL MCP: multi-provider orchestration, `consensus` with stance steering, cross-model context continuity | [repo](https://github.com/BeehiveInnovations/pal-mcp-server) |
| 13 | Codex CLI ships a native stdio MCP server (`codex mcp-server`); ChatGPT-subscription auth | [OpenAI docs](https://learn.chatgpt.com/docs/mcp-server) |
| 14 | Official Codex plugin for Claude Code with review/adversarial-review/delegate commands | [openai/codex-plugin-cc](https://github.com/openai/codex-plugin-cc) |

Refuted in verification (claims that did NOT survive): pure self-critique gaining ~20% absolute (0–3 votes); open-source-only ensembles beating best proprietary models (0–2).
