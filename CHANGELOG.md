# Changelog

All notable changes to Fontbrew will be documented in this file.

## 0.0.8 - 2026-07-05

- Changed the multi-family install flag from `--all-families` to `--all` with
  short form `-a`.
- Removed legacy `--otf` and `--ttf` install flags; use repeated `--format`
  values instead.
- Fixed Fontsource package family reporting so provider installs and updates
  preserve the provider family name even when font metadata uses a different
  style-linked family.

## 0.0.7 - 2026-07-05

- Added header separators to human-readable tables for easier scanning.
- Improved `fontbrew self-update` so downloads and verification happen before
  taking the replacement lock, then re-checks the installed version before
  replacing the binary.
- Fixed Fontsource installs to preserve provider variant weights in managed font
  metadata.

## 0.0.6 - 2026-07-05

- Fixed `fontbrew list` human output so packages with many recorded families
  stay aligned by showing a concise family summary.

## 0.0.5 - 2026-07-05

- Reduced supported source kinds to Fontsource, GitHub Releases, and local
  archives.
- Changed unprefixed install IDs such as `fontbrew install inter` to resolve as
  exact Fontsource IDs.
- Improved provider-backed search results and the human-readable reporter
  headers.
- Removed registry and Google Fonts source support from the built-in sources.

## 0.0.4 - 2026-07-05

- Changed `fontbrew info` human output to show a concise package summary and a
  per-font status table with weight, italic, installed, and activated state.
- Removed default long managed file and activation artifact paths from
  `fontbrew info`; use verbose output when those details are needed.

## 0.0.3 - 2026-07-05

- Fixed multi-family install progress so repeated planning for the same source
  reports `Resolving` once.
- Fixed desktop format selection to apply the configured preference even when
  OTF and TTF coverage differs.
- Improved interactive family selection by making checked and unchecked states
  more visible.

## 0.0.2 - 2026-07-05

- Added `fontbrew self-update` for standalone release binaries.
- Added family selection for direct GitHub and local archive installs when a
  source contains multiple font families.
- Added non-interactive `install --family <name>` and `install --all-families`
  options.
- Fixed direct GitHub updates for packages installed from one family inside a
  multi-family source.
- Improved JSON errors for multi-family install sources by returning structured
  candidate family names.

## 0.0.1 - 2026-07-04

Initial public release.

- Added the `fontbrew` macOS CLI for installing, listing, inspecting, updating,
  and removing managed fonts.
- Added install support for local archives, GitHub Releases, and Fontsource.
- Added human-readable and JSON output modes.
- Added GitHub Actions CI and automated GitHub Release publishing.
- Added one-line installer, MIT license, and release archives for Apple Silicon
  and Intel Macs.
