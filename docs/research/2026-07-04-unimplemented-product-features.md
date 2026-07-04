# Fontbrew 未实现功能盘点

日期：2026-07-04

## 范围

本盘点基于以下材料对照：

- 交接文档：`/var/folders/xh/gvfmyldx403f4fl4wjgmbbjw0000gn/T/fontbrew-handoff-2026-07-04-final.md`
- 产品规格：[`../product_spec.md`](../product_spec.md)
- 实现计划：`docs/superpowers/plans/2026-07-04-fontbrew-mvp.md`
- 实现设计和 ADR：`docs/implementation-design.md`、`docs/adr/*.md`
- 当前源码：`crates/fontbrew-core`、`crates/fontbrew-cli`

交接文档明确写到“当前计划没有已知剩余 MVP 实现任务”，并记录最终验证通过；因此这里不把 GUI、跨平台、rollback、显式 activate/deactivate、任意 GitHub 搜索等产品规格明确列为非 MVP 的内容算作缺口。

## 总结

MVP 主线能力已经基本实现：registry/GitHub/local/provider 安装，list/info/search/outdated/update/remove，manifest 管理，symlink activation，JSON 输出，配置读写，冲突保护和更新安全流程都有源码和测试覆盖。

仍然没完全实现的功能主要集中在三类：

- metadata 自动刷新策略：配置项和部分旧 flag 存在，但需要联网的命令还没有统一默认刷新 registry/provider metadata。
- registry recipe 的 package boundary 覆盖：文档说 recipe 可以覆盖 family grouping，但安装实现没有用 recipe families 约束或筛选安装边界。
- 若干规格/设计层细节：copy activation、local archive `--id` 等还没有产品化。

## 未实现或部分实现清单

### 1. Registry/provider metadata 的自动刷新

状态：部分实现。

计划/规格要求：

- 产品规格写明需要 registry/provider metadata 的命令应自动刷新 metadata，不再暴露用户可见的 refresh/offline 模式。
- 产品规格的 install/outdated 流程也需要使用最新 registry/provider metadata。

当前实现：

- 配置项已经存在：`registry.auto_update`、`network.metadata_ttl_hours`、`network.update_concurrency` 可以读写（`crates/fontbrew-core/src/config/mod.rs:210-280`）。
- 旧的 search refresh 参数只调用 `registry_update()`，没有刷新 provider metadata；这属于旧 flag 路径，应被默认刷新行为替代（`crates/fontbrew-core/src/app/mod.rs:189-198`）。
- Fontsource/Google provider 目前仍有 offline search 分支；这属于旧用户模式，应收敛为默认联网刷新 metadata（`crates/fontbrew-core/src/providers/mod.rs:57-98`、`crates/fontbrew-core/src/providers/mod.rs:169-187`）。
- `install_plan_with_cancellation` 直接解析 registry snapshot 或 provider/GitHub 源，没有看到 `request.refresh` 或 `registry.auto_update` 的消费路径（`crates/fontbrew-core/src/app/mod.rs:56-109`）。

影响：

- 用户能手动 `fontbrew registry update`，但需要联网或 metadata 的命令还没有统一默认刷新 registry/provider metadata。
- `network.metadata_ttl_hours` 和 `registry.auto_update` 当前更像已持久化的旧策略配置，不是完整生效的产品能力。

建议：

- 删除用户可见 refresh/offline 模式。
- 在 `search/install/outdated` 的 source resolution 前统一默认刷新 registry/provider metadata。
- 保留 provider metadata snapshot 作为内部缓存/记录，不作为用户可选 offline 模式。

### 2. refresh/offline CLI surface 需要移除

状态：部分实现。

计划/规格要求：

- 产品规格不再规划手动 refresh/offline 用户 flag。

当前实现：

- CLI 仍有部分手动 refresh/offline 参数和 request 字段（`crates/fontbrew-cli/src/cli/mod.rs:86-162`）。

影响：

- CLI help 仍可能暴露已经不符合产品方向的手动 refresh/offline 模式。

建议：

- 删除 `InstallArgs`、`SearchArgs`、`OutdatedArgs` 中的 refresh/offline 用户参数。
- 删除或收敛 core request 中只服务于用户 refresh/offline 模式的字段。
- 用默认刷新行为覆盖原先需要用户手动选择的路径。

### 3. Registry recipe 尚未真正覆盖 package boundary

状态：部分实现。

计划/规格要求：

- 产品规格说默认按 font family 分包，但 registry recipes 可覆盖 automatic family-name grouping。
- ADR 0001 也明确 recipe 可在一个 archive 发布多个用户可见 variant，或需要把多个相关 family 作为一个 package 时覆盖边界（`docs/adr/0001-package-boundaries-use-family-name-with-recipe-overrides.md:1-3`）。
- 产品规格还要求当发现多个 package families 且没有 recipe 解决意图时停止猜测。

当前实现：

- registry record 保存 `families`，但 recipe 生成后主要用于 search 展示；安装路径使用 recipe 的 package id、asset selector 和 format preference，没有使用 recipe families 来筛选或校验安装边界（`crates/fontbrew-core/src/registry/mod.rs:250-330`、`crates/fontbrew-core/src/app/mod.rs:206-215`）。
- archive 解析会收集所有 family，然后在没有 `package_id_hint` 时取排序后的第一个 family 生成 package id；没有在多 family 且无 recipe 时直接拒绝（`crates/fontbrew-core/src/install/mod.rs:1090-1135`）。
- 安装记录会保留 selected files 中的所有 families（`crates/fontbrew-core/src/install/mod.rs:1158-1207`）。

影响：

