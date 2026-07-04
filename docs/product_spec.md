# Fontbrew Product Spec

## 1. 产品定义

Fontbrew 是一个面向 macOS 的第三方开源字体包管理器。它通过 CLI 让用户像管理软件包一样管理字体：搜索、安装、查看、更新、移除，并清楚知道每个被管理字体来自哪里、当前版本是什么、是否可以更新、移除时会删除哪些文件。

一句话定位：

```text
Fontbrew brings package-manager discipline to third-party open-source fonts on macOS.
```

Fontbrew 管理的是字体 package，而不是零散字体文件。用户操作的对象应该是 `inter`、`maple-mono`、`source-code-pro` 这样的 package，而不是手动处理一堆 `.ttf` 或 `.otf` 文件。

Fontbrew 的核心信任规则是：

```text
Fontbrew 只能更新或移除由 Fontbrew 安装并记录在 manifest 中的 managed package。
```

因此，Fontbrew 不替代 Font Book，不管理 macOS 系统字体，不接管用户手动安装的字体，也不试图成为通用字体收藏、预览、设计或授权管理工具。

## 2. 产品目标

Fontbrew 要解决的是第三方开源字体在本机管理中的混乱感。

常见问题包括：

- 用户从 GitHub、Google Fonts、Fontsource 或压缩包下载字体后，很快忘记来源。
- 手动安装后不知道当前版本，也不知道是否有更新。
- 卸载时不确定应该删除哪些文件。
- 同一个字体可能有 OTF、TTF、Nerd Font、CN、webfont 等多个 variant，用户容易选错。
- 用户需要一个工具保证不会误删系统字体或手动安装的字体。

Fontbrew 的产品目标是：

- 管理 macOS 上由用户主动安装的第三方开源字体。
- 以 package 为单位安装、查看、更新和移除字体。
- 记录每个 managed package 的来源、版本、family、安装文件和 activation artifact。
- 保持本机字体环境可追踪、可解释、可安全清理。
- 在不确定或有风险时停止，让用户明确选择，而不是猜测。

目标用户：

- 经常安装开源字体的开发者。
- 喜欢调整终端字体、代码字体、UI 字体的 macOS 用户。
- 需要频繁更新字体版本的设计/开发混合型用户。
- 希望保持本机字体环境干净、可追踪的用户。
- 习惯用 CLI 管理开发环境的人。

典型用户不是专业字体收藏家，而是希望字体管理像 Homebrew 一样干净、可解释、可撤销的人。

## 3. 产品背后的 idea

Fontbrew 的核心 idea 是把“字体安装”从一次性的文件复制行为，变成可追踪、可更新、可撤销的 package management 行为。

这意味着 Fontbrew 需要维护三条清晰边界：

- 用户边界：用户管理的是 package，不是 archive 内部的松散字体文件。
- 来源边界：每个 managed package 都有明确 source、version 和 update source。
- 权限边界：Fontbrew 只能改变自己 managed store、manifest 和 activation directory 内的状态。

产品上，Fontbrew 更接近 Homebrew、Cargo、npm 这类包管理器，而不是 Font Book。它不负责字体预览、收藏或设计，只负责让用户可信地管理第三方开源字体的生命周期。

架构上，这个 idea 落到两个动作：

- 先计划：install、update、remove 等风险操作先构建 plan，明确将发生的变化和风险。
- 后应用：core 只有在 execution policy 允许时才 apply plan，并且只修改 Fontbrew 管理边界内的文件。

## 4. 产品原则

### 4.1 克制

Fontbrew 只做字体包管理。它不扩张成字体设计工具、字体收藏工具、团队资产管理平台、商业授权管理平台或后台自动激活服务。

### 4.2 可追踪

每个 Fontbrew-managed package 都应该能回答：

- 它叫什么？
- 它的 package ID 是什么？
- 它从哪里来？
- 当前版本是什么？
- 包含哪些 font family 和 font file？
- 当前是否 activated？
- 是否可以更新？
- 移除时会删除哪些 managed 文件？

