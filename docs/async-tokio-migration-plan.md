# Async Tokio Migration Plan

Date: 2026-07-05

## Status

Proposed implementation plan. This document records the shared design decisions before changing the workspace to async Rust and Tokio.

## Context

Fontbrew currently uses synchronous Rust APIs throughout both crates. The CLI entry point is synchronous, `FontbrewApp` exposes synchronous methods, and the remote paths use `reqwest::blocking::Client` behind a generic `HttpClient` trait.

Blocking work is broader than HTTP:

- GitHub and Fontsource metadata fetches.
- Remote font and release asset downloads.
- Provider metadata snapshot reads and writes.
- ZIP and tar extraction.
- Font metadata parsing through `ttf-parser`.
- Manifest and config reads and atomic writes.
- Package store copying, activation, remove, update replacement, self-update replacement, and filesystem locking.

The most important existing safety rule is that planning may prepare staged files, but applying install/update/remove work is transactional and guarded by the global file lock. Async migration must preserve that rule.

## Goals

- Make the core command flows async-first.
- Use Tokio as the async runtime for both `fontbrew-core` and `fontbrew-cli`.
- Remove `reqwest::blocking` and use async `reqwest::Client`.
- Keep CLI behavior stable: stdout/stderr separation, JSON stdout purity, cancellation cleanup, and confirmation behavior.
- Preserve update semantics: `update --jobs` controls package-level prepare concurrency.
- Keep transactional filesystem mutation serialized and rollback-aware.

## Non-Goals

- Do not preserve the synchronous `fontbrew-core` public interface.
- Do not add runtime-agnostic abstractions for async-std or other executors.
- Do not make apply/install/update/remove transactions internally concurrent.
- Do not introduce fine-grained download progress or cross-task reporter writes in the first migration.
- Do not rewrite all network tests to a mock HTTP server in the same change.

## Decisions

### Async-first core

`fontbrew-core` should accept a breaking change. Public app methods become async instead of gaining parallel sync and async variants.

Example target shape:

```rust
impl FontbrewApp {
    pub async fn install_plan_with_progress_and_cancellation(
        &self,
        request: InstallRequest,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallPlan>;

    pub async fn apply_install(
        &self,
        plan: InstallPlan,
        policy: ExecutionPolicy,
        progress: &mut dyn ProgressSink,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<InstallReport>;
}
```

Do not create Tokio runtimes inside library functions. The CLI owns the runtime through `#[tokio::main]`.

`ProgressSink` stays as a borrowed, synchronous CLI-facing interface and must not cross a `spawn_blocking` call. The async app method may emit progress before and after awaited work, but blocking helpers must accept owned inputs and return progress events for the caller to emit after the await.

### Tokio in both crates

Both crates should depend on Tokio. Core needs Tokio for `spawn_blocking`, bounded async orchestration, and limited async filesystem use. CLI needs Tokio for the runtime.

Suggested workspace dependency shape:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "fs", "io-util", "sync"] }
futures = "0.3"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
```

`futures` is useful for scoped bounded concurrency with borrowed context, such as `stream::iter(...).buffered(jobs)`, without forcing every prepare task into a `'static` `tokio::spawn`.

`download_to_file` should use `reqwest::Response::chunk().await` instead of `bytes_stream()` in the first migration. That avoids adding reqwest's optional `stream` feature while still streaming downloads in bounded chunks.

### Network module instead of generic HTTP trait

Replace the current generic `HttpClient` trait and `ReqwestHttpClient` wrapper with a concrete network module.

Target shape:

```rust
pub struct NetworkClient {
    client: reqwest::Client,
}

impl NetworkClient {
    pub fn new() -> Result<Self>;
    pub fn with_client(client: reqwest::Client) -> Self;

    pub async fn get(&self, request: NetworkRequest) -> Result<NetworkResponse>;

    pub async fn download_to_file(
        &self,
        request: NetworkRequest,
        destination: &Path,
        max_bytes: u64,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<u64>;
}
```

This keeps one deep module for transport behavior: timeout setup, header application, display URL redaction, status handling, content length rejection, chunked download, size accounting, cancellation checks, destination cleanup on failure, and network error mapping.

Provider and GitHub modules should stay as ordinary business modules that call `NetworkClient`. They should not each create their own `reqwest::Client`.

The external seam is `NetworkClient`, not a public transport trait. Tests may keep temporary internal helpers during migration, but the production caller interface should not depend on fake HTTP adapters.

To keep the first migration bounded, `NetworkClient` may contain a crate-private or test-only transport seam. That seam is an implementation detail of the network module, not part of the production interface. Existing high-level tests can route through it while focused `NetworkClient` tests use a local HTTP server. A later cleanup can replace the remaining high-level fake routes with local-server tests and configurable provider/GitHub endpoints.

### Blocking filesystem work

Do not mechanically replace every `std::fs` call with `tokio::fs`.

Use async filesystem APIs only where the operation is small, non-transactional, and does not require low-level durability behavior. Use `tokio::task::spawn_blocking` for:

- ZIP and tar extraction.
- Font metadata parsing.
- SHA-256 file hashing.
- Directory scans.
- Large package file copies.
- Atomic writes using `tempfile`, `persist`, `flush`, `sync_all`, and parent directory sync.
- `GlobalFileLock` acquisition and all work performed while holding that lock.
- Self-update executable replacement and smoke-test process execution.

No async function should `await` while holding `GlobalFileLock` or while halfway through a package store / activation / manifest transaction. If a transaction needs blocking work, run the whole transaction inside a blocking closure and return the final report plus any progress events that must be emitted afterward.

Blocking helper interfaces must be owned and `Send + 'static`. Do not pass `&mut dyn ProgressSink`, `&dyn CancellationToken`, or borrowed path/request data into `spawn_blocking`. Use owned request structs and `Arc<dyn CancellationToken>`.

Example target shape:

```rust
struct ApplyInstallBlockingInput {
    paths: FontbrewPaths,
    plan: InstallPlan,
    policy: ExecutionPolicy,
    cancellation: Arc<dyn CancellationToken>,
}

fn apply_install_blocking(
    input: ApplyInstallBlockingInput,
) -> Result<(InstallReport, Vec<ProgressEvent>)>;
```

The async caller awaits the join handle, then emits the returned progress events through the borrowed `ProgressSink`.

### Self-update locking

Self-update must not hold `GlobalFileLock` across network awaits.

Split self-update into:

1. Async prepare: detect install method, resolve the latest release, decide the planned action, ask for confirmation, create staging, download archive and checksum, verify checksum in blocking work, extract the candidate binary in blocking work, set executable bits, and smoke-test the candidate binary.
2. Blocking replace critical section: acquire `GlobalFileLock`, re-read the current executable state, re-run `fontbrew --version` or equivalent version validation, re-evaluate whether the prepared release is still applicable, perform backup/rename/sync/smoke-test, and remove the backup after success.

If the current executable changes after prepare and before the locked replace section, abort with a clear self-update error unless the revalidated state still matches the planned operation. Do not perform async HTTP inside the locked replace section.

### Update concurrency

Keep the current user-facing meaning of `update --jobs`: the number of packages prepared concurrently.

Implementation should replace `tasks::map_bounded` with bounded async concurrency. Preserve deterministic result ordering by carrying each input index and sorting or collecting in input order after completion.

Package preparation remains sequential inside one package for the first migration:

1. Resolve latest version.
2. Download candidate asset or provider files.
3. Extract if needed.
4. Parse font metadata.
5. Validate package identity.
6. Return prepared package or per-package failure.

Do not add provider asset-level download concurrency in this migration.

### Progress and reporting

Reporter access stays serialized through the CLI task. Do not write to `Reporter` or `ProgressSink` from multiple Tokio tasks.

First migration rules:

- `ProgressSink` remains a synchronous, non-shared interface.
- Single-package install can emit progress directly from the main command flow.
- `update --jobs` emits `PreparingUpdate` before scheduling work and emits apply progress from the serial apply phase.
- Concurrent prepare tasks should return outcomes, not write to stderr.
- JSON mode continues to emit only final structured JSON on stdout.

A later change can introduce a `tokio::mpsc` progress event channel if richer concurrent progress is worth the extra interface.

### Cancellation

Keep the existing `CancellationToken` trait for the first migration. Do not add `tokio-util` just for cancellation.

Async code should check cancellation:

- Before starting remote requests.
- Between response chunks during downloads.
- Before and after `spawn_blocking` calls.
- Before applying a prepared plan.
- After cancellation-sensitive prepare work, with staging cleanup preserved.

Blocking closures cannot be preempted by Tokio cancellation. They must be small enough to complete, or they must keep using explicit cancellation checks inside existing loops where available.

The public async interface is not drop-cancel-safe. Fontbrew's supported cancellation contract is cooperative: set the cancellation token and continue awaiting the future until it returns and cleanup runs. Dropping or aborting a future after it has started `spawn_blocking` may allow detached blocking work to keep running, and cleanup or rollback guarantees are not provided for that usage. CLI commands should always signal cancellation through the token and await command completion.

## Migration Steps

### 1. Dependencies and entry point

- Add Tokio and `futures` workspace dependencies.
- Change `reqwest` workspace features from `blocking` to async Rustls usage. Do not add the `stream` feature unless the implementation switches from `Response::chunk()` to `bytes_stream()`.
- Convert `fontbrew-cli/src/main.rs` to `#[tokio::main] async fn main() -> ExitCode`.
- Convert `cli::run`, `execute`, and command handlers to async.

### 2. Network module

- Replace `fetch::ReqwestHttpClient` with `NetworkClient`.
- Remove the public generic `HttpClient` trait from production code.
- Implement async `get` and chunked `download_to_file`.
- Keep `HttpRequest`/`HttpResponse` names or rename them to `NetworkRequest`/`NetworkResponse`; choose one naming scheme and update callers consistently.
- Preserve display URL redaction and cleanup-on-failed-download behavior.
- Add a crate-private or test-only network transport seam for high-level tests that cannot move to local HTTP server tests in the first migration.

### 3. Async provider and GitHub flows

