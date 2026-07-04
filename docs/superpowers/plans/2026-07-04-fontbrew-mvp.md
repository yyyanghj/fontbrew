# Fontbrew MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Fontbrew MVP CLI as a Rust workspace that can safely install, list, inspect, remove, and later update managed macOS font packages.

**Architecture:** Use a Cargo workspace with `fontbrew-core` for Fontbrew-owned application logic and `fontbrew-cli` for terminal presentation. Core exposes product-shaped request/plan/report use cases, while CLI owns `clap`, reporters, prompts, stdout/stderr behavior, JSON mode, progress display, and exit codes.

**Tech Stack:** Rust, Cargo workspace, `clap`, `serde`, `serde_json`, `toml`, `thiserror`, `anyhow`, `reqwest` blocking mode, `zip`, `tempfile`, `directories`, `globset`, `ttf-parser`, `fs2` or `fd-lock`, `assert_cmd`, `predicates`, and mature small terminal crates such as `anstream`/`anstyle` or `owo-colors` when useful.

---

## Implementation Principles

- Prefer mature ecosystem crates over custom utility code when the crate is small, focused, maintained, and commonly used.
- Do not reimplement ANSI color handling, terminal detection, glob matching, file locking, JSON/TOML parsing, ZIP parsing, temporary file handling, or command parsing.
- Avoid large framework dependencies unless they solve a real MVP problem.
- Keep `fontbrew-core` reusable by Fontbrew-owned frontends, but do not design it as a public third-party SDK.
- Preserve the future Tauri GUI path by keeping core free of terminal I/O, prompts, process exits, and CLI-only output models.
- Keep side effects behind narrow modules: manifest, activation, archive extraction, HTTP, task runner, and platform paths.
- Every write path must be testable in a temp directory.
- Do not introduce Tokio in MVP; keep HTTP and parallelism behind adapters so Tokio can be added later.

## Planned File Structure

```text
Cargo.toml
crates/
  fontbrew-core/
    Cargo.toml
    src/
      lib.rs
      app/mod.rs
      model/mod.rs
      error.rs
      config/mod.rs
      manifest/mod.rs
      registry/mod.rs
      sources/mod.rs
      fetch/mod.rs
      archives/mod.rs
      fonts/mod.rs
      install/mod.rs
      activation/mod.rs
      update/mod.rs
      platform/mod.rs
      fs/mod.rs
      tasks/mod.rs
      version/mod.rs
  fontbrew-cli/
    Cargo.toml
    src/
      main.rs
      cli/mod.rs
      reporter/mod.rs
      reporter/human.rs
      reporter/json.rs
      confirm/mod.rs
      progress/mod.rs
      exit.rs
fixtures/
  fonts/
    README.md
tests/
```

## Task 1: Create the Rust Workspace Skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `crates/fontbrew-core/Cargo.toml`
- Create: `crates/fontbrew-core/src/lib.rs`
- Create: `crates/fontbrew-cli/Cargo.toml`
- Create: `crates/fontbrew-cli/src/main.rs`

- [ ] Create a Cargo workspace with members `crates/fontbrew-core` and `crates/fontbrew-cli`.
- [ ] Add core dependencies: `serde`, `serde_json`, `toml`, `thiserror`, `tempfile`, `directories`, `globset`.
- [ ] Add CLI dependencies: `clap`, `anyhow`, `fontbrew-core`.
- [ ] Add workspace lint and formatting defaults conservatively; do not add strict custom lint policy yet.
- [ ] Make `fontbrew-cli` compile and print minimal command help through `clap`.
- [ ] Run `cargo fmt`.
- [ ] Run `cargo check --workspace`.
- [ ] Commit: `chore: create rust workspace`.

**Acceptance criteria:**

- `cargo check --workspace` succeeds.
- `fontbrew-cli` depends on `fontbrew-core`.
- No product behavior is implemented yet.

## Task 2: Define Core Models, Errors, and App Boundary