### 4.3 不越权

Fontbrew 不碰它没有安装并记录的字体。即使发现同名 family 已经存在于用户字体目录或系统字体目录，Fontbrew 也不能 adopt、覆盖、更新或删除这些非 managed 字体。

### 4.4 可预期

每个命令都应该行为清楚、输出明确、默认保守。无变化时明确说明无变化；失败时说明失败原因，并尽量给出下一步命令。

### 4.5 面向 package

用户关心的是“安装 Inter”或“更新 Maple Mono”，不是压缩包里某个文件夹下的具体 `.ttf` 文件。字体文件是安装 artifact，package 才是用户管理单位。

## 5. 产品边界

### 5.1 MVP 支持

当前 MVP 支持：

- macOS。
- 从 Fontbrew Registry 安装桌面字体。
- 从 approved providers 安装桌面字体，初始 provider 为 Google Fonts 和 Fontsource。
- 从显式 GitHub repository 安装桌面字体，例如 `rsms/inter`。
- 从本地 archive 安装桌面字体，例如 `./MapleMono.zip`。
- MVP local archive format 为 ZIP，不递归解压 nested archive。
- 搜索可安装 package。
- 查看 managed package 列表。
- 查看 package 详情。
- 检查 managed package 是否 outdated。
- 在用户确认后更新 managed package。
- 安全移除 managed package。
- 解析字体 metadata，用于 family、style、weight、format、package boundary 和 update validation。
- 通过 global config 和 install flag 控制字体格式偏好。

### 5.2 MVP 不支持

当前 MVP 明确不支持：

- Linux 或 Windows activation。
- 项目级 `fontbrew.json`、dependency file 或 lockfile。
- rollback 命令或成功更新后的历史版本保留。
- 字体 archive 下载缓存。
- 显式 `activate` / `deactivate` workflow。
- 任意 GitHub repository 搜索。
- 管理 macOS 系统字体。
- 接管用户已经手动安装的字体。
- 商业字体授权管理。
- GUI。
- 后台自动 activation service。
- webfont dependency management。
- 字体预览、收藏、分类、标签或设计能力。
- 非 ZIP archive 作为 MVP 必须支持格式。

### 5.3 管理边界

属于 Fontbrew 管理范围：

- 通过 `fontbrew install` 安装并写入 manifest 的 package。
- 通过 `fontbrew update` 更新并写入 manifest 的 package。
- Fontbrew managed store 中属于 manifest 记录的 package 文件。
- `~/Library/Fonts/Fontbrew/` 下属于 manifest 记录的 activation artifact。

不属于 Fontbrew 管理范围：

- macOS 系统字体。
- 用户手动拖入 Font Book 的字体。
- 用户手动复制到 `~/Library/Fonts` 的字体。
- `/Library/Fonts` 或 `/System/Library/Fonts` 中的字体。
- Adobe Fonts 或其他字体管理器激活的字体。
- 没有 manifest 记录的同名 family 或同名文件。

## 6. 核心概念

