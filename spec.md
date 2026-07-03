# Fontbrew MVP Spec

## 1. Summary

Fontbrew is a macOS-only CLI package manager for third-party open-source fonts. It lets users search, install, list, update, and remove fonts as managed packages instead of loose files.

The core safety rule is:

> Fontbrew can only update or remove packages it installed and recorded in its manifest.

Fontbrew does not manage system fonts, does not adopt user-installed fonts, and does not replace Font Book. It provides package-manager discipline for a user's local font environment.

## 2. Product Name

- Product name: Fontbrew
- CLI command: `fontbrew`
- First-party registry: Fontbrew Registry
- Primary managed paths use `fontbrew` or `Fontbrew` consistently.

## 3. MVP Scope

MVP supports macOS only.

Supported:

- Install desktop fonts from the Fontbrew Registry
- Install desktop fonts from approved providers, initially Google Fonts and Fontsource
- Install desktop fonts from explicit GitHub repositories
- Install desktop fonts from local archives
- List managed packages
- Search installable packages
- Check outdated managed packages
- Update managed packages with explicit confirmation
- Remove managed packages safely
- Read font metadata to identify family, style, weight, and package boundaries

Not supported in MVP:

- Linux or Windows activation
- Project-level `fontbrew.json` or lockfile
- Rollback command or historical version retention
- Download cache for font archives
- Explicit activate/deactivate workflows
- Arbitrary GitHub search
- Managing macOS system fonts
- Adopting existing user-installed fonts
- Commercial font license management
- GUI
- Background auto-activation service
- Webfont dependency management

## 4. Core Domain Model

### Package

A package is the user-facing managed unit. Users install, list, update, and remove packages.

By default, Fontbrew groups font files with the same font family name into one package. A package may include multiple weights, styles, italics, and variants belonging to that family.

Registry recipes can override automatic family-name grouping when a source publishes variants that should be separate packages, or when related families should be installed together.

### Source

A source is an upstream location or provider that Fontbrew can resolve into one or more packages.

Supported MVP source types:

- Registry short name, for example `inter`
- Provider-qualified name, for example `google:roboto` or `fontsource:inter`
- GitHub repository, for example `rsms/inter`
- Local archive path, for example `./MapleMono.zip`

### Managed Package

A managed package is a package installed by Fontbrew and recorded in the local manifest. Only managed packages may be updated or removed by Fontbrew.

### Installed vs Activated

Installed means the package files and metadata are present in Fontbrew's managed store.

Activated means Fontbrew has exposed the installed font files to macOS through the Fontbrew-owned activation directory.

MVP `install` performs both installation and activation by default.

## 5. Filesystem Layout

Default paths:

```text
Managed store:
~/.local/share/fontbrew/

Package store:
~/.local/share/fontbrew/packages/<package-id>/<version>/

Manifest:
~/.local/share/fontbrew/manifest.json

Registry snapshot:
~/.local/share/fontbrew/registry.json

Provider metadata snapshots:
~/.local/share/fontbrew/providers/

Config:
~/.config/fontbrew/config.toml

Activation directory:
~/Library/Fonts/Fontbrew/
```

Fontbrew does not maintain a separate download cache. Install operations download directly into the managed package store or staging area. Removing a package deletes its managed store files.

## 6. Activation Strategy

MVP default activation uses symlinks:

```text
~/Library/Fonts/Fontbrew/Inter-Regular.otf
-> ~/.local/share/fontbrew/packages/inter/4.1/files/Inter-Regular.otf
```

The activation layer must support switching to copy-based activation later if macOS or font tooling compatibility requires it.

Fontbrew only manages activation artifacts inside:

```text
~/Library/Fonts/Fontbrew/
```

It must not modify:

```text
~/Library/Fonts/*
/Library/Fonts/*
/System/Library/Fonts/*
```

except for its own `~/Library/Fonts/Fontbrew/` directory.

## 7. Font Formats

MVP installable desktop font formats:

- `.ttf`
- `.otf`
- `.ttc`
- `.otc`

MVP must not activate web-only formats:

- `.woff`
- `.woff2`
- `.eot`
- `.svg`
- CSS files

If an archive contains both desktop and web assets, Fontbrew only uses desktop font files.

### Format Preference

