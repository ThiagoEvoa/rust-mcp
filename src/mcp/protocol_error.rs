//! Protocol / tool error mapping — task `[048][M4]`, AC2.
//!
//! Maps this crate's internal retrieval errors to `rmcp` protocol / tool errors
//! ([`rmcp::model::ErrorData`]) with **sanitized, actionable** messages that
//! never leak internal filesystem paths or Rust backtraces, and with the
//! JSON-RPC error code chosen **per the negotiated protocol version**:
//!
//! * **Invalid caller input** (blank / oversized query, an unknown source, a
//!   malformed command key, a malformed or mismatched continuation cursor) maps
//!   to `INVALID_PARAMS` (`-32602`).
//! * **A requested resource that is absent from the current snapshot** (an
//!   unknown section, an unknown API-guideline category, an unsupported command
//!   key, an unknown resource URI) maps to `RESOURCE_NOT_FOUND` (`-32002`).
//! * **Operational / server-side failures** (an unloadable corpus, a malformed
//!   or stale on-disk snapshot, an I/O failure, a corpus-integrity
//!   inconsistency such as a dangling checklist citation) map to
//!   `INTERNAL_ERROR` (`-32603`).
//!
//! The version-aware rule (MCP `2026-07-28` error-code change, see
//! [`crate::model`]'s `ErrorData` docs): a `RESOURCE_NOT_FOUND` reported to a
//! peer negotiating `2026-07-28` or newer is upgraded to `INVALID_PARAMS`.
//! [`version_aware`] applies that upgrade; [`ProtocolError::protocol_error`]
//! applies it when a version is supplied, so the same internal error maps to the
//! correct code for both pre- and post-`2026-07-28` peers.
//!
//! Every message here is a fixed, human sentence built from **domain
//! identifiers** (section ids, source ids, command keys, categories) — never
//! from a `Debug`/`Display` of an error chain that could carry a path or a
//! backtrace. The `data` payload carries only **discoverable alternatives**
//! (e.g. the supported command keys) so the client can self-correct.
//
// Transitional: the mapping below is consumed by the [049]+ handler dispatch,
// which turns retrieval errors into `ErrorData` on the wire. Nothing reaches
// it yet, so `dead_code` is silenced for this module only, mirroring the
// precedent in `runtime` / `ingest/*`.
#![allow(dead_code)]

use rmcp::model::{ErrorCode, ErrorData, ProtocolVersion};

use crate::commands::{CommandGuidanceError, LookupError};
use crate::error::AppError;
use crate::model::SchemaError;
use crate::pagination::{CursorError, MAX_RESPONSE_BYTES};
use crate::reads::ReadError;
use crate::runtime::LoadError;
use crate::search::SearchError;

/// The protocol/tool error class an internal failure maps to.
///
/// This is the *logical* class; the concrete JSON-RPC code is chosen by
/// [`ErrorClass::code_for`] given the negotiated protocol version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    /// Bad caller input → `INVALID_PARAMS`.
    InvalidParams,
    /// A requested resource is absent → `RESOURCE_NOT_FOUND`, upgraded to
    /// `INVALID_PARAMS` for peers on `2026-07-28` or newer.
    ResourceNotFound,
    /// An operational / server-side failure → `INTERNAL_ERROR`.
    Internal,
}

impl ErrorClass {
    /// The base JSON-RPC code for this class (before any version upgrade).
    pub fn base_code(self) -> ErrorCode {
        match self {
            Self::InvalidParams => ErrorCode::INVALID_PARAMS,
            Self::ResourceNotFound => ErrorCode::RESOURCE_NOT_FOUND,
            Self::Internal => ErrorCode::INTERNAL_ERROR,
        }
    }

    /// The JSON-RPC code for this class given the negotiated protocol version.
    ///
    /// Applies the `2026-07-28` upgrade: a `ResourceNotFound` becomes
    /// `INVALID_PARAMS` when the peer is on `2026-07-28` or newer; other
    /// classes are unaffected.
    pub fn code_for(self, version: Option<&ProtocolVersion>) -> ErrorCode {
        if self == Self::ResourceNotFound
            && version.is_some_and(|v| *v >= ProtocolVersion::V_2026_07_28)
        {
            ErrorCode::INVALID_PARAMS
        } else {
            self.base_code()
        }
    }
}

/// Apply the version-aware upgrade to a [`ErrorData`] that already carries a
/// base code: a `RESOURCE_NOT_FOUND` is upgraded to `INVALID_PARAMS` for a peer
/// on `2026-07-28` or newer. Any other code passes through unchanged.
///
/// This is the same rule the `rmcp` server router applies automatically;
/// [`ProtocolError::protocol_error`] composes it so the mapping is explicit and
/// unit-testable at the application layer.
pub fn version_aware(mut err: ErrorData, version: Option<&ProtocolVersion>) -> ErrorData {
    if err.code == ErrorCode::RESOURCE_NOT_FOUND
        && version.is_some_and(|v| *v >= ProtocolVersion::V_2026_07_28)
    {
        err.code = ErrorCode::INVALID_PARAMS;
    }
    err
}

