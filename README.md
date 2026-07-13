<a id="readme-top"></a>

<!-- PROJECT SHIELDS -->
<div align="center">

[![CI][ci-shield]][ci-url]
[![Version][version-shield]][roadmap-url]
[![License][license-shield]][license-url]
[![Tests][tests-shield]][tests-url]
[![Live driver][e2e-shield]][e2e-url]
[![Tamper-evident][chain-shield]][guide-url]
[![Zero tokens][zero-token-shield]][tests-url]

</div>

<!-- PROJECT LOGO -->
<br />
<div align="center">
<pre>
██████╗ ██╗████████╗██╗   ██╗ █████╗ ██╗     
██╔══██╗██║╚══██╔══╝██║   ██║██╔══██╗██║     
██████╔╝██║   ██║   ██║   ██║███████║██║     
██╔══██╗██║   ██║   ██║   ██║██╔══██║██║     
██║  ██║██║   ██║   ╚██████╔╝██║  ██║███████╗
╚═╝  ╚═╝╚═╝   ╚═╝    ╚═════╝ ╚═╝  ╚═╝╚══════╝
  s u m m o n  ·  r e v i e w  ·  v e r i f y
</pre>

  <p align="center">
    A fast, eldritch-themed TUI that drives a <strong>multi-LLM coding workflow</strong>:<br />
    Claude Code implements, OpenAI Codex adversarially reviews plans, designs tests,<br />
    and second-reviews diffs, every loop grounded in your project's <code>./check.sh</code>.
    <br />
    <br />
    <a href="docs/guide.md"><strong>Explore the guide »</strong></a>
    <br />
    <br />
    <a href="#about-the-project">View Demo</a>
    ·
    <a href="ROADMAP.md">Roadmap</a>
    ·
    <a href="multi-llm-playbook.md">Playbook</a>
    ·
    <a href="#usage">Usage</a>
  </p>
</div>

<!-- TABLE OF CONTENTS -->
<details>
  <summary>Table of Contents</summary>
  <ol>
    <li>
      <a href="#about-the-project">About The Project</a>
      <ul>
        <li><a href="#built-with">Built With</a></li>
      </ul>
    </li>
    <li>
      <a href="#features">Features</a>
      <ul>
        <li><a href="#the-loop">The loop</a></li>
        <li><a href="#running-things">Running things</a></li>
        <li><a href="#trust--audit">Trust &amp; audit</a></li>
        <li><a href="#ergonomics">Ergonomics</a></li>
      </ul>
    </li>
    <li>
      <a href="#getting-started">Getting Started</a>
      <ul>
        <li><a href="#prerequisites">Prerequisites</a></li>
        <li><a href="#installation">Installation</a></li>
      </ul>
    </li>
    <li>
      <a href="#usage">Usage</a>
      <ul>
        <li><a href="#quick-start">Quick start</a></li>
        <li><a href="#configuration">Configuration</a></li>
        <li><a href="#recipes">Recipes</a></li>
        <li><a href="#default-keys">Default keys</a></li>
      </ul>
    </li>
    <li><a href="#roadmap">Roadmap</a></li>
    <li><a href="#design-notes">Design Notes</a></li>
    <li><a href="#documentation">Documentation</a></li>
    <li><a href="#development">Development</a></li>
    <li><a href="#acknowledgments">Acknowledgments</a></li>
  </ol>
</details>

<!-- ABOUT THE PROJECT -->
## About The Project

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

ritual is built on one bet, backed by the research: **external feedback is the quality engine**. Tests, checks, mutation kills, and cross-model review beat any single model talking to itself. Everything here exists to make that loop fast, auditable, and cheap to repeat:

- A **per-branch pipeline** (`spec → plan → plan-review → tests-red → implement → dual-review`) where the adversarial stages run a *different vendor's* model against Claude's work
- **Runs are daemons**: archived raw before parsing, resumable after any crash, tamper-evident forever
- **Findings are the currency**: every gate (models, mutation testing, secret scanning, a third reviewer) emits the same anchored JSON, adjudicated with two keys and enforced by one exit-code contract

