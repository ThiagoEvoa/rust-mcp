//! Command-line parsing and application-data configuration.
//!
//! Task [003] M1: parse `serve` / `sync` / `status` and an explicit
//! `--data-dir` override *without* implementing corpus ingestion. The
//! subcommand bodies are deliberately wired to clearly-labelled placeholders;
//! their real behaviour lands in later milestones (`serve` → M2/M4, `sync`/
//! `status` → M2/M4/M5). What this task delivers is the *contract*: a parseable,
//! documented CLI surface and a predictable data-directory resolution with
//! actionable errors.
//!
//! Two invariants from `plan.md` §8/§9 and AGENTS.md are honoured from the start
//! and enforced by the test suite:
//!
//! - During `serve`, **stdout carries only MCP protocol traffic**. Every stub in
//!   this task routes diagnostics to **stderr** and never to stdout.
//! - Invalid arguments produce *actionable* errors. `clap` writes a concise
//!   usage/argument message to stderr and exits non-zero; the data-directory
//!   edge case (no OS default available) is reported the same way.
//!
//! The module owns no network, filesystem, or protocol logic yet; those arrive
//! with `corpus` ([029]-[030]), `ingest` ([010]-[028]), and `mcp` ([048]-[059]).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::error::AppResult;
use clap::{Parser, Subcommand};

/// Name used to build the OS-appropriate application data directory.
///
/// Kept as a constant so the default location is inspectable and testable
/// without depending on a third-party platform-directory crate (task [003] stays
/// within its stated scope; only `clap` is added here — see `docs/COMPATIBILITY.md`
/// §3, where `clap` `4.6` is the dependency explicitly assigned to task [003]).
const APP_DIR: &str = "rust-docs-mcp";

/// Parsed command-line interface.
///
/// Mirrors `plan.md` §8:
///
/// ```text
/// rust-docs-mcp serve  [--verbose] [--data-dir PATH]
/// rust-docs-mcp sync   [--verbose] [--data-dir PATH]
/// rust-docs-mcp status [--verbose] [--data-dir PATH]
/// ```
///
/// `--data-dir` is a *global* option so it is documented once and accepted both
/// before and after the subcommand (`rust-docs-mcp --data-dir X serve` and
/// `rust-docs-mcp serve --data-dir X` are equivalent). Omitting it selects the
/// OS-appropriate application data directory; see [`apply_override`]/
/// [`resolve_data_dir`].
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(
    name = "rust-docs-mcp",
    version,
    about = "Local stdio MCP server exposing official Rust documentation to coding agents",
    long_about = None,
    after_help =
        "During `serve`, stdout carries only MCP protocol traffic; all\n\
         diagnostics go to stderr. Data under a snapshot is read-only from the\n\
         server's perspective; refresh it with `sync`."
)]
pub struct Cli {
    /// Use `PATH` as the application data directory instead of the OS default.
    ///
    /// Accepted globally, before or after the subcommand.
    #[arg(long = "data-dir", global = true, value_name = "PATH")]
    pub data_dir: Option<PathBuf>,

    /// Increase diagnostics verbosity (`--verbose` → `debug` level, or set
    /// `RUST_LOG`). Diagnostics always go to **stderr**; this flag never
    /// writes query text to stdout.
    #[arg(short = 'v', long = "verbose", global = true, default_value_t = false)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// The three top-level subcommands.
///
/// Each is a *placeholder dispatch target* for task [003]; the bodies do not run
/// the corpus or the MCP runtime yet. See [`Cli::run`].
#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Launch the stdio MCP server against the selected immutable snapshot.
    ///
    /// Task [005] wires this to the real M1 skeleton runtime ([`crate::mcp::serve`]);
    /// the catalog/section resources and the five tools land in M4 ([048]-[059]).
    Serve,

    /// Refresh the documentation corpus from the official sources.
    ///
    /// Bounded, explicit network ingestion that publishes only a validated
    /// snapshot. (Lands in M2; task [003] parses and dispatches only.)
    Sync,

    /// Report available snapshots, the selected corpus, and freshness.
    ///
    /// Runs without network access and reads the one snapshot the process would
    /// serve. Staleness is judged against a configurable threshold; the default
    /// is seven days (`plan.md` §5).
    Status {
        /// Override the staleness threshold in days. Omit to use the seven-day
        /// default.
        #[arg(long = "stale-days", value_name = "DAYS")]
        stale_days: Option<u64>,
    },

