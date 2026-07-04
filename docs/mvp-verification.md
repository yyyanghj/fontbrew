# MVP Verification

Date: 2026-07-04

This note records the final Task 20 verification pass for the Fontbrew MVP.

## Automated Verification

Run from the repository root:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

Result:

- `cargo fmt --all` passed.
- `cargo clippy --workspace --all-targets` passed after final warning cleanup.
- `cargo test --workspace` passed.

The test suite uses temp directories or injected paths for filesystem behavior. The CLI integration tests set an isolated `HOME`; core filesystem tests use `FontbrewPaths::for_tests` or direct temp paths.

## CLI Smoke Test

Smoke test variables:

```text
SMOKE_ROOT=$TMPDIR/fontbrew-smoke
HOME_DIR=$SMOKE_ROOT/home
REGISTRY=$SMOKE_ROOT/registry.json
ARCHIVE=$SMOKE_ROOT/source-code-pro.zip
```

The smoke test used:

```text
HOME=$HOME_DIR
FONTBREW_REGISTRY_URL=file://$REGISTRY
```

The local archive fixture was built from committed Source Code Pro fixture fonts:

```text
$ARCHIVE
```

Commands verified:

```bash
HOME="$HOME_DIR" target/debug/fontbrew --quiet install "$ARCHIVE"
HOME="$HOME_DIR" target/debug/fontbrew list
HOME="$HOME_DIR" target/debug/fontbrew info source-code-pro
HOME="$HOME_DIR" target/debug/fontbrew remove source-code-pro
FONTBREW_REGISTRY_URL="file://$REGISTRY" HOME="$HOME_DIR" target/debug/fontbrew registry update
HOME="$HOME_DIR" target/debug/fontbrew search inter
HOME="$HOME_DIR" target/debug/fontbrew --quiet install inter
HOME="$HOME_DIR" target/debug/fontbrew --quiet install --format ttf inter
HOME="$HOME_DIR" target/debug/fontbrew info inter
HOME="$HOME_DIR" target/debug/fontbrew outdated inter
HOME="$HOME_DIR" target/debug/fontbrew update --dry-run inter
HOME="$HOME_DIR" target/debug/fontbrew remove --dry-run inter
```

Observed results:

- Local archive install reported `Installed source-code-pro local (Source Code Pro)`.
- `list` showed `source-code-pro` active before removal.
- `info source-code-pro` showed the expected family and source.
- `remove source-code-pro` reported removal.
- `registry update` loaded a one-package test registry snapshot from `file://.../registry.json`.
- `search inter` returned `inter` as an installable registry result.
- `install inter` without a format override refused to choose because Inter's OTF, TTF, and TTC coverage differs.
- `install --format ttf inter` installed Inter from GitHub release `v4.1`.
- `info inter` reported source `registry:inter`, update source `github:rsms/inter`, activation state, managed state, update availability as `unknown`, installed files, and activation artifacts.
- `outdated inter` reported the update status for the GitHub-backed package.
- `update --dry-run inter` completed with `No updates prepared.`
- `remove --dry-run` reports the managed font files and activation artifacts that would be removed without mutating the package.

The smoke HOME contained only expected Fontbrew data and activation artifacts under the injected temp HOME:

```text
$HOME/.local/share/fontbrew/
$HOME/Library/Fonts/Fontbrew/
```

## Output Stream Check

Manual stream checks used redirected stdout and stderr:

```bash
HOME="$HOME_DIR" target/debug/fontbrew list >out 2>err
HOME="$HOME_DIR" target/debug/fontbrew info missing-package >out 2>err
HOME="$HOME_DIR" target/debug/fontbrew --json list >out 2>err
HOME="$HOME_DIR" target/debug/fontbrew --json info missing-package >out 2>err
```

Observed results:

- Human `list`: exit 0, package rows on stdout, stderr empty.
- Human missing `info`: exit 1, stdout empty, human error on stderr.
- JSON `list`: exit 0, parseable JSON on stdout, stderr empty.
- JSON missing `info`: exit 1, structured JSON error on stdout, stderr empty.

This matches the stream rules in `docs/implementation-design.md`: primary results and JSON payloads on stdout; human errors, warnings, prompts, progress, and diagnostics on stderr.

## MVP Trust Test

The spec's trust test is:

```text
Can the user always tell which fonts Fontbrew manages, where they came from,
what version they are, whether they can be updated, and what will be removed?
```

For the smoke-installed Inter package:

- `fontbrew list` showed the managed package ID, version, families, and active state.
- `fontbrew info inter` showed source, update source, version, families, activation state, managed state, update availability, installed files, and activation artifacts.
- `fontbrew outdated inter` reported the update status for the GitHub-backed package.
- `fontbrew update --dry-run inter` reported the update plan without applying changes.
- `fontbrew remove --dry-run inter` reported the managed font files and activation artifacts that would be removed without applying changes.
- Manifest-backed remove/update behavior is covered by automated CLI and core tests using injected paths.

## Reference Docs

Provider behavior was implemented against the official provider documentation:

- Fontsource API documentation: <https://fontsource.org/docs/api/introduction>
- Google Fonts Developer API documentation: <https://developers.google.com/fonts/docs/developer_api>
