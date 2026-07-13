# pr-comment validation scratch

Throwaway file for validating `ritual pr-comment --inline` against a real GitHub PR.
This branch (`test/pr-comment-validation`) and its PR are disposable — safe to delete.

## Synthetic anchors

- anchor A — a synthetic "confirmed" dual-review finding is anchored to this line.
- anchor B — a second synthetic finding is anchored to this line to prove multiple inline posts.

Clean up with: `gh pr close <N> --delete-branch && git checkout main`.
