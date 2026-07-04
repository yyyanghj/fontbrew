# CLI Output Streams Research

Date: 2026-07-03

## Question

Do mainstream CLI tools send human progress, warnings, and diagnostics to stderr while keeping stdout for primary command output or machine-readable data?

## Findings

Git explicitly sends clone progress to stderr by default when attached to a terminal, and `--progress` forces progress to stderr even when stderr is not a terminal. `--quiet` suppresses progress on stderr. Source: https://git-scm.com/docs/git-clone

Cargo's contributor guide says most Cargo output goes to stderr, while JSON mode goes to stdout. `cargo metadata` also documents JSON output to stdout and recommends an explicit format version for future compatibility. Sources: https://doc.crates.io/contrib/implementation/console.html and https://doc.rust-lang.org/cargo/commands/cargo-metadata.html

npm's config documentation says its progress bar is shown during time-intensive operations only when both stderr and stdout are TTYs, and can be disabled with `progress=false`. npm's logger package states that logs are written to stderr by default. Sources: https://docs.npmjs.com/cli/v11/using-npm/config/ and https://github.com/npm/npmlog

pnpm exposes reporter modes for install progress/debug output. Its default reporter is used for TTY stdout, append-only is used for non-TTY stdout, and `ndjson` is a structured log reporter. Source: https://pnpm.io/cli/install

curl has a progress meter and disables it when response data would be written to the terminal because it would mix progress and response output. It also has `--stderr` to redirect stderr writes and `--no-progress-meter` to suppress progress without muting warnings/errors. Source: https://curl.se/docs/manpage.html

kubectl's Go CLI architecture separates `In`, `Out`, and `ErrOut` streams. The default kubectl command wires `Out` to `os.Stdout` and `ErrOut` to `os.Stderr`, and warning output uses `ErrOut`. Sources: https://github.com/kubernetes/cli-runtime/blob/master/pkg/genericiooptions/io_options.go and https://github.com/kubernetes/kubernetes/blob/master/staging/src/k8s.io/kubectl/pkg/cmd/cmd.go

Homebrew documents `brew info --json` as JSON-formatted information designed for parsing, with explicit schema versioning behavior. Source: https://docs.brew.sh/Querying-Brew

## Conclusion

The common pattern is:

- stdout is for the primary result the caller asked for.
- stderr is for human-facing progress, diagnostics, warnings, prompts, and errors.
- JSON or other machine-readable modes should keep stdout clean.
- TTY detection should decide whether progress bars/spinners are shown.
- Structured output should be versioned or designed for backward-compatible extension.

Fontbrew should follow this pattern:

- Human table/result output goes to stdout.
- Progress, warnings, confirmation prompts, and diagnostics go to stderr.
- `--json` writes only JSON to stdout and disables interactive prompts unless paired with `--yes` or `--dry-run`.
- Progress bars render only when stderr is a TTY.
