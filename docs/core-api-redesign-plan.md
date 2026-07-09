# Core API Redesign Plan

This plan reshapes `fontbrew-core` from an application-style facade into a set of orthogonal core operations with a small number of workflow helpers. The CLI should still be easy to build on top of the core crate, but callers should not have to accept one monolithic command flow when they need a smaller operation.

## Goals

- Keep `Fontbrew` as the single public entry object.
- Expose flat, orthogonal methods on `Fontbrew`; do not group methods behind secondary facade objects.
- Keep method names specific enough that the flat surface does not become another app object.
- Preserve current behavior unless the new interface cannot express it.
- Keep confirmation outside `fontbrew-core`; callers own prompts and non-interactive approval rules.
- Make request structs constructible by callers without forcing them to call the previous Fontbrew step first.
- Keep staged install resources owned by explicit token types that clean themselves up on drop.
- Keep default paths and behavior aligned with the current implementation.

## Non-Goals

- Do not inject a network client through `FontbrewOptions`.
- Do not add multiple constructors for `Fontbrew`.
- Do not move CLI prompting, JSON rendering, or human output behavior into `fontbrew-core`.
- Do not make `config.toml` a second managed state file; it remains user preferences, not package state.

## Fontbrew Construction

`FontbrewOptions` configures locations only. Preferences are not passed through this type.

```rust
pub struct FontbrewOptions {
    pub store_dir: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub activation_dir: Option<PathBuf>,
}

impl Default for FontbrewOptions {
    fn default() -> Self {
        Self {
            store_dir: None,
            config_path: None,
            activation_dir: None,
        }
    }
}

impl Fontbrew {
    pub fn new(options: FontbrewOptions) -> Result<Self>;
}
```

Defaults stay consistent with today:

- `store_dir`: `~/.local/share/fontbrew`
- `config_path`: `~/.config/fontbrew/config.toml`
- `activation_dir`: `~/Library/Fonts/Fontbrew`
- staging directory: fixed at `store_dir.join("staging")`

Relative paths are accepted and resolved against the current working directory during `Fontbrew::new`. Do not use `canonicalize` at construction time because these paths may not exist yet and canonicalization would resolve symlinks too early. Path safety checks remain part of write/extract/apply operations.

`Fontbrew::new(FontbrewOptions::default())` is the only construction pattern. Do not add `Fontbrew::default`, `new_default`, or alternate constructors.

## Preferences

`config.toml` is the persisted source of default preferences. If it does not exist, built-in defaults are used. Each operation reads config when it needs preferences; `Fontbrew` does not cache config.

Current defaults remain:

- `format_preference`: `Otf`, `Ttf`, `Ttc`, `Otc`
- `activation_strategy`: `Copy`
- `metadata_ttl`: 24 hours
- `update_concurrency`: 4

Per-operation request fields may override loaded config without writing back to `config.toml`. For example, a CLI `--format` flag becomes an install request override; it does not become a second source of default preferences.

## Confirmation Ownership

Confirmation is caller-owned. Core operations expose risks and require an explicit apply policy, but they never prompt.

```rust
let plans = fontbrew.plan_install(request)?;
let policy = caller_confirms(plans.risks())?;
let reports = fontbrew.apply_install(plans, ApplyOptions { policy }).await?;
```

`apply_install`, `apply_remove`, and update apply operations reject risky plans unless `ApplyOptions` explicitly allows the risk. CLI and GUI layers decide how to collect that approval.

## Install Flow

The public install workflow has four stages:

1. `fetch_install_metadata`
2. `prepare_install_asset`
3. `plan_install`
4. `apply_install`

This replaces the current `prepare_install -> Plan | FamilySelection` shape. Metadata fetching stops before archive download. Asset preparation stops after candidate discovery. It does not implicitly continue to planning, even if only one family was found.

### Fetch Install Metadata

`fetch_install_metadata` performs source-level metadata work:

1. Resolve the install source.
2. For GitHub, resolve the latest stable release and filter installable zip assets.
3. For Fontsource, resolve provider package metadata.
4. Return selectable release assets when a source has them.

