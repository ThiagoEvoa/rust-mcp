//! [049][M4] — the `list_sources` MCP tool.
//!
//! This module builds the complete [`ListSourcesResponse`](crate::mcp::schema::ListSourcesResponse)
//! from the active runtime snapshot and renders it in the two forms the
//! [output contract](crate::mcp::schema) requires: a **structured** JSON object
//! (for clients that consume structured output) and a **concise text** summary
//! (for older clients and human inspection). Both carry the same eight
//! registered sources, in the same order, with the same per-source freshness
//! verdicts, and both fit within the response cap
//! ([`MAX_RESPONSE_BYTES`](crate::pagination::MAX_RESPONSE_BYTES)).
//!
//! # What the tool reports
//!
//! `list_sources` is the catalog entry point: an empty-object call returns every
//! source the server can serve, plus the snapshot metadata that pins the result.
//!
//! * **Sources** — the eight registered collections, in registry order, each with
//!   its identity, title, freshness verdict, and captured-content identity. The
//!   per-source freshness is computed by [`build_report`](crate::status::build_report)
//!   against the same `as_of` / threshold the status command uses, so the verdict
//!   is deterministic and reproducible.
//! * **Command catalog** — the distinct, alphabetised command keys the
//!   `get_command_guidance` tool can serve, surfaced in the `commands` field.
//! * **Snapshot metadata** — the snapshot id, the corpus content hash, and the
//!   schema version, so a result is always attributable to a specific, immutable
//!   corpus.
//!
//! # Errors
//!
//! The response is built from the *active, validated* snapshot, so the only
//! fallible step is serialising that snapshot for its content hash — an
//! operational failure that surfaces as an internal error, never a panic. An
//! over-cap response is likewise an internal error (the catalog is small enough
//! that this never occurs in practice).
//!
//! # Provenance
//!
//! The source registry, the command catalog, and the per-source freshness all
//! derive from the snapshot the server already serves; this tool reads them and
//! adds no upstream fetch, no execution, and no network call.

// This module exposes the `list_sources` tool logic (response construction,
// rendering, and `CallToolResult` assembly). It is the API surface that the
// handler dispatch in `crate::mcp` calls when a client invokes the tool; now
// that the dispatch consumes every public function, the module no longer needs
// `#![allow(dead_code)]`.

use rmcp::model::{CallToolResult, ContentBlock};

use crate::error::AppError;
use crate::mcp::schema::{ListSourcesResponse, SourceStatusView};
use crate::model::SnapshotManifest;
use crate::pagination::MAX_RESPONSE_BYTES;
use crate::provenance::{Instant, StaleThreshold};
use crate::status::{SourceStatus, build_report};

/// The canonical tool name.
pub const TOOL_NAME: &str = "list_sources";

/// Build the complete, ordered [`ListSourcesResponse`] from the active snapshot.
///
/// `as_of` and `threshold` pin the per-source freshness verdict (so the result is
/// reproducible and matches the `status` command). The source set, ordering, and
/// counts come from the registry; the command catalog from the snapshot body.
pub fn response(
    manifest: &SnapshotManifest,
    as_of: Instant,
    threshold: StaleThreshold,
) -> Result<ListSourcesResponse, AppError> {
    let report = build_report(manifest, as_of, threshold);
    let corpus_hash = corpus_hash(manifest)?;
    Ok(ListSourcesResponse {
        snapshot_id: report.snapshot_id.as_str().to_string(),
        corpus_hash,
        sources: report.sources.iter().map(view_from).collect(),
        commands: manifest
            .body
            .commands
            .iter()
            .map(|entry| entry.key.as_str().to_string())
            .collect(),
        schema_version: manifest.schema_version.as_u32().to_string(),
    })
}