**Files:**
- Create: `crates/fontbrew-core/src/app/mod.rs`
- Create: `crates/fontbrew-core/src/model/mod.rs`
- Create: `crates/fontbrew-core/src/error.rs`
- Modify: `crates/fontbrew-core/src/lib.rs`

- [ ] Define newtype models for `PackageId`, `PackageVersion`, `FamilyName`, and `OperationId`.
- [ ] Define request types for `InstallRequest`, `RemoveRequest`, `InfoRequest`, `OutdatedRequest`, `UpdateRequest`, and `SearchRequest`.
- [ ] Define plan/report shells for install, remove, list, info, outdated, update, and search.
- [ ] Define `ExecutionPolicy`, `PlanRisk`, `PlannedChange`, `ProgressEvent`, `ProgressSink`, and `CancellationToken`.
- [ ] Define `FontbrewError` with `thiserror`; include variants for already installed, ambiguous assets, conflict, execution policy required, no update source, identity mismatch, archive rejected, registry validation, I/O, network, and font parse errors.
- [ ] Add `FontbrewApp` with stub methods returning structured `NotImplemented` errors only where needed.
- [ ] Ensure frontend-facing models derive `Debug`, `Clone` where useful, and `Serialize` for JSON/report use.
- [ ] Keep persistent manifest models out of `model/mod.rs`.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: define core app boundary`.

**Acceptance criteria:**

- Core public surface is product-shaped and does not expose low-level modules.
- CLI can import core request/report/error types.
- Core does not print, prompt, or exit.

## Task 3: Implement Path Resolution, Config, Atomic Writes, and File Locking

**Files:**
- Create: `crates/fontbrew-core/src/platform/mod.rs`
- Create: `crates/fontbrew-core/src/config/mod.rs`
- Create: `crates/fontbrew-core/src/fs/mod.rs`
- Modify: `crates/fontbrew-core/Cargo.toml`

- [ ] Add `directories`-based path resolution for managed store, manifest, registry snapshot, provider metadata directory, config path, staging directory, package store, and activation directory.
- [ ] Allow tests to inject root paths instead of writing to the real home directory.
- [ ] Implement config loading with v1 `schema_version`; provide defaults when the config file is missing.
- [ ] Add config fields for format preference, activation strategy, registry auto update, metadata TTL, and update concurrency.
- [ ] Implement atomic file write helper using temp file, flush, sync, and rename.
- [ ] Add a global file lock helper using a mature crate selected during implementation, preferably `fs2` or `fd-lock`.
- [ ] Test config defaults, config parsing, unknown/new schema handling, atomic write replacement, and lock acquisition behavior in temp directories.
- [ ] Run `cargo test -p fontbrew-core`.
- [ ] Commit: `feat: add paths config and safe file primitives`.

**Acceptance criteria:**

- No test writes to real Fontbrew user paths.
- Missing config has deterministic defaults.
- Atomic writer never leaves partial manifest content at the final path in tests.
- A second write lock attempt fails or waits according to the chosen lock behavior and test expectation.

## Task 4: Implement Package ID and Version Utilities

**Files:**
- Create: `crates/fontbrew-core/src/version/mod.rs`
- Modify: `crates/fontbrew-core/src/model/mod.rs`

- [ ] Implement lowercase ASCII kebab-case package ID validation and normalization.
- [ ] Reject unsafe IDs containing separators, uppercase, non-ASCII, empty components, leading/trailing hyphens, or repeated hyphens.
- [ ] Implement best-effort version comparator for equal, semver-like, numeric sequence, and date-like versions.
- [ ] Return an explicit unknown/ambiguous comparison result when ordering cannot be trusted.
- [ ] Keep source-aware update decisions outside the generic comparator.
- [ ] Add unit tests covering package IDs and version comparison cases from the spec.
- [ ] Run `cargo test -p fontbrew-core version model`.
- [ ] Commit: `feat: add package id and version utilities`.

**Acceptance criteria:**

- Package IDs are safe for use in directories.
- Version comparison never claims ordering for ambiguous strings.
- Original version strings remain preserved.

## Task 5: Build the Font Metadata Spike

**Files:**
- Create: `crates/fontbrew-core/src/fonts/mod.rs`
- Create: `fixtures/fonts/README.md`
- Add: small open-source fixture fonts under `fixtures/fonts/`
- Modify: `crates/fontbrew-core/Cargo.toml`

- [ ] Select small open-source fixture fonts with clear license records.
- [ ] Record each fixture's name, source URL, license, original filename, and whether it was modified in `fixtures/fonts/README.md`.
- [ ] Add `ttf-parser`.
- [ ] Define `FontMetadataReader` and `FontFaceMetadata`.
- [ ] Implement `TtfParserMetadataReader`.
- [ ] Parse family, subfamily/style, full name, PostScript name, weight, italic/slant where available, format, and face index for collections.
- [ ] Add tests for `.ttf`, `.otf`, variable font if available, and `.ttc/.otc` if an acceptable fixture is found.
- [ ] If `ttf-parser` cannot satisfy MVP metadata needs, document the gap and switch the task to `skrifa`/`read-fonts` before proceeding.
- [ ] Run `cargo test -p fontbrew-core fonts`.
- [ ] Commit: `feat: read font metadata`.

**Acceptance criteria:**

- Metadata reader returns enough data to group fonts by family name.
- Collections return multiple faces.
- Fixture licensing is documented.
- Parser choice is hidden behind `FontMetadataReader`.

## Task 6: Implement Safe ZIP Archive Extraction

**Files:**
- Create: `crates/fontbrew-core/src/archives/mod.rs`
- Modify: `crates/fontbrew-core/Cargo.toml`

- [ ] Add the `zip` crate.
- [ ] Implement `ArchiveExtractor` for ZIP only.
- [ ] Reject absolute paths, `..` path traversal, symlinks, special files, and unsupported compression outcomes.
- [ ] Ignore non-desktop font files.
- [ ] Enforce total extracted size, file count, and single file size limits.
- [ ] Extract only into staging directories.
- [ ] Add tests for safe extraction, path traversal rejection, webfont ignoring, mixed web/desktop archive handling, and oversized archive rejection.
- [ ] Run `cargo test -p fontbrew-core archives`.
- [ ] Commit: `feat: safely extract font archives`.

**Acceptance criteria:**

- Archive extraction cannot write outside staging.
- Only desktop font files are returned to package discovery.
- Unsafe archives fail with structured errors.

## Task 7: Implement Manifest V1 Persistence

**Files:**
- Create: `crates/fontbrew-core/src/manifest/mod.rs`
- Modify: `crates/fontbrew-core/src/fs/mod.rs`

- [ ] Define separate `ManifestV1` persistence models.
- [ ] Include `schemaVersion`, package records, source/update source, families, font files, activation artifacts, installed timestamp, and active version.
- [ ] Implement read, write, empty manifest creation, package insert, package remove, and package lookup.
- [ ] Use atomic writer for all manifest writes.
- [ ] Reject missing or newer schema versions with structured errors.
- [ ] Add tempdir tests for read/write, no partial writes, package add/remove, and schema errors.
- [ ] Run `cargo test -p fontbrew-core manifest`.
- [ ] Commit: `feat: persist managed package manifest`.

**Acceptance criteria:**

- Manifest is the only source of managed package state.
- Report DTOs and manifest storage models are separate.
- Manifest writes are atomic.

## Task 8: Implement Activation Strategy and macOS Activation Spike

**Files:**
- Create: `crates/fontbrew-core/src/activation/mod.rs`
- Create: `docs/research/macos-activation-spike.md`

- [ ] Implement `ActivationStrategy` enum with `Symlink` and internal space for future `Copy`.
- [ ] Implement symlink activation and deactivation under the injected activation directory.
- [ ] Ensure activation refuses to overwrite unmanaged files.
- [ ] Ensure activation artifacts are tracked for manifest records.
- [ ] Add tempdir tests for symlink activation, deactivation, and unmanaged file conflict.
- [ ] Run a macOS spike manually or via a small ignored tool to verify whether `~/Library/Fonts/Fontbrew` symlinks are loaded by macOS.
- [ ] Record activation spike findings in `docs/research/macos-activation-spike.md`.
- [ ] Run `cargo test -p fontbrew-core activation`.
- [ ] Commit: `feat: add symlink activation strategy`.

**Acceptance criteria:**

- Core can activate and deactivate only within the Fontbrew activation directory.
- Unmanaged activation conflicts require a risk-bearing plan.
- Spike result determines whether symlink remains default or copy strategy must be promoted.

## Task 9: Implement Local Archive Install/List/Info/Remove Vertical Slice

**Files:**
- Create: `crates/fontbrew-core/src/install/mod.rs`
- Modify: `crates/fontbrew-core/src/app/mod.rs`
- Modify: `crates/fontbrew-core/src/model/mod.rs`
- Modify: `crates/fontbrew-core/src/manifest/mod.rs`
- Modify: `crates/fontbrew-core/src/activation/mod.rs`

- [ ] Implement local archive source resolution.
- [ ] Implement install plan for a local archive, including staging, archive extraction, font metadata parsing, package family grouping, package ID derivation, and conflict detection.
- [ ] Implement apply install with `ExecutionPolicy`.
- [ ] Implement list report from manifest.
- [ ] Implement info report from manifest.
- [ ] Implement remove plan and apply remove for managed packages.
- [ ] Ensure local archive packages have no update source by default.
- [ ] Add integration tests for install/list/info/remove using tempdir roots and fixture archive.
- [ ] Add tests for repeated install no-op and remove not touching unmanaged files.
- [ ] Run `cargo test -p fontbrew-core`.
- [ ] Commit: `feat: install local archive packages`.

**Acceptance criteria:**

- A local archive can be installed, listed, inspected, and removed entirely through core.
- Remove deletes activation artifacts and managed package store files but not registry/config/provider metadata.
- Failed install does not update manifest.

## Task 10: Build CLI Commands, Reporter, JSON Mode, and Prompt Handling

**Files:**
- Create: `crates/fontbrew-cli/src/cli/mod.rs`
- Create: `crates/fontbrew-cli/src/reporter/mod.rs`
- Create: `crates/fontbrew-cli/src/reporter/human.rs`
- Create: `crates/fontbrew-cli/src/reporter/json.rs`
- Create: `crates/fontbrew-cli/src/confirm/mod.rs`
- Create: `crates/fontbrew-cli/src/progress/mod.rs`
- Create: `crates/fontbrew-cli/src/exit.rs`
- Modify: `crates/fontbrew-cli/src/main.rs`
- Modify: `crates/fontbrew-cli/Cargo.toml`

- [ ] Implement `clap` commands for `install`, `list`, `info`, `remove`, and `uninstall` alias.
- [ ] Add global `--json`, `--quiet`, `--verbose`, and `--no-color` if color support is added.
- [ ] Use mature terminal/color crates when useful; prefer `anstream`/`anstyle` or a similarly mature small crate over custom ANSI escapes.
- [ ] Add `HumanReporter` with stdout for primary results and stderr for progress, warnings, prompts, diagnostics, and errors.
- [ ] Add `JsonReporter` with clean stdout JSON payloads and no interactive prompts.
- [ ] Add `Confirmer` that maps human approval to `ExecutionPolicy`.
- [ ] Add progress sink adapter that maps core `ProgressEvent` to reporter behavior.
- [ ] Ensure JSON mode fails with structured error instead of prompting when approval is required.
- [ ] Add CLI tests with `assert_cmd` and `predicates` for list, install local archive, remove, JSON mode, and stdout/stderr separation.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: expose local archive workflow in cli`.

