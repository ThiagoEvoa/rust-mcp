//! Project-wide typed error layer.
//!
//! Task [004] M1: replace ad-hoc failure handling with a single typed error,
//! [`AppError`], that `plan.md` §8 ("typed errors") and §9 ("external input must
//! not cause an avoidable panic") call for. Every variant carries *why* and
//! *where* it happened so a startup failure surfaces as an actionable,
//! non-panicking stderr message plus a non-zero exit instead of an unwind.
//!
//! `DataDirError` from the CLI layer folds in as a variant: it keeps the
//! actionable "`--data-dir` remedy" message that task [003]'s contract (and its
//! tests) already established.

use std::io;

use thiserror::Error;

// Re-export the CLI data-directory error so callers that want the typed layer
// also get the one actionable failure task [003] defined, without reaching into
// the `cli` module.
pub use crate::cli::DataDirError;

/// Failure to start or run the CLI (e.g. an unavailable data directory, a bad
/// snapshot read, or an MCP runtime that cannot start).
///
/// Each variant *preserves the cause and its context*: the CLI boundary turns
/// any [`AppError`] into a stderr diagnostic and a non-zero exit. No variant
/// panics.
#[derive(Debug, Error)]
pub enum AppError {
    /// The application data directory could not be resolved.
    ///
    /// Kept as its own variant so the `[003]` remedy text (naming
    /// `--data-dir`) is preserved end to end.
    #[error("data directory: {0}")]
    DataDir(#[from] DataDirError),

    /// A startup/IO failure occurred somewhere in the pipeline (reading a
    /// bundled snapshot, writing an update, …), tagged with *where* it happened.
    #[error("{context}: {source}")]
    Io {
        /// Short, human description of the operation that failed.
        context: String,
        /// The underlying I/O cause, preserved for the error chain.
        #[source]
        source: io::Error,
    },

    /// The MCP stdio runtime could not start.
    ///
    /// The runtime itself is wired in a later milestone (M4); the variant exists
    /// now so that *any* future start failure surfaces as an actionable message
    /// rather than an unwrap/panic on external input.
    #[error("mcp runtime failed to start: {0}")]
    RuntimeStart(String),
}

/// The crate's fallible-operation result type.
pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    /// Build an [`AppError::Io`] that records *where* the failure happened.
    ///
    /// Use at the call site so the diagnostic names the operation (e.g.
    /// `"reading bundled snapshot"`), not just a bare `io::Error`.
    pub fn io<S: Into<String>>(context: S, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// A generic I/O failure with a default context so `?` on plain `io::Result`s
/// still yields a typed [`AppError`].
impl From<io::Error> for AppError {
    fn from(source: io::Error) -> Self {
        Self::io("io operation failed", source)
    }
}

// ---------------------------------------------------------------------------
// Unit tests. These pin the "useful context, no panic" acceptance criterion of
// task [004] at the type boundary, independent of the CLI.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::DataDirError;
    use std::error::Error;

    #[test]
    fn data_dir_failure_is_actionable_and_prefixed() {
        let app = AppError::from(DataDirError::NoDefault);
        let message = app.to_string();
        assert!(
            message.contains("--data-dir"),
            "the remedy must be named in the action message: {message}"
        );
        assert!(
            message.to_lowercase().contains("data directory"),
            "the failure must carry its where-clause: {message}"
        );
        // `no panics on external input` is demonstrated structurally: the
        // conversion is a plain, non-panicking `From` impl and `run` returns
        // `AppResult`, not an unwrap. (We can't `catch_unwind` an `AppError` that
        // holds an `io::Error`, which is not `UnwindSafe`, so the property is
        // pinned by the type shape rather than a runtime unwind.)
    }

    #[test]
    fn io_failure_records_context_and_preserves_the_source() {
        let cause = io::Error::new(io::ErrorKind::UnexpectedEof, "truncated");
        let app = AppError::io("reading bundled snapshot", cause);
        let message = app.to_string();
        assert!(
            message.contains("reading bundled snapshot"),
            "the where-context must be preserved: {message}"
        );
        assert!(
            message.contains("truncated"),
            "the underlying cause must be preserved: {message}"
        );
        // The source chain reaches back to the original `io::Error`.
        let source = app
            .source()
            .and_then(|s| s.downcast_ref::<io::Error>())
            .expect("the `Io` variant must carry its `io::Error` as `source()`");
        assert_eq!(source.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn io_question_mark_is_typed() {
        // The `?` operator on a fallible closure should yield a typed
        // `AppError`, not a panic.
        let io_err = io::Result::<()>::Err(io::Error::new(io::ErrorKind::NotFound, "gone"));
        let map = || -> AppResult<()> {
            io_err?;
            Ok(())
        };
        assert!(map().is_err());
    }

    #[test]
    fn runtime_start_failure_is_actionable() {
        let app = AppError::RuntimeStart("stdio transport unavailable".into());
        assert!(
            app.to_string().contains("stdio transport unavailable"),
            "the start cause must be reported: {app}"
        );
    }
}
