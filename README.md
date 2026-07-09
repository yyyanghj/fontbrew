# Fontbrew

[![CI](https://github.com/yyyanghj/fontbrew/actions/workflows/ci.yml/badge.svg)](https://github.com/yyyanghj/fontbrew/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/yyyanghj/fontbrew?sort=semver)](https://github.com/yyyanghj/fontbrew/releases)
[![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)](#)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A macOS font manager for the terminal.

Fontbrew installs, activates, updates, and removes open-source fonts from Fontsource, GitHub Releases, and local archives.

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/yyyanghj/fontbrew/main/install.sh | sh
```

## Quick Start

Search for a font:

```bash
fontbrew search inter
```

Install and activate it:

```bash
fontbrew install inter
```

Install asks you to confirm the discovered font family. For scripts or JSON mode,
pass `--family <name>` or `--all`.

Prefer a format for this install:

```bash
fontbrew install inter --format ttf
```

See what Fontbrew manages:

```bash
fontbrew list
fontbrew info inter
```

Check what would change before updating:

```bash
fontbrew outdated
fontbrew update --dry-run
```

Remove it when you no longer need it:

```bash
fontbrew remove inter
```

## Sources

Install from Fontsource:

```bash
fontbrew install inter
fontbrew install fontsource:inter
```

Unprefixed names are exact Fontsource IDs. Use `fontsource:<id>` when you want to be explicit.

Install from a GitHub Release:

```bash
fontbrew install rsms/inter --format ttf --all
```

Each selected font family is installed as its own package. Its default package ID comes from the family name, not the GitHub repository name.

If a release has more than one installable zip asset, select one by name or glob:

```bash
fontbrew install owner/repo --asset "*desktop*.zip"
```

Install from a local archive:

```bash
fontbrew install ./SomeFont.zip
```

Choose one family or install every discovered family without prompting:

```bash
fontbrew install ./SomeFont.zip --family "Some Font"
fontbrew install ./SomeFont.zip --all
```

Local archives can use an explicit package ID:

```bash
fontbrew install ./SomeFont.zip --id some-font --all
```

## Configuration

Fontbrew stores user preferences in `~/.config/fontbrew/config.toml`.

```bash
fontbrew config get install.format_preference
fontbrew config set install.format_preference ttf,otf
fontbrew config set network.update_concurrency 2
```

Supported config keys are:

- `install.format_preference`
- `install.activation_strategy`
- `network.metadata_ttl_hours`
- `network.update_concurrency`

`install.activation_strategy` supports `copy` and `symlink`. `copy` is the default and writes real font files into Fontbrew's activation directory.

## Output

Use `--json` for machine-readable output. JSON mode writes only structured JSON to stdout.

```bash
fontbrew --json list
```

Use `--quiet` to suppress progress and warnings, and `-v` for more detailed progress.

## Self Update

Standalone release binaries can update themselves:

```bash
fontbrew self-update --dry-run
fontbrew self-update --yes
```

## Safety

Fontbrew does not modify system fonts. Updates and removals only apply to fonts installed through Fontbrew.

## Credits

Fontsource discovery and metadata are powered by [Fontsource](https://fontsource.org/), a collection of open-source fonts packaged for self-hosting.

## License

[MIT](LICENSE)