**Acceptance criteria:**

- `fontbrew install ./fixture.zip`, `fontbrew list`, `fontbrew info <id>`, and `fontbrew remove <id>` work end-to-end in test-controlled paths.
- JSON stdout is parseable and not polluted by progress or warnings.
- Human reporter output follows documented stream rules.

## Task 11: Implement Registry Snapshot and Registry Short-Name Install

**Files:**
- Create: `crates/fontbrew-core/src/registry/mod.rs`
- Modify: `crates/fontbrew-core/src/sources/mod.rs`
- Modify: `crates/fontbrew-core/src/app/mod.rs`
- Modify: `crates/fontbrew-cli/src/cli/mod.rs`

- [ ] Define `registry.json` v1 persistence structs separate from report models.
- [ ] Implement schema validation for package IDs, source types, GitHub repo syntax, glob patterns, required fields, and unknown required behavior.
- [ ] Implement registry snapshot read and write.
- [ ] Add a fixed official registry URL constant and `FONTBREW_REGISTRY_URL` development override.
- [ ] Add `fontbrew registry update` and `fontbrew registry status`.
- [ ] Resolve registry short names to recipes.
- [ ] Add tests for registry validation, snapshot update using a fake HTTP adapter, short-name resolution, and invalid registry rejection.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: add registry snapshot support`.

**Acceptance criteria:**

- Short names come only from the registry snapshot.
- Invalid registry data is rejected before use.
- Registry update does not cache downloaded fonts.

## Task 12: Implement GitHub Release Resolution and Asset Fetching

**Files:**
- Create: `crates/fontbrew-core/src/sources/mod.rs`
- Create: `crates/fontbrew-core/src/fetch/mod.rs`
- Modify: `crates/fontbrew-core/src/app/mod.rs`
- Modify: `crates/fontbrew-core/Cargo.toml`

- [ ] Add `reqwest` blocking HTTP adapter behind an `HttpClient` interface.
- [ ] Read optional GitHub token from `GITHUB_TOKEN`; do not store it in config.
- [ ] Resolve latest non-draft, non-prerelease GitHub release.
- [ ] Use GitHub release tag as default package version.
- [ ] Apply recipe asset include/exclude rules with `globset`.
- [ ] Return `AmbiguousAssets` when multiple installable assets remain unresolved.
- [ ] Support explicit `--asset` selection through request models and CLI.
- [ ] Download selected assets directly into staging.
- [ ] Add tests with fake HTTP responses; no live GitHub calls in normal tests.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: install packages from github releases`.