Default global format preference:

```text
otf, ttf, ttc, otc
```

The user can configure the global preference and override it per install.

Commands:

```bash
fontbrew install inter --format otf
fontbrew install inter --format ttf
fontbrew install inter --otf
fontbrew install inter --ttf
```

Equivalent format sets can be resolved by preference. If different formats contain different family/style/weight coverage, Fontbrew must not silently choose; it must ask for explicit selection.

## 8. Registry

The Fontbrew Registry is a first-party curated recipe index. It provides stable short names and reliable install recipes for well-known fonts.

Registry v1 is a single remote JSON file:

```text
registry.json
```

The CLI stores a local registry snapshot at:

```text
~/.local/share/fontbrew/registry.json
```

The curated registry is intentionally small. Broad discovery is delegated to approved providers.

Example shape:

```json
{
  "schemaVersion": 1,
  "updatedAt": "2026-07-03T00:00:00Z",
  "packages": {
    "inter": {
      "name": "Inter",
      "source": {
        "type": "github",
        "repo": "rsms/inter"
      },
      "families": ["Inter"],
      "release": {
        "channel": "stable"
      },
      "asset": {
        "include": ["*Inter*.zip"],
        "exclude": ["*web*", "*.woff2"]
      },
      "install": {
        "formatPreference": ["otf", "ttf"]
      }
    }
  }
}
```

Short names such as `inter` only come from the first-party registry.

## 9. Search

`fontbrew search <query>` returns installable package candidates only.

Search sources in MVP:

- Fontbrew Registry
- Google Fonts
- Fontsource

Search must not perform arbitrary GitHub repository search.

Search results must resolve to explicit install sources. Example:

```text
ID              Name             Source      Install
inter           Inter            registry    fontbrew install inter
roboto          Roboto           google      fontbrew install google:roboto
source-sans-3   Source Sans 3    google      fontbrew install google:source-sans-3
```

If Fontbrew cannot install a result, it should not appear in `search`.

## 10. Provider Metadata

Fontbrew may keep local provider metadata snapshots for Google Fonts and Fontsource to reduce repeated API calls and support limited offline behavior.

Provider metadata snapshots are metadata only. They do not contain downloaded font archives or font binaries.

Default metadata refresh behavior:

- Registry/provider metadata has a 24-hour freshness window
- `--refresh` forces metadata refresh
- `--offline` uses only local snapshots

Commands that may refresh metadata when stale:

- `search`
- `install`
- `outdated`
- `update`

## 11. GitHub Sources

GitHub source syntax:

```bash
fontbrew install owner/repo
```

Default GitHub version rule:

- Select the latest non-draft, non-prerelease release
- Package version equals the selected GitHub release tag

Recipes may override release selection and asset selection.

Fontbrew must not infer package version from font file metadata by default.

### Asset Selection

Asset selection happens before parsing font files.

Rules:

- If a recipe selects an asset, use the recipe
- If the user passes an explicit asset selector, use it
- If exactly one installable font asset exists, use it
- If multiple installable assets exist and no recipe/user selector resolves the choice, fail and ask the user to choose

Example:

```text
Multiple installable assets found for subframe7536/maple-font:

1. MapleMono-TTF.zip
2. MapleMono-OTF.zip
3. MapleMono-NF-TTF.zip
4. MapleMono-CN-TTF.zip

Install with:
fontbrew install subframe7536/maple-font --asset MapleMono-OTF.zip
```

Fontbrew must not guess among multiple font assets.

## 12. Local Archives

Local archive syntax:

```bash
fontbrew install ./MapleMono.zip
```

Local archive installs are managed packages after installation.

By default, local archive installs do not have an update source, so they are not updatable.

They can still be:

- listed
- inspected
- removed
- reinstalled explicitly

Future versions may allow binding a local archive install to an upstream update source, but that is not part of MVP.

## 13. Manifest

The manifest records actual local Fontbrew-managed state. It is not a desired-state file or project lockfile.

Manifest path:

```text
~/.local/share/fontbrew/manifest.json
```

The manifest should record enough information to safely list, update, and remove packages.

Example shape:

```json
{
  "schemaVersion": 1,
  "packages": {
    "inter": {
      "id": "inter",
      "name": "Inter",
      "version": "v4.1",
      "source": {
        "type": "github",
        "repo": "rsms/inter"
      },
      "updateSource": {
        "type": "github",
        "repo": "rsms/inter"
      },
      "families": ["Inter"],
      "fontFiles": [
        {
          "path": "~/.local/share/fontbrew/packages/inter/v4.1/files/Inter-Regular.otf",
          "family": "Inter",
          "style": "Regular",
          "weight": 400,
          "format": "otf"
        }
      ],
      "activationArtifacts": [
        "~/Library/Fonts/Fontbrew/Inter-Regular.otf"
      ],
      "installedAt": "2026-07-03T00:00:00Z"
    }
  }
}
```

Only files recorded as Fontbrew-managed package state may be removed or replaced by Fontbrew.

## 14. Config

Config path:

```text
~/.config/fontbrew/config.toml
```

MVP config:

```toml
[install]
format_preference = ["otf", "ttf", "ttc", "otc"]
activation_strategy = "symlink"

[registry]
auto_update = true

[network]
metadata_ttl_hours = 24
```

Commands:

```bash
fontbrew config get install.format_preference
fontbrew config set install.format_preference otf,ttf,ttc,otc
```

Config records preferences. Manifest records installed state. They must remain separate.

## 15. Commands

### `fontbrew install <source>`

Installs and activates a package.

Supported examples:

```bash
fontbrew install inter
fontbrew install google:roboto
fontbrew install fontsource:inter
fontbrew install rsms/inter
fontbrew install ./MapleMono.zip
```

Useful flags:

```bash
--format <otf|ttf|ttc|otc>
--otf
--ttf
--asset <asset-name-or-pattern>
--reinstall
--yes
--refresh
--offline
```

Default behavior:

1. Resolve source
2. Refresh registry/provider metadata if needed
3. Select release/version
4. Select asset
5. Download directly to managed store staging area
6. Parse desktop font files
7. Determine package identity and family metadata
8. Detect conflicts
9. Show install plan when confirmation is required
10. Install into managed store
11. Activate into `~/Library/Fonts/Fontbrew`
12. Record manifest

MVP install always activates by default. `--no-activate`, `activate`, and `deactivate` are deferred even though the domain model distinguishes installed and activated state.

Repeated install of an already managed package is a no-op:

```text
Inter is already installed at v4.1.
Use `fontbrew update inter` to check for updates or `fontbrew install inter --reinstall` to reinstall.
```

Changing an installed package's source must not happen implicitly. MVP may require `remove` followed by `install` for source changes.

### `fontbrew list`

Lists managed packages only.

Example:

```text
inter          v4.1    registry    rsms/inter
maple-mono     v7.4    registry    subframe7536/maple-font
my-font        local   local       ./MyFont.zip
```

### `fontbrew info <package>`

Shows package details:

- package name
- package ID
- version
- source
- update source
- families
- installed files
- activation status
- whether Fontbrew manages it
- whether update is available

### `fontbrew search <query>`

Searches installable packages from the registry, Google Fonts, and Fontsource.

It does not search GitHub repositories.

### `fontbrew outdated`

Checks managed packages for available updates.

Default behavior:

- Refresh metadata if stale
- Use update source for each package
- Mark local archive packages without update sources as not updatable

Useful flags:

```bash
--refresh
--offline
```

Example:

```text
inter          v4.0 -> v4.1
maple-mono     v7.3 -> v7.4

Not updatable:
my-local-font  local archive, no update source
```

### `fontbrew update [package]`

Updates one package or all updatable managed packages.

`fontbrew update` means update font packages. Registry refresh is a supporting step, not the primary meaning of the command.

Default flow:

1. Refresh registry/provider metadata if configured
2. Check update sources
3. Build update plan
4. Show target versions and skipped packages
5. Ask for confirmation
6. Apply each update using conservative two-phase replacement
7. Show result summary

Useful flags:

```bash
--yes
--dry-run
--refresh
--offline
```

Example:

```text
Refreshing registry...
Checking managed packages...

The following packages will be updated:

inter          v4.0 -> v4.1
maple-mono     v7.3 -> v7.4

Skipped:
my-local-font  local archive, no update source

Continue? [y/N]
```

### `fontbrew remove <package>`

