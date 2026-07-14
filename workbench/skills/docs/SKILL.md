---
name: docs
description: Generate or refresh project documentation (README sections, module docs, usage guides) from the actual code. Use for "document this", "update the README", "write usage docs".
argument-hint: "[what to document - file, module, or 'readme']"
---

# Documentation

Docs describe what the code DOES, verified by running it - never what it's hoped to do.

## Procedure

1. Read the code being documented AND its tests (tests are the honest spec). Note public surface, defaults, error behavior.
2. **Execute every example before writing it down.** A command in the docs must have been run; its output shown must be real (trim, don't fabricate). If something can't be run here, mark it explicitly as unverified.
3. Structure by reader intent:
   - README: what it is (one sentence) → install → smallest working example → common tasks → configuration → troubleshooting.
   - Module/API docs: purpose, contract (inputs/outputs/errors), one example, gotchas.
4. Match the project's existing voice and formatting (heading depth, code-fence styles, tables vs lists).
5. Update in place; for a README, keep stable section anchors so links survive.

## Guardrails
- No marketing adjectives (blazing, powerful, seamless, beautiful). State capabilities; let readers judge.
- Don't document internals that would pin implementation details users shouldn't rely on.
- Stale-doc sweep: when changing one section, scan the rest for statements the code no longer supports; fix or flag them.
- Never delete a caveats/known-issues section without evidence the issue is gone.
