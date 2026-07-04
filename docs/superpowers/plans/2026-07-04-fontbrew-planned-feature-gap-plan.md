# Fontbrew Planned Feature Gap Plan

Date: 2026-07-04

## Context

The handoff states that the MVP implementation plan has no known remaining MVP tasks. The repository also has final MVP verification notes showing that the main local archive, registry/GitHub, provider, list/info/search/outdated/update/remove, JSON, and stdout/stderr paths passed verification.

This plan is therefore not a second MVP plan. It covers planned product behavior that is documented in `docs/product_spec.md`, `docs/implementation-design.md`, ADRs, and the original MVP backlog, but is either not implemented, only partially wired, or exposed through user-facing flags/config before the behavior is complete.

## Source Documents Checked

- Product spec: `docs/product_spec.md`
- Implementation design: `docs/implementation-design.md`
- MVP backlog: `docs/issues/fontbrew-mvp-issues.md`
- MVP implementation plan: `docs/superpowers/plans/2026-07-04-fontbrew-mvp.md`
- Existing gap research: `docs/research/2026-07-04-unimplemented-product-features.md`
- ADRs: `docs/adr/*.md`
- Current code under `crates/fontbrew-core` and `crates/fontbrew-cli`

Background material read but not treated as durable source of truth:

- Temporary handoff: `/var/folders/xh/gvfmyldx403f4fl4wjgmbbjw0000gn/T/fontbrew-handoff-2026-07-04-final.md`

## Non-Goals

Do not use this plan to add scope that the product spec explicitly excludes from the MVP:

- GUI
- Linux or Windows activation
- project-level dependency files or lockfiles
- rollback or retained historical versions after successful update
- download archive cache
- explicit `activate` / `deactivate` workflow
- arbitrary GitHub repository search
- system font management or adoption of manually installed fonts
- commercial font license management
- font preview, tagging, collection, or design features

## Priority Summary

1. Remove user-facing refresh/offline planning and make network-backed commands refresh registry metadata by default.
2. Keep provider metadata snapshots internal and metadata-only.
3. Make registry recipe family/package-boundary rules real during install and update identity validation.
4. Either fully implement copy activation or stop accepting `install.activation_strategy = copy`.
5. Add local archive `--id` for unsafe or non-normalizable family names.
6. Add registry schema/version details to `registry status`.

## Task 1: Make Metadata Refresh Automatic

### Why

The product spec no longer exposes manual refresh/offline user modes. Commands that need registry/provider metadata should refresh it automatically, while snapshots remain an internal optimization and local state record.

The current config exposes `registry.auto_update` and `network.metadata_ttl_hours`, and the current CLI/core still contain refresh/offline request fields. The next plan should remove those product commitments rather than adding more flag coverage.

### Implementation

- Remove user-facing refresh/offline flags from current product documentation and future planning.
- Remove or deprecate request fields whose only purpose is user-facing refresh/offline behavior.
- Before registry-backed `search` and short-name `install`, refresh the registry snapshot automatically.
- Before provider-backed `search` and provider install, refresh provider metadata automatically.
- Before `outdated`, use the package update source directly and refresh metadata as needed without a user flag.
- Keep local archive install independent from registry refresh.
- Keep provider metadata snapshots metadata-only; do not add downloaded font binaries.

### Files

- `crates/fontbrew-core/src/app/mod.rs`
- `crates/fontbrew-core/src/config/mod.rs`
- `crates/fontbrew-core/src/providers/mod.rs`
- `crates/fontbrew-core/src/registry/mod.rs`
- `crates/fontbrew-core/src/update/mod.rs`
- `crates/fontbrew-cli/src/cli/mod.rs`
- reporter tests and core provider/search/outdated tests

### Acceptance Criteria

- Product docs and this plan contain no manual refresh/offline command examples or planned flags.
- Registry-backed `search` and short-name `install` refresh the registry snapshot by default.
- Provider-backed `search` and provider install refresh provider metadata by default.
- `outdated` checks update sources without requiring a refresh/offline user mode.
- Human and JSON output remain clean and structured.

## Task 2: Remove Refresh/Offline CLI Surface

### Why