**Acceptance criteria:**

- `fontbrew install owner/repo` can resolve and fetch an unambiguous release asset in tests.
- Multiple asset ambiguity fails conservatively.
- Registry recipes can choose a GitHub asset.

## Task 13: Implement Search and Outdated for Registry/GitHub/Local

**Files:**
- Modify: `crates/fontbrew-core/src/app/mod.rs`
- Modify: `crates/fontbrew-core/src/registry/mod.rs`
- Modify: `crates/fontbrew-core/src/update/mod.rs`
- Modify: `crates/fontbrew-cli/src/cli/mod.rs`
- Modify: `crates/fontbrew-cli/src/reporter/human.rs`
- Modify: `crates/fontbrew-cli/src/reporter/json.rs`

- [ ] Implement registry search returning installable registry candidates.
- [ ] Implement `outdated` for managed GitHub-backed packages.
- [ ] Report local archive packages without update sources as `not_updatable`, not command failures.
- [ ] Refresh registry metadata by default where registry snapshot behavior is needed.
- [ ] Add CLI commands `search` and `outdated`.
- [ ] Add tests for registry search, outdated GitHub package, local archive not-updatable report, and JSON outputs.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: search registry and check outdated packages`.

**Acceptance criteria:**

- Search returns only installable candidates.
- Outdated reports updateable and not-updatable packages distinctly.
- Local archive packages never pretend to be updateable.

## Task 14: Implement Conservative Update with Bounded Parallel Prepare

**Files:**
- Create: `crates/fontbrew-core/src/update/mod.rs`
- Create: `crates/fontbrew-core/src/tasks/mod.rs`
- Modify: `crates/fontbrew-core/src/app/mod.rs`
- Modify: `crates/fontbrew-cli/src/cli/mod.rs`
- Modify: `crates/fontbrew-cli/src/reporter/human.rs`
- Modify: `crates/fontbrew-cli/src/reporter/json.rs`

- [ ] Implement a bounded task runner for prepare work without Tokio.
- [ ] Add update plan generation with prepared updates and prepare failures.
- [ ] Validate package identity before including a prepared update.
- [ ] Apply only prepared packages permitted by `ExecutionPolicy`.
- [ ] Keep old version active until new version is validated and activation succeeds.
- [ ] Delete old version only after successful apply.
- [ ] Preserve manifest and activation when prepare or apply fails.
- [ ] Add `fontbrew update [package]`, `--dry-run`, `--yes`, and `--jobs`.
- [ ] Add tests for partial prepare failure, identity mismatch, apply failure preserving old version, and bounded parallel prepare behavior.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: update managed packages safely`.