| 概念 | 含义 |
| --- | --- |
| Package | 用户操作的字体包单位。用户 install、list、info、update、remove 的对象都是 package。 |
| Package ID | package 的稳定本地标识，使用 lowercase ASCII kebab-case，例如 `inter`、`source-sans-3`。 |
| Package Identity | 判断一个新解析 package 是否仍然是同一个 managed package 的稳定身份，优先使用 registry package ID，并结合 expected families、recipe rules 和 provider identity。 |
| Source | Fontbrew 能解析为 package 的上游来源，例如 registry short name、provider name、GitHub repo 或 local archive。 |
| Recipe | 描述一个 source 如何解析为 package 的 curated 规则，可覆盖默认 package boundary、release selection、asset selection 和 format preference。 |
| Registry | Fontbrew 第一方 curated recipe index，提供稳定 short name 和可靠安装 recipe。 |
| Registry Snapshot | CLI 本地保存的 registry JSON 副本，用于 short-name install 和 registry search。 |
| Search Provider | 第三方字体目录或 API，例如 Google Fonts、Fontsource。它们用于发现可安装 package，但不是 Fontbrew Registry。 |
| Provider Metadata Snapshot | 本地保存的 provider metadata，只包含 metadata，不包含字体 archive 或 font binary。 |
| Font File | 具体字体二进制文件，例如 `.ttf`、`.otf`、`.ttc`、`.otc`。 |
| Desktop Font File | 可安装到系统字体环境的字体文件，MVP 支持 `.ttf`、`.otf`、`.ttc`、`.otc`。 |
| Web Font File | 面向网页分发的字体文件，例如 `.woff`、`.woff2`。MVP 不 activation webfont。 |
| Family Name | 从字体 metadata 读取的 font family 身份，默认用于 package grouping。 |
| Managed Package | 由 Fontbrew 安装并记录在 manifest 中的 package。只有 managed package 可以被 Fontbrew update 或 remove。 |
| Installed Package | package 文件和 metadata 已经放入 managed store。 |
| Activated Package | installed package 的 font file 已经通过 activation directory 暴露给 macOS。 |
| Managed Store | Fontbrew 私有存储目录，保存 package 文件、metadata、manifest、registry snapshot 和 provider metadata snapshot。 |
| Activation Directory | Fontbrew 在用户字体目录下拥有的 activation 边界：`~/Library/Fonts/Fontbrew/`。 |
| Manifest | 本机实际 managed package 状态记录，不是项目 dependency file 或 lockfile。 |
| Config | 用户偏好文件，记录 format preference、activation strategy、metadata refresh 等偏好。 |
| Update Source | 后续可用于检查新版本的来源。local archive 默认没有 update source。 |
| Package Version | Fontbrew 用来判断 outdated/update 的版本，默认来自 source release 或 provider metadata，不默认来自 font metadata。 |
| Release | source 发布的版本，例如 GitHub Release。 |
| Asset | release 内可下载的 artifact，例如 zip archive。asset selection 发生在解析字体文件之前。 |
| Conflict | 安装或 activation 可能影响非 managed 字体、已有 activation artifact 或不同来源的 managed package 的风险。 |

## 7. 产品架构

Fontbrew 由 core 和 frontend 分层构成。

```text
fontbrew-cli
  负责 CLI 参数、human/JSON 输出、prompt、progress、stdout/stderr、exit code

fontbrew-core
  负责产品 use case、source resolution、fetch、archive extraction、font metadata、
  package discovery、conflict detection、manifest/config、activation、update workflow
```

`fontbrew-core` 是 Fontbrew 自己前端使用的 application core，不是第三方 SDK。CLI 和未来可能的 GUI 都应该通过 core 的 product-shaped use case 操作，而不是自行编排 manifest 写入、下载、字体解析或 activation。

核心 workflow：

```text
install source
  -> resolve source
  -> select release/version
  -> select asset
  -> download or read archive into staging
  -> extract desktop font files safely
  -> parse font metadata
  -> discover package boundary
  -> detect conflicts
  -> build plan
  -> apply with execution policy
  -> write managed store
  -> activate under ~/Library/Fonts/Fontbrew
  -> update manifest
```

update workflow 使用更保守的两阶段模型：

```text
prepare
  -> refresh/check update source
  -> download to staging
  -> extract and parse
  -> validate package identity

apply
  -> keep old version active until validation succeeds
  -> switch activation artifacts
  -> update manifest
  -> delete old version after successful commit
```

Prepare 阶段可以使用 bounded parallelism。Apply 阶段会修改 activation directory 和 manifest，应受控执行，实际效果接近串行提交。

如果 validation 或 activation 失败，旧版本保持 active，manifest 保持不变，staging files 应被清理。

## 8. 本地文件布局

默认路径：

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

Fontbrew 不维护单独的字体 archive 下载缓存。安装和更新时下载的字体文件属于 package state，会进入 staging 或 managed package store；移除 package 时，对应 managed store 文件会被删除。

