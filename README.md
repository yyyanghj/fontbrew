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
fontbrew install rsms/inter --format ttf
```

Install from a local archive:

```bash
fontbrew install ./SomeFont.zip
```

## Safety

Fontbrew does not modify system fonts. Updates and removals only apply to fonts installed through Fontbrew.

## Credits

Fontsource discovery and metadata are powered by [Fontsource](https://fontsource.org/), a collection of open-source fonts packaged for self-hosting.

## License

[MIT](LICENSE)