/// Whether `version` is on or after `2026-07-28` (the error-code change).
pub fn is_post_2026_07_28(version: &ProtocolVersion) -> bool {
    *version >= ProtocolVersion::V_2026_07_28
}

/// Map an internal retrieval error to a version-aware [`ErrorData`].
///
/// Implementations choose the [`ErrorClass`] per variant and build a sanitized
/// message; the version-aware code upgrade is applied when `version` is `Some`.
pub trait ProtocolError {
    /// The logical error class of this failure.
    fn error_class(&self) -> ErrorClass;

    /// A sanitized, human-readable, actionable message. Never contains an
    /// internal filesystem path or a Rust backtrace.
    fn message(&self) -> String;

    /// Optional, machine-readable, discoverable alternatives (e.g. the
    /// supported command keys). May be `None`.
    fn data(&self) -> Option<serde_json::Value> {
        None
    }

    /// The base [`ErrorData`] (no version upgrade applied).
    fn base_error(&self) -> ErrorData {
        // `ErrorData::new` takes the base code, a sanitized message, and the
        // discoverable-alternatives payload (an `Option<Value>`, not a boxed
        // one on this SDK). The version-aware upgrade is applied separately by
        // `version_aware` so the mapping stays explicit and unit-testable.
        ErrorData::new(self.error_class().base_code(), self.message(), self.data())
    }

    /// The version-aware [`ErrorData`]: the base error with the
    /// `2026-07-28` `ResourceNotFound` → `INVALID_PARAMS` upgrade applied when
    /// `version` is `Some`.
    fn protocol_error(&self, version: Option<&ProtocolVersion>) -> ErrorData {
        version_aware(self.base_error(), version)
    }
}

impl ProtocolError for SearchError {
    fn error_class(&self) -> ErrorClass {
        // Every search error is a caller-input problem.
        ErrorClass::InvalidParams
    }

    fn message(&self) -> String {
        match self {
            SearchError::BlankQuery => {
                "query must not be blank; provide a non-empty search term".to_string()
            }
            SearchError::QueryTooLarge { bytes, limit } => format!(
                "query is too large: {bytes} bytes exceeds the limit of {limit} bytes; \
                 shorten the query"
            ),
            SearchError::UnsupportedSource(source) => format!(
                "unknown source '{source}'; use one of the sources returned by `list_sources`"
            ),
        }
    }

    fn data(&self) -> Option<serde_json::Value> {
        match self {
            SearchError::QueryTooLarge { bytes, limit } => Some(serde_json::json!({
                "bytes": bytes,
                "limit": limit,
            })),
            SearchError::UnsupportedSource(source) => Some(serde_json::json!({
                "source": source,
            })),
            SearchError::BlankQuery => None,
        }
    }
}

impl ProtocolError for ReadError {
    fn error_class(&self) -> ErrorClass {
        match self {
            // A missing section or category is a resource the caller asked for
            // that the snapshot does not contain.
            ReadError::UnknownSection(_) | ReadError::UnknownCategory(_) => {
                ErrorClass::ResourceNotFound
            }
            // A dangling checklist citation is a *corpus-integrity* problem the
            // caller did not cause — an operational error.
            ReadError::DanglingExplanation { .. } => ErrorClass::Internal,
        }
    }

    fn message(&self) -> String {
        match self {
            ReadError::UnknownSection(id) => format!(
                "section '{id}' is not in this snapshot; list the available sections with \
                 `list_sources` or `search_docs`"
            ),
            ReadError::UnknownCategory(category) => format!(
                "API-guideline category '{category}' is not in this snapshot; ask `get_api_checklist` \
                 without a category to list the available ones"
            ),
            ReadError::DanglingExplanation { code, missing } => format!(
                "the API checklist entry '{code}' cites section '{missing}', which is not in the \
                 current index; this snapshot's citation is inconsistent and the guidance was not \
                 fabricated — refresh the snapshot to resolve it"
            ),
        }
    }

    fn data(&self) -> Option<serde_json::Value> {
        Some(match self {
            ReadError::UnknownSection(id) => serde_json::json!({ "section_id": id.to_string() }),
            ReadError::UnknownCategory(category) => serde_json::json!({ "category": category }),
            ReadError::DanglingExplanation { code, missing } => serde_json::json!({
                "code": code,
                "missing": missing,
            }),
        })
    }
}

impl ProtocolError for LookupError {
    fn error_class(&self) -> ErrorClass {
        match self {
            // A malformed key is a caller-input problem.
            LookupError::InvalidKey { .. } => ErrorClass::InvalidParams,
            // An unsupported key is a resource the caller asked for that the
            // catalog does not contain.
            LookupError::UnknownKey { .. } => ErrorClass::ResourceNotFound,
            // A dangling citation is a corpus-integrity problem.
            LookupError::UnresolvedSection { .. } => ErrorClass::Internal,
        }
    }

