# Fontbrew

Fontbrew is a macOS-only CLI for managing third-party open-source fonts as local packages. It installs fonts into a Fontbrew-managed store, activates them through a Fontbrew-owned directory under the user's font folder, and records installed state in a manifest.

The core safety rule is:

```text
Fontbrew can only update or remove packages it installed and recorded in its manifest.
```

## Status

This repository contains the MVP implementation:

- local archive installs
- registry short-name installs
- explicit GitHub release installs
- Fontsource and Google Fonts provider sources
- list, info, search, outdated, update, remove, config, and registry commands
- human and JSON reporters
- temp-path-heavy tests for filesystem behavior

The MVP targets macOS. It does not manage system fonts, adopt existing user fonts, or retain historical package versions.

## Build

```bash
cargo build --workspace
target/debug/fontbrew --help
```

During development you can also run:

```bash
cargo run -p fontbrew-cli -- list
```

## Quick Start

Install from a local archive:

```bash
fontbrew install ./SomeFont.zip
fontbrew list
fontbrew info some-font
fontbrew remove some-font
```

Search provider packages:

```bash
fontbrew search inter
fontbrew install fontsource:inter
```

Use a registry package when a registry URL is configured:

```bash
FONTBREW_REGISTRY_URL=file:///path/to/registry.json fontbrew registry update
FONTBREW_REGISTRY_URL=file:///path/to/registry.json fontbrew install inter --format ttf
```

Install from explicit sources:

```bash
fontbrew install rsms/inter --format ttf
fontbrew install fontsource:inter
GOOGLE_FONTS_API_KEY=... fontbrew install google:roboto
```

When a source publishes multiple desktop formats with different family/style coverage, Fontbrew refuses to guess. Use `--format otf`, `--format ttf`, `--otf`, or `--ttf` to select the intended desktop format. For example, the Inter GitHub release requires an explicit format in the current registry recipe.

Check and prepare updates:

```bash
fontbrew outdated
fontbrew update --dry-run
fontbrew update --yes
```

`update --dry-run` prepares and reports the update plan without applying changes. `update --yes` applies approved changes without an interactive prompt.

## Sources

Fontbrew accepts these MVP source forms:

- registry package ID: `inter`
- provider package ID: `fontsource:inter` or `google:roboto`
- GitHub repository: `owner/repo`
- local archive path: `./SomeFont.zip`

Google Fonts requires `GOOGLE_FONTS_API_KEY` in the environment for `google:<id>` search and install. The key is intentionally not stored in config, manifests, or registry snapshots.

GitHub API requests can use `GITHUB_TOKEN` from the environment when available. The token is not persisted.

Fontbrew ships an empty default registry snapshot. If `FONTBREW_REGISTRY_URL` is not set, commands read the local snapshot without refreshing it. When `FONTBREW_REGISTRY_URL` is set, `registry update` and registry-backed commands download and validate that JSON into the same local snapshot path. The value can be an HTTP(S) URL or a `file://` path, and it is not persisted.

## Filesystem Layout

Default paths:

```text
Managed store:              ~/.local/share/fontbrew/
Package store:              ~/.local/share/fontbrew/packages/<package-id>/<version>/
Manifest:                   ~/.local/share/fontbrew/manifest.json
Registry snapshot:          ~/.local/share/fontbrew/registry.json
Provider metadata snapshots: ~/.local/share/fontbrew/providers/
Config:                     ~/.config/fontbrew/config.toml
Activation directory:       ~/Library/Fonts/Fontbrew/
```

Fontbrew only manages activation artifacts inside `~/Library/Fonts/Fontbrew/`. Remove and update operations use the manifest and do not delete non-Fontbrew fonts.

## Config

Known config keys:

```bash
fontbrew config get install.format_preference
fontbrew config set install.format_preference "ttf,otf"

fontbrew config get install.activation_strategy
fontbrew config set install.activation_strategy symlink

fontbrew config get registry.auto_update
fontbrew config set registry.auto_update false

fontbrew config get network.metadata_ttl_hours
fontbrew config set network.metadata_ttl_hours 24

fontbrew config get network.update_concurrency
fontbrew config set network.update_concurrency 4
```

Defaults are `otf,ttf,ttc,otc`, symlink activation, registry auto-update enabled, a 24-hour metadata TTL, and update concurrency of 4.

## Output

Human command results are written to stdout. Human progress, warnings, prompts, diagnostics, and errors are written to stderr.

Use `--json` for machine-readable output:

```bash
fontbrew --json list
fontbrew --json info inter
fontbrew --json outdated
fontbrew --json update --dry-run
```

JSON mode writes only JSON to stdout and includes `schemaVersion`. It does not prompt interactively; commands requiring approval must use `--yes`, `--dry-run`, or fail with a structured JSON error.

## Verification

The final MVP verification pass is recorded in [`docs/mvp-verification.md`](docs/mvp-verification.md).

Core commands:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

Fixture font sources and licenses are documented in [`fixtures/fonts/README.md`](fixtures/fonts/README.md).
