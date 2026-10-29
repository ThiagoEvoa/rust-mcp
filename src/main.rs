//! `rust-docs-mcp` command-line entry point.
//!
//! This is the [002] bootstrap stub. The `serve` / `sync` / `status` subcommands
//! and their application-data configuration are added by task [003] (CLI and
//! application-data configuration), and typed errors plus stderr tracing are
//! added by task [004].
//!
//! Per `plan.md` section 8/section 9 (and the AGENTS "product boundaries" note),
//! during `serve` **stdout carries only MCP protocol messages** and every
//! diagnostic goes to **stderr**. This stub honours that from the start.
//!
//! The binary deliberately owns no logic and delegates to the internal library,
//! so the package and its module tree boot together as a unit.
use rust_docs_mcp::bootstrap_check;

fn main() {
    // No-op self-check for the [002] bootstrap; task [003] replaces this with the
    // CLI dispatch.
    bootstrap_check();

    // ASCII diagnostic on stderr only - never stdout.
    eprintln!("rust-docs-mcp: bootstrap self-check passed");
}