<p align="right">(<a href="#readme-top">back to top</a>)</p>

### Built With

[![Rust][rust-badge]][rust-url]
[![ratatui][ratatui-badge]][ratatui-url]
[![tokio][tokio-badge]][tokio-url]
[![Claude Code][claude-badge]][claude-url]
[![Codex CLI][codex-badge]][codex-url]

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- FEATURES -->
## Features

### The loop

- **The pipeline**: per-branch stages `spec → plan → plan-review → tests-red → implement → dual-review`; one-key launch; headless stages stream live; interactive stages hand you a real attached `claude` session and resume the TUI on exit.
- **Chat to author the spec/plan** (`s`): a split view with the live document on the left (focused section highlighted in place) and a conversation on the right: type an instruction and Claude edits `spec.md` (or `plan.md`) in place while you watch, scopable to the whole doc or one `##` section (`Tab` to switch; a missing plan is *drafted from the spec*). `Ctrl+Z`/`Alt+Z` undo/redo (persisted 10-deep stack), `Ctrl+X` cancel, `Alt+Enter` multi-line, messages queue while an edit runs, and reopening chat reattaches to a still-running edit. The agent is hard-scoped at the permission layer: it can read the project but write only the targeted document. Also headless: `ritual chat "<msg>" [--plan] [--section …]`.
- **Findings lifecycle**: on the findings tab, `f` marks fixed and `d` dismisses (write-through to the JSON; `v` shows/hides resolved); the selected finding shows the verbatim source **snippet** it anchors to. The exit-code/CI contract follows: a confirmed critical blocks until resolved. `ritual pr-comment [N] [--inline]` posts the open findings to the branch's GitHub PR, redacted.
- **Quality gates**: `ritual mutants` mutates only your diff (cargo-mutants) and turns every mutant the tests failed to kill into an anchored finding (a proven test gap, advisory); `ritual secrets` gitleaks-scans exactly what changed (incl. untracked files) and its critical findings **block until dismissed or fingerprinted** (auto-run before every dual-review). `.ritual/invariants.md` is the project constitution: every bullet becomes an acceptance criterion re-injected into each review stage. `ritual lessons` distills your f/d dispositions into review memory the critic reads first. It stops re-flagging what you already dismissed.
- **Third reviewer, ensemble-style**, optional CodeRabbit CLI review before each dual-review: its comments land as *unconfirmed* single-source findings that never block; the dual-review skill verifies or refutes each one. Three agreeing sources is the strongest signal there is.
- **Reproducible workbench**: the whole multi-LLM setup (13 skills incl. `/spec` and the optional `/consensus`, the code-reviewer agent, both hooks) is vendored in `workbench/` and installed by `ritual init --skills`; `ritual skills diff` shows exactly where installed copies diverge. An optional third-model **consensus tier** (`[consensus] enabled`, pal MCP + Gemini) lets plan-review escalate one contested finding for arbitration.

### Running things

- **Runs are daemons**: every headless run detaches (`setsid`) and survives the TUI, the terminal, and reboots of your session. The raw event stream is archived to `.ritual/runs/*.jsonl` *before* parsing; the TUI is just a tailer. Restart `ritual` and it reattaches to live runs, reconciles anything that finished while you were away, and announces parallel runs it can't attach. Cancel kills the whole process group.
- **Run control from anywhere**: `ritual ps` lists live daemons, `ritual attach <run-id>` streams one into any terminal (`--kill` stops it). `ritual doctor [--deep]` checks agents, auth, MCP wiring, skills drift, hooks, check.sh, gates, and disk pressure. `ritual clean [--keep N] [--dry-run]` prunes old run artifacts: live/state-referenced/today's runs protected, pruned chained runs attested by a tamper-evident **checkpoint** so `verify-log` never breaks.
- **Attempts + resilience**: `fallback_model` keeps headless runs alive through provider overloads; `[retry] models` offers *retry `<stage>` with `<model>`* in the palette for failed stages (`run --model` on the CLI); the sidebar shows `×N` attempts and history/reports grow a model column.
- **Parallel features in git worktrees**: `ritual new "Title" --worktree feat/x` creates a worktree sharing ONE `.ritual/` state in the main repo. The sidebar lists all features with a needs-you queue (`!` badge, attention-first ordering); `[` `]` cycle features; runs execute in the right checkout automatically.
- **One-key takeover**: `a` reattaches the selected stage's recorded session interactively (`claude --resume <session-id>`).

