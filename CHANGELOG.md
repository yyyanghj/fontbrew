# Changelog

All notable changes to Fontbrew will be documented in this file.

## 0.0.2 - 2026-07-05

- Added `fontbrew self-update` for standalone release binaries.
- Added family selection for direct GitHub and local archive installs when a source contains multiple font families.
- Added non-interactive `install --family <name>` and `install --all-families` options.
- Fixed direct GitHub updates for packages installed from one family inside a multi-family source.
- Improved JSON errors for multi-family install sources by returning structured candidate family names.

## 0.0.1 - 2026-07-04

Initial public release.

- Added the `fontbrew` macOS CLI for installing, listing, inspecting, updating, and removing managed fonts.
- Added install support for local archives, registry entries, GitHub Releases, Fontsource, and Google Fonts.
- Added human-readable and JSON output modes.
- Added GitHub Actions CI and automated GitHub Release publishing.
- Added one-line installer, MIT license, and release archives for Apple Silicon and Intel Macs.
