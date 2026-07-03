# Fontbrew MVP Issue Backlog

This is a draft issue backlog derived from [`../implementation-design.md`](../implementation-design.md) and [`../../spec.md`](../../spec.md). It is not yet published to an issue tracker.

The issues are ordered as vertical slices where possible. Early prefactoring/spike issues are included first because they reduce implementation risk for later slices.

Cross-cutting implementation rules:

- Prefer mature, focused ecosystem crates over custom utility code when the crate is small and maintained.
- Do not introduce Tokio in the MVP; keep HTTP and task scheduling behind adapters so Tokio can be added later.
- Keep `fontbrew-core` reusable by Fontbrew-owned CLI and future Tauri GUI frontends, but do not design it as a third-party SDK.
- Keep prompts, terminal colors, progress bars, JSON rendering, and stdout/stderr behavior in `fontbrew-cli`.

## Issue 1: Create Rust workspace and core/CLI crate boundary

**Blocked by:** None - can start immediately

**User stories covered:** Enables every MVP command by establishing the build and crate boundary.

## What to build

Create the Cargo workspace with `fontbrew-core` and `fontbrew-cli`. The CLI should compile, depend on core, and expose minimal command help. No product behavior is required in this issue.

## Acceptance criteria

- [ ] `cargo check --workspace` succeeds.
- [ ] `fontbrew-cli` depends on `fontbrew-core`.
- [ ] The workspace has the planned crate layout.
- [ ] No CLI command performs real install/remove/update behavior yet.

## Issue 2: Define core app models, structured errors, execution policy, progress, and cancellation

**Blocked by:** Issue 1

**User stories covered:** Enables CLI and future GUI to use the same core plans and reports.

## What to build

Define the product-shaped `FontbrewApp` surface and the request, plan, report, error, execution policy, progress, and cancellation types that future issues will implement.

## Acceptance criteria

- [ ] Core exposes request/plan/report models for install, remove, list, info, outdated, update, and search.
- [ ] Core errors are structured with typed variants.
- [ ] Core models terminal approval as execution policy, not as a prompt.
- [ ] Core defines progress and cancellation seams without depending on CLI output.
- [ ] Frontend-facing report types can be serialized.

## Issue 3: Add filesystem foundations: paths, config, atomic writes, and global lock

**Blocked by:** Issue 2

**User stories covered:** Supports safe local state for all install/remove/update commands.

## What to build

Implement path resolution, config defaults, atomic file writes, and global file locking. All paths must be injectable for tests so automated tests never write to the user's real Fontbrew directories.

## Acceptance criteria

- [ ] Managed store, manifest, registry snapshot, config, staging, package store, and activation paths are resolved consistently.
- [ ] Tests can inject temp roots.
- [ ] Config v1 defaults work when no config file exists.
- [ ] Atomic writes use temp file and rename.
- [ ] Write commands can acquire a global file lock.

## Issue 4: Add package ID and version utility layer

**Blocked by:** Issue 2

**User stories covered:** Ensures package names are safe and versions are compared conservatively.

## What to build

Implement package ID validation/normalization and a source-aware version utility layer with best-effort comparison for common formats.

## Acceptance criteria

- [ ] Package IDs are lowercase ASCII kebab-case slugs.
- [ ] Unsafe IDs are rejected.
- [ ] Original package version strings are preserved.
- [ ] Best-effort comparison handles semver-like, numeric, and date-like versions.
- [ ] Ambiguous versions are not treated as safely ordered.

## Issue 5: Prove font metadata parsing with real fixtures

**Blocked by:** Issue 1

**User stories covered:** Enables package discovery by family name and safe display of installed font metadata.

## What to build

Run the font metadata spike by adding licensed fixture fonts and implementing the first `FontMetadataReader` adapter using `ttf-parser`. Document fixture sources and licenses.

## Acceptance criteria

- [ ] Fixtures have documented source URLs and licenses.
- [ ] `.ttf` and `.otf` metadata can be parsed.
- [ ] `.ttc/.otc` support is verified if a suitable fixture exists.
- [ ] Family, style, full name, PostScript name, weight, italic/slant, format, and face index are extracted where available.
- [ ] Parser choice is hidden behind `FontMetadataReader`.

## Issue 6: Safely extract ZIP font archives

**Blocked by:** Issue 5

**User stories covered:** Enables local archive install without unsafe filesystem writes.

## What to build

Implement the ZIP-only `ArchiveExtractor` with path traversal protection, file type filtering, and size/count limits.

## Acceptance criteria

- [ ] Absolute paths and `..` traversal are rejected.
- [ ] Symlinks and special files inside archives are rejected.
- [ ] Webfont/docs/image files are ignored for activation.
- [ ] Only desktop font files are extracted.
- [ ] Extraction cannot write outside staging.

