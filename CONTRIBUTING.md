# Contributing to ritual

Thanks for your interest. ritual is a solo-power-user TUI built by dogfooding
the multi-LLM workflow it drives, so the contribution bar is simple: **every
change leaves `./check.sh` green and lands as one coherent commit.**

## Prerequisites

- **Rust 2024** (stable, ≥ 1.85). `rustup component add rustfmt clippy`.
- `claude` and `codex` CLIs are only needed to run *live* agent stages. The
  entire test suite runs without them (see below), so you don't need API
  keys to develop or to pass CI.

## The one rule: `./check.sh`

```sh
./check.sh fast   # fmt --check + clippy -D warnings   (after every edit)
./check.sh        # the above + cargo test              (before every commit)
```

This is exactly what CI runs. `clippy` is `-D warnings`, so lints are errors.
`cargo fmt` before committing. Snapshot tests use [insta]; when a UI change is
intentional, review the diff and re-approve deliberately - don't blanket-accept.

## Testing is token-free

Unit, CLI, and snapshot tests are pure. The live lifecycle driver
(`bash tests/e2e_live.sh`, ~80 checks) exercises the installed binary through
a fake agent via the `RITUAL_CLAUDE_CMD` seam - no network, no tokens. Please
add tests for new behavior; this repo writes them **red-first from the spec**
before the implementation.

## How changes are made here

- **Nontrivial features**: plan first, then a cross-model plan review before
  writing code.
- **Implementation**: tests designed from the spec, written red, then the code.
- **Before a significant commit**: a two-model diff review; only confirmed
  findings get fixed.

You don't need the agent tooling to contribute - but do keep changes small,
one logical change per commit, with a clear message.

## Code conventions

- **Parsers stay drift-tolerant**: unknown JSON events become `AgentEvent::Raw`,
  never hard errors. Raw agent output is archived verbatim *before* parsing.
- **All TUI state flows through the single `AppMsg` channel**; render never
  blocks on I/O.
- **Terminal enter/leave only via the `term.rs` guard** - never call crossterm
  enable/disable elsewhere.
- **Theme colors only via `theme.rs` semantic names**, never raw hex in UI code.
- Config structs are `deny_unknown_fields`; add a field to `FileConfig` *and*
  its flattened `Config` when introducing a knob.

## Reporting bugs & proposing features

Open an issue - the bug template asks for `ritual doctor` output and
`ritual --version`, which resolve most reports quickly. For anything security-
related, see [SECURITY.md](SECURITY.md) instead of a public issue.

## More context

- [Guide](docs/guide.md) - full feature reference
- [Roadmap](ROADMAP.md) - what's built and why, plus the non-goals
- [Multi-LLM playbook](multi-llm-playbook.md) - the workflow's research grounding

[insta]: https://insta.rs/