    fn message(&self) -> String {
        match self {
            LookupError::InvalidKey { requested, reason } => format!(
                "command key '{requested}' is not a valid lookup key: {reason}; use a plain \
                 subcommand key such as `build` or `clippy`"
            ),
            LookupError::UnknownKey {
                requested,
                supported,
            } => format!(
                "no command guidance for '{requested}'; available commands: [{}]",
                supported
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            LookupError::UnresolvedSection { key, section } => format!(
                "command '{key}' cites section '{section}', which is not in the current index; the \
                 guidance was not fabricated — refresh the snapshot to resolve it"
            ),
        }
    }

    fn data(&self) -> Option<serde_json::Value> {
        Some(match self {
            LookupError::InvalidKey { requested, reason } => serde_json::json!({
                "requested": requested,
                "reason": reason,
            }),
            LookupError::UnknownKey {
                requested,
                supported,
            } => serde_json::json!({
                "requested": requested,
                "supported": supported.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
            }),
            LookupError::UnresolvedSection { key, section } => serde_json::json!({
                "key": key,
                "section": section.to_string(),
            }),
        })
    }
}

impl ProtocolError for CursorError {
    fn error_class(&self) -> ErrorClass {
        // A malformed or mismatched cursor is a caller-input problem.
        ErrorClass::InvalidParams
    }

    fn message(&self) -> String {
        match self {
            CursorError::Malformed(reason) => {
                format!("continuation cursor is not usable: {reason}; start a fresh request")
            }
            CursorError::Mismatch {
                field,
                expected,
                found,
            } => format!(
                "continuation cursor does not match the current {field} \
                 (expected '{expected}', found '{found}'); start a fresh request"
            ),
        }
    }

    fn data(&self) -> Option<serde_json::Value> {
        Some(match self {
            CursorError::Malformed(reason) => serde_json::json!({ "malformed": reason }),
            CursorError::Mismatch {
                field,
                expected,
                found,
            } => serde_json::json!({
                "field": field.to_string(),
                "expected": expected,
                "found": found,
            }),
        })
    }
}

impl ProtocolError for CommandGuidanceError {
    fn error_class(&self) -> ErrorClass {
        match self {
            CommandGuidanceError::Lookup(inner) => inner.error_class(),
            CommandGuidanceError::Cursor(inner) => inner.error_class(),
            CommandGuidanceError::Read(inner) => inner.error_class(),
        }
    }

    fn message(&self) -> String {
        match self {
            CommandGuidanceError::Lookup(inner) => inner.message(),
            CommandGuidanceError::Cursor(inner) => inner.message(),
            CommandGuidanceError::Read(inner) => inner.message(),
        }
    }

    fn data(&self) -> Option<serde_json::Value> {
        match self {
            CommandGuidanceError::Lookup(inner) => inner.data(),
            CommandGuidanceError::Cursor(inner) => inner.data(),
            CommandGuidanceError::Read(inner) => inner.data(),
        }
    }
}

impl ProtocolError for SchemaError {
    fn error_class(&self) -> ErrorClass {
        // A malformed or stale on-disk snapshot is a server-side / operational
        // condition, never caller input.
        ErrorClass::Internal
    }

    fn message(&self) -> String {
        match self {
            // Only fixed, safe, non-path values are echoed: a `u32` version and
            // the list of supported versions are not filesystem paths.
            SchemaError::UnsupportedVersion { found, supported } => format!(
                "the on-disk snapshot uses schema version {found}, which this build does not support \
                 (supported: [{}]); refresh the snapshot with `sync`",
                supported
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            // The raw stored string (`found`/`id`/`uri`/`operator`/`reason`)
            // is arbitrary on-disk content that could itself be a path or
            // contain sensitive material, so it is NOT echoed. The message is
            // fixed and points the caller at the remedy (`sync`).
            SchemaError::UnknownVersion(_) => {
                "the on-disk snapshot carries an unparseable schema version; refresh the snapshot \
                 with `sync`"
                    .to_string()
            }
            SchemaError::InvalidId(_) => {
                "the on-disk snapshot contains a malformed identifier; it is corrupt, refresh the \
                 snapshot with `sync`"
                    .to_string()
            }
            SchemaError::InvalidField { .. } => {
                "the on-disk snapshot has an invalid field value; refresh the snapshot with `sync`"
                    .to_string()
            }
            SchemaError::InvalidUri(_) => {
                "the on-disk snapshot references a malformed URL; refresh the snapshot with `sync`"
                    .to_string()
            }
            SchemaError::ShellOperator(_) => {
                "the on-disk command catalog is corrupt; refresh it with `sync`".to_string()
            }
            SchemaError::Json(_) => {
                "the stored snapshot failed to (de)serialize; refresh the snapshot with `sync`"
                    .to_string()
            }
        }
    }

    fn data(&self) -> Option<serde_json::Value> {
        // Only the safe, fixed version fields are surfaced (a `u32` and a list
        // of supported `u32`s). No arbitrary on-disk string is echoed, so no
        // path or sensitive content can leak through this error.
        match self {
            SchemaError::UnsupportedVersion { found, supported } => Some(serde_json::json!({
                "found": found,
                "supported": supported,
            })),
            _ => None,
        }
    }
}

impl ProtocolError for LoadError {
    fn error_class(&self) -> ErrorClass {
        // An unloadable corpus is an operational / server-side failure.
        ErrorClass::Internal
    }