## 9. 关键产品决策

### 9.1 产品名和 CLI 名称

产品名为 Fontbrew，CLI 命令为 `fontbrew`。

### 9.2 package boundary 默认按 family name，recipe 可以覆盖

默认情况下，Fontbrew 将相同 family name 的 font file 归为一个 package。Registry recipe 可以覆盖这个默认行为，用于处理一个 archive 内含多个 user-facing variant，或多个相关 family 应作为一个 package 安装的情况。

当发现多个 package family 且没有 recipe 或用户选择能解释边界时，Fontbrew 应停止而不是猜测。

对 direct GitHub 和 local archive source，如果解析出多个 family，human TTY CLI 应显示 family 多选 prompt。用户选择一个或多个 family 后，Fontbrew 将每个被选中的 family 作为独立 managed package 安装，package ID 从 family name 自动派生，例如 `Geist Mono` 派生为 `geist-mono`。非交互、JSON mode 或无 TTY 时不能 prompt，用户必须通过 `--family <name>` 明确选择一个或多个 family，或通过 `--all-families` 明确安装全部解析出的 family。`--yes` 只表示批准风险，不表示自动选择 family。

### 9.3 Registry 是 curated index，search 由 provider 扩展

Fontbrew Registry 是小而可靠的第一方 recipe index，负责稳定 short name 和可信安装 recipe。它不是全网字体搜索引擎。

广泛发现交给 approved providers，例如 Google Fonts 和 Fontsource。Search 结果必须可安装，不能返回 Fontbrew 无法解析和安装的候选项。

### 9.4 Registry v1 是单个 JSON 文件

Fontbrew Registry v1 使用一个远程 `registry.json` 文件作为 curated recipe source。CLI 将它保存为本地 registry snapshot。MVP 不假设默认官方 registry 域名；首次读 registry 时，如果本地 snapshot 不存在，CLI 会先写入内置的空 registry snapshot。只有配置了 registry URL 时才刷新远程 registry。

选择单 JSON 是为了让 MVP 的 registry update、schema validation、本地 snapshot 管理和 registry search 保持简单。广泛字体发现不靠扩张第一方 registry，而靠 approved providers。

### 9.5 Provider metadata 只保存 metadata

Fontbrew 可以保存 Google Fonts、Fontsource 等 provider 的 metadata snapshot，用于减少重复 API 调用。这些 snapshot 不包含下载的字体 archive 或 font binary。

默认产品规则是需要 registry/provider metadata 的命令自动刷新 metadata。需要联网解析 registry、provider 或 update source 的命令不提供用户可见的 refresh/offline 模式。

以下命令可以自动刷新 metadata：

- `fontbrew search`
- `fontbrew install`
- `fontbrew outdated`

### 9.6 GitHub source 必须显式

用户可以安装显式 GitHub repo：

```bash
fontbrew install rsms/inter
```

但 `fontbrew search` 不做任意 GitHub 搜索。GitHub 上的模糊搜索结果太容易包含无关项目、web-only 资源或无法判断 package boundary 的 archive。

### 9.7 GitHub version 默认使用 release tag

GitHub source 的默认 release selection 是 latest non-draft、non-prerelease release。默认 package version 是被选中的 GitHub Release tag。

Recipe 可以覆盖 release selection 和 asset selection。Fontbrew 不默认从 font metadata 推断 package version，因为不同字体项目的 metadata 版本不稳定，不适合作为 update 判断来源。

### 9.8 ambiguous asset 必须显式选择

如果一个 release 中存在多个可安装字体 asset，而 recipe 或用户 flag 无法唯一确定选择，Fontbrew 必须失败并提示用户使用 `--asset`。

示例：

```text
Multiple installable assets found for subframe7536/maple-font:

1. MapleMono-TTF.zip
2. MapleMono-OTF.zip
3. MapleMono-NF-TTF.zip
4. MapleMono-CN-TTF.zip

Install with:
fontbrew install subframe7536/maple-font --asset MapleMono-OTF.zip
```

