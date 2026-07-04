# Fontbrew Self-Update Plan

## Goal

Add a `fontbrew self-update` command that updates the `fontbrew` CLI binary itself to the latest stable GitHub release.

This is CLI-owned behavior. It must not be modeled as a managed font package update, must not write the font manifest, and must not route through `FontbrewApp` application tasks.

## Command Shape

```text
fontbrew self-update [--dry-run] [--yes] [--force]
```

Flags:

- `--dry-run`: check the latest stable release and report what would happen without replacing the executable.
- `--yes`: skip the interactive replacement confirmation.
- `--force`: reinstall the latest stable release even when the current version is already latest or newer than latest.

The first version does not support selecting a specific version. The command always targets the latest stable GitHub release.

## Source Of Truth

The command downloads official release assets from GitHub Releases for the Fontbrew repository.

Default repository:

```text
yyyanghj/fontbrew
```

The command may reuse advanced/test environment behavior already established by `install.sh`, such as `FONTBREW_REPO`, but this should not become a normal user-facing CLI flag in the first version.

`GITHUB_TOKEN` may be used for GitHub API requests when present. It must remain an environment variable only and must never be persisted to config, manifests, fixtures, or docs examples with real token values.

## Release Asset Contract

The release asset format is fixed and matches `install.sh`.

For Apple Silicon macOS:

```text
asset:  fontbrew-aarch64-apple-darwin.tar.gz
sha256: fontbrew-aarch64-apple-darwin.tar.gz.sha256
inside: fontbrew-aarch64-apple-darwin/fontbrew
```

For Intel macOS:

```text
asset:  fontbrew-x86_64-apple-darwin.tar.gz
sha256: fontbrew-x86_64-apple-darwin.tar.gz.sha256
inside: fontbrew-x86_64-apple-darwin/fontbrew
```

If the current platform cannot be mapped to one of these targets, fail with an unsupported platform error.

If the release is missing the current target asset, checksum file, or expected archive entry, fail with an invalid release asset error. Do not guess alternate asset names or archive paths.

## Version Rules

Fontbrew release tags must follow the project-owned stable release convention and parse as semver with an optional leading `v`.

Examples:

```text
v0.1.2
0.1.2
```

The current version comes from `CARGO_PKG_VERSION`.

Rules:

- `current < latest`: update.
- `current == latest`: report up to date; with `--force`, reinstall latest.
- `current > latest`: report current is newer than latest; with `--force`, reinstall latest.
- latest release tag cannot be parsed as semver: fail as invalid release metadata.

Do not add fallback behavior for arbitrary tag formats, nightly tags, prerelease channels, or custom version strings in the first version.

## Install Method Detection

Detection is path-based. Do not introduce install metadata files in the first version.

Use `std::env::current_exe()` to identify the executable to replace. Allow direct replacement only when all of these are true:

- executable file name is `fontbrew`;
- current executable is a writable file;
- parent directory is writable;
- path is not under a known package manager, system, Cargo, or development build location.

Reject direct replacement for known protected locations, including:

- `/opt/homebrew/*`
- `/usr/local/Cellar/*`
- `/usr/local/Homebrew/*`
- `/usr/local/opt/*`
- `/opt/local/*`
- `/nix/store/*`
- `/run/current-system/*`
- `/usr/bin/*`
- `/bin/*`
- `~/.cargo/bin/*`
- any path containing `/target/debug/`
- any path containing `/target/release/`

When rejecting a package-manager-managed binary, do not overwrite it and do not install another copy elsewhere. Report the appropriate action instead, such as:

```text
This fontbrew binary appears to be managed by Homebrew.
Run: brew upgrade fontbrew
```

Package-manager handling is advisory only in the first version. Do not automatically run `brew upgrade`, `port upgrade`, or any other external package manager command.

Symlink-specific behavior is not a first-version design concern. The implementation can rely on the path and permission rules above.

## Safety Checks

Before replacing the current executable:

1. Resolve the latest stable GitHub release.
2. Download `fontbrew-${target}.tar.gz`.
3. Download `fontbrew-${target}.tar.gz.sha256`.
4. Verify the archive SHA-256 against the checksum file.
5. Extract the expected `fontbrew-${target}/fontbrew` binary.
6. Mark the extracted binary executable.
7. Run the extracted binary with `--version`.

