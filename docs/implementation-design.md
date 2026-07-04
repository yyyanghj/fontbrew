# Fontbrew Implementation Design

Fontbrew is a Rust 2021 workspace with a reusable core crate and a thin CLI crate.

## Crates

- `crates/fontbrew-core`: source resolution, provider metadata, archive handling, font parsing, manifest persistence, activation, update planning, config, and app tasks.
- `crates/fontbrew-cli`: argument parsing, confirmation flow, progress rendering, exit mapping, and human/JSON reporters.

## Source Resolution

`InstallSource` supports three install paths:

- `Provider { provider: Fontsource, id }`
- `GitHubRepo { owner, repo }`
- `LocalArchive { path }`

CLI parsing treats unprefixed names as exact Fontsource IDs. The explicit `fontsource:<id>` prefix maps to the same provider source. `owner/repo` maps to GitHub. Existing local filesystem paths ending in `.zip` map to local archive sources.

Install source parsing should stay conservative:

- Do not fuzzy-resolve install IDs.
- Do not infer GitHub from arbitrary URLs.
- Do not accept provider prefixes that the model does not support.

## Core Modules

- `app.rs`: orchestrates high-level use cases and keeps request/response models stable for CLI and tests.
- `providers.rs`: Fontsource list/detail metadata, metadata snapshots, search, and provider asset download requests.
- `github.rs`: GitHub release lookup, release asset filtering, asset selector matching, and release metadata.
- `archive.rs`: archive extraction and format filtering.
- `font.rs`: desktop font metadata parsing and family/style detection.
- `install.rs`: install plan construction, staging, package identity validation, and manifest record creation.
- `update.rs`: update planning and two-phase replacement.
- `manifest.rs`: manifest schema and persistence.
- `activation.rs`: Fontbrew-owned activation artifacts.
- `config.rs`: user configuration for install format preference, activation strategy, metadata TTL, and update concurrency.
- `tasks.rs`: filesystem lock handling and app-level task helpers.

## Provider Metadata

Fontsource metadata is cached under the provider metadata directory as JSON snapshots. The snapshots contain only metadata. Font files downloaded during install or update go through staging and then into the managed package store.

Metadata refresh should be implementation detail. Commands that need Fontsource data may use fresh snapshots when valid and should fall back to stale snapshots when a refresh fails and stale metadata can still answer safely.

## GitHub Release Assets

GitHub package versions use the selected release tag. The resolver chooses the latest stable release by default. An asset is installable when it is an archive containing supported desktop font files.

If multiple installable assets are possible, planning fails unless the user provides an explicit asset selector. The selector is a user-facing disambiguation tool for direct GitHub installs and must not be persisted as a secret or credential.

## Local Archives

Local archives are copied or read through staging and parsed with the same archive and font pipeline as remote archives. Local archives have no update source.

Package ID override is allowed for local archives only. Provider and GitHub identities come from their source model and parsed package metadata.

## Package Boundary

Fontbrew groups parsed font files by font family. A single-family source can plan directly. A multi-family GitHub or local archive requires explicit family selection when non-interactive.

Selected families become the package boundary recorded in the manifest. Update validation reuses the manifest family boundary to avoid silently replacing a package with unrelated font files.

## Manifest

The manifest records local package state:

- package ID
- installed version
- source
- optional update source
- managed font file paths
- family names
- activation artifacts
- installed timestamp

The manifest is local machine state, not a desired-state lockfile.

## Update Flow

Update planning:

1. Read manifest.
2. Resolve each package's update source.
3. Download candidate assets into staging.
4. Parse candidate desktop font files.
5. Validate candidate identity against manifest family boundaries and source identity.
6. Build a plan that either prepares a replacement or records a per-package failure.

Update apply:

1. Install candidate files into the managed package store.
2. Switch Fontbrew-owned activation artifacts.
3. Atomically update the manifest.
4. Remove the old managed package store version after success.

Dry-run update reports the planned changes without mutating manifest, activation artifacts, or package store state.

## Safety Boundaries

- All writes stay under Fontbrew-owned paths.
- Staging is cleaned after failed or cancelled work.
- Activation conflicts with non-managed fonts require consent.
- `GITHUB_TOKEN` is read from the environment and never persisted.
- JSON stdout must contain only JSON payloads.

## Verification

Use temp-directory tests for filesystem behavior. Network behavior must use fake HTTP clients or explicit manual verification.

Required local checks before release-facing changes:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```