The current CLI and core still expose some refresh/offline fields from the older plan. The product direction is now simpler: network-backed commands update metadata by default.

This task should be implemented together with Task 1, but it is called out separately because it changes public CLI shape and request models.

### Implementation

- Remove manual refresh/offline flags from CLI command definitions.
- Remove corresponding fields from request models when they no longer have internal meaning.
- Update CLI help tests, JSON request serialization tests, and smoke docs.
- Keep any internal snapshot reuse behavior private to core.

### Acceptance Criteria

- No current CLI help advertises manual refresh/offline flags.
- No future plan task asks to add refresh/offline flags.
- Commands that need registry/provider metadata refresh it by default.
- Tests cover the default refresh behavior without user flags.

## Task 3: Make Registry Recipe Package Boundaries Enforceable

### Why

The product spec and ADR 0001 say package boundaries default to family name, but registry recipes can override that boundary when a source publishes multiple user-facing variants in one archive or when multiple related families should be installed as one package.

Current registry recipes carry `families`, asset selection, and format preference. During registry install, the recipe package ID and asset/format rules are used, but recipe families are not used to filter selected files or validate the package boundary. The parser collects all families from the selected archive and records them all. When there is no package ID hint, the implementation chooses the first sorted family as the package ID instead of failing on multiple package families.

### Recommended Shape

Keep the schema conservative:

- Treat top-level `families` as the expected package identity and default install family set for registry packages.
- Add optional `install.includeFamilies` and `install.excludeFamilies` only when a recipe must select a subset from a larger archive.
- Match family names exactly after normalizing obvious whitespace/case, not by fuzzy search.
- Do not add broad glob/fuzzy family selectors until a real registry entry needs them.

### Implementation

- Extend `RegistryInstallOptions` with family boundary fields.
- Extend `RegistryPackageRecipe` with a resolved family boundary model.
- Pass recipe family boundary into `prepare_github_release_archive` and `parse_staged_font_files`.
- For registry installs:
  - filter selected font files/faces to recipe-included families;
  - fail if expected families are missing;
  - fail if unexpected families remain unless explicitly allowed by the recipe model.
- For direct GitHub/local installs:
  - fail when multiple package families are discovered and no recipe/user selection explains the boundary.
- Update `validate_update_identity` so registry-managed packages validate against recipe family rules, not only the previous manifest family list.
- Keep provider packages as single provider family unless provider data explicitly says otherwise.

### Files

- `crates/fontbrew-core/src/registry/mod.rs`
- `crates/fontbrew-core/src/install/mod.rs`
- `crates/fontbrew-core/src/update/mod.rs`
- `crates/fontbrew-core/tests/registry.rs`
- `crates/fontbrew-core/tests/github_install.rs`
- `crates/fontbrew-core/tests/update.rs`

### Acceptance Criteria

- A registry recipe can install one package from an archive that contains extra unrelated families.
- A registry recipe can require multiple related families and fail if any are missing.
- Direct local/GitHub install fails conservatively when an archive contains multiple package families and no explicit boundary.
- Update rejects a new release that no longer satisfies the recipe family boundary.
- Existing single-family registry/GitHub/local tests continue to pass.

## Task 4: Resolve Copy Activation Exposure

### Why

ADR 0002 and the activation spike keep activation strategy switchable because macOS symlink loading is not fully proven. The code exposes `ActivationStrategy::Copy`, config parsing accepts `copy`, and config can persist it. But activation/deactivation return `NotImplemented` for copy.

This is a user-facing footgun: users can configure a supported-looking value that makes install/remove/update paths fail.

### Recommendation

Make the product decision explicit before broadening behavior. The smallest safe fix is to stop accepting `install.activation_strategy = copy` until copy activation is an approved product path. Implement copy activation only if the product spec or an ADR is updated to promote copy from reserved strategy to supported strategy.

### Implementation

- Option A, minimal product-safe fix:
  - reject `fontbrew config set install.activation_strategy copy` with a clear "reserved but not supported" error;
  - reject persisted config files that set `activation_strategy = "copy"` with the same clear error;
  - keep `ActivationStrategy::Copy` internal/reserved only if needed for future compatibility.
