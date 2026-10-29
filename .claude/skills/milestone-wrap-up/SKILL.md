---
name: milestone-wrap-up
description: Finish a Couch Co-App milestone or polish step the way the author works — run the check skills its areas need on Windows and Linux, write its results page, keep the docs, CLAUDE.md and the skills true, bump the version, have a fresh review agent go over the change and verify its findings, then commit only when asked and hand the author the PR text; and after the merge, tidy up. Use when a milestone's code is done, when the author says "wrap up", before asking to commit, and after a PR is merged.
---

# Wrapping up a milestone

The author tests by hand, merges the PRs and pushes; everything else is ours, in this order. Nothing is committed until they say so.

## 1. The checks its areas need

Every change: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` on Windows, and on Linux through the `linux-checks` skill (CI runs Linux too). Then by area:

| Touched | Run |
|---|---|
| The wire, IPC | the `protocol-change` skill |
| Rooms, admission, the room actor, remote play, what the engine refuses | the `room-security-checks` skill (rooms and sessions three times) |
| Input, teardown, sessions, the stream window's launch | the `input-safety-checks` skill, its live checks too (ask the author first: windows take the focus) |
| The video path, rate control | the `rate-control-gates` skill |
| Sound | the `audio-checks` skill |
| Performance claims | the `performance-checks` skill |
| Unsafe Windows, COM, D3D, SDL | the `windows-ffi-pitfalls` skill |
| The UI | `npx tsc --noEmit`, `npx vitest run`, `npm run shots` (no problems, nothing jumps); a new state gets a preview scenario |
| Anything the author will try | the `app-manual-run` skill: what to ask them to do, and what one PC can't show |

A scenario that fails once in ten runs is a race to understand, not noise.

## 2. The docs

- **`docs/<id>-results.md`:** Why, What changed, Checked (with the numbers), Found while checking, The review, Still to come. The author's own choices are named as theirs, with the date.
- **`docs/architecture.md`:** the milestone's row (done, its results, its version), and the version line under "Installing": what this version carries, and whether it talks to the last one.
- **The version:** `cargo xtask bump patch` for fixes and small features, `minor` for a milestone or a new wire revision (with its `REVISIONS` row). Once per PR.
- **CLAUDE.md:** new rules and commands where they belong (a rule that guards safety goes beside its kin). `docs/development.md` and `docs/self-hosting.md` stay in step with its commands. The README's top is the author's, above its "human-made section" marker.
- **ADRs:** what was settled while building, under the status.
- **Stale claims:** search the docs and skills for what the change made untrue (the old behaviour's words, the files it moved) and fix them; a skill's tables of guards gain the new rule.
- **Skills:** ask the author whether a new one would help; write it under `.claude/skills/`, with any scripts it needs in the repo (the scratchpad doesn't survive the session).

## 3. The review

- **A fresh review agent** (a general-purpose subagent) over the uncommitted diff. Its prompt names: the change and its intent, the rules from CLAUDE.md it touches, what to try to break, and "report only, with file:line, why and a fix, ranked; say plainly if nothing".
- **Verify each finding** against the code before fixing; fix what holds; re-run the checks the fixes touch (on both systems if the code is shared).
- **High-severity findings get a second, fresh review** after the fixes (the author asked for that in M22).
- **Its results go in the results page,** under The review.

## 4. Commit, when the author says

- On a branch (never `main`): `git checkout -b <id>-<name>`.
- The message in a file, `git commit -F <file>`: what and why, what was checked, the review, the `Co-Authored-By` line the session gives.
- Generated files that changed only in line endings drop out on staging; anything else unexpected in `git status` is looked at first.
- Hand the author the PR description, ready to paste, ending with the attribution line the session gives. They push and open the PR (no `gh` here); CI's Linux job runs on it, and a failure's log comes from them.

## 5. After the merge

- `git checkout main && git pull --ff-only`, delete the merged branch.
- An installer for friends when the author wants one: the `release-installer` skill.
- My memory notes: where things stand, what is next.
