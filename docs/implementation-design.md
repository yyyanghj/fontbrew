# Fontbrew Implementation Design

## 1. Purpose

This document turns the MVP product spec into a Rust implementation design. It is concerned with module boundaries, crate layout, dependency choices, safety invariants, testing strategy, and implementation phases.

The product behavior source of truth is [`product_spec.md`](product_spec.md). This document describes how the MVP should be built.

## 2. Architecture Summary

Fontbrew should start as a Cargo workspace with two crates:

```text
fontbrew/
  Cargo.toml
  crates/
    fontbrew-core/
      Cargo.toml
      src/
        lib.rs
    fontbrew-cli/
      Cargo.toml
      src/
        main.rs
```

Future GUI support should add a third crate:

```text
crates/
  fontbrew-gui/
```

`fontbrew-core` is an application core for Fontbrew-owned frontends. It should be reusable by the CLI and future Tauri GUI, but it is not a public third-party SDK. That means its interface should be clean and stable enough for Fontbrew frontends, without over-generalizing for external plugin authors or arbitrary integrations.

The CLI and future GUI are presentation layers. They should not directly orchestrate manifest writes, package fetching, font parsing, activation, or update commits.

## 3. Crate Responsibilities

### `fontbrew-core`

Owns:

- product use cases
- request, plan, report, and error models
- manifest and config persistence
- registry snapshot loading and validation
- source resolving
- package fetching
- archive extraction
- font metadata parsing
- package discovery
- conflict detection
- activation planning and application
- update prepare/apply workflow
- filesystem safety, file locking, and staging
- provider adapters when added

Does not own:

- terminal output
- prompts
- colors
- progress bars
- `--json` rendering
- process exit codes
- CLI argument parsing
- GUI event rendering

### `fontbrew-cli`

Owns:

- command-line parsing with `clap`
- mapping CLI args into core request types
- human reporter
- JSON reporter
- confirmer/prompt handling
- stdout/stderr routing
- progress rendering
- exit code mapping

### Future `fontbrew-gui`

Expected shape:

- Tauri frontend calls `fontbrew-core` directly as a Rust library
- no daemon
- no shelling out to `fontbrew`
- long-running core operations run in Tauri background tasks
- core progress events are forwarded to frontend events

## 4. Core Interface Shape

`fontbrew-core` should expose a small use-case-oriented interface, not low-level modules for frontends to manually compose.

Sketch:

```rust
pub struct FontbrewApp { /* internal dependencies */ }

impl FontbrewApp {
    pub fn install_plan(&self, request: InstallRequest) -> Result<InstallPlan>;
    pub fn apply_install(
        &self,
        plan: InstallPlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<InstallReport>;

    pub fn list_packages(&self) -> Result<ListReport>;
    pub fn package_info(&self, request: InfoRequest) -> Result<PackageInfo>;

    pub fn remove_plan(&self, request: RemoveRequest) -> Result<RemovePlan>;
    pub fn apply_remove(
        &self,
        plan: RemovePlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<RemoveReport>;

    pub fn outdated(&self, request: OutdatedRequest) -> Result<OutdatedReport>;

    pub fn update_plan(
        &self,
        request: UpdateRequest,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<UpdatePlan>;

    pub fn apply_update(
        &self,
        plan: UpdatePlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: &dyn CancellationToken,
    ) -> Result<UpdateReport>;

    pub fn search(&self, request: SearchRequest) -> Result<SearchReport>;
}
```

This interface is intentionally product-shaped. It is not a generic font management framework.

## 5. Execution Policy

Core should not model terminal confirmation as a UI concept. Prompts belong to CLI or GUI.

Core should enforce explicit execution policy for risky operations:

```rust
pub enum ExecutionPolicy {
    SafeOnly,
    AllowUserApprovedRisk,
    AssumeYes,
    DryRun,
}
```

Plans should describe their risks:

```rust
pub struct InstallPlan {
    pub changes: Vec<PlannedChange>,
    pub risks: Vec<PlanRisk>,
}
```

Core applies a plan only if the execution policy permits the plan's risks. CLI/GUI obtains user approval and maps it to `ExecutionPolicy`; core validates the invariant.