## Issue 7: Persist managed package state in manifest v1

**Blocked by:** Issue 3

**User stories covered:** Lets users list, inspect, remove, and later update only Fontbrew-managed packages.

## What to build

Implement `manifest.json` v1 read/write with package records, source/update source, families, font files, activation artifacts, installed timestamp, and active version.

## Acceptance criteria

- [ ] Manifest has `schemaVersion`.
- [ ] Missing or newer schema versions fail clearly.
- [ ] Manifest writes are atomic.
- [ ] Package add, lookup, list, and remove are implemented.
- [ ] Persistent manifest structs are separate from frontend report structs.

## Issue 8: Activate managed fonts safely on macOS

**Blocked by:** Issue 3, Issue 5

**User stories covered:** Makes installed fonts available to macOS while preserving the Fontbrew management boundary.

## What to build

Implement symlink activation under `~/Library/Fonts/Fontbrew` with conflict checks, and run the macOS activation spike to verify whether symlink activation works as expected.

## Acceptance criteria

- [ ] Activation artifacts are created only inside the Fontbrew activation directory.
- [ ] Unmanaged activation file conflicts are detected.
- [ ] Deactivation removes only managed activation artifacts.
- [ ] macOS symlink/subdirectory behavior is documented in a research note.
- [ ] Copy strategy remains possible if symlink activation fails the spike.

## Issue 9: Deliver local archive install/list/info/remove

**Blocked by:** Issue 4, Issue 6, Issue 7, Issue 8

**User stories covered:** User can install a downloaded font archive, see it, inspect it, and remove it safely.

## What to build

Implement the first full vertical slice for local archives through core and CLI: `install ./file.zip`, `list`, `info`, `remove`, and `uninstall` alias.

## Acceptance criteria

- [ ] Local archive install stages, extracts, parses, stores, activates, and records manifest state.
- [ ] Local archive packages have no update source by default.
- [ ] `list` shows managed packages only.
- [ ] `info` shows package details from manifest.
- [ ] `remove` deletes activation artifacts, managed package files, and manifest record only.
- [ ] Repeated install is a no-op unless reinstall is explicitly requested.

## Issue 10: Add CLI reporter, JSON mode, prompts, and progress stream behavior

**Blocked by:** Issue 2, Issue 9

**User stories covered:** Users and scripts get predictable CLI output.

## What to build

Implement `HumanReporter`, `JsonReporter`, prompt handling, progress rendering, and stdout/stderr separation. Use mature terminal/color crates instead of handwritten ANSI handling.

## Acceptance criteria

- [ ] Human primary results go to stdout.
- [ ] Progress, warnings, prompts, diagnostics, and human errors go to stderr.
- [ ] `--json` stdout contains only JSON.
- [ ] JSON mode does not prompt.
- [ ] Commands requiring approval require `--yes`, `--dry-run`, or fail in JSON mode.
- [ ] Color/progress behavior respects TTY and no-color/quiet/verbose options where implemented.

## Issue 11: Add first-party registry snapshot and short-name install

**Blocked by:** Issue 9, Issue 10

**User stories covered:** User can install well-known packages by stable short name.

## What to build

Implement `registry.json` v1 validation, local snapshot handling, `registry update/status`, and registry short-name resolution to recipes.

## Acceptance criteria

- [ ] Registry snapshot has `schemaVersion`.
- [ ] Invalid registry entries are rejected before use.
- [ ] Short names come only from the first-party registry.
- [ ] `FONTBREW_REGISTRY_URL` can override the registry URL for development and tests.
- [ ] Registry update stores metadata only, not downloaded fonts.

## Issue 12: Install fonts from GitHub releases

**Blocked by:** Issue 6, Issue 9, Issue 11

**User stories covered:** User can install fonts from explicit GitHub repositories and registry recipes backed by GitHub releases.

## What to build

Implement GitHub release resolution, asset selection, optional `GITHUB_TOKEN`, explicit asset override, and direct download into staging.

## Acceptance criteria

- [ ] Latest stable GitHub release is selected by default.
- [ ] Package version defaults to the release tag.
- [ ] Recipes can refine release and asset selection.
- [ ] Multiple installable assets fail with an actionable ambiguity error.
- [ ] `--asset` resolves ambiguity.
- [ ] No normal test depends on live GitHub.

## Issue 13: Add registry search and outdated reporting

**Blocked by:** Issue 11, Issue 12

**User stories covered:** User can discover installable registry packages and see which managed packages can be updated.

## What to build

Implement `search` over the registry and `outdated` for GitHub-backed managed packages. Local archive packages without update sources should appear as not updatable.

## Acceptance criteria

