---
name: deps-audit
description: Dependency security + license audit for the current project (supply-chain complement to /security-review, which covers the code itself). Use for "audit dependencies", "any vulnerable packages?", "license check", or before a release.
argument-hint: "[optional: focus, e.g. 'licenses only']"
---

# Dependency audit

Three passes over the project's dependency tree: vulnerabilities, staleness, licenses. Tool output is the evidence; your job is triage, not alarm.

## Procedure

1. **Detect stacks** (a project can be several): Cargo.toml → Rust; package.json → Node; pyproject.toml/requirements → Python.
2. **Vulnerabilities** (run what exists; note what's missing rather than failing):
   - Rust: `cargo audit` (offer `cargo install cargo-audit` if absent)
   - Node: `npm audit --json` (or pnpm/yarn equivalent per lockfile)
   - Python: `pip-audit` (or `uvx pip-audit`)
3. **Staleness**: `cargo outdated -R` / `npm outdated` / `pip list --outdated` - report major-version-behind and unmaintained (no release >2y) packages only; minor lag is noise.
4. **Licenses**: `cargo license` / `npx license-checker --summary` / `pip-licenses`. Flag copyleft (GPL/AGPL/SSPL) and unknown/missing licenses against the project's own license.
5. **Triage each vulnerability**: is the vulnerable code path actually reachable here? Severity × reachability → verdict. An unreachable critical is still listed, marked "not reachable in this usage".
6. **Report** (most severe first): package, version, issue/CVE, severity, reachability verdict, fix (upgrade target or mitigation). End with the 1-3 actions actually worth taking now.
7. If `.ritual/` exists, ALSO write `${RITUAL_FINDINGS_DIR:-.ritual/findings}/<UTC yyyymmddTHHMMSSZ>-deps-audit.json` - NEW file, existing findings schema (`ritual_findings: 1`, `stage: "deps-audit"`, findings with severity critical|major|minor mapped from CVSS, `sources: ["tooling"]`, `verdict: "confirmed"`, file = manifest path).

## Guardrails
- Never auto-upgrade dependencies - propose the upgrade set; the user applies (or asks you to).
- Missing audit tooling is a finding in itself (minor) - name the install command.
- No breathless CVE-counting: 40 findings in dev-only tooling ≠ 1 reachable RCE in prod code; say which is which.