    fn message(&self) -> String {
        match self {
            // Operational messages are fixed and path-free: the caller needs to
            // know the corpus is unavailable, not the local filesystem layout.
            LoadError::NoValidCorpus { .. } => {
                "no usable documentation corpus is available: the local snapshot and any \
                 bundled snapshot both failed to load; run `sync` to (re)fetch it"
                    .to_string()
            }
            LoadError::Source { context, .. } => {
                format!("an internal error occurred while {context}; please retry")
            }
        }
    }

    fn data(&self) -> Option<serde_json::Value> {
        match self {
            LoadError::NoValidCorpus { .. } => None,
            LoadError::Source { context, .. } => Some(serde_json::json!({ "context": context })),
        }
    }
}

impl ProtocolError for AppError {
    fn error_class(&self) -> ErrorClass {
        ErrorClass::Internal
    }

    fn message(&self) -> String {
        match self {
            // A human message the caller can act on; the underlying `io::Error`
            // (which may carry a path) is deliberately not surfaced.
            AppError::DataDir(_) => "the documentation data directory is unavailable or invalid; \
                 set `RUST_DOCS_MCP_DATA_DIR` to a writable location and retry"
                .to_string(),
            AppError::Io { context, .. } => {
                format!("an internal error occurred while {context}; please retry")
            }
            AppError::RuntimeStart { message, .. } => message.clone(),
        }
    }

    fn data(&self) -> Option<serde_json::Value> {
        None
    }
}

/// Map any [`ProtocolError`] to its version-aware [`ErrorData`].
///
/// This is the single entry point handlers use: `map(err, version)` returns the
/// `ErrorData` (with the version-aware code upgrade) to return from the handler.
pub fn map<E: ProtocolError>(err: E, version: Option<&ProtocolVersion>) -> ErrorData {
    err.protocol_error(version)
}

// ---------------------------------------------------------------------------
// Response-size bounding — issue #154.
//
// The 32 KiB response cap ([`crate::pagination::MAX_RESPONSE_BYTES`]) bounds a
// *successful* page, but an error response carries the same JSON-RPC envelope, so
// it must be bounded too. #154 is the gap: a 40,000-character identifier echoed
// in *both* the error message and the structured `data` payload produced an ~80 KB
// error that bypassed the cap, because no input-length gate or bounded-error
// assembly protected the error path. Two layers close it:
//
//   1. **Handler-boundary input cap.** A user-controlled identifier (section id,
//      category, command key, source, cursor, resource URI) larger than
//      [`MAX_IDENTIFIER_BYTES`] is rejected *before* it reaches a handler's
//      not-found mapping, so the full input is never echoed. The rejection is a
//      bounded [`IdentifierTooLarge`] error: a short diagnostic preview plus the
//      original byte length, never the full string — the "bounded preview plus
//      original length" the issue asks for, replacing the duplicate full echo.
//   2. **Final serialized-size backstop.** Even a bounded error is serialized and
//      checked against [`ERROR_RESPONSE_BUDGET`] ([`bound_error_response`]); a
//      response that would still exceed the cap (a pathological case the input cap
//      did not anticipate) has its structured `data` dropped to a truncation
//      marker so the whole message fits under the cap.
//
// Together they guarantee no error response — from any of the five tools or the
// two resources — can exceed the 32 KiB cap.

/// The per-identifier UTF-8 byte cap applied at the handler boundary. A
/// user-controlled selector larger than this is rejected with a bounded
/// [`IdentifierTooLarge`] error rather than echoed into the response.
///
/// 4096 is far larger than any real section id, category, command key, source, or
/// cursor (all well under a few hundred bytes), so a legitimate input is never
/// rejected; yet it keeps even a *rejected* input's diagnostic small enough that
/// the message + `data` together stay far under the 32 KiB cap.
pub const MAX_IDENTIFIER_BYTES: usize = 4096;

/// The bounded preview length (UTF-8 bytes) carried by an over-identifier
/// diagnostic. The original byte length is carried alongside so the caller knows
/// how much was truncated.
pub const IDENTIFIER_PREVIEW_BYTES: usize = 64;

/// The serialized-byte budget for a single error response. The 32 KiB response
/// cap ([`MAX_RESPONSE_BYTES`]) is shared with the JSON-RPC envelope
/// (`jsonrpc`, `id`, `error` keys and the `data` truncation marker); this leaves
/// a margin so the `error` object itself never pushes the whole message over the
/// cap.
pub const ERROR_RESPONSE_BUDGET: usize = MAX_RESPONSE_BYTES - 512;

/// A user-controlled identifier that exceeded [`MAX_IDENTIFIER_BYTES`].
///
/// The handler rejects it *before* dispatch (the #154 fix), so the full input
/// never reaches a not-found mapping and is never echoed. The error carries a
/// bounded preview ([`bounded_preview`]) and the original byte length — enough to
/// be actionable (the caller knows the input was too long, and by how much,
/// without re-sending the oversized string).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifierTooLarge {
    /// Which field was over the cap (`section_id`, `category`, `command`,
    /// `source`, `cursor`, `uri`, …).
    pub field: &'static str,
    /// The original UTF-8 byte length of the input.
    pub bytes: usize,
    /// The cap that was exceeded ([`MAX_IDENTIFIER_BYTES`]).
    pub limit: usize,
    /// A bounded, UTF-8-safe preview of the input (never the full string).
    pub preview: String,
}

impl ProtocolError for IdentifierTooLarge {
    fn error_class(&self) -> ErrorClass {
        // An over-long identifier is a caller-input problem, not a missing
        // resource: the input is malformed *by length*, so `INVALID_PARAMS`.
        // (This also means the 2026-07-28 resource-not-found upgrade does not
        // touch it — the code is already `INVALID_PARAMS`.)
        ErrorClass::InvalidParams
    }

