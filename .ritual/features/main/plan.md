# Plan: `ritual clean` subcommand

*(revised after /plan-review, 2026-07-11)*

## Context
`.ritual/runs/` grows without bound - up to four files per run: `.jsonl`,
`.meta.json`, `.request.json`, `.status`. Add a `clean` subcommand that prunes
old run artifacts safely.

## Steps
1. cli.rs: `Clean { keep: usize (default 50), dry_run: bool, allow_chain_break: bool }`.
2. Enumerate by FILENAME, not `history::load_all`: scan `.ritual/runs/`, strip
   the four known suffixes, group files by run-id prefix. Rationale: `load_all`
   silently skips malformed metas, so those runs (and orphan sidecars from
   crashed launches) would be invisible to cleanup and the directory would keep
   growing. Run ids for deletion come ONLY from discovered filenames, never
   from `RunMeta.run_id` (untrusted JSON - `"run_id": "../../x"` must not
   escape the runs dir); assert every deletion target resolves inside
   `runs_dir`.
3. Classify each group with `runner::run_state()`:
   - `Running` (live pid in `.status`) → never prune, regardless of age.
   - `Finished` (parseable meta) → subject to retention policy.
   - `Vanished` (no meta, dead/absent pid) → prunable as garbage.
4. Retention: keep the newest `keep` finished runs (run-id sort; ids are
   timestamp-prefixed). Protection is ADDITIVE: any run referenced by a feature
   stage in `state.json` is always kept and does not consume a keep slot, so
   total retained may exceed `keep`. Missing/unparseable `state.json` →
   protect nothing, but print a notice. Document these semantics in the CLI
   help text.
5. Chained runs: if any prune candidate has `meta.chain`, refuse (with an
   explanation that `verify-log` would break PERMANENTLY - the oldest retained
   link no longer chains from GENESIS, and future runs chain onto the retained
   head) unless `--allow-chain-break` is passed. `--dry-run` exercises the same
   refusal/warning path.
6. Deletion order per group: `.meta.json` FIRST, then `.request.json`,
   `.status`, `.jsonl`. A partial failure then leaves an orphan group the next
   `clean` collects, and `verify-log` never observes meta-without-archive.
   Continue past individual failures, report each one, exit nonzero if
   anything failed.
7. `--dry-run` prints what would be deleted / kept / skipped and why; no
   filesystem changes.
8. Tests:
   - keep-count respected; `--keep 0`
   - stage-referenced runs protected both inside and outside the keep window;
     duplicate references across features
   - `--dry-run` mutates nothing (snapshot dir before/after)
   - group with malformed `.meta.json` still pruned; orphan
     `.jsonl`/`.request.json`/`.status` files pruned
   - group with live `.status` (test uses own pid) never pruned; dead-pid
     `Vanished` group pruned
   - chained run: refused without `--allow-chain-break`, pruned with it
   - `run_id` path-escape in a meta file cannot delete outside `runs_dir`
   - partial deletion failure: continues, reports, exits nonzero (unix
     permissions trick)
   - missing `state.json` handled with notice

## Risks / open decision
- Deleting any chained run breaks `verify-log` permanently (not just for the
  deleted runs). Default behavior refuses; `--allow-chain-break` overrides
  after an explicit warning. A checkpoint/rebased-chain design for
  `verify_log` would lift this limitation but is out of scope for this
  feature - user to decide whether to pursue it separately.