### 9.9 MVP 只 activation desktop font

MVP 可安装并 activation 的 desktop font format：

- `.ttf`
- `.otf`
- `.ttc`
- `.otc`

MVP 不 activation：

- `.woff`
- `.woff2`
- `.eot`
- `.svg`
- CSS 文件

如果 archive 同时包含 desktop font 和 webfont，Fontbrew 只使用 desktop font。

### 9.10 format preference 可以配置并决定 desktop format 取舍

默认 format preference：

```text
otf, ttf, ttc, otc
```

用户可以全局配置，也可以在 install 时覆盖：

```bash
fontbrew install inter --format otf
fontbrew install inter --format ttf
fontbrew install inter --otf
fontbrew install inter --ttf
```

如果不同 format 的 family/style/weight coverage 不等价，Fontbrew 仍按 preference 选择一个 desktop format。上游字体包本身的覆盖差异不是 Fontbrew 需要修复的冲突；用户需要另一个 format 时可以通过全局配置或 install flags 显式覆盖。

### 9.11 conflict 需要用户明确同意，且不能 adopt

安装或 activation 可能与非 managed 字体冲突时，Fontbrew 必须提示风险并要求明确 consent。即使用户同意继续，Fontbrew 也只能安装自己的 managed copy，不能 adopt、覆盖或删除已有非 managed 字体。

### 9.12 manifest 记录实际本机状态

Manifest 记录当前机器上实际安装的 Fontbrew-managed package。它不是项目 lockfile，也不是 desired state。

只有 manifest 中记录的 package 文件和 activation artifact 才能被 update 或 remove。

Manifest 应记录足够信息，保证 Fontbrew 可以安全 list、info、update 和 remove。示例结构：

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

### 9.13 config 和 manifest 分离

Config 记录用户偏好，例如 format preference、activation strategy、metadata TTL。Manifest 记录实际安装状态。两者必须分离。

### 9.14 activation strategy 可切换

MVP 以 symlink activation 为主要实现方向：

```text
~/Library/Fonts/Fontbrew/Inter-Regular.otf
-> ~/.local/share/fontbrew/packages/inter/v4.1/files/Inter-Regular.otf
```

activation 层必须保持 strategy-based，以便未来在 macOS 或应用兼容性需要时切换到 copy activation。

当前 research 记录显示，`system_profiler SPFontsDataType` 未观察到 symlink fixture 被加载，这不能完全证明所有应用都不支持 symlink，但说明 symlink loader 支持仍需手动 app-level 验证。如果后续验证失败，应将 copy activation 提升为默认策略。

### 9.15 core 不负责 prompt，core 负责 safety invariant

CLI 或 GUI 负责人机交互和确认。Core 不能把“用户确认”建模为终端 prompt，而应该要求调用方传入明确的 execution policy，例如 safe only、user-approved risk、assume yes 或 dry-run。

这样 JSON mode、GUI 和 CLI 都能共享同一套安全规则。

### 9.16 输出流保持可脚本化

CLI 输出遵循常见包管理器和系统 CLI 约定：

- human primary result 输出到 stdout。
- progress、warning、prompt、diagnostic、human error 输出到 stderr。
- `--json` 时 stdout 只包含 JSON。
- JSON payload 需要 `schemaVersion`。
- JSON mode 不交互；需要 approval 的命令必须使用 `--yes`、`--dry-run`，否则返回结构化错误。

### 9.17 font metadata 用于识别和验证，不用于默认版本判断

Fontbrew 必须从 desktop font file 解析 metadata，用于 package discovery、conflict detection、display 和 update validation。MVP 至少需要识别：

- family name。
- subfamily/style。
- weight。
- italic/slant，如果 metadata 可用。
- format。
- PostScript name，如果 metadata 可用。
- full name，如果 metadata 可用。

Font metadata 不是默认 package version 来源。Package version 默认来自 source release、provider metadata 或 recipe 指定的版本规则。

