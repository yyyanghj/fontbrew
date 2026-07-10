# Changelog

All notable changes to Fontbrew will be documented in this file.

## 0.0.14 - 2026-07-11

- 移除 macOS 无法可靠生效的软链接激活策略和对应配置项；安装、重装和更新统一复制
  真实字体文件，同时兼容读取并忽略 schema v1 的旧配置字段，并保留旧 manifest 软链接
  的识别与安全卸载。事务回滚通过重命名恢复原激活产物，不会改写 legacy 产物类型。
- 统一批量安装执行入口，移除未使用的旧安装状态机、任务运行器和仅用于 CLI 包装的
  core 请求/报告类型，缩小公开 API 与维护面。
- 复用单个网络客户端，并为 Fontsource 搜索详情请求和过期检查增加有界并发，减少
  串行网络等待与重复客户端初始化。
- 安装和移除按 manifest 原子写入的提交状态决定回滚行为，避免提交状态不确定时
  错误恢复出与 manifest 不一致的本地状态。
- 修复安装与移除回滚吞掉恢复错误、提交后备份清理失败仍报告成功的问题，并确保
  激活失败只清理本事务排他创建的字体副本；多字体包失活会先完整校验并事务性暂存
  全部激活产物，失败时恢复原始状态。
- 安装、重装、移除、更新和公共失活 API 在状态已提交但清理失败时统一返回
  committed-cleanup 错误；批量更新会继续处理后续包并在结束后汇总该错误，所有
  package-store 删除失败也会保留并上报。
- manifest 提交状态不确定时统一返回顶层 commit-uncertain 错误，更新不再以 skipped
  和成功退出码掩盖可能已经落盘的新状态；跨版本重装仅在新 manifest 提交后删除旧
  package store，失败时保留旧版本。
- 保持 JSON schema v1 的激活产物 `strategy` 字段兼容，移除重复的批量安装报告
  类型；`search --limit` 现在只接受大于零的值。

## 0.0.13 - 2026-07-10

- 将 `fontbrew-core` 的单体 `FontbrewApp` 接口替换为可组合的 `Fontbrew` API，
  支持分别获取安装元数据、准备资产、选择候选 family、生成计划和执行安装，
  并公开归档解压与字体解析等底层能力。
- 统一按字体 family 生成安装候选和独立管理包；直接从 GitHub 安装时，默认
  package ID 现在由字体 family 而非仓库名称派生，单个 family 仍可使用 `--id`
  覆盖。
- 调整 CLI family 选择流程：交互模式会在发现字体后请求确认，脚本、非交互和
  JSON 模式需使用 `--family <name>` 或 `--all` 明确选择；GitHub 多资产选择会在
  下载归档前完成，并复用已解析的 release 元数据。

## 0.0.12 - 2026-07-09

- 优化直接从 GitHub 安装字体时的多资产交互，human 模式下可在多个匹配的
  release asset 中直接选择，非交互和 JSON 模式继续使用 `--asset` 明确选择。
- 支持直接 GitHub 安装时使用 `--id` 指定包 ID，便于为远端字体包设置稳定的
  本地管理名称。
- 更新 GitHub 安装资产选择和包 ID 覆盖规则文档，保持 CLI help、产品说明和
  实现说明一致。

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