/// Map a per-source [`SourceStatus`] to its response view, keeping the
/// freshness verdict in the human label the output contract uses
/// (`fresh` / `stale` / `unknown`).
fn view_from(source: &SourceStatus) -> SourceStatusView {
    SourceStatusView {
        id: source.id.as_str().to_string(),
        title: source.title.clone(),
        last_checked: source
            .last_checked
            .as_ref()
            .map(|at| at.as_str().to_string()),
        fetched_at: source.fetched_at.as_str().to_string(),
        upstream_modified: source
            .upstream_modified
            .as_ref()
            .map(|at| at.as_str().to_string()),
        content_hash: source.content_hash.as_str().to_string(),
        freshness: verdict_label(source.freshness.is_stale()).to_string(),
    }
}

/// Render a freshness verdict as the label the output contract uses.
///
/// `Some(true)` → `stale`, `Some(false)` → `fresh`, `None` → `unknown` (the
/// verdict is unknown when there is no `last_checked` time to compare).
fn verdict_label(stale: Option<bool>) -> &'static str {
    match stale {
        Some(true) => "stale",
        Some(false) => "fresh",
        None => "unknown",
    }
}

/// The corpus content hash — a deterministic FNV-1a over the serialised,
/// validated manifest. The active snapshot always serialises (every field is a
/// serialisable, validated type), so a failure here is an operational error, not
/// a logic bug.
fn corpus_hash(manifest: &SnapshotManifest) -> Result<String, AppError> {
    let bytes = manifest
        .to_json_string()
        .map_err(|e| AppError::RuntimeStart {
            message: "serialise active snapshot for list_sources content hash".to_string(),
            source: Some(Box::new(e)),
        })?;
    Ok(crate::model::CorpusHash::compute(bytes.as_bytes())
        .as_str()
        .to_string())
}

/// Render the response as a concise, human-readable summary that **agrees** with
/// the structured output: the same snapshot metadata, the same eight sources in
/// the same order with the same verdicts, and the same command keys.
///
/// The catalog is small, so the text is well under the response cap; the
/// [`result`] function enforces that bound on both forms.
pub fn render_text(resp: &ListSourcesResponse) -> String {
    let mut lines = Vec::with_capacity(resp.sources.len() + 2);
    lines.push(format!(
        "list_sources — snapshot {}, corpus {}, schema v{}",
        resp.snapshot_id, resp.corpus_hash, resp.schema_version,
    ));
    for source in &resp.sources {
        lines.push(format!(
            "  - {} [{}]: {} (last checked {}, content {})",
            source.id,
            source.freshness,
            source.title,
            source
                .last_checked
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            source.content_hash,
        ));
    }
    let command_list = resp.commands.join(", ");
    lines.push(format!(
        "command guidance: {} key(s): {}",
        resp.commands.len(),
        if command_list.is_empty() {
            "(none)".to_string()
        } else {
            command_list
        },
    ));
    lines.join("\n")
}