- Convert Fontsource list/detail fetches and GitHub release lookup to async.
- Keep parsing and asset selection as pure synchronous helpers.
- Keep provider metadata snapshot freshness semantics unchanged.
- Avoid constructing clients inside provider/GitHub functions.

### 4. Async app interface

- Convert `FontbrewApp` methods to async.
- Store an `Arc<NetworkClient>` or lazily create one through a helper.
- Accept cancellation as `Arc<dyn CancellationToken>` so async and blocking work can receive owned, cloneable cancellation handles.
- Replace `with_paths_and_http_client` with a testable constructor such as `with_paths_and_network_client`.
- Remove synchronous app methods instead of adding compatibility wrappers.

### 5. Plan-stage blocking isolation

- Move archive extraction, font parsing, and local archive reads behind blocking helper calls.
- Keep staging cleanup behavior identical on prepare errors and cancellation.
- Keep provider asset downloads serial within one package.

### 6. Apply-stage blocking isolation

- Keep install, update, and remove mutation phases serial.
- Ensure the global write lock is held only inside blocking code.
- Avoid `await` between package store copy, activation change, and manifest commit.
- Preserve rollback behavior for manifest write failure, activation failure, cancellation, and self-update replacement failure.
- Route blocking apply helpers through owned `Send + 'static` input structs and return `(Report, Vec<ProgressEvent>)` where progress must be emitted after await.

### 7. Self-update split

- Move release lookup, confirmation, download, checksum verification, extraction, candidate chmod, and candidate smoke test into async prepare plus short blocking helpers.
- Limit `GlobalFileLock` to the blocking replace critical section.
- Revalidate the current executable/version after acquiring the lock and before replacing the executable.
- Abort if the installed binary changed after prepare and the prepared release no longer matches the planned operation.

### 8. Async update jobs

- Replace `tasks::map_bounded` with bounded async concurrency.
- Preserve input-order reporting of prepared, failed, and up-to-date outcomes.
- Keep `jobs` defaulting to config `update_concurrency`, with a minimum of one.

### 9. Tests

- Convert affected tests to `#[tokio::test]`.
- Keep filesystem tests temp-directory based.
- Preserve current fake-route behavior through migration-local helpers where needed.
- Add focused `NetworkClient` tests with a local HTTP server for status handling, headers, chunked response reads, size rejection, and failed download cleanup.
- Defer broad replacement of existing higher-level fake HTTP tests with mock-server tests to a separate cleanup change.

## Testing Strategy

Required checks after each migration slice:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

Additional behavior to verify:

- Human output still writes command results to stdout and progress/errors to stderr.
- JSON mode still writes only JSON to stdout.
- Ctrl-C cancellation removes staging for downloads, archive extraction failures, and prepared update plans.
- Failed downloads remove partial destination files.
- Provider metadata snapshots are still metadata-only.
- Update planning with `--jobs 1` and `--jobs 2` produces deterministic reports.
- Apply phases do not interleave writes across packages.
- Blocking apply helpers do not capture borrowed `ProgressSink`, borrowed cancellation tokens, or borrowed paths.
- Cancellation tests use token cancellation plus awaiting completion; future drop/abort is documented as outside the cleanup guarantee.
- Self-update does not hold the global lock while fetching release metadata or downloading assets.
- Self-update revalidates the current executable inside the locked replace section.
- Self-update still restores the original executable on replacement or smoke-test failure.

## Risks and Mitigations

- **Runtime blocking:** Put extraction, parsing, atomic writes, locks, and replacement transactions in `spawn_blocking`.
- **Transaction fragmentation:** Do not insert `await` points inside package store / activation / manifest mutation sequences.
- **Test churn:** Migrate tests in two phases; do not combine async conversion with a full mock-server rewrite.
- **Progress interleaving:** Keep reporter writes on the main command path.
- **Cancellation gaps:** Preserve explicit cancellation checks and cleanup guards around staging.
- **Future drop cancellation:** Document that cooperative token cancellation is the supported model and do not claim drop/abort cleanup guarantees around detached blocking work.
- **Self-update lock scope:** Split self-update into async prepare and blocking locked replace, with executable revalidation after acquiring the lock.
- **Hidden sync compatibility:** Do not add sync wrappers that create nested runtimes.
- **Network abstraction drift:** Keep one concrete `NetworkClient` module; any test transport seam remains crate-private or test-only unless a second production adapter appears.

## References

- Tokio `spawn_blocking`: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html
- Tokio filesystem module notes: https://docs.rs/tokio/latest/tokio/fs/
- reqwest `Response`: https://docs.rs/reqwest/latest/reqwest/struct.Response.html
- futures `StreamExt::buffered`: https://docs.rs/futures/latest/futures/stream/trait.StreamExt.html#method.buffered
- Cargo registry HTTP implementation: https://github.com/rust-lang/cargo/blob/master/src/cargo/sources/registry/http_remote.rs
- rustup download flow: https://github.com/rust-lang/rustup/blob/master/src/dist/download.rs
- uv client modules: https://github.com/astral-sh/uv/tree/main/crates/uv-client/src