**Acceptance criteria:**

- `fontbrew update` is not fully serial in prepare.
- Failed prepare for one package does not block applying other prepared packages after approval.
- Manifest never points at missing active font files.

## Task 15: Add Conflict Detection and Safety Polishing

**Files:**
- Modify: `crates/fontbrew-core/src/install/mod.rs`
- Modify: `crates/fontbrew-core/src/activation/mod.rs`
- Modify: `crates/fontbrew-core/src/model/mod.rs`
- Modify: `crates/fontbrew-cli/src/reporter/human.rs`
- Modify: `crates/fontbrew-cli/src/reporter/json.rs`

- [ ] Detect non-managed same-family fonts where feasible.
- [ ] Detect activation artifact path conflicts.
- [ ] Detect package already managed from another source.
- [ ] Ensure conflicts produce `PlanRisk` entries.
- [ ] Ensure apply refuses risky plans under `ExecutionPolicy::SafeOnly`.
- [ ] Ensure human CLI prompts when risks exist and JSON mode requires `--yes` or `--dry-run`.
- [ ] Add tests for each conflict class.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: enforce install and activation conflict safety`.

**Acceptance criteria:**

- Fontbrew never adopts, overwrites, or removes non-managed fonts silently.
- Risk acceptance is explicit and represented as execution policy, not core prompting.

## Task 16: Add Config Commands and Format Preference Overrides

**Files:**
- Modify: `crates/fontbrew-core/src/config/mod.rs`
- Modify: `crates/fontbrew-core/src/install/mod.rs`
- Modify: `crates/fontbrew-cli/src/cli/mod.rs`
- Modify: `crates/fontbrew-cli/src/reporter/human.rs`
- Modify: `crates/fontbrew-cli/src/reporter/json.rs`

- [ ] Implement `fontbrew config get`.
- [ ] Implement `fontbrew config set` for known keys only.
- [ ] Apply global format preference during install.
- [ ] Add per-install `--format`, `--otf`, and `--ttf` overrides.
- [ ] Fail conservatively when different formats have different family/style/weight coverage.
- [ ] Add tests for config get/set, format preference selection, and format ambiguity.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: add config and format preference controls`.