This follows the pattern used by established CLIs: UI owns prompts, while the core operation requires explicit intent for risky work.

## 6. Progress and Cancellation

Core should emit structured progress events through a small sink:

```rust
pub trait ProgressSink {
    fn emit(&mut self, event: ProgressEvent);
}
```

Example events:

```rust
pub enum ProgressEvent {
    ResolvingSource { source: SourceDisplay },
    DownloadStarted { package: PackageId, bytes: Option<u64> },
    DownloadProgress { package: PackageId, downloaded: u64, total: Option<u64> },
    ExtractingArchive { package: PackageId },
    ParsingFonts { package: PackageId },
    PreparingUpdate { package: PackageId },
    ApplyingUpdate { package: PackageId },
    FinishedPackage { package: PackageId },
}
```

Core should accept a cancellation token:

```rust
pub trait CancellationToken {
    fn is_cancelled(&self) -> bool;
}
```

MVP can implement CLI Ctrl-C with an atomic flag. GUI cancellation can be added later by mapping a GUI operation id to a token. Cancellation prevents future work; it does not promise rollback of already committed package changes.

## 7. Module Layout

Recommended `fontbrew-core/src` layout:

```text
app/
  mod.rs                 # FontbrewApp use cases
model/
  mod.rs                 # frontend-facing request/plan/report models
error.rs                 # structured FontbrewError
config/
manifest/
registry/
providers/
sources/
fetch/
archives/
fonts/
install/
activation/
update/
platform/
fs/
tasks/
version/
```

Recommended `fontbrew-cli/src` layout:

```text
main.rs
cli/
  mod.rs                 # clap command definitions
reporter/
  mod.rs
  human.rs
  json.rs
confirm/
progress/
exit.rs
```

Keep most internal modules `pub(crate)`. Public surface should be concentrated around app use cases, frontend-facing models, and errors.

## 8. Source Resolution and Fetching

Use a two-layer model:

```text
SourceResolver:
  InstallSource -> ResolvedSource

PackageFetcher:
  ResolvedSource + selection options -> staged package content
```

Source types:

```rust
pub enum InstallSource {
    RegistryName(String),
    Provider { provider: ProviderKind, id: String },
    GitHubRepo { owner: String, repo: String },
    LocalPath(PathBuf),
}
```

Resolved source sketch:

```rust
pub struct ResolvedSource {
    pub package_id: Option<PackageId>,
    pub display_name: String,
    pub version: PackageVersion,
    pub update_source: Option<UpdateSource>,
    pub recipe: Option<Recipe>,
    pub fetch: FetchSpec,
}
```

Fetch spec sketch:

```rust
pub enum FetchSpec {
    GitHubReleaseAsset { repo: GitHubRepo, release: ReleaseId, asset: AssetId },
    Urls { files: Vec<UrlFile> },
    LocalArchive { path: PathBuf },
}
```

Phase 1 implementation should support local archive, GitHub `owner/repo`, and registry short names resolving to GitHub recipes. Google Fonts and Fontsource adapters should be added later after the core install/update loop is reliable.

## 9. Filesystem Safety

Use JSON manifest plus atomic filesystem operations, not SQLite.

Rules:

- all install/update work starts in a staging directory
- package files move from staging into the managed package store only after validation
- manifest writes use temp file + fsync + atomic rename
- activation artifacts are created only after package files exist
- update keeps old version active until new version is validated
- update deletes old version only after successful activation and manifest update
- remove deletes activation artifacts, package store files, and manifest record
- no separate download cache exists

Paths:

```text
Managed store:
~/.local/share/fontbrew/

Staging:
~/.local/share/fontbrew/staging/<operation-id>/

Packages:
~/.local/share/fontbrew/packages/<package-id>/<version>/

Manifest:
~/.local/share/fontbrew/manifest.json

Activation:
~/Library/Fonts/Fontbrew/
```

Startup or write-operation setup can clean stale staging directories.

## 10. File Locking

All write operations should take a global file lock:

```text
~/.local/share/fontbrew/fontbrew.lock
```

Write operations:

