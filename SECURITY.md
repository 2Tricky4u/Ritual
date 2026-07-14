# Security Policy

## Supported versions

ritual ships from `main`; the **latest tagged release** receives security
fixes. Older tags are not maintained - upgrade to the newest version.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately through GitHub's
[Private Vulnerability Reporting](https://github.com/2Tricky4u/Ritual/security/advisories/new)
(the "Report a vulnerability" button on the Security tab). If that is
unavailable, email **xaga.ogay@gmail.com** with details and, if possible, a
reproduction.

Please include:

- affected version (`ritual --version`) and platform,
- a description of the issue and its impact,
- steps to reproduce or a proof of concept.

You can expect an acknowledgement within a few days. Once a fix is available,
a patched release is tagged and the advisory published; credit is given to
reporters who want it.

## Scope notes

ritual runs coding agents and records their raw output under `.ritual/runs/`.
A few things worth knowing when assessing risk:

- **Redaction** scrubs secret-shaped strings (keys, tokens, PEM blocks) from
  archives, streams, and reports before they are written (`redaction = true`
  by default). It is a safety net, not a guarantee - review artifacts before
  sharing them.
- **The run archive is tamper-evident**: each run's metadata is hash-chained,
  and `ritual verify-log` walks the chain. Treat a failed verification as a
  signal worth investigating.
- **Agent commands are configurable seams** (`claude_cmd`, `codex_cmd`, the
  `[sandbox]` wrapper). Point them only at binaries you trust; a project-level
  `.ritual/config.toml` can change what gets executed.