/// Assemble the complete [`CallToolResult`]: the structured JSON plus the
/// concise text rendering, both within the response cap.
///
/// The structured JSON is the source of truth; the text content block is a
/// readable summary that carries the same data. The cap is enforced on **both**
/// rendered forms: the structured body is the larger of the two (it carries every
/// field plus JSON overhead, while the text is a rendered subset of the same
/// data), so the structured measurement is the binding one and the text
/// measurement is the guard that keeps the two forms from ever disagreeing with
/// the "both within the cap" contract. An over-cap catalog would be an
/// operational error, which never occurs for the eight-source corpus.
pub fn result(resp: &ListSourcesResponse) -> Result<CallToolResult, AppError> {
    let structured = serde_json::to_value(resp).map_err(|e| AppError::RuntimeStart {
        message: "encode list_sources structured output".to_string(),
        source: Some(Box::new(e)),
    })?;
    // The cap is enforced on *both* rendered forms (see the function docs): the
    // structured body is the larger, so its measurement is the binding one, and
    // the text measurement guards the "both within the cap" contract.
    let structured_bytes = serde_json::to_vec(&structured)
        .map_err(|e| AppError::RuntimeStart {
            message: "measure list_sources structured output".to_string(),
            source: Some(Box::new(e)),
        })?
        .len();
    let text = render_text(resp);
    // `str::len` is the UTF-8 byte length, which is the cap's unit.
    let byte_len = structured_bytes.max(text.len());
    if byte_len > MAX_RESPONSE_BYTES {
        return Err(AppError::RuntimeStart {
            message: format!(
                "list_sources response ({byte_len} bytes) exceeds the {MAX_RESPONSE_BYTES}-byte response cap"
            ),
            source: None,
        });
    }
    let object = structured
        .as_object()
        .cloned()
        .ok_or_else(|| AppError::RuntimeStart {
            message: "list_sources structured output is not a JSON object".to_string(),
            source: None,
        })?;
    // `CallToolResult::success` sets is_error=Some(false); we add the
    // structured content so structured-capable clients get the typed payload.
    let mut call = CallToolResult::success(vec![ContentBlock::text(text)]);
    call.structured_content = Some(serde_json::Value::Object(object));
    Ok(call)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundled;
    use crate::registry::SourceCollection;

    /// A complete, eight-source bundled snapshot with real section and
    /// command-catalog entries, pinned to a fixed `as_of`.
    fn snapshot() -> SnapshotManifest {
        bundled::manifest()
    }

    fn as_of() -> Instant {
        // A fixed "now" so the report is deterministic. The bundled seed has
        // `last_checked: None` for every source (see `src/bundled.rs`), so every
        // verdict resolves to "unknown": freshness is still computed from the
        // snapshot's sources (via `build_report` / `evaluate_freshness`), there is
        // simply no `last_checked` to compare, so the verdict is never guessed as
        // "fresh". This is what `verdicts_are_unknown_for_a_snapshot_without_
        // last_checked` asserts.
        Instant::parse("2026-01-01T00:00:00Z").expect("valid")
    }

    fn threshold() -> StaleThreshold {
        StaleThreshold::days(30)
    }

    #[test]
    fn response_lists_all_eight_sources_in_registry_order() {
        let resp = response(&snapshot(), as_of(), threshold()).expect("build");
        assert_eq!(resp.sources.len(), 8);
        let order: Vec<&str> = resp.sources.iter().map(|s| s.id.as_str()).collect();
        let expected: Vec<&str> = SourceCollection::ALL.iter().map(|s| s.id()).collect();
        assert_eq!(order, expected);
    }

    #[test]
    fn response_carries_snapshot_metadata_and_schema_version() {
        let resp = response(&snapshot(), as_of(), threshold()).expect("build");
        assert_eq!(resp.schema_version, "1");
        assert!(resp.corpus_hash.starts_with("c128-"));
        assert!(!resp.snapshot_id.is_empty());
    }

    #[test]
    fn response_surfaces_command_catalog_discovery() {
        let resp = response(&snapshot(), as_of(), threshold()).expect("build");
        // The bundled snapshot ships a small command catalog; the commands
        // field carries the discoverable keys for `get_command_guidance`.
        assert!(!resp.commands.is_empty());
    }

    #[test]
    fn text_and_structured_agree() {
        let resp = response(&snapshot(), as_of(), threshold()).expect("build");
        let text = render_text(&resp);

        // Per-source, field-by-field agreement: each structured source's *whole*
        // rendered line — id, verdict, title, last-checked (or "unknown"), and
        // content hash — must appear in the text, in registry order. This pins
        // that the text and structured forms agree on every field, not merely
        // that the ids appear in order.
        let mut last = usize::MIN;
        for source in &resp.sources {
            let last_checked = source
                .last_checked
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let line = format!(
                "  - {} [{}]: {} (last checked {}, content {})",
                source.id, source.freshness, source.title, last_checked, source.content_hash,
            );
            let pos = text.find(&line).unwrap_or_else(|| {
                panic!(
                    "text disagrees with structured source {}: {line}",
                    source.id
                )
            });
            assert!(pos > last, "sources appear in order in the text");
            last = pos;
        }

        // Snapshot metadata in the header agrees with the structured fields.
        assert!(
            text.contains(&format!("snapshot {}", resp.snapshot_id)),
            "the header carries the snapshot id"
        );
        assert!(
            text.contains(&format!("corpus {}", resp.corpus_hash)),
            "the header carries the corpus hash"
        );
        assert!(
            text.contains(&format!("schema v{}", resp.schema_version)),
            "the header carries the schema version"
        );

        // Command-catalog keys agree: every key appears, and the full
        // comma-joined list matches the structured `commands` field.
        for key in &resp.commands {
            assert!(
                text.contains(key.as_str()),
                "command key {key} appears in the text"
            );
        }
        let command_list = resp.commands.join(", ");
        assert!(
            text.contains(&command_list),
            "the text carries the full command key list"
        );
    }

    #[test]
    fn result_produces_structured_and_text_content_within_cap() {
        let resp = response(&snapshot(), as_of(), threshold()).expect("build");
        let call = result(&resp).expect("result");
        // Structured content is a JSON object carrying all eight sources.
        let structured = call
            .structured_content
            .as_ref()
            .expect("structured content present");
        let sources = structured
            .get("sources")
            .and_then(|v| v.as_array())
            .expect("sources array");
        assert_eq!(sources.len(), 8);
        // The text content block is the concise rendering.
        assert_eq!(call.content.len(), 1);
        // The complete response fits the cap.
        let bytes = serde_json::to_vec(&structured).expect("serialise");
        assert!(bytes.len() <= MAX_RESPONSE_BYTES);
        assert!(!call.is_error.unwrap_or(false));
    }

    #[test]
    fn verdicts_are_unknown_for_a_snapshot_without_last_checked() {
        let resp = response(&snapshot(), as_of(), threshold()).expect("build");
        // The bundled seed has `last_checked: None` for every source, so the
        // freshness verdict is always "unknown" (never guessed as fresh).
        for source in &resp.sources {
            assert_eq!(source.freshness, "unknown");
        }
    }

    #[test]
    fn corpus_hash_is_stable_for_an_identical_snapshot() {
        let a = response(&snapshot(), as_of(), threshold()).expect("build");
        let b = response(&snapshot(), as_of(), threshold()).expect("build");
        assert_eq!(a.corpus_hash, b.corpus_hash);
    }

    #[test]
    fn tool_name_is_list_sources() {
        assert_eq!(TOOL_NAME, "list_sources");
    }

    // F-049-2: exercise `verdict_label`'s `Some(true)`/`Some(false)` arms here
    // (the `None` arm is already covered by the bundled-snapshot tests), so the
    // freshness-to-label mapping is pinned in-module, not only upstream at the
    // status layer.
    #[test]
    fn verdict_label_maps_every_arm() {
        assert_eq!(verdict_label(Some(true)), "stale");
        assert_eq!(verdict_label(Some(false)), "fresh");
        assert_eq!(verdict_label(None), "unknown");
    }

    // F-049-3: drive `result`'s over-cap rejection (`byte_len > MAX_RESPONSE_BYTES`
    // -> `Err`) without an 8-source catalog by building a response whose serialised
    // body (and its text rendering) blow past the 32 KiB cap on their own.
    #[test]
    fn result_rejects_a_response_over_the_cap() {
        let wide = "x".repeat(512);
        let sources = (0..128)
            .map(|i| SourceStatusView {
                id: format!("source-{i:03}"),
                title: wide.clone(),
                last_checked: Some(wide.clone()),
                fetched_at: wide.clone(),
                upstream_modified: Some(wide.clone()),
                content_hash: wide.clone(),
                freshness: "unknown".to_string(),
            })
            .collect();
        let resp = ListSourcesResponse {
            snapshot_id: "over-cap".to_string(),
            corpus_hash: "c128-over-cap".to_string(),
            sources,
            commands: vec![wide.clone()],
            schema_version: "1".to_string(),
        };
        // Precondition: the complete serialised body really does exceed the cap.
        assert!(
            serde_json::to_vec(&resp).expect("serialise").len() > MAX_RESPONSE_BYTES,
            "precondition: the response body exceeds the cap"
        );
        let err = result(&resp).expect_err("an over-cap response is a typed error");
        let message = err.to_string();
        assert!(
            message.contains("exceeds the") && message.contains(&MAX_RESPONSE_BYTES.to_string()),
            "the rejection names the cap: {message}"
        );
    }
}
