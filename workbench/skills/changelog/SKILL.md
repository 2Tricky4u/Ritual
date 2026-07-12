---
name: changelog
description: Generate Keep-a-Changelog release notes from a git range. Use for "write the changelog", "release notes for vX", or before tagging a release.
argument-hint: "[range or tag, e.g. v0.1..HEAD]"
---

# Changelog

Written for USERS of the software, not its developers: every line answers "what does this mean for me?"

## Procedure

1. Determine the range: argument, else `$(git describe --tags --abbrev=0)..HEAD`, else ask.
2. Read `git log <range> --oneline` and, for anything unclear, the touched files — the entry describes the effect, not the commit message.
3. Classify into Keep-a-Changelog sections, in this order: **Breaking changes** (first, prominent), Added, Changed, Fixed, Deprecated, Removed, Security.
4. Drop internal noise: chore/refactor/test/CI commits appear only when they change observable behavior (perf, compatibility, install).
5. Rewrite developer-speak into user-facing wording ("`fix(runner): tolerate EPIPE`" → "Fixed a crash when an agent process exited mid-stream").
6. Output the section for this release (`## [X.Y.Z] - YYYY-MM-DD`); if `CHANGELOG.md` exists, insert it in place (preserving the file's existing style), else offer to create the file.

## Guardrails
- Never pad: a two-line release is a two-line release.
- Uncertain classification (breaking or not?) → check the actual code change, don't guess.
- Version number comes from the user or the tag — never invent the next version.
