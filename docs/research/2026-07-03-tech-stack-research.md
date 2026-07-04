# Fontbrew Tech Stack Research

Date: 2026-07-03

## Question

Should Fontbrew use Rust, Go, or Node.js for the MVP CLI, given the requirement to depend only on mature, ecosystem-tested third-party libraries?

## Findings

Rust is a strong fit for a macOS font package manager because it produces native binaries, has first-class package tooling through Cargo, and is designed for reliable, efficient software with memory safety and no garbage collector. Sources: https://www.rust-lang.org/ and https://doc.rust-lang.org/cargo/.

The mature Rust dependency set for Fontbrew's general CLI needs is strong:

- `clap` for command parsing: https://docs.rs/clap/
- `serde`, `serde_json`, and `toml` for manifest, registry, and config data: https://serde.rs/
- `reqwest` for HTTP calls, including blocking clients and JSON support: https://docs.rs/reqwest/
- `zip` for ZIP archive reading: https://docs.rs/zip/latest/zip/
- `tempfile` for staging and cleanup-safe temporary files: https://docs.rs/tempfile/
- `directories` for OS-specific config/data paths: https://docs.rs/directories/latest/directories/
- `globset` for registry asset include/exclude matching: https://docs.rs/globset/latest/globset/

The main Rust dependency decision is font parsing. Two serious options exist:

- `ttf-parser`: high-level, safe, zero-allocation parser for TrueType, OpenType, and AAT fonts. Source: https://docs.rs/ttf-parser/
- `skrifa` / `read-fonts`: Google Fonts-backed Fontations crates for OpenType metadata and lower-level font parsing. Sources: https://docs.rs/skrifa/latest/skrifa/, https://docs.rs/read-fonts/latest/read_fonts/, and https://github.com/googlefonts/fontations.

Go is viable for the CLI and filesystem work. Its standard library covers HTTP, JSON, and ZIP well, and Cobra is mature for CLI structure. Sources: https://pkg.go.dev/net/http, https://pkg.go.dev/encoding/json, https://pkg.go.dev/archive/zip, and https://pkg.go.dev/github.com/spf13/cobra. The weaker point is font metadata parsing depth: `golang.org/x/image/font/sfnt` can decode TTF/OTF, but its docs describe it as low-level. Source: https://pkg.go.dev/golang.org/x/image/font/sfnt.

Node.js is viable for rapid development and has mature font libraries. `fontkit` supports TTF, OTF, WOFF, WOFF2, TTC, metadata properties, variations, and more; `opentype.js` is also a widely used parser/writer. Sources: https://github.com/foliojs/fontkit and https://opentype.js.org/. The weaker point is distribution: Node's official single executable application feature exists, but introduces a packaging layer and runtime embedding complexity compared with Rust or Go native binaries. Source: https://nodejs.org/api/single-executable-applications.html.

Provider integration is not language-specific. Google Fonts exposes a REST JSON Developer API with family metadata, variants, version, last modified date, and file URLs, but requires an API key. Source: https://developers.google.com/fonts/docs/developer_api. Fontsource exposes a read-only HTTP API with documented rate-limit behavior. Source: https://fontsource.org/docs/api/introduction.

## Recommendation

Use Rust for the MVP, but keep the dependency policy conservative:

1. Use mature, common crates for CLI, HTTP, JSON/TOML, ZIP, temporary files, paths, globs, and version parsing.
2. Hide font parsing behind a small `FontMetadataReader` interface.
3. Start with `ttf-parser` if MVP metadata needs are limited to family, subfamily, names, and basic style attributes.
4. Switch or expand to `skrifa` / `read-fonts` if variable fonts, font collections, localized names, or deeper OpenType metadata require it.

Go is the credible fallback if Rust learning cost dominates. Node.js is best treated as a prototype option, not the first choice for a polished system CLI.
