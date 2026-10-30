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
//! standard `RUST_LOG` environment variable that `tracing-subscriber` honours.
//! The query-body gate is **separate** and off by default: it can be turned on
//! only through the explicit `LOG_QUERY_BODIES` environment variable, never
//! through `--verbose` or `RUST_LOG` (see [`LOG_QUERY_BODIES`]).
//!
//! [`QUERY_TARGET`]: [`crate::logging::QUERY_TARGET`]

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing_subscriber::filter::EnvFilter;

/// The target under which **query bodies** (caller-supplied text) would be
/// logged. Kept as a constant so the gate policy is expressible both as a pure
/// decision ([`decision`], unit-tested) and as the always-off `EnvFilter`
/// directive wired in [`init_tracing`].
pub const QUERY_TARGET: &str = "rust_docs_mcp::query";

/// Off-by-default switch that permits logging caller-supplied query bodies.
///
/// Seeded `false` for the MVP. [`init_tracing`] flips it to `true` *only* when
/// the explicit `LOG_QUERY_BODIES` environment variable is truthy (`1`, `true`,
/// `yes`, or `on`, case-insensitive). It is **never** influenced by `--verbose`
/// or by the `RUST_LOG` base level, so ordinary verbosity cannot re-expose
/// payload text on its own — the query-body invariant from `plan.md` §9. Nothing
/// emits under [`QUERY_TARGET`] at M1, so the switch is a no-op today; the gate
/// is nonetheless enforced now, so a later M2/M4 query-body emit cannot bypass it
/// by accident.
static LOG_QUERY_BODIES: AtomicBool = AtomicBool::new(false);

/// Read the query-body log switch.
fn log_query_bodies() -> bool {
    LOG_QUERY_BODIES.load(Ordering::Relaxed)
}

/// The default gate decision: ordinary diagnostics pass, query-body events are
/// held back unless [`LOG_QUERY_BODIES`] is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Emitted to stderr (ordinary diagnostics).
    Allow,
    /// Held back by default (a query-body event).
    Suppress,
}

/// The default-gate decision for one target, reading the live
/// [`LOG_QUERY_BODIES`] switch.
///
/// `pub(crate)` (not `pub`): the gate decision is an internal policy, not a
/// public API. Only [`init_tracing`] and [`QUERY_TARGET`] are re-exported from
/// the crate root, so the hermetic cores and the public entry point share one
/// crate-visible surface.
pub(crate) fn decision(target: &str) -> Decision {
    decision_for(target, log_query_bodies())
}

/// Pure gate decision for one target given an explicit query-body opt-in flag.
///
/// This is the unit-testable core of the gate: it is independent of the
/// process-global [`LOG_QUERY_BODIES`] switch, so the opt-in branches can be
/// exercised without mutating global state under a parallel test harness.
/// [`filter_for_with_rust_log`] derives its query-body directive from this.
fn decision_for(target: &str, query_bodies: bool) -> Decision {
    if target == QUERY_TARGET && !query_bodies {
        Decision::Suppress
    } else {
        Decision::Allow
    }
}

