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
5. Chained runs: prune only as a contiguous prefix of the chain in LINKAGE
   order (finish order - under parallel runs that is NOT run-id order),
   covered by a tamper-evident `Checkpoint` written BEFORE any deletion.
   Verify -> checkpoint -> prefix selection runs under the chain lock so a
   finishing daemon cannot append between the two. `verify-log` treats the
   checkpoint as a rolling genesis, so the log stays intact forever - no
   override flag exists or is needed. Chained candidates outside the prefix
   are kept (`ChainContinuity`). If the chain is already broken, refuse
   chained pruning with a notice - never checkpoint over a broken chain.
   `--dry-run` reports the would-be checkpoint without writing it.
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
   - chained runs: only a contiguous linkage-order prefix of candidates is
     pruned, behind a checkpoint written before deletion; a keeper ends the
     prefix (`ChainContinuity`); already-broken chain refuses chained
     pruning; `--dry-run` reports the checkpoint without writing it
   - `run_id` path-escape in a meta file cannot delete outside `runs_dir`
   - partial deletion failure: continues, reports, exits nonzero (unix
     permissions trick)
   - missing `state.json` handled with notice

## Deliverables
- [x] D1: `Clean` CLI variant with `keep` (default 50) and `dry_run` (no chain-break override - chained pruning is checkpoint-based, per step 5) - accept: `ritual clean --help` shows both flags and documents the additive-protection retention semantics (protected runs - state-referenced, today's, live - don't consume keep slots) - route: cli.rs
- [x] D2: Filename-based run enumeration with path-escape guard - accept: run-ids come only from scanned filenames (never `RunMeta.run_id`); a meta with `"run_id": "../../x"` cannot cause deletion outside `runs_dir` - route: clean module, per step 2
- [x] D3: Run-state classification of each group - accept: `Running` (live pid) never pruned regardless of age; `Finished` subject to retention; `Vanished` (no meta, dead/absent pid) prunable as garbage - route: `runner::run_state()`, per step 3
- [x] D4: Additive stage-reference retention - accept: newest `keep` finished runs kept; runs referenced by a feature stage in `state.json` always kept without consuming a keep slot; missing/unparseable `state.json` protects nothing but prints a notice - route: retention logic, per step 4
- [x] D5: Checkpoint-based chained pruning - accept: chained candidates prune only as a contiguous linkage-order prefix covered by a tamper-evident `Checkpoint` written BEFORE any deletion under the chain lock; `verify-log` stays intact (checkpoint acts as rolling genesis); an already-broken chain refuses chained pruning with a notice; `--dry-run` reports the would-be checkpoint without writing it - route: per step 5
- [x] D6: Meta-first deletion order with failure tolerance - accept: per group deletes `.meta.json`, `.request.json`, `.status`, `.jsonl` in that order; continues past individual failures, reports each, exits nonzero if any failed - route: per step 6
- [x] D7: `--dry-run` mode - accept: prints what would be deleted / kept / skipped and why; snapshot of the runs dir before/after is identical - route: per step 7
- [x] D8: Test suite covering step 8's list - accept: tests exist and pass for keep-count & `--keep 0`, stage-reference protection (in/out of window, duplicate refs), dry-run immutability, malformed-meta and orphan-sidecar pruning, live-pid protection & dead-pid pruning, checkpoint-based chained pruning (contiguous-prefix selection, checkpoint before deletion, broken-chain refusal, dry-run checkpoint reporting), path-escape guard, partial-failure exit code, missing-`state.json` notice - route: tests, per step 8

## Risks / open decision
- Deleting any chained run breaks `verify-log` permanently (not just for the
  deleted runs). Default behavior refuses; `--allow-chain-break` overrides
  after an explicit warning. A checkpoint/rebased-chain design for
  `verify_log` would lift this limitation but is out of scope for this
  feature - user to decide whether to pursue it separately.