    /// Inspect corpus coverage and extraction fidelity — the M2 operator view.
    /// Records source/document/section counts per registered collection, any
    /// coverage gap, and which content types the extractors retain (nested
    /// headings, anchors, lists, tables, command syntax, stability callouts,
    /// relative links, lint "known issues", fenced examples) while navigation
    /// chrome and scripts are stripped. No panic; it exits non-zero only on a
    /// snapshot-load failure, otherwise `0`.
    Inspect,
}

impl Cli {
    /// Parse arguments from the process environment.
    ///
    /// On a parse error `clap` prints a concise, actionable usage message to
    /// stderr and aborts with a conventional non-zero exit; this is the
    /// "invalid arguments produce actionable errors" path.
    pub fn from_env() -> Self {
        Self::parse()
    }

    /// Resolve the effective data directory: an explicit `--data-dir` wins;
    /// otherwise the OS-appropriate application data directory is used.
    ///
    /// Returns [`DataDirError::NoDefault`] only when no `--data-dir` was given
    /// *and* the platform exposes no default location (no home / app-data),
    /// turning that edge into an actionable error rather than a silent default.
    pub fn resolve_data_dir(&self) -> Result<PathBuf, DataDirError> {
        resolve_data_dir(self.data_dir.as_deref())
    }

    /// Dispatch to the selected subcommand.
    ///
    /// Resolves the data directory first (so an unavailable default is reported
    /// through the typed error layer before any subcommand work, for *every*
    /// subcommand including `serve`), then dispatches. `serve` runs the real
    /// M1 stdio runtime and `sync` runs the bounded ingestion pipeline; `status`
    /// loads the selected snapshot and reports its freshness offline. A startup
    /// failure yields an [`AppError`] so the caller ([`main`]) can turn it into
    /// a non-zero exit *without ever panicking* on external input.
    pub fn run(self) -> AppResult<ExitCode> {
        // `DataDirError` folds into `AppError` via `From`, so `?` gives a typed
        // context-carrying failure instead of a bare unwrap. M1 does not read the
        // corpus, but resolving first preserves [003]'s "an unavailable default is
        // a typed error before a command runs" contract and keeps the CLI's
        // data-dir handling uniform across subcommands.
        let data_dir = self.resolve_data_dir()?;

        match &self.command {
            // `serve` is wired to the real (M1 skeleton) stdio runtime. Any
            // startup/transport failure is surfaced as a typed `AppError` *here*
            // so `main` turns it into a non-zero exit and an stderr diagnostic —
            // never query text on stdout.
            Command::Serve => crate::mcp::serve(),

            // `sync` (task [027]): wire the explicit refresh command to the
            // bounded ingestion pipeline — discovery → fetch → extract →
            // validate → atomic publish — against the production registry gate.
            // A per-source report and any failure are *diagnostics* that go to
            // **stderr**, so `sync` (like `serve`) never writes query text to
            // stdout (plan.md §8/§9 / task [004]).
            Command::Sync => {
                let config = crate::ingest::pipeline::SyncConfig::production();
                // Build the bounded fetch client synchronously (registry gate,
                // default policy). A constructor failure is a typed, actionable
                // diagnostic — no runtime is needed to build the client, so it
                // is reported here rather than inside the async pipeline.
                let client = match crate::ingest::pipeline::production_client() {
                    Ok(client) => client,
                    Err(err) => {
                        tracing::error!(
                            "rust-docs-mcp: sync failed: the fetch client could not start: {err}"
                        );
                        return Ok(ExitCode::FAILURE);
                    }
                };
                let data_dir = data_dir.clone();
                let future =
                    async move { crate::ingest::pipeline::sync(&config, &data_dir, &client).await };

                // Drive the async pipeline on a dedicated multi-threaded runtime
                // with network IO enabled, mirroring the `serve` runtime
                // construction so the client's HTTP transport is supported.
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| {
                        crate::error::AppError::runtime_start(
                            "the sync runtime failed to start",
                            Some(Box::new(err)),
                        )
                    })?;

                match runtime.block_on(future) {
                    // A successful run reports the per-source inventory changes
                    // and the new snapshot identity on stderr, then exits 0.
                    Ok(report) => {
                        // The report is the success output: emit it on stderr
                        // through the gated subscriber (`info` shows by default),
                        // keeping stdout exclusive to MCP messages (plan §8/§9).
                        tracing::info!("rust-docs-mcp: {}", report.to_stderr());
                        Ok(ExitCode::SUCCESS)
                    }
                    // AC2: a fetch/parser/validation failure aborts the run and
                    // leaves the last valid corpus intact. Report it on stderr
                    // and exit non-zero so a partial corpus is never presented as
                    // a successful refresh.
                    Err(sync_err) => {
                        tracing::error!("rust-docs-mcp: sync failed: {sync_err}");
                        Ok(ExitCode::FAILURE)
                    }
                }
            }

            // `status` (task [030]): report the one snapshot the process would
            // serve — its identity, source/section counts, and each source's
            // freshness — judged against a configurable threshold. This is a
            // pure, **offline** read: it loads the already-selected snapshot
            // (local preferred, bundled fallback) and makes **no** network
            // request. The human report is a *diagnostic* emitted on **stderr**
            // through the gated subscriber (like `sync`/`serve`), so stdout
            // stays exclusive to MCP messages (plan.md §8/§9 / task [004]).
            Command::Status { stale_days } => {
                // Select the fixed runtime snapshot: the local published snapshot
                // (from a prior `sync`) when it exists, else the validated
                // snapshot **embedded in the binary** (`crate::bundled::JSON`) —
                // the first-sync fallback that makes a fresh install work
                // offline (`plan.md` §5, `docs/LICENSING.md` §5). This is a pure,
                // **offline** read: `load_snapshot_embedded` makes **no** network
                // request. When *neither* source yields a valid corpus, it
                // returns an actionable `LoadError` that directs the operator to
                // `sync` — surfaced like the `sync` failure path (a typed,
                // stderr diagnostic and a non-zero exit, never a panic).
                let snapshot = match crate::runtime::load_snapshot_embedded(
                    &data_dir,
                    crate::bundled::JSON.as_bytes(),
                ) {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        tracing::error!("rust-docs-mcp: status failed: {err}");
                        return Ok(ExitCode::FAILURE);
                    }
                };

                // "Now" is read from the wall clock at this process boundary and
                // passed to the pure report as an injected instant, so the
                // freshness logic itself never touches the clock (tests inject a
                // fixed `as_of`).
                let as_of =
                    crate::provenance::Instant::from_system_time(std::time::SystemTime::now());

                // The threshold is configurable (AC1): an explicit `--stale-days`
                // wins, otherwise the plan's seven-day default. A `u64` flag
                // keeps the window non-negative; the cast saturates the
                // practically-unreachable `> i64::MAX` case rather than wrapping
                // or panicking.
                let threshold = match *stale_days {
                    Some(days) => crate::provenance::StaleThreshold::days(
                        i64::try_from(days).unwrap_or(i64::MAX),
                    ),
                    None => crate::provenance::StaleThreshold::default(),
                };

                // Build the report purely from the in-memory snapshot, then emit
                // it on stderr through the gated subscriber, keeping stdout
                // exclusive to MCP messages.
                let report = crate::status::build_report(&snapshot.manifest, as_of, threshold);
                tracing::info!("rust-docs-mcp: {}", report.to_stderr());
                Ok(ExitCode::SUCCESS)
            }
            Command::Inspect => {
                let snapshot = match crate::runtime::load_snapshot_embedded(
                    &data_dir,
                    crate::bundled::JSON.as_bytes(),
                ) {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        tracing::error!("rust-docs-mcp: inspect failed: {err}");
                        return Ok(ExitCode::FAILURE);
                    }
                };