- `install`
- `remove`
- `update`
- `registry update`
- `config set`

Read operations can avoid the write lock and rely on atomic manifest replacement:

- `list`
- `info`
- `search`
- `outdated`

Use a mature file locking crate such as `fs2` or `fd-lock`. Do not implement locking with a hand-rolled PID file unless the chosen crate cannot cover the requirement.

## 11. Update Engine

`fontbrew update` should not be fully serial.

Split update into two stages:

```text
Prepare stage:
  refresh metadata
  check latest versions
  download into staging
  extract
  parse metadata
  validate package identity

Apply stage:
  replace activation artifacts
  atomically update manifest
  delete old package files
```

Prepare can run with bounded parallelism. Apply should be controlled and effectively serial because it mutates the activation directory and manifest.

Partial prepare failure is allowed:

```text
Prepared:
  inter          v4.0 -> v4.1
  maple-mono     v7.3 -> v7.4

Failed to prepare:
  jetbrains-mono network timeout
  noto-sans      asset ambiguity
```

CLI/GUI can ask whether to proceed with prepared packages. Core applies only the packages included in the approved plan.

## 12. Concurrency and Tokio

MVP should not introduce Tokio.

Use:

- synchronous `fontbrew-core` interface
- `reqwest::blocking` for HTTP
- bounded worker pool for update prepare parallelism
- serial or locked apply phase

Tokio is a future optimization, not a rejected direction. To avoid major refactors later:

- hide HTTP behind an adapter
- hide parallel scheduling behind a task runner
- keep progress and cancellation structured
- do not expose `reqwest::blocking` types through core models

Possible adapter sketches:

```rust
pub trait HttpClient {
    fn get_json<T: DeserializeOwned>(&self, request: HttpRequest) -> Result<T>;
    fn download(&self, request: DownloadRequest, dest: &Path, progress: &mut dyn ProgressSink) -> Result<DownloadReport>;
}

pub trait TaskRunner {
    fn map_bounded<I, O, F>(&self, jobs: Vec<I>, limit: usize, f: F) -> Vec<Result<O>>
    where
        F: Fn(I) -> Result<O> + Send + Sync;
}
```

Future Tauri GUI should call synchronous core operations from background tasks and forward progress events to the frontend.

## 13. Font Metadata

MVP should start with a metadata spike before binding the full implementation to a parser.

Initial parser choice:

- `ttf-parser`

Fallback / later expansion:

- `skrifa`
- `read-fonts`

Core should hide parser choice behind:

```rust
pub trait FontMetadataReader {
    fn read_file(&self, path: &Path) -> Result<Vec<FontFaceMetadata>>;
}
```

Return `Vec<FontFaceMetadata>` because `.ttc` and `.otc` files can contain multiple faces.

Metadata required for MVP:

- family name
- subfamily/style
- full name
- PostScript name
- weight when available
- italic/slant when available
- format
- face index for collections

Font metadata is used for discovery, display, conflict detection, and update validation. It is not the default package version source.

## 14. Archive Extraction

MVP supports ZIP archives only.

Archive extraction must be safety-filtered:

- reject absolute paths
- reject `..` path traversal
- reject symlinks and special files inside archives
- extract only needed desktop font files
- ignore web fonts, docs, images, and CSS
- enforce maximum total extracted size
- enforce maximum file count
- enforce maximum single file size
- do not recursively extract nested archives
- extract only into staging directories

Wrap this behavior in an `ArchiveExtractor` module rather than using the `zip` crate directly across use cases.

## 15. Activation

Default activation strategy is symlink-based:

```text
~/Library/Fonts/Fontbrew/<font-file>
-> ~/.local/share/fontbrew/packages/<package-id>/<version>/files/<font-file>
```

Activation must be strategy-based so Fontbrew can switch to copy activation if macOS compatibility requires it.

Before implementation, run an activation spike to verify:

- whether macOS loads fonts in `~/Library/Fonts/Fontbrew/`
- whether macOS follows symlinked font files
- whether applications see fonts without cache reset
- whether copy activation is needed

Do not call private APIs. Do not clear global font caches by default.

## 16. Persistent Data

