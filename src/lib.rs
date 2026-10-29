//! Internal library for the `rust-docs-mcp` documentation server.
//!
//! `plan.md` sections 4 and 8 call for one Cargo package with a binary
//! (`src/main.rs`) and this small internal library. The modules below mirror the
//! planned layout and are the crate's **internal entry points**; each is owned by
//! a milestone and is filled in by that task:
//!
//! - [`cli`] — command-line parsing and application-data configuration
//!   (`[003]`), plus the pure data-directory resolution helpers,
//! - [`error`] — the project-wide typed error layer (`[004]`),
//! - [`logging`] — stderr-only, configurable-verbosity diagnostics with the
//!   query-body gate (`[004]`),
//! - [`model`] — corpus and tool-contract types (`[007]`-`[008]`),
//! - [`ingest`] — fetching, extraction, validation, publication (`[010]`-`[028]`),
//! - [`corpus`] — snapshot loading, IDs, bounded reads (`[029]`-`[030]`),
//! - [`search`] — tokenization, ranking, pagination (`[033]`-`[047]`),
//! - [`commands`] — source-backed command catalog/guidance (`[039]`-`[044]`),
//! - [`mcp`] — tool/resource handlers and protocol errors (`[048]`-`[059]`).
//!
//! Modules are **private** by default: per `plan.md` section 8 the package keeps
//! its implementation details private unless a caller needs them. Only the
//! data-directory helpers below are exposed, because the binary ([`main`],
//! via the `cli` module) needs to resolve and use a data directory without
//! reaching into the module tree.

mod cli;
mod commands;
mod corpus;
mod error;
mod ingest;
mod logging;
mod mcp;
mod model;
mod search;

pub use cli::{Cli, Command, DataDirError, apply_override, default_data_dir, resolve_data_dir};
pub use error::{AppError, AppResult};
pub use logging::{QUERY_TARGET, init_tracing};