                // Coverage of the corpus the operator currently has, and
                // extraction fidelity over the representative fixtures: an
                // offline proof that the content types agents rely on survive
                // extraction while navigation chrome and scripts are stripped
                // (task [032] / plan.md section 11). Both are diagnostics on
                // stderr, keeping stdout exclusive to MCP messages.
                let coverage = crate::coverage::record_coverage(&snapshot.manifest);
                let fidelity = crate::coverage::inspect_fidelity(
                    &crate::coverage::representative_fidelity_sections(),
                );
                // A one-line operator verdict: the corpus the operator has is
                // coverage-complete, and every content type the extractors must
                // retain survives in the representative fixtures.
                tracing::info!(
                    "rust-docs-mcp: coverage_complete={}",
                    coverage.is_complete()
                );
                tracing::info!(
                    "rust-docs-mcp: fidelity_complete={}",
                    fidelity.all_types_present()
                );
                tracing::info!("rust-docs-mcp: {}", coverage.render());
                tracing::info!("rust-docs-mcp: {}", fidelity.render());
                Ok(ExitCode::SUCCESS)
            }
        }
    }
}

/// Failure to determine the application data directory.
///
/// Defined here as the one actionable failure task [003] must report; task
/// [004] folds it into the project's typed [`AppError`](crate::error::AppError)
/// via `From` so the CLI boundary can surface it without panicking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataDirError {
    /// No `--data-dir` was supplied and the platform exposes no default location.
    NoDefault,
}