Use separate models for frontend reports and persistent files.

All persistent files have explicit versions:

- `manifest.json` has `schemaVersion`
- `registry.json` has `schemaVersion`
- `config.toml` has `schema_version`

MVP supports v1 only:

- current v1 reads and writes normally
- missing version is an error
- version newer than supported is an error
- older versions can error until a real migration is needed

Do not reuse report DTOs as manifest storage structs.

## 17. Version Comparison

Store package versions as original source strings.

Do not globally require semver. Source or recipe decides the comparison strategy.

Provide a best-effort comparator for common cases:

- `v` prefix stripping
- semver and semver-like versions
- numeric sequence comparison
- date-like versions

If comparison is ambiguous, Fontbrew should not claim a safe upgrade order. It can report unknown or require a recipe rule.

Update decisions remain source-aware and conservative.

## 18. Package IDs

Package IDs are lowercase ASCII kebab-case slugs.

Allowed shape:

```text
[a-z0-9][a-z0-9-]*[a-z0-9]
```

Examples:

```text
Inter            -> inter
JetBrains Mono   -> jetbrains-mono
Source Sans 3    -> source-sans-3
Maple Mono NF CN -> maple-mono-nf-cn
```

Registry IDs must be unique. Provider and local archive IDs should normalize to the same rule. If a local archive cannot produce a safe ID, CLI should ask the user to provide one with `--id`.

## 19. GitHub Integration

MVP supports unauthenticated GitHub API usage and optional token-based usage.

Token rule:

- read token from `GITHUB_TOKEN`
- do not write the token into config
- config may later allow changing the environment variable name, but not storing the secret

GitHub version rule:

- default package version is the selected GitHub release tag
- default release is latest stable non-draft, non-prerelease release
- recipes can refine release and asset selection

If multiple installable assets exist and neither recipe nor user selector resolves them, fail with an `AmbiguousAssets` error.

## 20. Registry Trust

MVP uses a fixed official registry URL.

Development and CI can override it through an environment variable such as:

```text
FONTBREW_REGISTRY_URL
```

Registry downloads must be schema-validated before use.

MVP does not implement registry signature verification.

Validation should check:

- schema version
- package id slug shape
- unique package IDs
- known source types
- valid GitHub `owner/repo`
- valid glob patterns
- required fields
- known required field behavior

## 21. CLI Reporter

`fontbrew-cli` should own all reporting.

Do not scatter `println!` and `eprintln!` across command handlers. Use reporter adapters:

```text
HumanReporter
JsonReporter
```

Stream rules:

- primary human command results go to stdout
- JSON payloads go to stdout
- progress, warnings, prompts, diagnostics, and human-readable errors go to stderr
- progress bars render only when stderr is a TTY
- JSON mode disables human progress and prompts

Prompting belongs to CLI/GUI, not core.

Reporter sketch:

```rust
pub trait Reporter {
    fn render_list(&mut self, report: ListReport) -> Result<()>;
    fn render_info(&mut self, report: PackageInfo) -> Result<()>;
    fn render_search(&mut self, report: SearchReport) -> Result<()>;
    fn render_install_plan(&mut self, plan: &InstallPlan) -> Result<()>;
    fn render_install_report(&mut self, report: InstallReport) -> Result<()>;
    fn render_update_plan(&mut self, plan: &UpdatePlan) -> Result<()>;
    fn render_update_report(&mut self, report: UpdateReport) -> Result<()>;
    fn warn(&mut self, warning: Warning) -> Result<()>;
    fn error(&mut self, error: &FontbrewError) -> Result<()>;
}
```

Use a separate confirmer:

```rust
pub trait Confirmer {
    fn confirm(&mut self, prompt: ConfirmPrompt) -> Result<bool>;
}
```

## 22. JSON Mode

MVP should support global `--json`.

Rules:

- stdout contains only JSON
- JSON payloads include `schemaVersion`
- no progress bars
- no interactive prompts
- commands requiring approval must use `--yes`, `--dry-run`, or fail with a structured error
- diagnostics should not pollute stdout

Examples:

```bash
fontbrew --json list
fontbrew --json info inter
fontbrew --json outdated
fontbrew --json update --dry-run
```

