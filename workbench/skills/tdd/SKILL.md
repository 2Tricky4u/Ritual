---
name: tdd
description: Test-first implementation loop with cross-model test design. Use when starting implementation of an approved plan or a nontrivial feature — OpenAI Codex designs the test list independently from the spec, Claude writes failing tests first, then implements until green.
argument-hint: "[feature or plan to implement]"
---

# Cross-model test-driven implementation

Tests-first is the single biggest quality lever in agentic coding, and tests designed by a model that is NOT the implementer are measurably more objective (the implementer's tests inherit its own misunderstandings). You implement; Codex designs tests.

## Procedure

1. **Extract the behavioral contract.** From the approved plan/spec, write the contract: inputs, outputs, invariants, edge cases, failure modes. This is WHAT, never HOW — do not include your intended implementation approach. If `${RITUAL_INVARIANTS_FILE:-.ritual/invariants.md}` exists and has bullets, fold in every invariant the change touches — each must yield at least one test.

2. **Codex designs the test list.** Call the `codex` MCP tool with ONLY the behavioral contract (not your implementation plan — the test designer must not inherit your bias):

   > Design a test list for this specification. Cover: happy paths, boundary/edge cases, failure modes and error handling, concurrency/ordering issues if applicable, and candidates for property-based tests. For each test: a name, given/when/then in one line each, and what real bug it could catch. Flag anything in the spec that is untestable as written.

3. **Merge and harden.** Combine Codex's list with your own additions. Discard tautological tests (tests that restate the implementation) and tests of framework behavior. If Codex flagged untestable spec points, surface them to the user before proceeding.

4. **Write the tests — red first.** Implement the merged list in the project's test framework. Run them: **every new test must fail** before implementation exists. A new test that already passes is not testing the new behavior — rewrite or delete it. Commit the failing tests if in a git repo.

5. **Implement until green.** Write the implementation. The `check.sh` hook gives you lint/typecheck feedback on every edit; run the test suite as you go. Iterate on the code, not the tests.

6. **The tests are the contract.** Never weaken, skip, or delete a failing test to make the suite pass without explicitly flagging it to the user with the reason. If a test turns out to be wrong, say so and show the correction.

7. **Finish.** Run the full `./check.sh` (or the project's full suite). Report: the test list vs. the original contract (what's covered, what isn't), and final suite status.