Removes a managed package.

`uninstall` is an alias:

```bash
fontbrew uninstall inter
```

Remove deletes:

- activation artifacts in `~/Library/Fonts/Fontbrew`
- package files in the managed store
- package record in the manifest

Remove must not delete:

- system fonts
- user-installed fonts outside the Fontbrew activation directory
- provider metadata snapshots
- registry snapshot
- global config
- other managed packages

### `fontbrew registry update`

Refreshes the local Fontbrew Registry snapshot.

### `fontbrew registry status`

Shows local registry snapshot status, version, and last refresh time.

### `fontbrew config get/set`

Reads or writes global Fontbrew preferences.

## 16. Conflict Handling

A conflict exists when installation or activation may overlap with fonts outside Fontbrew's management boundary.

Examples:

- The same family appears to be installed manually in `~/Library/Fonts`
- An activation artifact path already exists inside `~/Library/Fonts/Fontbrew`
- A package ID is already managed from another source

Rules:

- Fontbrew must not silently overwrite conflicts
- Fontbrew must not adopt existing non-managed fonts
- Fontbrew must not delete non-managed fonts
- Conflict continuation requires explicit user consent

Example:

```text
Warning: Inter appears to already be installed outside Fontbrew.
Fontbrew will install and activate its own managed copy in ~/Library/Fonts/Fontbrew.
It will not modify the existing font.

Continue? [y/N]
```

## 17. Update Safety

Updates use conservative two-phase replacement.

Process:

1. Keep current version active
2. Download new version to staging area
3. Parse and validate new font files
4. Verify package identity still matches the managed package
5. Prepare new activation artifacts
6. Switch activation only after validation succeeds
7. Update manifest
8. Delete old version after successful activation

If validation or activation fails:

- old version remains active
- manifest remains unchanged
- staging files are cleaned up

Package identity validation should use:

- registry package ID when available
- expected family names
- recipe rules
- provider identity

If a new release unexpectedly changes package identity, family set, or variant coverage, update must stop and require recipe changes or user intervention.

MVP does not retain old versions after successful update and does not provide rollback.

## 18. Metadata Requirements

Fontbrew must parse font metadata from desktop font files to identify:

- family name
- subfamily/style
- weight
- italic/slant when available
- format
- PostScript name when available
- full name when available

Font metadata is used for package discovery, conflict detection, display, and validation.

Font metadata is not the default source of package version.

## 19. Failure Behavior

Fontbrew should fail conservatively.

It should stop instead of guessing when:

- multiple GitHub assets are installable and no recipe/user selector resolves them
- multiple package families are discovered and no recipe resolves the intended package
- equivalent desktop format coverage cannot be proven
- update package identity does not match the managed package
- activation would overwrite an unmanaged file
- source version cannot be determined for an updatable package

Failures should explain what happened and show the next command when possible.

## 20. MVP Implementation Plan

Suggested implementation slices:

1. CLI skeleton with `install`, `list`, `remove`, `info`
2. Manifest read/write with safe file operations
3. Managed store and activation directory handling
4. Font metadata parser integration
5. Local archive install
6. GitHub release install with asset ambiguity handling
7. Registry snapshot and registry short-name install
8. Search across registry, Google Fonts, and Fontsource
9. Outdated checks
10. Conservative update flow with confirmation
11. Config file and format preference
12. Conflict detection and warning prompts

The first vertical slice should prove:

```bash
fontbrew install ./SomeFont.zip
fontbrew list
fontbrew info <package>
fontbrew remove <package>
```

## 21. Acceptance Criteria

MVP is acceptable when a macOS user can:

- install a registry package such as Inter
- search installable packages from approved sources
- install from a local archive
- install from a GitHub release when asset selection is unambiguous or explicitly selected
- see all Fontbrew-managed packages
- see source, version, families, and activation state for a package
- update managed packages after reviewing an update plan
- remove a managed package without touching non-managed fonts
- avoid accidental overwrite when the same family already exists outside Fontbrew
- run repeated installs without unexpected source changes or duplicate state

The core trust test:

```text
Can the user always tell which fonts Fontbrew manages, where they came from,
what version they are, whether they can be updated, and what will be removed?
```

If yes, the MVP meets its product goal.