## 10. 关键技术决策

### 10.1 使用 Rust 和 Cargo workspace

MVP 使用 Rust 实现，工作区包含：

```text
crates/fontbrew-core/
crates/fontbrew-cli/
```

选择 Rust 的原因是它适合发布原生 CLI，生态中有成熟的 CLI、HTTP、JSON/TOML、ZIP、路径、glob、临时文件和字体解析库，并能在文件系统安全和二进制分发上保持可靠。

### 10.2 使用成熟依赖，不手写基础设施

优先使用成熟小型 crate：

- `clap`：CLI parsing
- `serde`、`serde_json`、`toml`：manifest、registry、config、JSON mode
- `reqwest` blocking client：HTTP
- `zip`：ZIP archive
- `tempfile`：staging 和 atomic write
- `directories`：平台路径
- `globset`：asset include/exclude
- `ttf-parser`：初始字体 metadata
- `fs2` 或 `fd-lock`：file locking
- `assert_cmd`、`predicates`：CLI tests

MVP 不引入 Tokio。HTTP 和 task scheduling 通过 adapter 隐藏，以便未来需要 async 或 GUI 后台任务时降低改造成本。

### 10.3 ZIP extraction 必须安全过滤

MVP local archive 支持 ZIP。Archive extraction 必须：

- 拒绝绝对路径。
- 拒绝 `..` path traversal。
- 拒绝 archive 内 symlink 和特殊文件。
- 只提取 desktop font file。
- 忽略 webfont、CSS、文档、图片等非 activation 文件。
- 限制总解压大小、单文件大小和文件数量。
- 只写入 staging directory。

### 10.4 写操作需要 staging、atomic write 和 global lock

所有 install、remove、update、registry update、config set 等写操作都应使用全局 file lock，避免并发破坏 manifest 或 activation state。

Manifest 等持久文件写入使用 temp file、flush、sync、atomic rename。Install/update 在 staging 中完成下载、解压和验证，验证成功后再进入 managed store 和 manifest。

### 10.5 update 不保留成功后的历史版本

MVP 不提供 rollback，也不在成功 update 后保留旧版本。update 过程中旧版本必须保持 active，直到新版本下载、解析、验证和 activation 成功；成功后旧版本删除。

### 10.6 token 和 API key 不写入 config

GitHub 可以通过 `GITHUB_TOKEN` 提升 API 使用额度。Google Fonts 如需 API key，也应从环境变量读取。Fontbrew 不应把这些 secret 写入 config。

## 11. 核心功能

### 11.1 `fontbrew install <source>`

安装并默认 activation 一个 package。

支持 source：

```bash
fontbrew install inter
fontbrew install google:roboto
fontbrew install fontsource:inter
fontbrew install rsms/inter
fontbrew install ./MapleMono.zip
```

常用 flags：

```bash
--format <otf|ttf|ttc|otc>
--otf
--ttf
--asset <asset-name-or-pattern>
--reinstall
--yes
```

默认流程：

1. 解析 source。
2. 按策略刷新 registry/provider metadata。
3. 选择 release/version。
4. 选择 asset。
5. 下载或读取 archive 到 staging。
6. 安全解压 desktop font file。
7. 解析 font metadata。
8. 确定 package identity 和 family metadata。
9. 检测 conflict。
10. 必要时展示 install plan 并要求确认。
11. 写入 managed store。
12. activation 到 `~/Library/Fonts/Fontbrew/`。
13. 写入 manifest。

MVP install 总是默认 activation。`--no-activate`、显式 `activate` 和显式 `deactivate` workflow 延后，即使 domain model 区分 installed 和 activated state。

重复安装已 managed package 默认是 no-op。若要重新安装，需要 `--reinstall`。改变已安装 package 的 source 不应隐式发生，MVP 可以要求先 `remove` 再 `install`。

### 11.2 `fontbrew list`

列出 Fontbrew-managed package。它只显示 manifest 中记录的 package，不扫描或显示系统字体、手动字体。

示例：

