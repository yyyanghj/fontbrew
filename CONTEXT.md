# Fontbrew

Fontbrew is a font package manager context. Its language distinguishes the user-facing package from the upstream place it comes from and the concrete font files installed on disk.

## Language

**Package**:
A user-facing managed font package that can be installed, updated, listed, and removed as one unit. A package is normally formed from font files that share the same family name and may include weights, styles, italics, or other variants for that family.
_Avoid_: Font, repository, archive

**Source**:
The upstream location or provider that Fontbrew can resolve into one or more packages, such as a registry entry, GitHub repository, Google Fonts family, Fontsource package, or local archive.
_Avoid_: Package, repository when the provider is not necessarily GitHub

**Recipe**:
A curated description of how a source resolves into packages. A recipe can refine package boundaries when family-name grouping alone would split or merge fonts in a way users would not expect.
_Avoid_: Manifest, package

**Registry**:
Fontbrew's first-party curated recipe index. The registry provides stable package names and install recipes for well-known fonts, and is updated through a remote source stored locally as a registry snapshot by the CLI.
_Avoid_: Search provider, static built-in table

**Registry Snapshot**:
Fontbrew's local copy of the remote first-party registry used for short-name installs, registry search, and limited offline behavior.
_Avoid_: Download cache, manifest, managed store

**Provider Metadata Snapshot**:
Fontbrew's local copy of third-party provider metadata used to reduce repeated API calls during search and outdated checks. It is metadata only and does not include downloaded font archives.
_Avoid_: Download cache, managed store

**Search Provider**:
A third-party font catalog or API that Fontbrew can query for discovery, such as Google Fonts or Fontsource. Search providers can return candidates that are not part of the first-party registry.
_Avoid_: Registry

**Search Result**:
An installable package candidate returned by the registry or an approved search provider. Search results must resolve to an explicit install source; Fontbrew does not return arbitrary GitHub repository matches as search results.
_Avoid_: GitHub search result, discovery result that cannot be installed

**Font File**:
A concrete font binary contained in a package, such as a `.ttf`, `.otf`, `.ttc`, or web font file. Font files are installation artifacts, not the primary thing users manage.
_Avoid_: Package

**Desktop Font File**:
A font file format Fontbrew can install and activate for the operating system font library, such as `.ttf`, `.otf`, `.ttc`, or `.otc`.
_Avoid_: Web font file

**Web Font File**:
A font file intended for web delivery, such as `.woff` or `.woff2`. Web font files may appear in archives or provider APIs, but they are not activated as system fonts in the MVP.
_Avoid_: Desktop font file

**Format Preference**:
The ordered preference Fontbrew uses when multiple equivalent desktop font formats are available. The default preference favors OTF, but users can configure the global preference or override it for a single install command.
_Avoid_: Asset selection

**Family Name**:
The canonical font family identity read from font metadata and used to group related font files into a package.
_Avoid_: Filename, package name

**Managed Package**:
A package that Fontbrew installed and recorded in its manifest. Only managed packages can be updated or removed by Fontbrew.
_Avoid_: Installed font, system font

**Installed Package**:
A package whose font files and metadata have been placed in Fontbrew's managed store.
_Avoid_: Activated package

**Activated Package**:
An installed package whose font files have been exposed to the operating system font loader, typically through the user's font directory.
_Avoid_: Installed package

**Activation Directory**:
The Fontbrew-owned location inside the user's operating-system font directory where managed packages are exposed for use. Fontbrew only manages activation artifacts inside this boundary.
_Avoid_: Font directory, managed store

**Managed Store**:
Fontbrew's private storage location for downloaded packages, extracted font files, package metadata, and manifests. The managed store is separate from the operating-system font directory.
_Avoid_: Activation directory

**Manifest**:
The local record of managed packages that are actually installed on this machine. The manifest records facts about Fontbrew-managed packages; it is not a project dependency declaration or lockfile.
_Avoid_: Lockfile, desired state, project manifest

**Config**:
The local user preference file that controls Fontbrew behavior such as format preference, activation strategy, and registry refresh behavior. Config is separate from the manifest because it records preferences rather than installed package facts.
_Avoid_: Manifest, lockfile

**Update Source**:
A source that can be checked later for newer package versions. Local archives are managed after installation but are not update sources unless the user or a recipe binds them to an upstream provider.
_Avoid_: Source

**Update Plan**:
The list of managed packages Fontbrew intends to change during an update operation, including current versions, target versions, and packages that cannot be updated. The update plan is shown to the user before changes are applied.
_Avoid_: Registry update

**Package Version**:
The version Fontbrew uses to decide whether a managed package is outdated. Package versions come from the source's release identity, provider API, or recipe, not from font file metadata by default.
_Avoid_: Font metadata version

**Release**:
A source-level published version of one or more packages, such as a GitHub Release or a provider version. Releases are selected before archive assets are downloaded and parsed.
_Avoid_: Package, font file version

**Asset**:
A downloadable artifact within a release, such as a zip archive containing font files. Asset selection happens before package discovery inside the downloaded content.
_Avoid_: Package, release

**Conflict**:
A condition where installing or activating a package may overlap with an existing non-Fontbrew font family, file, or activation artifact. Conflicts require explicit user consent and must not cause Fontbrew to adopt, overwrite, or delete non-managed fonts.
_Avoid_: Duplicate, overwrite

**Package Identity**:
The stable identity that lets Fontbrew decide whether a discovered package is the same managed package across installs and updates. Registry package IDs are primary; expected family names and recipe rules are used to validate that the resolved files still match the package.
_Avoid_: Filename, version

**Remove**:
The primary user command for deleting a managed package from Fontbrew's activation directory, managed store, and manifest. `uninstall` is an alias for remove.
_Avoid_: Delete, uninstall as the primary term

**Reinstall**:
An explicit install operation that replaces the files for an already managed package without changing its source identity by default.
_Avoid_: Update, source replacement