### Trust & audit

- **Safety + money**: gitleaks-style **secret redaction** on every archived line, stream, and report (vendor key shapes, PEM blocks, assignments, entropy tokens; `redaction = false` to opt out). **Daily budgets** (`budget_daily_usd`) with a status-bar meter and run refusal (`--force` overrides); `ritual costs` for per-stage, cache-aware spend analytics. Desktop **notifications** on stage completion.
- **Provenance**: every run records a **reproducibility bundle** (git commit, dirty-diff hash, claude/codex versions, skill-file hashes, config snapshot; `ritual repro <run-id>` diffs it against your current env) and a **tamper-evident hash chain** (`ritual verify-log` walks it and reports the first break).
- **Sandboxing**: `[sandbox] wrapper` spawns every headless run under Anthropic's [`srt`][srt-url] (or any argv prefix) from the single spawn chokepoint; supervisor-owned, persisted per run, recorded in the meta ([example settings](docs/srt-settings.example.json)).
- **CI mode**: `ritual run dual-review --ci` writes JUnit XML to `.ritual/ci/` (confirmed critical/major findings = failures) and exits nonzero. Findings browsing is scriptable: `--json` everywhere, `ritual findings` exits 1 on confirmed criticals.
- **Standards-shaped telemetry**: `ritual export` emits OTLP-JSON spans with OTel **GenAI semconv** attributes for any OpenTelemetry collector; `--audit-trail` emits IETF draft-sharif agent-audit-trail records (RFC 8785-canonical, SHA-256 hash-chained JSONL).

### Ergonomics

- **Keyboard-first**: every action is rebindable (`[keys]` table), and the `:` command palette fuzzy-matches all actions, per-stage runs, dynamic retries, and your own `[commands]` templates (lazygit-style, with `{{branch}}`, `{{run_id}}`, `{{finding.file}}`, `{{finding.line}}`).
- **nvim remote control**: ritual drives your *running* nvim (no suspend, no nested editors): `o` opens the selected finding at file:line in it, `Q` pushes all located findings into its quickfix list (`:copen` included). Discovery: `$NVIM` → newest `$XDG_RUNTIME_DIR/nvim.*.0` socket → `nvim_server` config; the sidebar shows ` nvim ok` when one is found. Falls back to attached `$EDITOR` when nvim isn't running.
- **Bench**: `ritual bench plan-review --runs 5 [--golden expected.json]` scores repeated runs (findings, cross-confirmation, golden recall, cost, mean/σ spread, cost-per-hit) for model/prompt comparison.
- **Reports** (`ritual report [--pdf]`): one Markdown document per feature (pipeline state, spec, plan, findings + evidence snippets, runs, per-stage costs), redacted, pandoc-converted when available.
- **In-app guide**: tab `5` renders [the full guide & tips](docs/guide.md) inside the TUI.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- GETTING STARTED -->
## Getting Started

### Prerequisites

Verified against: Claude Code 2.1.205, Codex CLI 0.144.1 (July 2026).

- **Claude Code**, logged in: the multi-LLM skills install with `ritual init --skills` (or see the [playbook][playbook-url])
- **Codex CLI**, authenticated
  ```sh
  npm i -g @openai/codex && codex login
  ```
- **The codex MCP bridge**
  ```sh
  claude mcp add --scope user codex -- codex mcp-server
  ```
- Optional, feature-gated: `gitleaks` (secrets gate), `cargo-mutants` (mutation gate), `gh` (pr-comment), `coderabbit` (third reviewer), `@anthropic-ai/sandbox-runtime` (sandbox), `pandoc` (PDF reports)