```text
inter          v4.1    registry    rsms/inter
maple-mono     v7.4    registry    subframe7536/maple-font
my-font        local   local       ./MyFont.zip
```

### 11.3 `fontbrew info <package>`

展示 package 详情：

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

### 11.4 `fontbrew search <query>`

搜索可安装 package candidate。

搜索范围：

- Fontbrew Registry
- Google Fonts
- Fontsource

搜索结果必须能解析为明确 install source。Search 不做任意 GitHub 搜索。

示例：

```text
ID              Name             Source      Install
inter           Inter            registry    fontbrew install inter
roboto          Roboto           google      fontbrew install google:roboto
source-sans-3   Source Sans 3    google      fontbrew install google:source-sans-3
```

### 11.5 `fontbrew outdated`

检查 managed package 是否有新版本。

默认行为：

- 按策略刷新 metadata。
- 使用每个 package 的 update source 检查版本。
- 对没有 update source 的 local archive package，报告为 not updatable，而不是让整个命令失败。

示例：

```text
inter          v4.0 -> v4.1
maple-mono     v7.3 -> v7.4

Not updatable:
my-local-font  local archive, no update source
```

### 11.6 `fontbrew update [package]`

更新一个 package 或所有可更新 managed package。

`fontbrew update` 的主要语义是更新字体 package。Registry refresh 不作为该命令的用户可见 flag；需要 registry metadata 的命令应默认刷新 registry。

默认流程：

1. 检查 update source。
2. 构建 update plan。
3. 展示目标版本和 skipped package。
4. 要求用户确认。
5. 使用 conservative two-phase replacement 应用更新。
6. 展示结果 summary。

常用 flags：

```bash
--yes
--dry-run
```

### 11.7 `fontbrew remove <package>`

移除 managed package。`uninstall` 是 alias，但主命令是 `remove`。

Remove 删除：

- `~/Library/Fonts/Fontbrew/` 中 manifest 记录的 activation artifact。
- managed store 中对应 package 文件。
- manifest 中对应 package record。

Remove 不删除：

- 系统字体。
- activation directory 外的用户字体。
- provider metadata snapshot。
- registry snapshot。
- global config。
- 其他 managed package。

### 11.8 `fontbrew registry update`

刷新本地 Fontbrew Registry snapshot。

### 11.9 `fontbrew registry status`

显示本地 registry snapshot 状态、schema/version 信息、package count 和 last refresh time。

### 11.10 `fontbrew config get/set`

读取或写入全局偏好。

MVP config 示例：

```toml
[install]
format_preference = ["otf", "ttf", "ttc", "otc"]
activation_strategy = "symlink"

[registry]
auto_update = true

[network]
metadata_ttl_hours = 24
```

示例命令：

```bash
fontbrew config get install.format_preference
fontbrew config set install.format_preference otf,ttf,ttc,otc
```

## 12. 用户如何使用 Fontbrew

### 12.1 安装常见字体

用户想安装 Inter：

```bash
fontbrew install inter
fontbrew info inter
```

理想情况下，用户不需要去 GitHub 找 release，不需要判断哪个 zip 是 OTF 或 TTF，也不需要手动拖文件到 Font Book。

### 12.2 搜索并安装 provider 字体

```bash
fontbrew search mono
fontbrew install google:roboto
fontbrew install fontsource:inter
```

Search 只返回 Fontbrew 可以安装的 candidate。不能安装的结果不应该出现。

### 12.3 从 GitHub repository 安装

```bash
fontbrew install rsms/inter
fontbrew install subframe7536/maple-font --asset MapleMono-OTF.zip
```

GitHub install 适合用户明确知道字体项目位置的情况。若 release asset 不唯一，用户必须通过 `--asset` 选择。

### 12.4 从本地 archive 安装

```bash
fontbrew install ./MapleMono.zip
```

本地 archive 安装后也是 managed package，可以 list、info、remove、reinstall。默认没有 update source，因此不会被 `update` 自动更新。