impl std::fmt::Display for DataDirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataDirError::NoDefault => write!(
                f,
                "no `--data-dir` was supplied and this platform exposes no default \
                 application data directory (set `--data-dir PATH` explicitly)"
            ),
        }
    }
}

impl std::error::Error for DataDirError {}

/// Pick the effective data directory.
///
/// Pure and fully testable: the explicit override wins when present, otherwise
/// `fallback` is used, and only an absent fallback yields [`DataDirError`].
pub fn apply_override(
    explicit: Option<&Path>,
    fallback: Option<PathBuf>,
) -> Result<PathBuf, DataDirError> {
    match explicit {
        Some(path) => Ok(path.to_path_buf()),
        None => fallback.ok_or(DataDirError::NoDefault),
    }
}

/// Compute the OS-appropriate application data directory, or `None` when the
/// platform exposes no default location.
///
/// - Windows: `%APPDATA%\rust-docs-mcp`.
/// - Unix: `$XDG_DATA_HOME/rust-docs-mcp`, else `$HOME/.local/share/rust-docs-mcp`.
///
/// Returns `None` only when the relevant environment variables are all absent,
/// which is the case [`resolve_data_dir`] turns into an actionable error.
pub fn default_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(base).join(APP_DIR))
    }

    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })?;
        Some(base.join(APP_DIR))
    }
}

/// Resolve the effective data directory: an explicit `--data-dir` wins, otherwise
/// the OS default is used. See [`apply_override`] and [`default_data_dir`].
pub fn resolve_data_dir(explicit: Option<&Path>) -> Result<PathBuf, DataDirError> {
    apply_override(explicit, default_data_dir())
}