**Acceptance criteria:**

- Users can inspect and change supported config keys.
- Format choice uses config by default and CLI overrides per install.
- Format ambiguity does not silently install the wrong files.

## Task 17: Add Cancellation, Staging Cleanup, and Operational Hardening

**Files:**
- Modify: `crates/fontbrew-core/src/fs/mod.rs`
- Modify: `crates/fontbrew-core/src/app/mod.rs`
- Modify: `crates/fontbrew-cli/src/main.rs`
- Modify: `crates/fontbrew-cli/Cargo.toml`

- [ ] Add a `NoCancellation` token for tests and simple commands.
- [ ] Add optional CLI Ctrl-C cancellation using a mature crate such as `ctrlc`.
- [ ] Check cancellation at stage boundaries and download chunks.
- [ ] Clean stale staging directories at write-operation setup.
- [ ] Ensure file lock releases on process exit through RAII.
- [ ] Add tests for cancellation before apply and staging cleanup.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: harden cancellation and staging cleanup`.

**Acceptance criteria:**

- Cancellation prevents future work without claiming rollback of committed work.
- Stale staging directories do not become managed package state.

## Task 18: Add Provider Adapter Interfaces, Then Implement Fontsource

**Files:**
- Create: `crates/fontbrew-core/src/providers/mod.rs`
- Modify: `crates/fontbrew-core/src/sources/mod.rs`
- Modify: `crates/fontbrew-core/src/app/mod.rs`
- Modify: `crates/fontbrew-cli/src/cli/mod.rs`

- [ ] Add provider adapter interface after registry/GitHub/local flows are stable.
- [ ] Implement provider metadata snapshot storage without downloaded font cache.
- [ ] Implement Fontsource search if its API can provide installable desktop font assets.
- [ ] If Fontsource cannot provide desktop font assets reliably, document the limitation and keep provider search disabled for MVP.
- [ ] Add provider tests with fake HTTP responses.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: add fontsource provider adapter`.