### Installation

1. Clone and install the binary
   ```sh
   git clone <this repo> && cd ritual
   cargo install --path . --root ~/.local   # → ~/.local/bin/ritual
   ```
2. Install the vendored workbench (skills, agent, hooks) into `~/.claude`
   ```sh
   ritual init --skills
   ```
3. Verify everything in one shot
   ```sh
   ritual doctor
   ```

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- USAGE -->
## Usage

### Quick start

```sh
cd your-project
ritual init                      # .ritual/, stack-detected check.sh, CLAUDE.md, invariants.md
ritual new "My feature"          # or: ritual new "Big thing" --worktree feat/big
ritual                           # the dashboard
```

Scriptable: `status|findings|history|costs [--json]`, `run <stage> [--force] [--ci] [--model m]`, `mutants [--base ref]`, `secrets`, `lessons [--stdout]`, `report [--pdf]`, `repro <run-id>`, `verify-log`, `bench <stage> --runs N`, `skills diff`, `export [--out f] [--audit-trail]`.

_For the full manual, including a start-to-finish walkthrough of every feature, see the **[guide][guide-url]** (also rendered in-app on tab `5`)._

### Configuration

`.ritual/config.toml` or `~/.config/ritual/config.toml` (layered: defaults ← user ← project ← env ← flags):

```toml
theme = "eldritch"            # or tokyonight; --ascii for no Nerd Font
base_ref = "main"
budget_daily_usd = 5.0        # omit for no ceiling
budget_plan_review_usd = 5.0  # per-run --max-budget-usd caps
budget_dual_review_usd = 10.0
budget_finding_fix_usd = 1.0  # per F-apply batch run (answers ALL queued findings)
redaction = true
notifications = true
check_timeout_secs = 600      # hung build / dead HIL board can't wedge the loop
offline = false               # true = skip all cloud auth preflights
fallback_model = ""           # overload fallback for headless claude runs
# nvim_server = "/run/user/1000/nvim.12345.0"   # explicit socket (auto-discovered otherwise)

[keys]                        # rebind any action
check-full = "W"

[models]                      # per-stage model routing
plan-review = "opus"

[effort]                      # per-stage reasoning effort (plan-fix = the F fix runs)
plan = "xhigh"

[retry]                       # palette offers for failed stages
models = ["claude-opus-4-8"]

[mutants]                     # mutation-kill gate (ritual mutants)
cmd = "cargo mutants"
timeout_secs = 300

[secrets]                     # gitleaks gate (auto before dual-review)
enabled = true

[sandbox]                     # wrap headless runs (srt recipe in the guide)
enabled = false
wrapper = ""

[coderabbit]                  # third reviewer (cloud-backed, off by default)
enabled = false

[consensus]                   # third-model arbitration (off by default)
enabled = false

[commands]                    # palette-invocable templates
blame = "git log --oneline -3 -- {{finding.file}}"
```

### Recipes

**GitHub Actions gate**

```yaml
- name: dual review gate
  run: |
    ritual run dual-review --ci ${{ github.base_ref }}
- uses: mikepenz/action-junit-report@v4
  if: always()
  with: { report_paths: ".ritual/ci/*.xml" }
```

**Air-gapped / local models**: set `offline = true` and point the seam at any local agent CLI: `claude_cmd = "my-ollama-agent"` (or env `RITUAL_CLAUDE_CMD`). Everything that matters (archives, findings, reports, chain) is local files; nothing requires cloud auth.

**Embedded / hardware-in-the-loop**: use `templates/check-hil.sh` (build → flash → capture serial → assert) as your project's check.sh and set `check_timeout_secs` low enough that a dead board fails fast.

### Default keys

All rebindable via `[keys]`:

`enter` run stage / finding details · `s` chat: edit spec/plan · `:` palette · `S` settings editor · `j/k` move · `tab` `1-5` panes (`5` = in-app guide & tips) · `f/d/v` finding fix/dismiss(+reason)/show-resolved · `F` queue + apply claude answers (batch, gated) · `m` queue manual · `t` one-touch recommended triage · `u` revert applied batch · `/` filter list · `[` `]` features · `a` takeover · `o` open in nvim · `Q` findings → quickfix · `c/C` check fast/full · `x` cancel · `e` editor · `r` refresh · `g/G` scroll/follow · `?` help · `q` quit. In chat: `Ctrl+Z` undo · `Alt+Z` redo · `Ctrl+X` cancel · `Alt+Enter` newline

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- ROADMAP -->
## Roadmap

- [x] **v0.2**: redaction, budgets, reports, CI mode, repro bundles + hash chain, model routing, worktrees, daemonized runs, offline mode, bench, palette, OTLP export
- [x] **v0.3**: interactive spec/plan chat (`s`) + `/spec` skill + `ritual chat`
- [x] **v0.4**: `clean` with chain checkpoints, findings lifecycle (f/d/v + CI contract), chat undo/cancel/queue/highlight, vendored workbench, hard permission-scoping, `ps`/`attach`, `doctor`, consensus tier, `pr-comment`
- [x] **v0.5**: mutation + secrets gates, invariants constitution, review memory, findings snippets, `costs`, fallback + retry-with-model, sandbox wrapper, CodeRabbit third reviewer, OTel GenAI semconv + IETF audit-trail export, chat undo stack + reattach
- [x] **v0.6**: finding detail overlay (enter), claude-scoped plan fix (`F`) with a mechanical section gate + one-key revert (`u`), plan-step routing for `o`/`Q`, `[effort]` routing
- [x] **v0.7**: findings triage → batch-apply — answer every finding (⚑A claude / ⚑M manual / dismiss+reason), ONE run fixes them all against one plan snapshot with a union gate + per-finding ANSWERS verdicts, atomic batch revert, ⚓ anchor-lost markers, Q manual pass
- [x] **v0.8**: decoded failure reasons (budget knob / tool-lock denials / `ritual attach` hint), right-aligned triage state chips + recommendation ghosts, `t` one-touch recommended triage, prose resolutions preserved as reasons, subdir-launch root canonicalization fix
- [x] **v0.9**: in-TUI settings editor (`S`) — effective values + source tags over the practical knobs, comment-preserving project-config writes (toml_edit), transactional live-apply with byte-exact revert
- [ ] **Deferred**: steerable runs (Agent SDK), `ritual mcp-server`, SQLite/fuzzy history, chat fork-at-turn, container worktrees, OTLP receiver + in-TUI span waterfall, tree-sitter repo map

See the full [ROADMAP.md][roadmap-url] for the design rationale behind each item, and the non-goals.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- DESIGN NOTES -->
## Design Notes

- **Drift-tolerant parsing**: unknown stream-json events render dimmed (`Raw`), never crash; field names verified against live captures in `tests/fixtures/`. The same philosophy covers the cargo-mutants, gitleaks, and CodeRabbit adapters.
- **Zero-token testing**: `RITUAL_CLAUDE_CMD`/`RITUAL_CODEX_CMD` (and `RITUAL_GH_CMD`, `RITUAL_MUTANTS_CMD`, `RITUAL_GITLEAKS_CMD`, `RITUAL_CODERABBIT_CMD`) swap in fake CLIs from `tests/`; the entire pipeline, including daemon survival, gates, and the audit chain, is E2E-tested without an API call.
- **Accessibility**: state is never color-only (every status has a distinct glyph); `--ascii` replaces Nerd Font icons; `NO_COLOR` and piped output disable color.
- **Terminal safety**: one guard owns raw-mode transitions; a panic hook restores your shell; Ctrl-C in a child can't kill the TUI.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- DOCUMENTATION -->
## Documentation

- [Guide & tips][guide-url]: the full manual, also rendered in-app on tab `5`
- [Roadmap][roadmap-url]: what shipped per version, what's deferred, design rationale
- [Multi-LLM playbook][playbook-url]: the workflow's research grounding and setup reference
- [srt sandbox settings example](docs/srt-settings.example.json): starting config for sandboxed runs

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- DEVELOPMENT -->
## Development