```rust
pub struct FetchInstallMetadataRequest {
    pub source: InstallSource,
}

pub struct InstallMetadata {
    // private resolved source metadata
}

impl InstallMetadata {
    pub fn source(&self) -> &InstallSource;
    pub fn package_id(&self) -> Option<&PackageId>;
    pub fn assets(&self) -> &[String];
}
```

The returned `InstallMetadata` owns resolved release or provider metadata so the next stage can reuse it without repeating network requests.

### Prepare Install Asset

`prepare_install_asset` performs asset-level work:

1. Apply `asset_selector` when the metadata exposes multiple assets.
2. Download the chosen remote asset when needed.
3. Extract the archive.
4. Parse desktop font metadata.
5. Return install candidates.

```rust
pub struct PrepareInstallAssetRequest {
    pub metadata: InstallMetadata,
    pub asset_selector: Option<String>,
    pub format_preference: Vec<FontFormat>,
}

pub struct InstallSourcePreparation {
    // private staging ownership
}

impl InstallSourcePreparation {
    pub fn candidates(&self) -> &[InstallCandidate];
}
```

The returned `InstallSourcePreparation` owns staging and removes it on drop. It exposes candidates as ordinary data for caller selection.

`prepare_install_source` remains as a convenience helper for callers that do not need a separate metadata/asset selection boundary. The CLI uses the finer `fetch_install_metadata -> prepare_install_asset` flow because it may prompt between those stages.

### Install Candidates

An install candidate is richer than a family name. It is displayable selection data plus an opaque id for planning.

```rust
pub struct InstallCandidate {
    pub id: InstallCandidateId,
    pub package_id: Option<PackageId>,
    pub families: Vec<FamilyName>,
    pub version: Option<PackageVersion>,
    pub source: InstallCandidateSource,
    pub fonts: Vec<InstallCandidateFont>,
}

pub struct InstallCandidateId(String);

pub struct InstallCandidateFont {
    pub family: FamilyName,
    pub style: String,
    pub weight: u16,
    pub format: FontFormat,
}
```

`InstallCandidateId` is opaque and valid only within the `InstallSourcePreparation` that produced it. Callers should store or display it, but must not parse it.

`package_id` is the default package id Fontbrew would use if the caller does not override it. It is optional because local archives may contain a family name that cannot be normalized into a safe package id; callers can still select that candidate and provide `package_id_override` during planning. If no default id exists and no local archive override is supplied, `plan_install` fails with the same invalid package id behavior as the current install flow.

### Plan Install

`plan_install` consumes an `InstallSourcePreparation` and caller-selected targets. It validates the selected candidate ids, package id overrides, package boundaries, source identity, current managed state, activation risks, and unmanaged overlap risks.

```rust
pub struct PlanInstallRequest {
    pub preparation: InstallSourcePreparation,
    pub targets: Vec<InstallTarget>,
}

pub struct InstallTarget {
    pub candidate_id: InstallCandidateId,
    pub package_id_override: Option<PackageId>,
    pub reinstall: bool,
}

pub struct InstallPlanSet {
    // private prepared package ownership
}

impl InstallPlanSet {
    pub fn plans(&self) -> &[InstallPlanSummary];
    pub fn risks(&self) -> &[PlanRisk];
    pub fn changes(&self) -> &[PlannedChange];
}
```

Rules:

- Multiple targets are allowed from one preparation.
- `package_id_override` is allowed for local archive and direct GitHub candidates.
- Invalid targets fail the whole planning operation.
- Planning transfers selected staged files from `InstallSourcePreparation` into `InstallPlanSet`.
- Dropping `InstallPlanSet` cleans un-applied staging.

### Apply Install

`apply_install` consumes `InstallPlanSet` and applies plans serially.

```rust
pub async fn apply_install(
    &self,
    plans: InstallPlanSet,
    options: ApplyOptions,
) -> Result<InstallReportSet>;
```

Behavior stays consistent with the current install flow:

- Apply package plans in order.
- Each package apply is its own transaction.
- If a package fails, stop immediately and return the error.
- Already successful package installs remain installed.
- Unstarted package plans are cleaned up and not attempted.
- No cross-package rollback.

`InstallReportSet` is returned only when all package plans apply successfully.