## 23. Error Handling

Use structured core errors with `thiserror`.

CLI may use `anyhow` at the outermost `main` boundary, but core should expose typed errors that CLI and GUI can render differently.

Example errors:

- `PackageAlreadyInstalled`
- `AmbiguousAssets`
- `Conflict`
- `ExecutionPolicyRequired`
- `NoUpdateSource`
- `PackageIdentityMismatch`
- `ArchiveRejected`
- `RegistryValidationFailed`
- `Io`
- `Network`
- `FontParse`

Some conditions should be reports rather than errors. For example, a local archive package with no update source should appear under `not_updatable` in an outdated report instead of failing the whole command.

## 24. Dependency Plan

Initial dependencies:

- `clap` for CLI parsing
- `serde`, `serde_json`, `toml` for data files and JSON mode
- `thiserror` for core errors
- `anyhow` for CLI top-level glue and spikes
- `reqwest` with `blocking`, `json`, and `rustls-tls`
- `zip` for ZIP archive reading
- `tempfile` for staging and atomic writes
- `directories` for platform paths
- `globset` for asset include/exclude matching
- `ttf-parser` for the initial font metadata spike
- `fs2` or `fd-lock` for file locks
- `indicatif` for CLI progress bars, if progress rendering needs more than simple line logs
- `ctrlc` for CLI cancellation, if basic Ctrl-C cleanup is implemented in MVP

Do not introduce Tokio in MVP.

## 25. Testing Strategy

Prefer testing `fontbrew-core` through use-case interfaces.

Test layers:

1. Unit tests
   - package ID normalization
   - format preference
   - asset matching
   - version comparator
   - registry validation

2. Core tempdir integration tests
   - local archive install
   - remove deletes only managed artifacts
   - repeated install is no-op
   - ambiguous asset returns structured error
   - update prepare failure leaves manifest unchanged
   - conflict requires an execution policy that allows risk
   - staging cleanup
   - activation artifacts point to existing files

3. CLI tests
   - parse commands
   - stdout/stderr behavior
   - `--json` mode
   - exit codes
   - prompt behavior

4. Spikes
   - font metadata parsing
   - macOS activation

Use repo-local font fixtures with clear open-source licenses. Tests should not depend on system fonts or network downloads.

Recommended test crates:

- `tempfile`
- `assert_cmd`
- `predicates`
- optionally `insta` for output snapshots

## 26. Implementation Phases

### Phase 0: Technical Spikes

- font metadata spike with `ttf-parser`
- macOS activation spike for symlink and `~/Library/Fonts/Fontbrew`

### Phase 1: Workspace and Core Skeleton

- Cargo workspace
- `fontbrew-core`
- `fontbrew-cli`
- request/plan/report models
- structured errors
- paths and config loading

### Phase 2: Local Archive Vertical Slice

- ZIP extraction
- font metadata parsing
- package discovery by family name
- managed store staging
- manifest atomic write
- symlink activation
- `install`, `list`, `info`, `remove`

### Phase 3: Safety Hardening

- file lock
- conflict detection
- execution policy
- cancellation
- staging cleanup
- CLI reporter and JSON mode

### Phase 4: GitHub and Registry

- GitHub release resolver
- asset ambiguity handling
- `registry.json` loading and validation
- registry snapshot update
- registry short-name install

### Phase 5: Update

- outdated checks
- update plan
- bounded parallel prepare
- controlled apply
- partial prepare failure handling

### Phase 6: Providers

- Fontsource adapter
- Google Fonts adapter
- provider metadata snapshots
- provider search

## 27. Open Implementation Questions

These do not block the MVP design, but should be answered during spikes or early implementation:

- Which file lock crate behaves best on macOS for this use case?
- Does `ttf-parser` fully cover the metadata needed for TTC/OTC and variable fonts?
- Does macOS load fonts from `~/Library/Fonts/Fontbrew` when they are symlinks?
- Does activation require copy strategy instead of symlink strategy?
- What exact size/file-count limits should archive extraction enforce?
- Which small OFL fonts should be committed as test fixtures?
