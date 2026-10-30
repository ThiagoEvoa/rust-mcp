//! `rust-docs-mcp` command-line entry point.
//!
//! Parsed via the [`Cli`] surface (task [003]) and routed through the typed
//! error layer + stderr-only tracing established by task [004]. The subcommand
//! bodies are still labelled placeholders (their runtime/ingestion wiring lands
//! in M2/M4), but the diagnostic and error paths are now real: every diagnostic
//! goes to **stderr**, and a startup failure surfaces as an actionable message
//! plus a non-zero exit rather than a panic.
use rust_docs_mcp::{Cli, init_tracing};

fn main() -> std::process::ExitCode {
    // On a parse error `clap` already prints a concise, actionable usage message
    // to stderr and aborts non-zero, so a malformed invocation never reaches
    // dispatch.
    let cli = Cli::from_env();

    // stdio-exclusivity: install the stderr-only, gated subscriber before doing
    // any real work. A global can be installed once; `try_init()`'s failure (a
    // second install) is a harmless no-op for this single-process binary.
    init_tracing(cli.verbose);

    // Route every subcommand through the typed error layer: a startup failure
    // becomes an actionable stderr line (with its `source` chain) and a non-zero
    // exit — never an unwind on external input.
    match cli.run() {
        Ok(code) => code,
        Err(err) => {
            tracing::error!("rust-docs-mcp: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