`./check.sh` = fmt + clippy `-D warnings` + tests (294 across unit/CLI/snapshot suites, incl. proptest property tests); `bash tests/e2e_live.sh` drives the installed binary through 80 lifecycle checks token-free. Builds land in `/var/tmp/ritual-target` (see `.cargo/config.toml`). The repo dogfoods its own workflow (`CLAUDE.md`); `demo.tape` renders the README demo via [vhs][vhs-url].

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- LICENSE -->
## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. This is the conventional Rust
dual-license: it maximizes compatibility (MIT's simplicity, Apache-2.0's
explicit patent grant) so anyone can use `ritual` in any project.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.

<p align="right">(<a href="#readme-top">back to top</a>)</p>

<!-- ACKNOWLEDGMENTS -->
## Acknowledgments

- [ratatui][ratatui-url]: the TUI framework; [lazygit](https://github.com/jesseduffield/lazygit) and [k9s](https://k9scli.io/) set the bar for what a pro TUI owes its user
- [cargo-mutants](https://mutants.rs/) and [gitleaks](https://github.com/gitleaks/gitleaks): the engines behind the quality gates
- [Anthropic sandbox-runtime][srt-url]: supervisor-owned sandboxing done right
- Meta's [ACH](https://arxiv.org/abs/2501.12862) (mutation-guided test hardening) and the 2026 multi-agent evidence base: the research this workflow is shaped by
- [Best-README-Template](https://github.com/othneildrew/Best-README-Template) via [awesome-readme](https://github.com/matiassingers/awesome-readme): this README's skeleton

<p align="right">(<a href="#readme-top">back to top</a>)</p>

---

<div align="center">
<sub>ritual is a solo-power-user tool, built by dogfooding the workflow it drives. Every feature above was planned, cross-model-reviewed, and shipped through the pipeline itself.</sub>
</div>

<!-- MARKDOWN LINKS & IMAGES -->
[ci-shield]: https://github.com/2Tricky4u/Ritual/actions/workflows/ci.yml/badge.svg
[ci-url]: https://github.com/2Tricky4u/Ritual/actions/workflows/ci.yml
[version-shield]: https://img.shields.io/badge/version-0.9.0-9d7cd8?style=for-the-badge
[license-shield]: https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-a9b665?style=for-the-badge
[license-url]: #license
[tests-shield]: https://img.shields.io/badge/cargo_tests-379_passing-73daca?style=for-the-badge
[e2e-shield]: https://img.shields.io/badge/live_driver-80%2F80-7aa2f7?style=for-the-badge
[chain-shield]: https://img.shields.io/badge/audit-tamper--evident-e0af68?style=for-the-badge
[zero-token-shield]: https://img.shields.io/badge/testing-zero_tokens-bb9af7?style=for-the-badge
[rust-badge]: https://img.shields.io/badge/Rust_2024-000000?style=for-the-badge&logo=rust&logoColor=white
[rust-url]: https://www.rust-lang.org/
[ratatui-badge]: https://img.shields.io/badge/ratatui-0.30-1a1b26?style=for-the-badge
[ratatui-url]: https://ratatui.rs/
[tokio-badge]: https://img.shields.io/badge/tokio-async-2b303b?style=for-the-badge
[tokio-url]: https://tokio.rs/
[claude-badge]: https://img.shields.io/badge/Claude_Code-D97757?style=for-the-badge&logo=claude&logoColor=white
[claude-url]: https://claude.com/claude-code
[codex-badge]: https://img.shields.io/badge/Codex_CLI-412991?style=for-the-badge&logo=openai&logoColor=white
[codex-url]: https://github.com/openai/codex
[srt-url]: https://github.com/anthropic-experimental/sandbox-runtime
[vhs-url]: https://github.com/charmbracelet/vhs
[roadmap-url]: ROADMAP.md
[guide-url]: docs/guide.md
[playbook-url]: multi-llm-playbook.md
[tests-url]: tests/
[e2e-url]: tests/e2e_live.sh