**Acceptance criteria:**

- Provider search returns only installable candidates.
- Provider metadata snapshots contain metadata only.
- No provider implementation bypasses source resolver/fetcher safety.

## Task 19: Implement Google Fonts Provider

**Files:**
- Modify: `crates/fontbrew-core/src/providers/mod.rs`
- Modify: `crates/fontbrew-core/src/config/mod.rs`
- Modify: `crates/fontbrew-cli/src/reporter/human.rs`

- [ ] Add Google Fonts adapter using API key from environment only.
- [ ] Document the required environment variable for the API key.
- [ ] Implement search and install only for results that resolve to desktop font files.
- [ ] Add rate-limit and missing-key errors that are actionable.
- [ ] Add fake HTTP tests for successful search, missing key, rate limit, and non-installable result filtering.
- [ ] Run `cargo test --workspace`.
- [ ] Commit: `feat: add google fonts provider adapter`.

**Acceptance criteria:**

- Google token is never stored in config.
- Search remains installable-only.
- Missing API key produces a clear error.

## Task 20: Final MVP Verification and Documentation Pass

**Files:**
- Modify: `README.md` if added during implementation
- Modify: `docs/product_spec.md` only if behavior changed and user approves
- Modify: `docs/implementation-design.md` only if implementation diverged intentionally
- Modify: `fixtures/fonts/README.md`

- [ ] Run `cargo fmt --all`.
- [ ] Run `cargo clippy --workspace --all-targets`.
- [ ] Run `cargo test --workspace`.
- [ ] Run CLI smoke tests on macOS temp paths.
- [ ] Verify `fontbrew install ./fixture.zip`, `list`, `info`, `remove`, `registry update`, `install inter` through test registry, `outdated`, and `update --dry-run`.
- [ ] Verify stdout/stderr behavior manually for human and JSON modes.
- [ ] Verify no command writes outside injected/test paths in automated tests.
- [ ] Update docs for any accepted behavior changes.
- [ ] Commit: `docs: finalize mvp usage and verification notes`.

**Acceptance criteria:**

- All automated tests pass.
- Human and JSON CLI behavior matches the documented stream rules.
- The MVP trust test from `docs/product_spec.md` can be answered from CLI output and manifest state.

## Coverage Checklist

- [ ] Rust workspace and two-crate architecture
- [ ] Core request/plan/report boundary
- [ ] Execution policy instead of core confirmation
- [ ] Progress sink and cancellation token
- [ ] Mature crate dependency policy
- [ ] Metadata spike
- [ ] macOS activation spike
- [ ] Local archive vertical slice
- [ ] Manifest v1 and atomic writes
- [ ] Symlink activation
- [ ] Safe ZIP extraction
- [ ] CLI reporter and JSON mode
- [ ] Registry snapshot
- [ ] GitHub release install
- [ ] Search and outdated
- [ ] Conservative update
- [ ] File locking
- [ ] Conflict handling
- [ ] Config and format preference
- [ ] Provider phase
