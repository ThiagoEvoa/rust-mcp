//! Internal library for the `rust-docs-mcp` documentation server.
//!
//! `plan.md` sections 4 and 8 call for one Cargo package with a binary
//! (`src/main.rs`) and this small internal library. The modules below mirror the
//! planned layout and are the crate's **internal entry points**; each is owned by
//! a later milestone and is filled in by that task:
//!
//! - [`model`] — corpus and tool-contract types (`[007]`-`[008]`),
//! - [`ingest`] — fetching, extraction, validation, publication (`[010]`-`[028]`),
//! - [`corpus`] — snapshot loading, IDs, bounded reads (`[029]`-`[030]`),
//! - [`search`] — tokenization, ranking, pagination (`[033]`-`[047]`),
//! - [`commands`] — source-backed command catalog/guidance (`[039]`-`[044]`),
//! - [`mcp`] — tool/resource handlers and protocol errors (`[048]`-`[059]`).
//!
//! Every module is **private** by default: per `plan.md` section 8 the package
//! keeps its implementation details private unless a caller needs them. Only the
//! [`bootstrap_check`] self-check below is exposed, so the binary can confirm the
//! package boots without any domain logic present yet.

mod commands;
mod corpus;
mod ingest;
mod mcp;
mod model;
mod search;

/// No-op self-check that the package and its module tree assemble.
///
/// Returns a short human-readable status string. The `[002]` bootstrap wires the
/// binary to exactly this one public symbol; task `[003]` replaces the stub with
/// the real `serve`/`sync`/`status` dispatch.
pub fn bootstrap_check() -> &'static str {
    "bootstrap: package + module tree assembled"
}
