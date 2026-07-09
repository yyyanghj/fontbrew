# Changelog

All notable changes to Fontbrew will be documented in this file.

## 0.0.11 - 2026-07-09

- 将默认字体激活方式从软链改为复制真实字体文件，激活产物仍保留在
  `~/Library/Fonts/Fontbrew` 下，避免污染用户字体目录根路径。
- 保留 `symlink` 激活策略兼容旧 manifest 和显式配置，同时允许
  `install.activation_strategy = "copy"` 正常读写。
- 改进重新安装、更新和回滚流程，确保 copy 激活失败时清理新产物并恢复旧的
  Fontbrew 管理字体。

## 0.0.10 - 2026-07-08

- 为安装、移除、搜索、检查更新、自更新等可能耗时的 CLI 阶段增加活动
  loading 提示，TTY 下使用动态 spinner，非 TTY verbose 输出保持可读日志。
- 优化多 family 安装流程，交互式选择 family 后复用第一次下载和解析得到的
  pending archive，避免 GitHub Release 资产被重复下载。
- 优化批量 family 安装计划生成，同一 archive 只下载和解析一次，并合并扫描
  已安装字体风险，减少重复解析和重复扫描开销。
- 改进安装进度阶段展示，显式区分准备安装计划、检查已安装字体和实际安装阶段。

## 0.0.9 - 2026-07-07

- 修复 GitHub API 限流时的错误提示，明确说明可通过 `GITHUB_TOKEN`
  使用认证请求，并保留 GitHub 文档链接。
- 优化 GitHub Release 多个可安装资产匹配时的错误提示，提示用户使用
  `--asset <name-or-glob>` 选择资产，并列出匹配项示例。

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
