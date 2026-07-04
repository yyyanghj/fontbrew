# Fontbrew

[![CI](https://github.com/yyyanghj/fontbrew/actions/workflows/ci.yml/badge.svg)](https://github.com/yyyanghj/fontbrew/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/yyyanghj/fontbrew?sort=semver)](https://github.com/yyyanghj/fontbrew/releases)
[![Platform](https://img.shields.io/badge/platform-macOS-lightgrey)](#)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A macOS font manager for the terminal.

Fontbrew installs, activates, updates, and removes open-source fonts from Fontsource, Google Fonts, GitHub Releases, local archives, and registries.

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/yyyanghj/fontbrew/main/install.sh | sh
```

## Quick Start

```bash
fontbrew search inter
fontbrew install fontsource:inter
fontbrew list
fontbrew info inter
fontbrew outdated
fontbrew update --dry-run
fontbrew remove inter
```

## Sources

```bash
fontbrew install fontsource:inter
GOOGLE_FONTS_API_KEY=... fontbrew install google:roboto
fontbrew install rsms/inter --format ttf
fontbrew install ./SomeFont.zip
```

Registry-backed installs are available through `FONTBREW_REGISTRY_URL`.

## Safety

Fontbrew does not modify system fonts. Updates and removals only apply to fonts installed through Fontbrew.

## License

[MIT](LICENSE)