- [ ] Search returns installable candidates only.
- [ ] Search does not query arbitrary GitHub repositories.
- [ ] Outdated distinguishes updateable packages from not-updatable packages.
- [ ] Local archive packages without update sources do not fail the command.
- [ ] Human and JSON outputs are covered by tests.

## Issue 14: Update managed packages safely with bounded parallel prepare

**Blocked by:** Issue 12, Issue 13

**User stories covered:** User can review and apply updates without losing existing fonts if an update fails.

## What to build

Implement `update [package]`, dry-run, jobs/concurrency, bounded parallel prepare, conservative identity validation, partial prepare failure reporting, and controlled apply.

## Acceptance criteria

- [ ] Prepare can run concurrently with a bounded jobs setting.
- [ ] Apply mutates activation and manifest in a controlled phase.
- [ ] Old version remains active until new version validates and activates.
- [ ] Prepare failure for one package does not force failure of all prepared updates.
- [ ] Identity mismatch stops that package update.
- [ ] Successful update removes old version after commit.
- [ ] MVP does not retain rollback history.

## Issue 15: Harden conflict handling and execution policy enforcement

**Blocked by:** Issue 9, Issue 14

**User stories covered:** User can trust Fontbrew not to overwrite or delete fonts it does not manage.

## What to build

Detect same-family unmanaged fonts where feasible, activation file conflicts, and package source conflicts. Represent risks in plans and enforce execution policy in core.

## Acceptance criteria

- [ ] Conflicts appear as plan risks.
- [ ] `ExecutionPolicy::SafeOnly` refuses risky applies.
- [ ] Human CLI can ask for approval and pass approved policy.
- [ ] JSON mode requires `--yes` or `--dry-run` for risky applies.
- [ ] Fontbrew never adopts, overwrites, or deletes non-managed fonts silently.

## Issue 16: Add config commands and format preference behavior

**Blocked by:** Issue 9, Issue 10

**User stories covered:** User can control format preference globally and per install.

## What to build

Implement `config get/set`, global format preference, `--format`, `--otf`, and `--ttf` overrides.

## Acceptance criteria

- [ ] Known config keys can be read and written.
- [ ] Unknown config keys fail clearly.
- [ ] Install uses global format preference by default.
- [ ] CLI format flags override config for one install.
- [ ] Non-equivalent format coverage requires explicit selection.

## Issue 17: Add cancellation and staging cleanup hardening

**Blocked by:** Issue 14

**User stories covered:** User can interrupt long operations without corrupting managed state.

## What to build

Add cancellation token handling, optional Ctrl-C integration, stale staging cleanup, and tests for cancellation before commit.

## Acceptance criteria

- [ ] Cancellation is checked at safe stage boundaries.
- [ ] Staging cleanup removes stale operation directories.
- [ ] Cancellation before apply leaves manifest unchanged.
- [ ] Already committed operations are not claimed to be rolled back.
- [ ] File lock release uses RAII behavior.

## Issue 18: Add Fontsource provider if it can return installable desktop fonts

**Blocked by:** Issue 13

**User stories covered:** User can discover and install provider-backed fonts beyond the curated registry.

## What to build

Add provider adapter shape and implement Fontsource search/install only if its API can resolve results to desktop font files. If not, document why it remains disabled.

## Acceptance criteria

- [ ] Provider search returns installable candidates only.
- [ ] Provider metadata snapshot stores metadata only.
- [ ] Provider install uses the same source resolver/fetcher safety as registry/GitHub.
- [ ] If Fontsource cannot supply desktop fonts reliably, the limitation is documented and the provider is not exposed as installable.

## Issue 19: Add Google Fonts provider

**Blocked by:** Issue 18

**User stories covered:** User can discover and install Google Fonts results when configured with an API key.

## What to build

Implement Google Fonts API search/install with API key read from environment only.

## Acceptance criteria

- [ ] API key is read from environment and never stored in config.
- [ ] Missing key and rate limits produce actionable errors.
- [ ] Search results are filtered to installable desktop font candidates.
- [ ] Provider tests use fake HTTP responses.

## Issue 20: Final MVP verification and docs pass

**Blocked by:** Issues 1-19

**User stories covered:** Confirms the MVP is ready to use and documented.

## What to build

Run full workspace verification, CLI smoke tests, docs updates, and final behavior checks.

## Acceptance criteria

- [ ] `cargo fmt --all` succeeds.
- [ ] `cargo clippy --workspace --all-targets` succeeds.
- [ ] `cargo test --workspace` succeeds.
- [ ] Local archive install/list/info/remove smoke test works.
- [ ] Registry/GitHub install path works against a test registry or fixture server.
- [ ] Outdated and update dry-run work.
- [ ] Human stdout/stderr and JSON mode behavior match the spec.
- [ ] Docs reflect implemented behavior.