- 对简单单 family 包没问题。
- 对一个 release/archive 内含多个独立 family 或多个 variant 的情况，registry 现在主要依赖 asset include/exclude 和 format preference 解决，不能表达“只安装这些 family”或“这些 family 合并为一个 package”的完整边界规则。

建议：

- 扩展 registry install recipe，明确 package boundary 语义，例如 `includeFamilies`、`excludeFamilies`、`expectedFamilies` 或 variant grouping。
- 本地/GitHub 显式安装在发现多个 family 且没有 recipe/用户选择时，按 spec 返回保守错误。
- 更新 identity validation 时同时使用 recipe family rules，而不只依赖 manifest 中旧 families。

### 4. `install` 需要默认刷新 registry/provider metadata

状态：部分实现。

计划/规格要求：

- `fontbrew install <source>` 默认流程要求在需要时刷新 registry/provider metadata，不再通过手动 refresh 参数暴露给用户。

当前实现：

- CLI 已暴露旧的 install refresh 参数（`crates/fontbrew-cli/src/cli/mod.rs:86-120`）。
- `InstallRequest` 包含旧的 `refresh` 字段，但 `FontbrewApp::install_plan_with_cancellation` 分发到 local/registry/GitHub/provider install 时没有统一默认刷新 registry/provider metadata（`crates/fontbrew-core/src/app/mod.rs:56-109`）。
- provider install 当前总是在线 fetch detail；registry short-name install则依赖已有 snapshot。

影响：

- 对 registry short-name install，不会先默认更新 registry snapshot。
- 对 provider install，metadata 刷新行为没有和 registry 刷新形成统一默认策略。

建议：

- 在 registry/provider source resolution 前默认刷新所需 metadata。
- 删除旧 refresh/offline flag 的冲突处理需求。

### 5. Copy activation 只预留，未实现

状态：明确延期，但目前存在可配置入口。

计划/规格要求：

- MVP 默认 symlink activation；产品规格要求 activation layer 保留未来切换 copy strategy 的空间。

当前实现：

- `install.activation_strategy` 是可读写配置项（`crates/fontbrew-core/src/config/mod.rs:210-280`）。
- parser 接受 `copy`（`crates/fontbrew-core/src/config/mod.rs:340-344`）。
- 但 apply/deactivate 遇到 `ActivationStrategy::Copy` 会返回 `NotImplemented`（`crates/fontbrew-core/src/activation/mod.rs:124-149`）。

影响：

- 如果用户手动 `fontbrew config set install.activation_strategy copy`，后续安装/卸载会进入未实现路径。
- README 只展示设置为 `symlink`，所以正常路径不受影响。

建议：

- 短期：在 config set 阶段拒绝 `copy`，或标注 experimental 并给清晰错误。
- 中期：实现 copy activation/deactivation，并补齐 remove/update 的 copy transaction 测试。

### 6. Local archive 缺少 `--id` 兜底

状态：未实现，属于 implementation-design 层建议。

计划/规格要求：

- `docs/implementation-design.md` 写到：如果 local archive 不能生成安全 package id，CLI 应该让用户用 `--id` 提供（`docs/implementation-design.md:546-565`）。

当前实现：

- CLI `install` 参数没有 `--id`（`crates/fontbrew-cli/src/cli/mod.rs:86-120`）。
- local archive 无 package id hint 时用第一个 family name normalize 成 package id；normalize 失败会直接报错（`crates/fontbrew-core/src/install/mod.rs:1125-1135`）。

影响：

- 对 family name 含非 ASCII、特殊字符或无法安全 slug 化的本地字体包，用户没有 CLI 兜底入口。

建议：

- 增加 `fontbrew install ./font.zip --id <package-id>`。
- 只允许对 local archive 或明确安全的 manual source 使用，避免 registry/provider identity 被用户随意改写。

### 7. `registry status` 不显示 schema version

状态：小缺口。

计划/规格要求：

- 产品规格写到 `fontbrew registry status` 应展示 local registry snapshot status、version 和 last refresh time。

当前实现：

- `RegistryStatusReport` 包含 available、snapshot path、registry updated at、snapshot modified at、package count，但不包含 schema version（`crates/fontbrew-core/src/model/mod.rs:676-683`）。

影响：

- 不影响安装或安全性，但和 status 的“version”展示要求不完全一致。

建议：

- 在 `RegistryStatusReport` 加 `schema_version`，human/JSON reporter 一起输出。

## 不算未实现的项目

以下内容在产品规格中明确不是 MVP，或已经有当前等价实现，所以不列为缺口：

- 产品名：当前规格和源码使用 `Fontbrew/fontbrew`，不列为功能缺口。
- `uninstall`：当前 CLI 使用 `remove` 作为主命令，并提供 `uninstall` alias（`crates/fontbrew-cli/src/cli/mod.rs:60-64`）。
- GUI、跨平台 activation、rollback、download cache、项目级 lockfile、显式 activate/deactivate、任意 GitHub 搜索、商业字体授权管理：均为非 MVP。
- 安全主线：不管理系统字体、不接管手动字体、不删除非 managed 字体、冲突需要显式 consent，这些在产品规格中是核心边界，当前源码和测试已有覆盖。

## 优先级建议

1. 补齐默认 metadata refresh policy：这是最容易让用户感知“registry 没自动更新”的缺口。
2. 删除 refresh/offline 用户 flag：和第一项一起做，避免继续扩大旧 CLI surface。
3. 明确 registry package boundary recipe 语义：这是以后支持 Maple Mono、Nerd Font、CN/NF 等复杂发行包时的关键能力。
4. 处理 copy activation 配置入口：要么禁用，要么实现，避免用户配置到未实现状态。
5. 增加 local archive `--id` 和 registry status schema version：较小但能提升边界完整度。