/// Interpret a `LOG_QUERY_BODIES` value for the query-body opt-in.
///
/// A truthy value (`"1"`, `"true"`, `"yes"`, or `"on"`, case-insensitive and
/// surrounding whitespace ignored) enables logging query bodies; any other value
/// — including empty — leaves the gate in its safe default of `off`. Kept as a
/// pure function so the environment plumbing in [`init_tracing`] is unit-testable
/// without mutating the process environment.
fn parse_query_bodies(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Build the stderr, gated `EnvFilter` for a run.
///
/// `--verbose` widens the level filter to `debug` (otherwise it defaults to
/// `info`). A present `RUST_LOG` value forms the level *base* — it can never
/// re-enable [`QUERY_TARGET`], which the appended body-directive overrides. A
/// `{QUERY_TARGET}=off` directive is **always** appended, so caller-supplied
/// payload text stays suppressed by default regardless of verbosity; it can
/// reappear only through the explicit [`LOG_QUERY_BODIES`] opt-in.
///
/// The gate is wired as a directive *separate* from the base string: the query
/// directive is built without spaces around `=` and appended via
/// [`EnvFilter::add_directive`], so a base that came from `RUST_LOG` (with any
/// spacing the operator used) cannot corrupt it, and — appended last — it wins
/// over any base directive for the same target. This is what makes the gate
/// enforceable: a space-joined base string is exactly what made the original
/// gate a silent no-op (see `filter_enforces_query_body_gate_off_by_default`).
///
/// `pub(crate)` (not `pub`): the filter is an internal wiring detail; the public
/// surface the crate exposes is only [`init_tracing`] (which calls this) and
/// [`QUERY_TARGET`].
pub(crate) fn filter_for(verbose: bool) -> EnvFilter {
    // The live opt-in flows through the public gate. `decision(QUERY_TARGET)`
    // is `true` exactly when query-body events may emit, so this is the same
    // value as `log_query_bodies()`, routed through the named gate so the public
    // decision helper is the single source for "may this target emit".
    filter_for_with(verbose, decision(QUERY_TARGET) == Decision::Allow)
}

/// Compose the filter for a verbosity and an explicit query-body opt-in.
///
/// This is the hermetic core of [`filter_for`]; [`filter_for`] supplies the live
/// [`LOG_QUERY_BODIES`] value so the unit tests can drive the opt-in branch
/// without touching process-global state or the `RUST_LOG` environment.
pub(crate) fn filter_for_with(verbose: bool, query_bodies: bool) -> EnvFilter {
    // `RUST_LOG` is read here (not inside the pure core) so the degradation path
    // is unit-testable by passing the value directly.
    let rust_log = rust_log_base();
    filter_for_with_rust_log(verbose, query_bodies, rust_log.as_deref())
}

/// The core composition, decoupled from both the `LOG_QUERY_BODIES` switch and
/// `RUST_LOG` so the gate's guarantees are testable without global side effects.
///
/// `rust_log` is the raw `RUST_LOG` value when the variable is present. The gate
/// level follows the unit-tested [`decision_for`] policy (off unless the
/// explicit opt-in is on).
///
/// Deliberate asymmetry: the opt-in branch caps query bodies at `info` *even
/// under `--verbose`*. Verbose operation widens the base level for ordinary
/// diagnostics only; caller-supplied payload text stays bounded at `info` (never
/// `debug`/`trace`) so `--verbose` can never re-expose query text on its own. This
/// keeps the `plan.md` §9 invariant — query text is not logged by default, and
/// verbosity must not re-expose it — pinned rather than left to chance.
pub(crate) fn filter_for_with_rust_log(
    verbose: bool,
    query_bodies: bool,
    rust_log: Option<&str>,
) -> EnvFilter {
    // `--verbose` widens the *global* default level; otherwise it stays `info`.
    let level = if verbose { "debug" } else { "info" };

    // The base honours a *valid* `RUST_LOG` when present (the operator controls
    // the base level), else it defaults to the verbosity-derived level. External
    // input must never crash the server: a malformed or blank `RUST_LOG` degrades
    // to the default level via a lossy parse instead of panicking.
    let base = match rust_log {
        Some(spec) if !spec.trim().is_empty() => {
            EnvFilter::try_new(spec).unwrap_or_else(|_| EnvFilter::new(level))
        }
        _ => EnvFilter::new(level),
    };

    // The query-body gate. Built without spaces around `=` and appended as its
    // own directive, so it is whitespace-immune and always wins over the base.
    // The directive string is well-formed by construction
    // (`rust_docs_mcp::query={off|info}`), so parsing cannot fail; the `expect`
    // documents that invariant, and `filter_composes_query_off_with_level`
    // exercises the both-branch parse.
    //
    // The opt-in is capped at `info` deliberately: even with `--verbose` the
    // base widens to `debug` but the query target never does, so verbosity can
    // not re-expose caller-supplied payload text. See the doc on this function.
    let query_level = match decision_for(QUERY_TARGET, query_bodies) {
        Decision::Suppress => "off",
        Decision::Allow => "info",
    };
    let query_directive = format!("{QUERY_TARGET}={query_level}");
    base.add_directive(query_directive.parse().expect(
        "the query-body gate directive `rust_docs_mcp::query={{off|info}}` \
         is well-formed and always parses",
    ))
}

/// The `RUST_LOG` value to use as the level base, if present and non-blank.
///
/// Returns the raw value when the variable is set and non-empty/whitespace, and
/// `None` otherwise (so [`filter_for_with_rust_log`] falls back to its
/// verbosity-derived default). A *malformed* non-empty value is returned as-is;
/// [`filter_for_with_rust_log`] degrades it lossily rather than panicking.
fn rust_log_base() -> Option<String> {
    let value = std::env::var("RUST_LOG").ok()?;
    (!value.trim().is_empty()).then_some(value)
}

/// Initialise the stderr-only subscriber once.
///
/// Idempotent under [`Once`]. Seeds the query-body opt-in from the explicit
/// `LOG_QUERY_BODIES` environment variable *before* any emit could reach the
/// gateway, then installs the filter. `try_init()` is the simplest global
/// install path; if a subscriber is already set the returned `Err` is ignored.
pub fn init_tracing(verbose: bool) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // The opt-in is read once, at startup, so ordinary verbosity can never
        // flip it. Off unless `LOG_QUERY_BODIES` is truthy.
        LOG_QUERY_BODIES.store(
            std::env::var("LOG_QUERY_BODIES")
                .map(|value| parse_query_bodies(&value))
                .unwrap_or(false),
            Ordering::Relaxed,
        );
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

// ---------------------------------------------------------------------------
// Tests. The gate must be *enforced*, never merely parseable, so these route the
// composed `EnvFilter` through a real (in-memory) subscriber and assert the
// predicate on the exact targets that matter. No process-global environment is
// mutated, so the suite is safe under parallel execution.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Subscriber;
    use tracing::callsite::{DefaultCallsite, Identifier};
    use tracing::field::FieldSet;
    use tracing::metadata::{Kind, Metadata};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry::Registry;

    /// A `&'static Metadata` probe for one target at `INFO`, built with the
    /// canonical `DefaultCallsite` / `FieldSet::new(&[], Identifier(..))` idiom
    /// (`Metadata::new` is a const fn, so the cyclic statics are legal). The
    /// `Layered<EnvFilter, Registry>` routes the decision the way the real
    /// subscriber does — this is what actually runs `EnvFilter::enabled`.
    static CALLSITE_QUERY: DefaultCallsite = DefaultCallsite::new(QUERY_PROBE);
    static QUERY_PROBE: &Metadata = {
        static META: Metadata = Metadata::new(
            "probe",
            QUERY_TARGET,
            tracing::Level::INFO,
            None,
            None,
            None,
            FieldSet::new(&[], Identifier(&CALLSITE_QUERY)),
            Kind::EVENT,
        );
        &META
    };

    static CALLSITE_SYNC: DefaultCallsite = DefaultCallsite::new(SYNC_PROBE);
    static SYNC_PROBE: &Metadata = {
        static META: Metadata = Metadata::new(
            "probe",
            "rust_docs_mcp::sync",
            tracing::Level::INFO,
            None,
            None,
            None,
            FieldSet::new(&[], Identifier(&CALLSITE_SYNC)),
            Kind::EVENT,
        );
        &META
    };

    static CALLSITE_OTHER: DefaultCallsite = DefaultCallsite::new(OTHER_PROBE);
    static OTHER_PROBE: &Metadata = {
        static META: Metadata = Metadata::new(
            "probe",
            "rust_docs_mcp::other",
            tracing::Level::INFO,
            None,
            None,
            None,
            FieldSet::new(&[], Identifier(&CALLSITE_OTHER)),
            Kind::EVENT,
        );
        &META
    };

    /// Whether `meta` is enabled by `filter`, probed through a fresh
    /// `Layered<EnvFilter, Registry>` subscriber. `EnvFilter` is a `Layer`, so
    /// the layered `enabled` predicate *is* the gate decision.
    fn enabled_at(filter: &EnvFilter, meta: &Metadata) -> bool {
        let layered = Registry::default().with(filter.clone());
        layered.enabled(meta)
    }

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
    fn decision_for_is_pure_over_the_opt_in() {
        // The pure core of the gate over both opt-in branches, without touching
        // any process-global state.
        assert_eq!(decision_for(QUERY_TARGET, false), Decision::Suppress);
        assert_eq!(decision_for(QUERY_TARGET, true), Decision::Allow);
        assert_eq!(decision_for("rust_docs_mcp::sync", false), Decision::Allow);
        assert_eq!(decision_for("rust_docs_mcp::sync", true), Decision::Allow);
    }

    #[test]
    fn parse_query_bodies_truthy_only() {
        for value in ["1", "true", "TRUE", "yes", "YES", "on", " on "] {
            assert!(parse_query_bodies(value), "{value:?} must enable the gate");
        }
        for value in ["", "0", "false", "no", "off", "garbage", "2", "truely"] {
            assert!(
                !parse_query_bodies(value),
                "{value:?} must NOT enable the gate"
            );
        }
    }

    #[test]
    fn filter_composes_query_off_with_level() {
        // The gate directive the code builds is always well-formed and parseable
        // (no spaces around `=`) — the regression that P1 broke.
        assert!(EnvFilter::try_new("rust_docs_mcp::query=off").is_ok());
        assert!(EnvFilter::try_new("info,rust_docs_mcp::query=off").is_ok());
        // The space-joined form the original code produced was NOT parseable, so
        // it always fell through to the fallback and silently dropped the gate.
        assert!(EnvFilter::try_new("info, rust_docs_mcp::query = off").is_err());
        // Both verbosity variants still build without panicking and keep the
        // ordinary default alive.
        assert!(enabled_at(&filter_for(false), SYNC_PROBE));
        assert!(enabled_at(&filter_for(true), SYNC_PROBE));
    }

    #[test]
    fn filter_enforces_query_body_gate_off_by_default() {
        // The headline property: by default the query target is *disabled* while
        // an ordinary target at the same level is *enabled*. This is the
        // assertion that would have caught P1.
        let filter = filter_for(false);
        assert!(
            !enabled_at(&filter, QUERY_PROBE),
            "QUERY_TARGET must be disabled by default"
        );
        assert!(
            enabled_at(&filter, SYNC_PROBE),
            "an ordinary target must stay enabled"
        );
    }

    #[test]
    fn filter_respects_query_body_opt_in() {
        // The explicit, non-verbosity opt-in must re-enable the query target
        // without disturbing the ordinary default.
        let filter = filter_for_with(false, true);
        assert!(
            enabled_at(&filter, QUERY_PROBE),
            "the opt-in must re-enable QUERY_TARGET"
        );
        assert!(enabled_at(&filter, SYNC_PROBE));
    }

    #[test]
    fn rust_log_can_never_leak_query_bodies() {
        // Even when `RUST_LOG` tries to enable the whole subtree (or the query
        // target itself), the appended gate directive wins: query bodies stay off
        // unless the explicit `LOG_QUERY_BODIES` opt-in is set.
        let filter = filter_for_with_rust_log(
            false,
            false,
            Some("rust_docs_mcp=trace,rust_docs_mcp::query=info"),
        );
        assert!(
            !enabled_at(&filter, QUERY_PROBE),
            "a gate that overrides the base must hold even when RUST_LOG opens the query target"
        );
        // ...while the ordinary subtree the operator asked for stays open.
        assert!(enabled_at(&filter, OTHER_PROBE));
        // The same, with the opt-in, lets the query target through.
        let opted = filter_for_with_rust_log(
            false,
            true,
            Some("rust_docs_mcp=trace,rust_docs_mcp::query=info"),
        );
        assert!(enabled_at(&opted, QUERY_PROBE));
    }

    #[test]
    fn valid_rust_log_forms_the_level_base() {
        // A valid `RUST_LOG` is honoured as the level base; ordinary diagnostics
        // at or above it are enabled (here `debug` base → verbose-level events at
        // `info` still pass), with the gate intact.
        let filter = filter_for_with_rust_log(false, false, Some("debug"));
        assert!(enabled_at(&filter, SYNC_PROBE));
    }

    #[test]
    fn malformed_rust_log_degrades_without_panicking() {
        // The "malformed `RUST_LOG` degrades gracefully" invariant, exercised on
        // the real degradation path by passing the bad value directly (no
        // environment mutation).
        let filter = filter_for_with_rust_log(false, false, Some("!!! this is not a directive"));
        // Degrades to the default `info` base, gate still holding.
        assert!(enabled_at(&filter, SYNC_PROBE));
        assert!(!enabled_at(&filter, QUERY_PROBE));
    }
}