- Option B, if a product decision promotes copy activation:
  - implement `ActivationStrategy::Copy` in `ActivationPlan::apply`;
  - implement copy deactivation;
  - copy from managed package store to activation directory;
  - refuse to overwrite unmanaged files;
  - treat an existing managed copy with identical source/content as idempotent;
  - remove only artifacts recorded in manifest;
  - if the activation file differs from the managed source unexpectedly, fail with conflict instead of deleting;
  - add failed-update preservation tests for copy activation.
- Re-run or manually document app-level macOS visibility checks before changing the default activation strategy.

### Acceptance Criteria

- The current product spec does not expose a configurable strategy that immediately fails during install/remove/update.
- If Option A is chosen, `copy` is rejected at config set/load time with a clear error and symlink behavior is unchanged.
- If Option B is chosen, copy activation does not overwrite unmanaged files, remove deletes only manifest-recorded copy artifacts, failed updates preserve old active files, and JSON/human info reports show strategy `Copy` for copied artifacts.

## Task 5: Add Local Archive `--id`

### Why

The implementation design says that if a local archive cannot produce a safe package ID, the CLI should let the user provide one with `--id`. Current local archive install normalizes the first discovered family name; if normalization fails, there is no user escape hatch.

`--id` should not become a way to hide package boundary ambiguity. It should name the local package; family selection remains governed by the package-boundary rules from Task 3.

### Implementation

- Add `package_id_override: Option<PackageId>` to `InstallRequest` or a narrower local-archive request model.
- Add `fontbrew install ./SomeFont.zip --id <package-id>`.
- Accept `--id` only for local archive sources.
- Reject unsafe IDs with the existing package ID validation.
- Pass the override as the `package_id_hint` for local archive prepare.
- Keep source conflict behavior unchanged: reinstall or different source checks still apply.

### Acceptance Criteria

- Local archive with a non-normalizable family name can install with `--id`.
- `--id` is rejected for registry, provider, and direct GitHub sources.
- `--id` does not suppress multi-family boundary errors.
- Manifest records the provided package ID and the actual parsed family names.

## Task 6: Add Registry Schema Version to `registry status`

### Why

The product spec says `registry status` should show local snapshot status, schema/version information, package count, and last refresh time. Current `RegistryStatusReport` exposes availability, path, `updatedAt`, modified time, and package count, but not schema version.

### Implementation

- Add `schema_version: Option<u64>` to `RegistryStatusReport`.
- Populate it in `RegistrySnapshotStore::status`.
- Render it in human output.
- Include it in JSON output.

### Acceptance Criteria

- Missing snapshot reports no schema version.
- Valid snapshot reports `schemaVersion`.
- Invalid/newer schema still fails clearly before rendering misleading status.
- CLI tests cover human and JSON output.

## Task 7: Decide Whether Registry Release Selection Needs Real Behavior

### Why

The registry schema has a `release` field and the product spec says recipes can refine release selection. Current GitHub resolution always selects the latest non-draft, non-prerelease release. The only modeled registry release channel is `stable`, which is equivalent to current default behavior.

This is not a blocking user-facing bug yet, but it is an inert schema hook.

### Recommendation

Do not expand this until a concrete registry package needs non-default release selection. When that happens, add the smallest required selection mode, such as exact tag or tag prefix, and test it through fake GitHub responses.

### Acceptance Criteria

- No action required until a real package recipe needs it.
- If implemented later, registry release selection must affect both install and update.

## Suggested Execution Order

1. Task 1 and Task 2 together, because metadata policy and public flags are the same user-facing contract.
2. Task 4, because accepting `copy` without implementation creates a broken config state.
3. Task 3, because package-boundary semantics affect complex registry packages and update safety.
4. Task 5, because it is small but depends conceptually on the package-boundary decision.
5. Task 6, because it is a small status/reporting gap.
6. Task 7 only when a real registry recipe needs it.

## Verification Plan

Run after each completed task:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Add targeted CLI smoke checks for:

```bash
fontbrew search inter
fontbrew install ./SomeFont.zip --id some-font
fontbrew install inter
fontbrew outdated
fontbrew registry status
```

For copy activation, run smoke checks under an isolated `HOME` and verify both managed store files and activation artifacts are removed by `fontbrew remove`.