    fn message(&self) -> String {
        // A fixed, bounded sentence: the field name, the original byte length, and
        // the cap. The (bounded) preview is carried in `data`, not re-echoed here
        // — this is the duplicate-echo #154 flags, now removed.
        format!(
            "oversized {} identifier: {} UTF-8 bytes exceeds the {}-byte limit; \
             shorten the input to at most {} bytes (a bounded preview is in `data`)",
            self.field, self.bytes, self.limit, self.limit,
        )
    }

    fn data(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "field": self.field,
            "bytes": self.bytes,
            "limit": self.limit,
            "preview": self.preview,
            "truncated": true,
        }))
    }
}

/// Truncate `value` to a bounded, UTF-8-safe preview, appending a U+2026 ellipsis
/// when truncated. The cut is backed off to the nearest char boundary so the
/// prefix is always valid UTF-8 (a multibyte character is never split across the
/// cut); the original length is the caller's responsibility to record alongside.
///
/// `0` is always a char boundary, so the back-off loop always terminates even for
/// a value shorter than the cap (the early return below handles that case, so the
/// loop only runs when `value.len() > IDENTIFIER_PREVIEW_BYTES`).
pub fn bounded_preview(value: &str) -> String {
    if value.len() <= IDENTIFIER_PREVIEW_BYTES {
        return value.to_string();
    }
    let mut cut = IDENTIFIER_PREVIEW_BYTES;
    while !value.is_char_boundary(cut) {
        cut -= 1;
    }
    let prefix = &value[..cut];
    format!("{prefix}\u{2026}")
}

/// Check a user-controlled identifier against [`MAX_IDENTIFIER_BYTES`],
/// returning an [`IdentifierTooLarge`] when it exceeds the cap. This is the
/// handler-boundary gate ([`crate::mcp::DocsServer::dispatch_call`] /
/// [`dispatch_read`]) that keeps an over-long input from echoing into an
/// over-cap error response.
///
/// `value.len()` is the UTF-8 *byte* length — the quantity the response cap
/// measures — so a multibyte input is bounded by its serialized size, not its
/// character count.
pub fn check_identifier(field: &'static str, value: &str) -> Result<(), IdentifierTooLarge> {
    if value.len() <= MAX_IDENTIFIER_BYTES {
        return Ok(());
    }
    Err(IdentifierTooLarge {
        field,
        bytes: value.len(),
        limit: MAX_IDENTIFIER_BYTES,
        preview: bounded_preview(value),
    })
}

