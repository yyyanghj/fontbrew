# macOS Activation Spike

Date: 2026-07-04

## Question

Can Fontbrew keep symlink activation as the default by placing package font symlinks in `~/Library/Fonts/Fontbrew`, or does macOS require a copy-based activation strategy?

## What was verified

- Core symlink creation and removal were verified with tempdir tests.
- The spike created `~/Library/Fonts/Fontbrew/Fontbrew-Spike-SourceCodePro-Regular.ttf` as a symlink to `fixtures/fonts/SourceCodePro-Regular.ttf`.
- The symlink target was confirmed with `readlink`.
- `system_profiler SPFontsDataType` was queried while the symlink was present, searching for `Source Code`, `SourceCode`, and `Fontbrew-Spike`.
- The symlink was removed immediately after the check.

## Result

`system_profiler SPFontsDataType` did not report the symlinked Source Code Pro fixture while the symlink was present.

This does not safely prove that every macOS application ignores symlinked fonts in `~/Library/Fonts/Fontbrew`; it only shows that this non-private, no-cache-reset check did not observe the symlinked fixture as loaded.

A manual Font Book check on 2026-07-09 also did not show Fontbrew-managed symlink-only activations from `~/Library/Fonts/Fontbrew`.

## Constraints

- Global font caches were not cleared.
- Private APIs were not used.
- No automation attempted to inspect app-specific font menus.
- Tests do not touch `~/Library/Fonts`; they verify activation behavior only in temp directories.

## Decision

Promote `ActivationStrategy::Copy` to the default activation strategy while keeping activation artifacts under `~/Library/Fonts/Fontbrew`. Keep `ActivationStrategy::Symlink` implemented for compatibility with older manifests and explicit user configuration, but do not rely on symlink activation as the default macOS behavior.
