# Plan: `ritual clean` subcommand

## Context
`.ritual/runs/` grows without bound (one .jsonl + .meta.json per run). Add a
`clean` subcommand that prunes old run artifacts safely.

## Steps
1. Add `Clean { keep: usize (default 50), dry_run: bool }` to cli.rs.
2. In main.rs: list runs (history::load_all, newest first), keep the newest
   `keep`, delete the .jsonl + .meta.json of the rest.
3. Never delete runs referenced by any feature stage in state.json.
4. `--dry-run` prints what would be deleted.
5. Tests: prune respects keep-count and stage references.

## Risks
- Deleting a chained run breaks `verify-log` — acceptable, documented.