/// The final serialized-size backstop. Serialize `error_data`; if it fits the
/// `budget`, return it unchanged. If it exceeds the budget, drop its structured
/// `data` (the likely culprit — a large input echoed into the payload) to a
/// bounded truncation marker that records the original byte count, so the
/// `error` object fits. The `code` is preserved; the `message` is already bounded
/// by the input cap, but a pathological unbounded message is truncated to a
/// preview as a final guard so the whole object fits under any budget.
pub fn bound_error_response(mut error_data: ErrorData, budget: usize) -> ErrorData {
    let serialized = serde_json::to_vec(&error_data)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    if serialized <= budget {
        return error_data;
    }
    // Over budget: replace the structured `data` with a bounded truncation marker
    // that records how many bytes the full response would have been, so a caller
    // (or the operator) can see the error was dropped rather than mis-truncated.
    error_data.data = Some(serde_json::json!({
        "truncated": true,
        "bytes": serialized,
        "limit": budget,
    }));
    // Re-check: a bounded `data` may still leave the message over budget. If so,
    // truncate the message to a bounded preview as the final guard.
    let recheck = serde_json::to_vec(&error_data)
        .map(|bytes| bytes.len())
        .unwrap_or(0);
    if recheck <= budget {
        return error_data;
    }
    error_data.message = std::borrow::Cow::Owned(bounded_preview(&error_data.message));
    error_data
}

