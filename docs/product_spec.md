# Fontbrew Product Spec

Fontbrew is a macOS terminal font manager. It installs desktop font files into a Fontbrew-owned managed store, activates them in a Fontbrew-owned user-font directory, tracks local state in a manifest, and updates or removes only packages it installed.

## Goals

- Install and activate open-source desktop fonts from three source kinds:
  - Fontsource by exact ID, with unprefixed input such as `fontbrew install inter`.
  - Explicit GitHub repositories such as `fontbrew install rsms/inter`.
  - Local `.zip` archives such as `fontbrew install ./MapleMono.zip`.
- Keep `fontsource:<id>` as an explicit provider prefix for users and scripts that prefer source clarity.
- Search Fontsource for installable desktop font candidates.
- List, inspect, update, and remove Fontbrew-managed packages.
- Persist user preferences for format selection, metadata freshness, and update concurrency.
- Keep human output readable and keep JSON output structured and machine-readable.
- Avoid touching fonts that were not installed by Fontbrew.

## Non-Goals

- Full webfont dependency management.
- Arbitrary GitHub repository search.
- Project-level lockfiles.
- Cross-platform activation in the MVP.
- Rollback or long-term retention of old package versions.

## Source Model

### Fontsource

Fontsource is the default install source for unprefixed package names. `fontbrew install inter` means "install the Fontsource package whose exact ID is `inter`." Fontbrew does not fuzzy-resolve install IDs.

Users may write the same source explicitly:

```bash
fontbrew install fontsource:inter
```

Search queries go to Fontsource and return only candidates that Fontbrew can install as desktop fonts. Fontsource metadata may be cached locally as provider metadata, but downloaded font files are package state, not reusable cache entries.

### GitHub Repositories

GitHub sources use `owner/repo` syntax:

```bash
fontbrew install rsms/inter
```

Fontbrew resolves the latest stable release, selects an installable archive asset, downloads it, parses contained desktop font files, and records the GitHub repo as the update source. If multiple assets match, the user must select one with an asset selector.

### Local Archives

Local archive sources use paths to `.zip` files:

```bash
fontbrew install ./SomeFont.zip
```

Local archives have no upstream update source. A local archive may use an explicit package ID override when the parsed font metadata does not produce the desired package identity.

CLI source parsing treats explicit local paths as local before trying provider or GitHub resolution. Inputs beginning with `.`, beginning with `/`, containing a backslash, or ending in `.zip` are local paths. Other `owner/repo` inputs resolve as GitHub repositories only when they satisfy the conservative GitHub syntax rules.

## Package Identity

Fontbrew manages packages, not loose font files. By default, package identity comes from the parsed font family name. Provider packages use the provider ID as the package ID. GitHub and local archives may need explicit family selection when one archive contains multiple independent families.

For multi-family GitHub and local archives:

- Interactive human mode may ask the user to select one or more families.
- Non-interactive and JSON mode require explicit `--family` or `--all`.
- `--yes` approves risk prompts but does not silently choose a family boundary.
- `--id` is allowed for local archives only and cannot be combined with `--family` or `--all`.

## Supported Formats

Fontbrew installs desktop font formats:

- `.ttf`
- `.otf`
- `.ttc`
- `.otc`

Web-only formats such as `.woff` and `.woff2` are ignored for activation.

Format preference is applied in this order: per-command `--format` values, configured `install.format_preference`, then the built-in OTF, TTF, TTC, OTC fallback order. Repeated format entries are deduplicated while preserving the first occurrence.

## Update Behavior

Fontbrew updates a managed package by resolving its recorded update source, downloading the candidate version into staging, parsing the candidate font files, validating package identity, and only then replacing the active package. Failed updates leave the current package active.

Fontsource packages update through Fontsource detail metadata. GitHub packages update through GitHub Releases. Local archive packages are reported as not updatable unless a future source kind gives them an update source.

`fontbrew update --jobs <n>` controls package-level prepare concurrency. Apply work remains serialized under the global file lock.

## Configuration

Fontbrew config lives at `~/.config/fontbrew/config.toml` and uses schema version `1`.

Supported CLI config keys:

- `install.format_preference`: ordered list of `otf`, `ttf`, `ttc`, and `otc`.
- `install.activation_strategy`: currently `symlink`; `copy` is reserved but rejected until implemented.
- `network.metadata_ttl_hours`: positive integer metadata snapshot TTL.
- `network.update_concurrency`: positive integer default for `fontbrew update --jobs`.

## Safety

- Fontbrew writes only under Fontbrew-owned data, staging, package-store, manifest, provider-metadata, and activation paths.
- Activation conflicts with non-managed fonts require explicit consent.
- Remove deletes managed package files and Fontbrew-owned activation artifacts only.
- Credentials such as `GITHUB_TOKEN` are read from environment variables and are never persisted.

## CLI Output

Human command results go to stdout. Progress, prompts, warnings, diagnostics, and errors go to stderr.

JSON mode emits only structured JSON on stdout. It must not mix progress text, prompts, or diagnostics into stdout.

## Commands

- `fontbrew search <query>` searches Fontsource.
- `fontbrew install <source>` installs from Fontsource, GitHub, or local zip.
- `fontbrew list` lists managed packages.
- `fontbrew info <package-id>` shows manifest and file details for one package.
- `fontbrew outdated` reports packages with update sources and newer available versions.
- `fontbrew update [package-id...]` updates managed packages.
- `fontbrew remove <package-id>` removes managed packages.
- `fontbrew uninstall <package-id>` is an alias for `remove`.
- `fontbrew config get <key>` prints a known config key.
- `fontbrew config set <key> <value>` persists a known config key.
- `fontbrew self-update` updates the Fontbrew binary from the project release channel.