### 12.5 选择字体格式

```bash
fontbrew install inter --format ttf
fontbrew install inter --otf
```

当不同格式 coverage 不等价时，Fontbrew 应要求用户显式选择，避免安装错误 variant。

### 12.6 查看本机 managed fonts

```bash
fontbrew list
fontbrew info inter
```

`list` 用来回答“我通过 Fontbrew 管理了哪些字体”。`info` 用来回答“这个字体从哪里来、当前是什么版本、哪些文件会被移除、是否 active”。

### 12.7 更新字体

```bash
fontbrew outdated
fontbrew update --dry-run
fontbrew update
```

用户应先看到 update plan，再决定是否应用。Update 失败时，旧版本应保持 active，manifest 不应指向缺失文件。

### 12.8 移除字体

```bash
fontbrew remove inter
```

或使用 alias：

```bash
fontbrew uninstall inter
```

Fontbrew 只移除自己管理的 activation artifact、package store 文件和 manifest record，不碰用户手动安装的同名字体。

### 12.9 机器可读输出

脚本可以使用 JSON mode：

```bash
fontbrew --json list
fontbrew --json info inter
fontbrew --json outdated
fontbrew --json update --dry-run
```

JSON mode 下 stdout 必须保持纯 JSON，不应该混入 progress、prompt 或 warning。

## 13. 失败行为

Fontbrew 应默认保守失败。

以下情况应停止而不是猜测：

- GitHub release 有多个 installable asset，且无 recipe 或用户 selector。
- archive 中发现多个 package family，且无 recipe、interactive selection、`--family` 或 `--all-families` 能解释边界。
- 不同 desktop format 的 coverage 无法证明等价。
- update 后的新 package identity 与 managed package 不匹配。
- activation 会覆盖 unmanaged 文件。
- updatable package 的 source version 无法确定。

失败信息应该说明：

- 发生了什么。
- Fontbrew 没有做什么危险动作。
- 用户下一步可以执行什么命令。

## 14. MVP 验收标准

MVP 可接受的条件是 macOS 用户可以：

- 安装 registry package，例如 Inter。
- 从 approved sources 搜索 installable package。
- 从 local archive 安装字体。
- 从 GitHub release 安装字体，前提是 asset selection 明确或用户显式选择。
- 查看所有 Fontbrew-managed package。
- 查看 package 的 source、version、family 和 activation state。
- 在 review update plan 后更新 managed package。
- 移除 managed package 且不影响 non-managed 字体。
- 在同名 family 已存在于 Fontbrew 边界外时避免意外覆盖。
- 重复 install 时不会产生意外 source change 或重复 state。

核心 trust test：

```text
用户是否总能知道 Fontbrew 管理了哪些字体、它们来自哪里、
当前是什么版本、是否能更新，以及 remove 时会删除什么？
```

如果答案是 yes，MVP 达成产品目标。

## 15. 当前实现备注

截至 2026-07-04，MVP 主线能力已经基本实现并通过验证记录中的 `cargo fmt --all`、`cargo clippy --workspace --all-targets`、`cargo test --workspace` 和 CLI smoke test。已验证的主线包括 local archive、registry/GitHub install、list、info、search、outdated、update dry-run、remove dry-run、JSON 输出和 stdout/stderr 分流。

仍需注意的产品化差距：

- Registry/provider metadata 的 TTL 和自动刷新策略尚未完整串联。
- 需要联网或 metadata 的命令应默认刷新 registry/provider metadata，不再暴露 refresh/offline flag。
- Registry recipe 对 package boundary 的覆盖还需要更明确的 family include/exclude/expected 语义。
- Copy activation 是预留策略，当前不应被当作完整可用默认路径。
- Local archive 缺少 `--id` 兜底，遇到无法安全 slug 化的 family name 时用户缺少手动指定 package ID 的入口。
- `registry status` 需要完整展示 schema/version 信息。

这些差距不改变产品边界，但会影响用户对部分 flag、provider freshness 和复杂 font release 的预期。