// ---------------------------------------------------------------------------
// Unit tests. Offline, deterministic, and environment-independent: the
// override-vs-default *logic* is tested through the pure `apply_override`,
// while `resolve_data_dir` proves the explicit-path contract without mutating
// process-global environment state.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    fn parse(argv: &[&str]) -> Cli {
        Cli::try_parse_from(argv).expect("well-formed argv must parse")
    }

    #[test]
    fn each_subcommand_parses() {
        assert_eq!(parse(&["rust-docs-mcp", "serve"]).command, Command::Serve);
        assert_eq!(parse(&["rust-docs-mcp", "sync"]).command, Command::Sync);
        // `inspect` parses as a read-only diagnostic subcommand.
        assert_eq!(
            parse(&["rust-docs-mcp", "inspect"]).command,
            Command::Inspect
        );
        // `status` parses with no threshold flag (the default is applied later).
        assert_eq!(
            parse(&["rust-docs-mcp", "status"]).command,
            Command::Status { stale_days: None }
        );
    }

    // AC1 (CLI surface): the `--stale-days` threshold flag parses, and is
    // `None` when omitted so the seven-day default is applied at dispatch.
    #[test]
    fn status_stale_days_flag_parses_and_defaults_when_omitted() {
        // Omitted -> the caller applies the seven-day default.
        assert_eq!(
            parse(&["rust-docs-mcp", "status"]).command,
            Command::Status { stale_days: None }
        );
        // Explicit -> the window is configurable from the CLI.
        assert_eq!(
            parse(&["rust-docs-mcp", "status", "--stale-days", "14"]).command,
            Command::Status {
                stale_days: Some(14)
            }
        );
        // Zero is a valid (non-negative) window.
        assert_eq!(
            parse(&["rust-docs-mcp", "status", "--stale-days", "0"]).command,
            Command::Status {
                stale_days: Some(0)
            }
        );
    }

    #[test]
    fn status_stale_days_requires_a_value() {
        // A flag without its value is an actionable parse error.
        assert!(Cli::try_parse_from(["rust-docs-mcp", "status", "--stale-days"]).is_err());
        // A negative value is rejected: the window is a non-negative integer.
        assert!(Cli::try_parse_from(["rust-docs-mcp", "status", "--stale-days", "-3"]).is_err());
    }

    #[test]
    fn inspect_takes_no_flags() {
        // `inspect` is a read-only diagnostic: it accepts no per-subcommand flags,
        // so a stray flag is an actionable parse error.
        assert_eq!(
            parse(&["rust-docs-mcp", "inspect"]).command,
            Command::Inspect
        );
        assert!(
            Cli::try_parse_from(["rust-docs-mcp", "inspect", "--stale-days", "3"]).is_err(),
            "inspect has no `--stale-days` flag",
        );
    }

    #[test]
    fn data_dir_global_before_or_after_subcommand() {
        // Global: before the subcommand.
        let before = parse(&["rust-docs-mcp", "--data-dir", "/tmp/a", "serve"]);
        assert_eq!(
            before.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/a"))
        );
        assert_eq!(before.command, Command::Serve);

        // Global: after the subcommand.
        let after = parse(&["rust-docs-mcp", "serve", "--data-dir", "/tmp/a"]);
        assert_eq!(
            after.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/a"))
        );
        assert_eq!(after.command, Command::Serve);

        // Accepted on every subcommand.
        for sub in ["sync", "status"] {
            let parsed = parse(&["rust-docs-mcp", sub, "--data-dir", "/tmp/b"]);
            assert_eq!(
                parsed.data_dir.as_deref(),
                Some(std::path::Path::new("/tmp/b"))
            );
        }
    }

    #[test]
    fn data_dir_omitted_is_none() {
        assert!(parse(&["rust-docs-mcp", "status"]).data_dir.is_none());
    }

    #[test]
    fn override_wins_over_default() {
        // Explicit path is returned exactly, regardless of any default.
        let p = Path::new("/tmp/explicit");
        assert_eq!(
            apply_override(Some(p), Some(PathBuf::from("/ignored"))),
            Ok(p.to_path_buf())
        );
        // No fallback still honours the explicit path.
        assert_eq!(apply_override(Some(p), None), Ok(p.to_path_buf()));
        // No override but a fallback uses the fallback.
        let fb = PathBuf::from("/tmp/default");
        assert_eq!(apply_override(None, Some(fb.clone())), Ok(fb));
        // Neither available is an actionable error, not a panic.
        assert_eq!(apply_override(None, None), Err(DataDirError::NoDefault));
    }

    #[test]
    fn resolve_explicit_path_is_deterministic() {
        // "explicit paths resolve predictably": the override path is returned
        // verbatim and is independent of any platform default.
        let p = Path::new("/tmp/custom-dir");
        assert_eq!(resolve_data_dir(Some(p)).unwrap(), p.to_path_buf());
    }

    #[test]
    fn default_data_dir_shape_when_available() {
        // On a dev/CI machine a default normally exists; verify its *shape*
        // (it ends at the app directory) rather than an absolute value, so the
        // test is environment-independent. When no home/app-data exists, the
        // contract is a clean `None`, likewise acceptable.
        if let Some(dir) = default_data_dir() {
            assert!(
                dir.ends_with(APP_DIR),
                "default data dir {dir:?} should be nested under the app directory",
            );
        }
    }

    #[test]
    fn help_documents_subcommands_and_data_dir() {
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("serve"), "help must mention `serve`");
        assert!(help.contains("sync"), "help must mention `sync`");
        assert!(help.contains("status"), "help must mention `status`");
        assert!(help.contains("inspect"), "help must mention `inspect`");
        assert!(
            help.contains("--data-dir"),
            "help must document `--data-dir`"
        );
    }

    #[test]
    fn version_is_documented() {
        let version = Cli::command().render_version().to_string();
        assert!(version.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn invalid_arguments_produce_actionable_errors() {
        // Unknown subcommand.
        assert!(Cli::try_parse_from(["rust-docs-mcp", "frobnicate"]).is_err());
        // No subcommand at all.
        assert!(Cli::try_parse_from(["rust-docs-mcp"]).is_err());
        // Missing the required value of `--data-dir`.
        assert!(Cli::try_parse_from(["rust-docs-mcp", "sync", "--data-dir"]).is_err());
        // Unknown top-level flag.
        assert!(Cli::try_parse_from(["rust-docs-mcp", "--bogus"]).is_err());
    }

    #[test]
    fn help_and_version_exit_cleanly() {
        // `--help` / `--version` are not errors; they exit the *builder* with a
        // DisplayHelp/DisplayVersion error code, which we treat as success.
        for argv in [
            vec!["rust-docs-mcp", "--help"],
            vec!["rust-docs-mcp", "-h"],
            vec!["rust-docs-mcp", "--version"],
            vec!["rust-docs-mcp", "-V"],
        ] {
            let err = Cli::command()
                .try_get_matches_from(argv.iter())
                .expect_err("--help/--version are not successful parses");
            use clap::error::ErrorKind;
            assert!(
                matches!(
                    err.kind(),
                    ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
                ),
                "unexpected kind for {argv:?}: {err:?}"
            );
        }
    }

    #[test]
    fn no_data_dir_available_reports_actionable_error() {
        assert_eq!(apply_override(None, None), Err(DataDirError::NoDefault),);
        // The message points the operator at the fix.
        let msg = DataDirError::NoDefault.to_string();
        assert!(msg.contains("--data-dir"), "error should name the remedy");
    }
}
