---
name: release-flow
description: Standardize Fontbrew releases from release gate through published artifact verification. Use when cutting, publishing, tagging, shipping, or preparing a Fontbrew release, or when changelog, version, release notes, tags, and release automation must stay in sync.
---

# Release Flow

Use this as Fontbrew's release gate: do not push a release tag until version files, changelog, build, tests, and release notes are coherent.

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

5. Commit and push release prep.
   - Stage only release-related files.
   - Use the repository's conventional-style commit subject.
   - Push `main` and watch required CI until it succeeds.
   - Completion criterion: `origin/main` contains the release prep commit and CI is green.

6. Tag and publish.
   - Create an annotated tag matching the Cargo version, such as `v0.0.1`.
   - Push the tag only after the release gate and CI pass.
   - Watch the Release workflow through completion.
   - Completion criterion: the GitHub Release exists, is not draft unless requested, points at the intended tag, and contains expected assets.

7. Verify as a user.
   - Query the published release and asset names.
   - Check README install instructions against the published assets.
   - When practical, install from the release and run `fontbrew --help`.
   - Completion criterion: a user following README can discover and install the release.

8. Report precisely.
   - Include the commit, tag, release URL, CI status, and any residual warnings.
   - Mention skipped checks explicitly.
   - Completion criterion: the user can tell exactly what shipped and what remains.
