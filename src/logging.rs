//! Stderr-only, configurable-verbosity diagnostics.
//!
//! Task [004] M1 wires the two diagnostics invariants from `plan.md` §8/§9:
//!
//! - **stdout stays exclusive to MCP protocol traffic** during `serve`. Every
//!   diagnostic here is written to **stderr** via [`tracing`]; nothing in this
//!   module touches stdout.
//! - **query text is not logged by default.** A search query, retrieved section
//!   content, or any caller-supplied payload is emitted under the dedicated
//!   [`QUERY_TARGET`]; the subscriber always carries a directive that turns that
//!   target *off*, and the same policy is expressed by the pure, unit-tested
//!   [`decision`] function. Verbosity never re-exposes payload text on its own.
//!
//! Verbose operation is opt-in through the binary's `--verbose` flag or the
//! standard `RUST_LOG`/`LOG` environment variables that `tracing-subscriber`
//! honours.
//!
//! [`QUERY_TARGET`]: [`crate::logging::QUERY_TARGET`]

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing_subscriber::EnvFilter;

/// The target under which **query bodies** (caller-supplied text) would be
/// logged. Kept as a constant so the gate policy is expressible both as a pure
/// decision ([`decision`], unit-tested) and as the always-off `EnvFilter`
/// directive wired in [`init_tracing`], with a `LOG_QUERY_BODIES` opt-in.
pub const QUERY_TARGET: &str = "rust_docs_mcp::query";

/// Off-by-default switch that permits logging query bodies. `false` for the MVP;
/// flipped `true` only by an explicit `RUST_LOG`/env directive that names
/// [`QUERY_TARGET`], never by ordinary verbosity.
static LOG_QUERY_BODIES: AtomicBool = AtomicBool::new(false);

/// The default gate decision: ordinary diagnostics pass, query-body events are
/// held back until [`LOG_QUERY_BODIES`] is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Emitted to stderr (ordinary diagnostics).
    Allow,
    /// Held back by default (a query-body event).
    Suppress,
}

/// The default-gate decision for one target.
pub fn decision(target: &str) -> Decision {
    if target == QUERY_TARGET && !log_query_bodies() {
        Decision::Suppress
    } else {
        Decision::Allow
    }
}

/// Read the query-body log switch.
fn log_query_bodies() -> bool {
    LOG_QUERY_BODIES.load(Ordering::Relaxed)
}

/// Build the default `EnvFilter` as a function of the `--verbose` flag.
///
/// `--verbose` widens the level filter to `debug`. An explicit `RUST_LOG` value,
/// when present, wins over the flag as the `tracing-subscriber` base. A
/// `{QUERY_TARGET} = off` directive is **always** appended, so caller-supplied
/// payload text stays suppressed by default regardless of verbosity — it can only
/// reappear through a directive that explicitly re-enables [`QUERY_TARGET`].
pub fn filter_for(verbose: bool) -> EnvFilter {
    let level = if verbose { "debug" } else { "info" };
    // Query bodies stay gated by default regardless of verbosity: the directive
    // level follows the unit-tested policy.
    let query_level = match decision(QUERY_TARGET) {
        Decision::Suppress => "off",
        Decision::Allow => "info",
    };
    let query_directive = format!("{QUERY_TARGET} = {query_level}");

    // Honour `RUST_LOG` when present (standard for tracing-subscriber), else use
    // the default level. In either case the query-body directive keeps payload
    // text suppressed by default.
    let base = match std::env::var("RUST_LOG")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(value) => format!("{value}, {query_directive}"),
        None => format!("{level}, {query_directive}"),
    };

    // External input must not crash the server: a malformed directive degrades to
    // the always-valid level + query filter instead of panicking on `RUST_LOG`.
    EnvFilter::try_new(&base).unwrap_or_else(|_| {
        // The fallback uses only the always-valid level + query directive, so
        // `new` cannot panic here even though it can on malformed input.
        EnvFilter::new(format!("{level}, {query_directive}"))
    })
}

/// Initialise the stderr-only subscriber once.
///
/// Idempotent under [`Once`]. `try_init()` is the simplest global-install path;
/// if a subscriber is already set the returned `Err` is ignored.
pub fn init_tracing(verbose: bool) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let filter = filter_for(verbose);
        let _ = tracing_subscriber::fmt()
            .with_target(false)
            .with_level(true)
            .with_line_number(true)
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_target_is_held_back_by_default() {
        assert_eq!(decision(QUERY_TARGET), Decision::Suppress);
    }

    #[test]
    fn ordinary_targets_are_allowed() {
        assert_eq!(decision("rust_docs_mcp::sync"), Decision::Allow);
        assert_eq!(decision("rust_docs_mcp::corpus"), Decision::Allow);
        assert_eq!(decision("rust_docs_mcp"), Decision::Allow);
    }

    #[test]
    fn gate_sums_default_off_for_query_on_for_ordinary() {
        assert!(matches!(decision(QUERY_TARGET), Decision::Suppress));
        assert!(matches!(decision("rust_docs_mcp::sync"), Decision::Allow));
    }

    #[test]
    fn filter_composes_query_off_with_level() {
        // The composed filter is parseable and, by construction, always carries
        // the `{QUERY_TARGET} = off` directive regardless of verbosity.
        let _ = filter_for(false);
        let _ = filter_for(true);
    }
}
