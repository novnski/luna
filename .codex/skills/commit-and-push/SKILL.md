---
name: commit-and-push
description: Finish a completed Luna feature by cleaning up task artifacts, committing all feature changes, merging its branch into main when needed, and pushing verified main to origin. Use at the end of feature work in this repository.
---

# Commit and push a completed feature

The user's standing preference for Luna is to finish completed features on `origin/main`. Run this workflow after the feature, its required checks, and the LUNA Dev handoff are complete. A current user instruction to stop before a commit, merge, or push takes precedence.

1. Review the feature diff for accidental debug code, temporary files, and formatting issues. Remove only task-created temporary files and disposable QA resources, following `AGENTS.md`. Preserve user data, build caches, active apps, and unrelated work.
2. Inspect the current branch, worktrees, `git status`, staged and unstaged diffs, untracked files, and the commits that would enter `main`. Include **every** feature change, including relevant documentation and assets. Exclude unrelated pre-existing changes, secrets, `.env` files, and generated debris. Stage reviewed paths explicitly; do not use a blanket `git add -A`.
3. Run `git diff --check` and any required project checks that have not yet run. Commit the complete feature with a descriptive message. Inspect status, the committed diff, and `git log --oneline -3`.
4. Fetch `origin/main`. If work was done on another branch, merge the feature into local `main` (using its existing worktree if present). Confirm the branch contains no unrelated commits before merging. Resolve ordinary conflicts without discarding anyone's work; stop and explain any conflict or branch content whose intended resolution is unclear. If work was done on `main`, incorporate any remote updates without rewriting or dropping others' commits.
5. Push local `main` to `origin` over the repository's SSH remote. Never force-push or bypass a failing hook or protected-branch rule. Verify that `git ls-remote origin refs/heads/main` matches local `main`, then inspect final status and relevant diff. Report the commit hash, remote verification, and any remaining unrelated changes.

If a build, verification, merge, or push fails, report the exact blocker and the last confirmed state. Do not claim the feature reached `main` until the remote readback proves it.