/// The final serialized-size backstop using the module's
/// [`ERROR_RESPONSE_BUDGET`]. The handlers call this after [`map`] so no error
/// response can exceed the 32 KiB cap, even if an input cap is bypassed.
pub fn bound_error(error_data: ErrorData) -> ErrorData {
    bound_error_response(error_data, ERROR_RESPONSE_BUDGET)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::commands::{CommandGuidanceError, LookupError};
    use crate::model::{SchemaError, SectionId};
    use crate::pagination::{CursorError, MismatchField};
    use crate::reads::ReadError;
    use crate::runtime::LoadError;
    use crate::search::SearchError;
    use rmcp::model::{ErrorCode, ProtocolVersion};

    fn v(s: &str) -> ProtocolVersion {
        // `ProtocolVersion` has no `FromStr` on this SDK; map the known dated
        // revisions to their SDK constants (the same set the version-aware rule
        // keys off of).
        match s {
            "2024-11-05" => ProtocolVersion::V_2024_11_05,
            "2025-03-26" => ProtocolVersion::V_2025_03_26,
            "2025-06-18" => ProtocolVersion::V_2025_06_18,
            "2025-11-25" => ProtocolVersion::V_2025_11_25,
            "2026-07-28" => ProtocolVersion::V_2026_07_28,
            other => panic!("unknown protocol version {other}"),
        }
    }

    #[test]
    fn invalid_input_maps_to_invalid_params() {
        let err = SearchError::BlankQuery.protocol_error(None);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("blank"));

        let err = SearchError::QueryTooLarge {
            bytes: 500,
            limit: 256,
        }
        .protocol_error(None);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.data.is_some());

        let err = CursorError::Mismatch {
            field: MismatchField::Operation,
            expected: "search".to_string(),
            found: "get_section".to_string(),
        }
        .protocol_error(None);
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("cursor"));
    }

    #[test]
    fn unknown_section_is_resource_not_found_for_pre_2026() {
        let err = ReadError::UnknownSection("cargo/01-build".parse().expect("section id"))
            .protocol_error(Some(&v("2025-11-25")));
        assert_eq!(err.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(
            err.data.expect("data").get("section_id"),
            Some(&"cargo/01-build".into())
        );
    }

    #[test]
    fn resource_not_found_upgrades_to_invalid_params_for_2026_07_28() {
        // The version-aware upgrade: a missing section is INVALID_PARAMS for a
        // peer on 2026-07-28, but RESOURCE_NOT_FOUND for an older peer.
        let err = ReadError::UnknownSection("cargo/01-build".parse().expect("section id"))
            .protocol_error(Some(&v("2026-07-28")));
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);

        let err = ReadError::UnknownSection("cargo/01-build".parse().expect("section id"))
            .protocol_error(Some(&v("2025-11-25")));
        assert_eq!(err.code, ErrorCode::RESOURCE_NOT_FOUND);

        // A missing command key behaves the same way.
        let err = LookupError::UnknownKey {
            requested: "frobnicate".to_string(),
            supported: vec!["build".parse().expect("key")],
        }
        .protocol_error(Some(&v("2026-07-28")));
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(
            err.data.expect("data").get("supported"),
            Some(&serde_json::json!(["build"]))
        );
    }

    #[test]
    fn operational_errors_map_to_internal() {
        // A dangling checklist citation is a corpus-integrity problem.
        let err = ReadError::DanglingExplanation {
            code: "X".to_string(),
            missing: SectionId::from_str("api-guidelines/z").expect("sid"),
        }
        .protocol_error(None);
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);

        // An unloadable corpus is operational.
        let err = LoadError::NoValidCorpus {
            local: Some("local snapshot unreadable".to_string()),
            bundle: Some("no bundled snapshot".to_string()),
        }
        .protocol_error(None);
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);

        // A malformed on-disk snapshot is operational.
        let err = SchemaError::UnsupportedVersion {
            found: 99,
            supported: vec![1],
        }
        .protocol_error(None);
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn unknown_key_surfaces_discoverable_alternatives() {
        let err = LookupError::UnknownKey {
            requested: "frobnicate".to_string(),
            supported: vec![
                "build".parse().expect("build"),
                "clippy".parse().expect("clippy"),
            ],
        }
        .protocol_error(None);
        assert_eq!(err.code, ErrorCode::RESOURCE_NOT_FOUND);
        let data = err.data.expect("data");
        assert_eq!(
            data.get("supported"),
            Some(&serde_json::json!(["build", "clippy"]))
        );
        assert!(err.message.contains("build"));
        assert!(err.message.contains("clippy"));
    }

    #[test]
    fn command_guidance_error_delegates_to_inner() {
        let err = CommandGuidanceError::Lookup(LookupError::UnknownKey {
            requested: "nope".to_string(),
            supported: vec!["build".parse().expect("build")],
        })
        .protocol_error(Some(&v("2026-07-28")));
        // Delegates: UnknownKey is ResourceNotFound, upgraded to InvalidParams.
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    /// AC2: no internal filesystem path or Rust backtrace may leak into a
    /// protocol error's message or data.
    #[test]
    fn no_internal_paths_or_backtraces_leak() {
        let samples: Vec<ErrorData> = vec![
            SearchError::BlankQuery.protocol_error(None),
            SearchError::QueryTooLarge {
                bytes: 500,
                limit: 256,
            }
            .protocol_error(None),
            SearchError::UnsupportedSource("rustc".to_string()).protocol_error(None),
            ReadError::UnknownSection("cargo/01-build".parse().expect("sid")).protocol_error(None),
            ReadError::DanglingExplanation {
                code: "X".to_string(),
                missing: SectionId::from_str("api-guidelines/z").expect("sid"),
            }
            .protocol_error(None),
            LookupError::UnresolvedSection {
                key: "build".to_string(),
                section: SectionId::from_str("cargo/z").expect("sid"),
            }
            .protocol_error(None),
            LoadError::NoValidCorpus {
                local: Some("/Users/dev/.cache/rust-docs/manifest.json: No such file".to_string()),
                bundle: Some("C:\\Users\\dev\\bundle\\manifest.json: access denied".to_string()),
            }
            .protocol_error(None),
            // A malformed identifier that is itself a path must not leak.
            SchemaError::InvalidId("/etc/secret/manifest.json".to_string()).protocol_error(None),
            SchemaError::InvalidUri("/Users/dev/secret".to_string()).protocol_error(None),
            SchemaError::Json("/var/log/thing.txt: 5: no such field".to_string())
                .protocol_error(None),
        ];

        let forbidden = [
            "/Users/",
            "C:\\",
            "/etc/",
            "src/",
            "rust-mcp/",
            "backtrace",
            "panicked",
            "at ",
            "No such file",
            "access denied",
            "0x0",
        ];

        for err in samples {
            let hay = err.message.to_lowercase()
                + " "
                + err
                    .data
                    .as_ref()
                    .map(|d| d.to_string().to_lowercase())
                    .unwrap_or_default()
                    .as_str();
            for needle in forbidden {
                assert!(
                    !hay.contains(needle),
                    "leaked forbidden substring {needle:?} in: {hay}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // #154 response-bounding tests.
    // -----------------------------------------------------------------------

    #[test]
    fn bounded_preview_passes_through_short_values() {
        // A value at or under the preview cap is returned unchanged, no ellipsis.
        assert_eq!(bounded_preview("cargo/01-build"), "cargo/01-build");
        assert_eq!(
            bounded_preview("a".repeat(IDENTIFIER_PREVIEW_BYTES).as_str()),
            "a".repeat(IDENTIFIER_PREVIEW_BYTES),
        );
    }

    #[test]
    fn bounded_preview_truncates_long_values_with_ellipsis() {
        let preview = bounded_preview("a".repeat(1000).as_str());
        // The preview is the cap-sized prefix plus the U+2026 ellipsis — never the
        // full 1000-byte input.
        assert_eq!(
            preview,
            format!("{}\u{2026}", "a".repeat(IDENTIFIER_PREVIEW_BYTES)),
        );
        assert!(preview.len() <= IDENTIFIER_PREVIEW_BYTES + 3);
    }

    #[test]
    fn bounded_preview_is_utf8_safe_at_multibyte_boundaries() {
        // A multibyte character straddling the cut must not be split: the prefix
        // is valid UTF-8 (the back-off loop stops at the nearest char boundary).
        let value = "e".repeat(IDENTIFIER_PREVIEW_BYTES + 1) + "\u{201c}"; // a 3-byte char at the end
        let preview = bounded_preview(value.as_str());
        assert!(
            std::str::from_utf8(preview.as_bytes()).is_ok(),
            "the preview must be valid UTF-8"
        );
        assert!(preview.ends_with('\u{2026}'));
    }

    #[test]
    fn check_identifier_accepts_within_cap_and_rejects_over() {
        assert!(check_identifier("section_id", "cargo/01-build").is_ok());
        // Exactly at the cap is accepted; one byte over is rejected.
        assert!(check_identifier("section_id", "a".repeat(MAX_IDENTIFIER_BYTES).as_str()).is_ok());
        let err = check_identifier("section_id", "a".repeat(MAX_IDENTIFIER_BYTES + 1).as_str())
            .expect_err("one byte over the cap must be rejected");
        assert_eq!(err.field, "section_id");
        assert_eq!(err.bytes, MAX_IDENTIFIER_BYTES + 1);
        assert_eq!(err.limit, MAX_IDENTIFIER_BYTES);
        // The preview is bounded, not the full input.
        assert!(err.preview.len() <= IDENTIFIER_PREVIEW_BYTES + 3);
    }

    #[test]
    fn check_identifier_bounds_multibyte_utf8_by_bytes() {
        // A multibyte input is bounded by its UTF-8 *byte* length, not its
        // character count. Four 3-byte characters over the cap is 12 bytes over.
        let value = "\u{201c}".repeat(MAX_IDENTIFIER_BYTES / 3 + 2);
        let err = check_identifier("command", value.as_str())
            .expect_err("a multibyte input over the byte cap must be rejected");
        assert_eq!(err.bytes, value.len());
        assert_eq!(err.bytes, 3 * (MAX_IDENTIFIER_BYTES / 3 + 2));
    }

    #[test]
    fn oversized_identifier_maps_to_invalid_params_and_is_bounded() {
        // The over-cap identifier is `INVALID_PARAMS` (never `RESOURCE_NOT_FOUND`,
        // so the 2026-07-28 upgrade does not touch it) and the full input is not
        // echoed: the serialized error is small even for a 40,000-char input.
        let big = "a".repeat(40_000);
        let err = check_identifier("section_id", big.as_str()).unwrap_err();
        let mapped = err.protocol_error(Some(&v("2026-07-28")));
        assert_eq!(mapped.code, ErrorCode::INVALID_PARAMS);
        let bytes = serde_json::to_vec(&mapped).expect("serialize the error");
        assert!(
            bytes.len() < 512,
            "the over-cap error must stay tiny, got {} bytes",
            bytes.len(),
        );
        // The full 40,000-char input is not present anywhere in the response.
        let serialized = String::from_utf8(bytes).expect("utf-8");
        assert!(!serialized.contains("a".repeat(1000).as_str()));
        // The diagnostic carries the original length and a bounded preview.
        assert_eq!(
            mapped.data.as_ref().unwrap().get("bytes"),
            Some(&serde_json::json!(40_000))
        );
        assert_eq!(
            mapped.data.as_ref().unwrap().get("truncated"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn bound_error_response_is_a_noop_for_small_errors() {
        let small = SearchError::BlankQuery.protocol_error(None);
        let bounded = bound_error_response(small.clone(), ERROR_RESPONSE_BUDGET);
        // A small error passes through unchanged (its bytes are identical).
        assert_eq!(
            serde_json::to_vec(&bounded).unwrap(),
            serde_json::to_vec(&small).unwrap(),
        );
        // And a small error is still under the cap.
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= ERROR_RESPONSE_BUDGET);
    }

    #[test]
    fn bound_error_response_truncates_an_over_budget_error() {
        // A pathological error whose `data` is enormous must be brought under the
        // budget by dropping the structured data to a truncation marker.
        let huge = serde_json::Value::String("a".repeat(200_000));
        let err = ErrorData::new(ErrorCode::INVALID_PARAMS, "an error", Some(huge));
        let serialized_before = serde_json::to_vec(&err).unwrap().len();
        assert!(serialized_before > ERROR_RESPONSE_BUDGET);
        let bounded = bound_error_response(err, ERROR_RESPONSE_BUDGET);
        let serialized_after = serde_json::to_vec(&bounded).unwrap().len();
        assert!(
            serialized_after <= ERROR_RESPONSE_BUDGET,
            "the backstop must bring the error under the budget, got {} bytes",
            serialized_after,
        );
        // The truncation marker records the original byte count.
        let data = bounded.data.as_ref().expect("a truncation marker");
        assert_eq!(data.get("truncated"), Some(&serde_json::json!(true)));
        assert_eq!(
            data.get("bytes"),
            Some(&serde_json::json!(serialized_before))
        );
    }

    #[test]
    fn bound_error_truncates_a_pathological_message_too() {
        // Defense in depth: even a huge *message* (an input cap bypassed) is
        // truncated so the whole object fits.
        let err = ErrorData::new(
            ErrorCode::INTERNAL_ERROR,
            "x".repeat(100_000),
            Some(serde_json::json!({ "note": "x".repeat(100_000) })),
        );
        let bounded = bound_error(err);
        let serialized = serde_json::to_vec(&bounded).unwrap().len();
        assert!(
            serialized <= ERROR_RESPONSE_BUDGET,
            "a pathological error must be brought under the cap, got {} bytes",
            serialized,
        );
    }
}
