# Repository Guidelines

## Project Structure & Module Organization

Fontbrew is a Rust 2021 workspace with two crates under `crates/`. `crates/fontbrew-core` contains reusable domain logic and core APIs for Fontsource, GitHub, local archives, manifests, activation, archive handling, config, and command flows. `crates/fontbrew-cli` contains the `fontbrew` binary, CLI parsing, confirmation flow, progress handling, exit mapping, and human/JSON reporters. Integration tests live in each crate's `tests/` directory. Offline test assets live in `fixtures/fonts/`; keep fixture provenance documented in `fixtures/fonts/README.md`. Product notes, ADRs, plans, and verification records live under `docs/`.

## Build, Test, and Development Commands

- `cargo build --workspace`: build both crates.
- `cargo run -p fontbrew-cli -- --help`: run the CLI from source.
- `cargo run -p fontbrew-cli -- list`: run a normal development command.
- `cargo fmt --all`: format all Rust code.
- `cargo clippy --workspace --all-targets`: lint library, binary, and tests.
- `cargo test --workspace`: run the full test suite.

Use `GITHUB_TOKEN` only as a process environment variable when needed; do not persist secrets in config, manifests, fixtures, or docs.

## Coding Style & Naming Conventions

Follow standard `rustfmt` output and Rust 2021 idioms. Workspace lints forbid `unsafe_code`; keep new code safe by default. Use `snake_case` for modules, functions, variables, and test names; use `PascalCase` for types and enum variants. Keep CLI-facing output behavior consistent: human command results go to stdout, while progress, prompts, warnings, diagnostics, and errors go to stderr; JSON mode should emit only structured JSON on stdout.

## Testing Guidelines

Prefer temp-directory tests for filesystem behavior so tests do not touch user font directories or real Fontbrew state. Put integration coverage in `crates/fontbrew-core/tests/*.rs` or `crates/fontbrew-cli/tests/*.rs`, and name tests after the behavior being verified, such as `manifest_persists_installed_package`. Keep network-dependent behavior mocked, fixture-backed, or guarded by explicit environment setup.

## Commit & Pull Request Guidelines

Recent history uses concise conventional-style subjects such as `feat: add fontsource provider adapter`, `fix: expose managed files in info and remove plans`, and `docs: finalize mvp usage and verification notes`. Use the same imperative, scoped style. Pull requests should describe behavior changes, list verification commands run, link related issues or docs, and include CLI output examples when changing user-visible text or JSON schemas.