The checksum protects against transfer or asset corruption. The first version does not add release signing, cosign, minisign, or GPG verification. If the project later adds signed releases, `self-update` should require signature verification.

## Replacement And Recovery

Do not write into the existing executable in place.

Use a conservative staged replacement flow in the current executable's directory:

```text
current = /path/to/fontbrew
staging = /path/to/.fontbrew-update-<pid>/
new     = /path/to/.fontbrew-update-<pid>/fontbrew.new
backup  = /path/to/fontbrew.old-<timestamp>
```

Flow:

1. Create the staging directory beside the current executable.
2. Write or move the extracted binary to `fontbrew.new`.
3. Set mode `0755`.
4. Run `fontbrew.new --version`.
5. Rename the current executable to the backup path.
6. Rename `fontbrew.new` to the original current executable path.
7. Run the installed executable with `--version`.
8. On success, remove the backup and staging directory.

Failure handling:

- Failures before renaming the current executable must leave the current executable untouched.
- If backup creation fails, leave the current executable untouched and clean staging.
- If replacing `current` with `new` fails after backup creation, try to restore the backup.
- If post-replacement smoke test fails, try to restore the backup.
- If restore fails, leave the backup path in the report or error message so the user can recover manually.

macOS permits replacing the path of a running executable, which matches the project's current macOS-only MVP boundary.

## Locking

Use a global self-update lock to prevent concurrent self-updates.

Recommended lock path:

```text
~/.local/share/fontbrew/self-update.lock
```

The implementation may use `fontbrew_core::platform::FontbrewPaths::resolve()` and `fontbrew_core::fs::GlobalFileLock` for this.

The lock should cover install method detection, release resolution, download, verification, extraction, replacement, and recovery.

If the lock is unavailable, return the existing lock-style error instead of pretending the install method is unsupported.

## Confirmation

Only prompt when the command is about to replace the executable.

Rules:

- `current < latest`: prompt in interactive human mode; `--yes` skips prompt.
- `current == latest`: no prompt and no replacement; with `--force`, prompt before reinstalling.
- `current > latest`: no prompt and no replacement; with `--force`, prompt before replacing with latest.
- `--dry-run`: never prompt.
- `--json` without `--yes` or `--dry-run`, when replacement would happen: fail with a clear prompt-unavailable or approval-required error.

Do not reuse `PlanRisk` for this prompt. `PlanRisk` is package-oriented and requires package IDs. Add a CLI-owned confirmation path for self-update, such as:

```text
Replace /Users/example/.local/bin/fontbrew with fontbrew 0.1.2? [y/N]
```

## Output

Keep the existing stream contract:

- human command results go to stdout;
- progress, prompts, warnings, diagnostics, and errors go to stderr;
- JSON mode emits only structured JSON on stdout;
- `--quiet` suppresses progress.

Human examples:

```text
fontbrew 0.1.2 is up to date.
Planned self-update 0.1.1 -> 0.1.2; no changes applied.
Updated fontbrew 0.1.1 -> 0.1.2.
Reinstalled fontbrew 0.1.2.
fontbrew 0.1.3 is newer than the latest stable release 0.1.2.
```

Progress examples for human non-quiet mode:

```text
Checking latest fontbrew release...
Downloading fontbrew-aarch64-apple-darwin.tar.gz...
Verifying checksum...
Installing /Users/example/.local/bin/fontbrew...
```

Do not extend `fontbrew_core::ProgressEvent` for this first version. Self-update progress is CLI-owned.

## JSON Report

Add CLI-owned report types in `fontbrew-cli`.

Suggested shape:

```rust
SelfUpdateReport {
    current_version: String,
    latest_version: String,
    target_version: String,
    executable_path: PathBuf,
    install_method: SelfUpdateInstallMethod,
    status: SelfUpdateStatus,
    backup_path: Option<PathBuf>,
}
```

Suggested status values:

```text
up_to_date
planned
updated
reinstalled
skipped_newer_current
```

Suggested install method values:

```text
standalone
homebrew
macports
cargo
dev_build
system
unknown
```

Use the existing JSON envelope pattern with:

```text
command: "self_update"
```

Unsupported install methods should usually return structured errors instead of success reports. For example:

```json
{
  "schemaVersion": 1,
  "error": {
    "kind": "self_update_unavailable",
    "message": "This fontbrew binary appears to be managed by Homebrew. Run: brew upgrade fontbrew"
  }
}
```

