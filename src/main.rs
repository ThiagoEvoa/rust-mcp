//! `rust-docs-mcp` command-line entry point.
//!
//! The [003] CLI skeleton: parse `serve` / `sync` / `status` plus an explicit
//! `--data-dir` override and dispatch to the matching handler. The subcommand
//! bodies are placeholders at this milestone — real runtime/ingestion wiring
//! lands in M2/M4 — but the command surface, data-directory resolution, and
//! error routing are complete.
//!
//! Per `plan.md` §8/§9 (and the AGENTS "product boundaries" note), during
//! `serve` **stdout carries only MCP protocol messages** and every diagnostic
//! goes to **stderr**. This binary honours that from the start: it owns no logic
//! of its own and delegates to the internal library's parsing helpers.

use rust_docs_mcp::Cli;

fn main() -> std::process::ExitCode {
    // `from_env` prints an actionable usage/argument message to stderr and exits
    // non-zero on a parse error ("invalid arguments produce actionable errors").
    Cli::from_env().run()
}