## Lower-Level Operations

`Fontbrew` also exposes lower-level operations for tests, custom frontends, and callers that want only part of the install pipeline.

```rust
impl Fontbrew {
    pub async fn fetch_install_metadata(
        &self,
        request: FetchInstallMetadataRequest,
    ) -> Result<InstallMetadata>;

    pub async fn prepare_install_asset(
        &self,
        request: PrepareInstallAssetRequest,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallSourcePreparation>;

    pub fn extract_archive(
        &self,
        request: ExtractArchiveRequest,
    ) -> Result<ExtractedArchive>;

    pub fn parse_fonts(
        &self,
        request: ParseFontsRequest,
    ) -> Result<ParsedFonts>;
}
```

Lower-level requests should not require the exact return type from the previous step unless ownership matters. For example, parsing accepts ordinary font file inputs:

```rust
pub struct ParseFontsRequest {
    pub files: Vec<FontFileInput>,
}

pub struct FontFileInput {
    pub path: PathBuf,
    pub format: Option<FontFormat>,
}
```

Extraction can return `Vec<FontFileInput>` as a convenience, but callers may construct `ParseFontsRequest` themselves from any files they control.

## Query, Remove, Config, and Update Surface

These operations should also move away from command report shells where possible.

Recommended flat methods:

```rust
impl Fontbrew {
    pub fn load_config(&self) -> Result<FontbrewConfig>;
    pub fn set_config_value(&self, request: SetConfigValueRequest) -> Result<FontbrewConfig>;

    pub fn list_packages(&self) -> Result<Vec<ManagedPackageSummary>>;
    pub fn package_info(&self, package_id: PackageId) -> Result<PackageInfo>;

    pub fn plan_remove(&self, package_id: PackageId) -> Result<RemovePlan>;
    pub async fn apply_remove(
        &self,
        plan: RemovePlan,
        options: ApplyOptions,
    ) -> Result<RemoveReport>;

    pub async fn search_fontsource(
        &self,
        request: SearchFontsourceRequest,
    ) -> Result<Vec<SearchResult>>;

    pub async fn check_updates(
        &self,
        request: CheckUpdatesRequest,
    ) -> Result<UpdateCheck>;

    pub async fn plan_update(
        &self,
        request: PlanUpdateRequest,
    ) -> Result<UpdatePlan>;

    pub async fn apply_update(
        &self,
        plan: UpdatePlan,
        options: ApplyOptions,
    ) -> Result<UpdateReport>;
}
```

Update apply should preserve current behavior: package update failures are recorded and later prepared packages continue, except cancellation still aborts the operation.

CLI-specific report envelopes such as `ListReport`, `SearchReport`, and JSON command envelopes should move toward the CLI crate over time. Domain result types stay in `fontbrew-core`.

## Migration State

1. `Fontbrew` is the single core entry type.
2. `FontbrewOptions` provides flat path options for `store_dir`, `config_path`, and `activation_dir`.
3. User preferences are read from config on each operation and are not duplicated in `FontbrewOptions`.
4. Install exposes the staged core flow through `fetch_install_metadata`, caller asset selection, `prepare_install_asset`, caller family selection, `plan_install`, caller confirmation, and `apply_install`.
5. Install-specific metadata fetch and asset preparation are public operations; archive extraction and font parsing are public lower-level operations backed by existing internals.
6. The CLI uses `Fontbrew` directly for command flows.
7. `FontbrewApp` has been removed instead of kept as a compatibility adapter.

## Verification

After implementation slices:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

Behavior that must remain covered:

- Missing config uses built-in defaults.
- Per-operation format preference overrides config without writing config.
- Ambiguous GitHub assets in the atomic prepare API fail when no selector resolves one asset.
- The higher-level asset-selection flow reuses resolved GitHub release metadata after the caller selects an asset.
- Preparation with one candidate still stops before planning.
- Local archive package id override is allowed; provider and GitHub overrides are rejected.
- Multi-target planning from one preparation does not repeat download/extract/parse.
- Dropping preparation or plan sets cleans staging.
- Batch install stops on first apply failure and keeps already installed packages.
- Update apply continues after per-package failures, matching current behavior.