## Module Boundaries

Implementation belongs in the CLI crate.

Recommended files:

```text
crates/fontbrew-cli/src/self_update.rs
crates/fontbrew-cli/src/cli.rs
crates/fontbrew-cli/src/reporter.rs
crates/fontbrew-cli/src/reporter/human.rs
crates/fontbrew-cli/src/reporter/json.rs
crates/fontbrew-cli/src/exit.rs
```

Do not add `FontbrewApp::self_update_*` methods and do not add self-update request/report models to `fontbrew-core`.

Allowed reuse from `fontbrew-core`:

- `fontbrew_core::fetch::{ReqwestHttpClient, HttpClient, HttpRequest}`
- `fontbrew_core::platform::FontbrewPaths`
- `fontbrew_core::fs::GlobalFileLock`
- `fontbrew_core::version` helpers when useful
- cancellation traits/patterns where useful

Do not make `fontbrew-core/src/github.rs` public just for this feature. That module currently models font package GitHub release assets and filters installable font archives. Self-update has a different release asset contract.

The CLI crate will likely need additional dependencies for archive and checksum handling, such as:

```text
flate2
tar
sha2
```

Avoid a separate `hex` dependency unless it meaningfully simplifies the checksum implementation.

## Error Boundaries

Add CLI-owned self-update error variants instead of forcing these cases into `FontbrewError`.

Suggested stable error kinds:

- `self_update_unavailable`
- `self_update_invalid_release`
- `self_update_checksum_mismatch`
- `self_update_failed`

Keep existing kinds for existing generic failures where appropriate:

- `network`
- `io`
- `lock`
- `cancelled`
- `usage`

Package-manager and development-build rejections are unavailable errors, not failed updates.

Replacement or restoration failures are failed updates and must include enough path context for manual recovery.

## Tests

Prefer temp-directory tests and fake network responses. Do not hit GitHub in default tests.

Minimum coverage:

- install method detection:
  - `/opt/homebrew/bin/fontbrew` rejects as Homebrew-managed;
  - `/usr/local/Cellar/.../fontbrew` rejects as Homebrew-managed;
  - `/opt/local/bin/fontbrew` rejects as MacPorts-managed;
  - `~/.cargo/bin/fontbrew` rejects as Cargo-managed;
  - path containing `/target/debug/fontbrew` rejects as dev build;
  - path containing `/target/release/fontbrew` rejects as dev build;
  - tempdir `bin/fontbrew` is allowed when writable.
- version rules:
  - older current updates;
  - equal current skips unless `--force`;
  - newer current skips unless `--force`;
  - invalid latest release tag errors.
- checksum parsing:
  - accepts typical `<hash>  <filename>`;
  - rejects wrong filename;
  - rejects malformed hash;
  - rejects mismatched hash.
- release asset contract:
  - selects target asset and checksum;
  - rejects missing target asset;
  - rejects archive without `fontbrew-${target}/fontbrew`.
- replacement flow:
  - successful replacement;
  - pre-replacement failure leaves current unchanged;
  - replacement failure restores backup when possible;
  - post-replacement smoke-test failure restores backup when possible.
- reporting:
  - human stdout for up-to-date, planned, updated, reinstalled, and newer-current statuses;
  - JSON envelope uses `command = "self_update"`;
  - JSON errors expose stable error kinds.
- stream behavior:
  - JSON stdout contains only JSON;
  - human progress goes to stderr;
  - quiet suppresses progress.

Manual verification can include a real GitHub release download once release assets exist, but that must not be required for `cargo test --workspace`.

## Non-Goals

First version does not support:

- updating managed font packages through `self-update`;
- `fontbrew update --self`;
- selecting a specific version;
- prerelease, nightly, or channel updates;
- auto-running Homebrew, MacPorts, Nix, Cargo, or any external package manager;
- installing a second copy when the current executable is package-manager-managed;
- release signatures;
- Windows or Linux targets;
- install metadata files;
- symlink-specific behavior;
- exposing a general-purpose binary download command.

## Open Follow-Ups

- Add signed release verification if the release process adopts a signing mechanism.
- Consider version selection only after downgrade/rollback semantics are deliberately designed.
- Consider package-manager delegation only after Fontbrew has official package-manager distribution contracts.
