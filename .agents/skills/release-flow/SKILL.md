---
name: release-flow
description: Standardize Fontbrew releases through the local release gate, release prep commit, annotated tag, and a single push of both refs. Use when cutting, publishing, tagging, shipping, or preparing a Fontbrew release, or when changelog, version, release notes, tags, and release automation must stay in sync without waiting for asynchronous CI or Release completion.
---

# Release Flow

Use this as Fontbrew's release gate: do not push a release tag until version files, changelog, build, tests, and release notes are coherent. End the flow once the release prep commit and annotated tag are pushed in a single remote push; do not poll CI, Release workflow runs, GitHub Release status, or published assets unless the user explicitly asks.

## Steps

1. Inspect release state.
   - Read `git status`, current branch, remotes, latest tags, existing releases, and Cargo workspace version.
   - Check `.github/workflows/ci.yml` and `.github/workflows/release.yml` so the local gate matches automation.
   - Completion criterion: the target version, branch, remote, prior tag, and release workflow are known.

2. Protect local work.
   - Separate release changes from unrelated user edits.
   - Do not overwrite, revert, or stage unrelated changes without explicit user direction.
   - If the intended tag already exists locally or remotely, stop unless the user explicitly approves the exact retag or replacement plan.
   - Completion criterion: the release diff is isolated and safe to stage.

3. Align version and changelog.
   - Update `Cargo.toml`, `Cargo.lock`, and any other version source that affects packaged output.
   - Add or update `CHANGELOG.md` with a dated entry for the release.
   - Write user-facing bullets: added, changed, fixed, removed, security, and breaking changes where applicable.
   - Keep release notes consistent with the changelog.
   - Completion criterion: version sources agree, the changelog has the target version, and release notes can be derived from it.

4. Run the release gate locally.
   - Run `cargo fmt --all -- --check`.
   - Run `cargo clippy --workspace --all-targets -- -D warnings`.
   - Run `cargo test --workspace`.
   - Run `cargo build --release -p fontbrew-cli` when binary packaging changed or before a release tag.
   - Completion criterion: every required command passes, or a specific external blocker is recorded with its command and output.

5. Commit and tag locally.
   - Stage only release-related files.
   - Use the repository's conventional-style commit subject.
   - Create an annotated tag matching the Cargo version, such as `v0.0.1`.
   - Do not push yet.
   - Completion criterion: the release prep commit exists locally on `main` and the annotated tag points at that commit.

6. Push release refs once.
   - Push `main` and the tag in one command after the local release gate passes, such as `git push --atomic origin main v0.0.1`.
   - If `--atomic` is unsupported or the combined push fails, stop and record the exact blocker instead of splitting the branch and tag into separate pushes without explicit user direction.
   - Do not wait for remote CI, the Release workflow, GitHub Release, or asset publication unless the user explicitly asks.
   - Completion criterion: both `origin/main` and the intended remote tag contain the release refs, or the exact push blocker is recorded.

7. Report precisely.
   - Include the pushed commit, pushed tag, local release gate results, and any residual warnings.
   - Mention the single push command used or why it failed.
   - State that CI and Release automation were left to run asynchronously when not explicitly checked.
   - Mention skipped checks explicitly.
   - Completion criterion: the user can tell exactly which refs were pushed and what remote work remains asynchronous.
