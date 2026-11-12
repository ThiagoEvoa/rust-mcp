//! [052][M4] — `get_api_checklist`, as a real `rmcp` `Tool` over the [036]
//! checklist read and the [037] continuation cursor, projecting to the [048]
//! `GetApiChecklistRequest` / `GetApiChecklistResponse` types and mapping errors
//! through the [048] `ProtocolError` contract.
//!
//! # What this module does
//!
//! `get_api_checklist` lists the snapshot's API-guideline checklist entries —
//! each its upstream `code`, `title`, `category`, checklist `location`, and the
//! `section_id`s that explain it — optionally filtered by a single `category` and
//! paginated by a continuation cursor. It is the catalog entry point a client
//! calls to enumerate the API guidelines before drilling into an individual
//! section via `get_section`.
//!
//! * **All categories by default (AC1).** When the caller omits `category`, the
//!   read returns *every* entry; the [037] page bounds the response by the
//!   response cap and the caller's `limit`, and a `next_cursor` continues the
//!   walk until it is exhausted. Walking every page yields the complete set —
//!   nothing is dropped and nothing is fabricated.
//! * **A valid category filters; an invalid one is a typed, discoverable error
//!   (AC2).** A `category` that matches an entry returns only that category's
//!   entries; a `category` that matches no entry fails with the [048]
//!   `ProtocolError` the [036] `UnknownCategory` maps to (`RESOURCE_NOT_FOUND`,
//!   upgraded to `INVALID_PARAMS` for a `2026-07-28` peer) and surfaces the
//!   snapshot's *discoverable* categories in the error `data`, so a client can
//!   self-correct. No entry is fabricated for an unknown category, and no
//!   internal path or stack-trace leaks.
//! * **Explanations are verified.** The [036] read guarantees every returned
//!   entry's explanation `section_id`s resolve in the same snapshot; a dangling
//!   citation is a corpus-integrity error (never a fabricated section), and a
//!   matched entry with no explanations is vacuously satisfiable.
//!
//! # How it is wired
//!
//! Mirroring `list_sources` ([049]), `search_docs` ([050]), and `get_section`
//! ([051]), this exposes:
//!
//! * [`TOOL_NAME`] — the stable name matching the [048] registered tool;
//! * [`get_api_checklist_tool`] — the [`rmcp::model::Tool`] builder (input/output
//!   schemas from the [048] types);
//! * [`checklist`] — the core read that resolves a category filter (or all) plus
//!   a cursor to a paginated [`GetApiChecklistResponse`];
//! * [`result`] — assembles the final [`rmcp::model::CallToolResult`]
//!   (structured content + human-readable text) and enforces the cap;
//! * [`GetApiChecklistError`] — the tool's own typed error, mapped to the [048]
//!   `ProtocolError` class + sanitized message + the discoverable alternatives.
//!
//! It **consumes** the completed seams rather than reinventing them: the [036]
//! [`crate::reads::get_api_checklist`] category read (with its explanation
//! verification), the [037] [`crate::pagination::paginate_items`] cap-bounded,
//! cursor-bound page, the [048] request/response schemas and
//! [`crate::mcp::protocol_error::ProtocolError`] mapping.
//!
//! The [048] `ServerHandler` seam wires this in a later task, following the same
//! transitional pattern as `list_sources` / `search_docs` / `get_section`: the
//! tool logic and its `CallToolResult` assembly live here; the handler consumes
//! them once it is ready. The module is therefore marked `#![allow(dead_code)]`
//! while the seam is transitional.
//!
//! # Design decisions
//!
//! * **An omitted category is "all categories" (AC1).** `None` selects every
//!   entry; the page's `next_cursor` continues until exhausted, so concatenating
//!   every page equals the full set. A non-empty, non-final page carries a
//!   cursor; the final page carries `None`.
//! * **The cursor binds the category (or "all").** The [037] [`FilterKey`] is
//!   [`FilterKey::category`] when a category is given and [`FilterKey::all`] when
//!   it is omitted, so a cursor minted for one category (or for "all") cannot be
//!   replayed against a different category — the same anti-replay guarantee
//!   `search_docs` gives for its source filter.
//! * **Unknown categories surface the discoverable alternatives (AC2).** The
//!   [036] read's `UnknownCategory` names only the requested category; the tool
//!   augments it with the snapshot's distinct, sorted category list, carried in
//!   the error `data` under `categories` and echoed in the message — mirroring
//!   how [039]'s `LookupError::UnknownKey` surfaces the supported command keys.
//! * **Errors are typed and leak-free.** An unknown category maps to
//!   `RESOURCE_NOT_FOUND` (pre-`2026-07-28`) / `INVALID_PARAMS`
//!   (`2026-07-28`+); a malformed or mismatched cursor delegates to the [037]
//!   `CursorError` mapping (`INVALID_PARAMS`); a dangling citation maps to
//!   `INTERNAL_ERROR`. Every message is built from domain identifiers (the
//!   requested category, the available categories, the offending code/section),
//!   never from an internal path or a backtrace.
//! * **The limit is the [037] default-and-clamp.** `None` yields the five-hit
//!   default, a request above the cap is clamped to twenty, and a `0` yields an
//!   empty page — the [037] single source of truth, so the tool and the
//!   pagination layer cannot disagree.

#![allow(dead_code)]

use rmcp::model::{CallToolResult, ContentBlock, Tool};

use crate::error::AppError;
use crate::mcp::protocol_error::{ErrorClass, ProtocolError};
use crate::mcp::schema::{ChecklistEntryView, GetApiChecklistRequest, GetApiChecklistResponse};
use crate::model::{ChecklistEntry, SnapshotManifest};
use crate::pagination::{
    CursorError, FilterKey, Item, MAX_RESPONSE_BYTES, Operation, clamp_query_limit, paginate_items,
};
use crate::reads::ReadError;
use serde_json::{Value, json};

/// The [048] `rmcp` tool name for `get_api_checklist`.
pub const TOOL_NAME: &str = "get_api_checklist";

/// The [`rmcp::model::Tool`] for `get_api_checklist`, built from the [048]
/// `GetApiChecklistRequest` / `GetApiChecklistResponse` types so its input/output
/// JSON schemas agree with those types.
pub fn get_api_checklist_tool() -> Tool {
    crate::mcp::schema::get_api_checklist_tool()
}

/// The tool's own typed error, mapped through the [048]
/// [`ProtocolError`](crate::mcp::protocol_error::ProtocolError) contract.
///
/// * [`GetApiChecklistError::UnknownCategory`] — a requested category matches no
///   entry in the active snapshot; `supported` is the snapshot's discoverable
///   categories. Maps to `RESOURCE_NOT_FOUND` (upgraded to `INVALID_PARAMS` for
///   a `2026-07-28` peer), mirroring the [036]/[048] `UnknownCategory` mapping,
///   and surfaces the alternatives in `data`. No entry is fabricated.
/// * [`GetApiChecklistError::Cursor`] — a continuation cursor is malformed or
///   does not match this call. Delegates to the [037] `CursorError` mapping
///   (`INVALID_PARAMS`).
/// * [`GetApiChecklistError::Dangling`] — a matched entry cites an explanation
///   `section_id` that does not resolve in the snapshot. A corpus-integrity
///   problem the caller did not cause; maps to `INTERNAL_ERROR`, and no section
///   is fabricated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetApiChecklistError {
    /// A requested category matches no entry. `supported` is the snapshot's
    /// distinct, sorted categories — the discoverable alternatives.
    UnknownCategory {
        /// The category that matched no entry.
        requested: String,
        /// The snapshot's distinct, sorted categories (the alternatives).
        supported: Vec<String>,
    },
    /// A continuation cursor is malformed or does not match this call.
    Cursor(CursorError),
    /// A matched entry cites an explanation section that does not resolve; the
    /// snapshot's citation is inconsistent and nothing is fabricated.
    Dangling {
        /// The offending entry's upstream code.
        code: String,
        /// The explanation section id that did not resolve.
        missing: String,
    },
}

impl ProtocolError for GetApiChecklistError {
    /// The logical error class, mirroring the [048] mapping of the inner seams.
    fn error_class(&self) -> ErrorClass {
        match self {
            // Mirrors the [048] `ReadError::UnknownCategory` → `ResourceNotFound`
            // mapping (the version-aware upgrade to `INVALID_PARAMS` for a
            // `2026-07-28` peer is applied by `protocol_error`).
            Self::UnknownCategory { .. } => ErrorClass::ResourceNotFound,
            // Mirrors the [037] cursor mapping → `INVALID_PARAMS`.
            Self::Cursor(inner) => inner.error_class(),
            // Mirrors the [048] `ReadError::DanglingExplanation` → `INTERNAL_ERROR`
            // mapping (a corpus-integrity problem the caller did not cause).
            Self::Dangling { .. } => ErrorClass::Internal,
        }
    }

    /// The sanitized, actionable message. Built only from domain identifiers —
    /// the requested category, the available categories, or the offending
    /// code/section — never from an internal path or a backtrace.
    fn message(&self) -> String {
        match self {
            Self::UnknownCategory {
                requested,
                supported,
            } => format!(
                "API-guideline category '{requested}' is not in this snapshot; \
                 available categories: [{}]",
                supported
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Cursor(inner) => inner.message(),
            Self::Dangling { code, missing } => format!(
                "the API checklist entry '{code}' cites section '{missing}', which is not in the \
                 current index; this snapshot's citation is inconsistent and the guidance was not \
                 fabricated — refresh the snapshot to resolve it"
            ),
        }
    }

    /// The discoverable alternatives, mirroring [039]'s `LookupError::UnknownKey`
    /// (which surfaces the supported command keys). An unknown category carries
    /// the requested category plus the snapshot's available categories; a cursor
    /// delegates; a dangling citation names the offending code and missing
    /// section. No internal path is surfaced.
    fn data(&self) -> Option<Value> {
        match self {
            Self::UnknownCategory {
                requested,
                supported,
            } => Some(json!({
                "category": requested,
                "categories": supported,
            })),
            Self::Cursor(inner) => inner.data(),
            Self::Dangling { code, missing } => Some(json!({
                "code": code,
                "missing": missing,
            })),
        }
    }
}

/// Resolve a `get_api_checklist` request to its [048] response, paginating the
/// matching entries through the [037] continuation cursor.
///
/// This composes the completed seams in dependency order:
///
/// 1. **Select the filter key** for the [037] cursor — [`FilterKey::category`]
///    for a given category, [`FilterKey::all`] when it is omitted (AC1).
/// 2. **Read** the matching entries (or all) through the [036]
///    [`crate::reads::get_api_checklist`], which verifies every entry's
///    explanations resolve; an unknown category and a dangling citation each map
///    to a typed [`GetApiChecklistError`] (no fabrication, AC2).
/// 3. **Paginate** the matching entries through the [037]
///    [`paginate_items`] page — applying the response cap, the clamped limit, and
///    the continuation-cursor contract — so a result is never truncated
///    implicitly; an omitted category walks every entry across pages (AC1).
/// 4. **Project** the returned entries to the [048] [`ChecklistEntryView`] and
///    assemble the response, echoing the snapshot identity and the effective
///    limit.
///
/// # Errors
///
/// Returns [`GetApiChecklistError::UnknownCategory`] for a category that matches
/// no entry (carrying the snapshot's available categories),
/// [`GetApiChecklistError::Cursor`] for a malformed or mismatched continuation
/// cursor, and [`GetApiChecklistError::Dangling`] for an entry whose explanation
/// does not resolve.
pub fn checklist(
    manifest: &SnapshotManifest,
    request: &GetApiChecklistRequest,
) -> Result<GetApiChecklistResponse, GetApiChecklistError> {
    // 1. The filter key binds the [037] cursor to the category (or "all"), so a
    //    cursor minted for one category cannot be replayed against another.
    let filter = match &request.category {
        None => FilterKey::all(),
        Some(category) => FilterKey::category(category),
    };

    // 2. Read the matching entries (or all) through the [036] checklist read,
    //    which verifies every entry's explanations resolve. An unknown category
    //    or a dangling citation is a typed error — no entry is fabricated.
    let read = match crate::reads::get_api_checklist(manifest, request.category.as_deref()) {
        Ok(read) => read,
        Err(ReadError::UnknownCategory(requested)) => {
            return Err(GetApiChecklistError::UnknownCategory {
                requested,
                supported: available_categories(manifest),
            });
        }
        Err(ReadError::DanglingExplanation { code, missing }) => {
            return Err(GetApiChecklistError::Dangling {
                code,
                missing: missing.to_string(),
            });
        }
        // `UnknownSection` is only produced by the section read, never the
        // checklist read; keep the read total and panic-free by mapping it to
        // the same not-found family (the checklist-scoped `UnknownCategory`).
        Err(_) => {
            return Err(GetApiChecklistError::UnknownCategory {
                requested: String::new(),
                supported: available_categories(manifest),
            });
        }
    };

    // 3. Paginate the matching entries through the [037] cap-bounded, cursor-
    //    bound page. AC1: an omitted category walks every entry across pages.
    let items: Vec<Item> = read
        .entries
        .into_iter()
        .map(|entry| Item::Checklist(Box::new(entry)))
        .collect();
    let page = paginate_items(
        Operation::Checklist,
        &filter,
        &manifest.body.snapshot_id,
        &items,
        request.limit.map(|limit| limit as usize),
        request.cursor.as_deref(),
    )
    .map_err(GetApiChecklistError::Cursor)?;

    // 4. Project the returned entries to the [048] view. Only `Item::Checklist`
    //    items are fed, so the mapping is total in practice; the `filter_map`
    //    keeps it panic-free regardless.
    let entries = page
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Checklist(entry) => Some(view_from(entry.as_ref())),
            _ => None,
        })
        .collect();

    // The effective limit the [037] layer applied (default five, clamped to
    // twenty), echoed so a client knows what window the page used.
    let limit = clamp_query_limit(request.limit.map(|limit| limit as usize));

    Ok(GetApiChecklistResponse {
        category: request.category.clone(),
        entries,
        snapshot_id: manifest.body.snapshot_id.as_str().to_string(),
        limit: limit as u32,
        truncated: page.truncated,
        next_cursor: page.next_cursor,
    })
}

/// The snapshot's distinct checklist categories, sorted — the discoverable
/// alternatives an unknown-category error surfaces.
///
/// A [`std::collections::BTreeSet`] yields a sorted, de-duplicated list, so the
/// alternatives are stable and comparable. An empty corpus yields an empty list
/// (honestly so).
fn available_categories(manifest: &SnapshotManifest) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for entry in &manifest.body.checklist {
        set.insert(entry.category.clone());
    }
    set.into_iter().collect()
}

/// Project a [036]/[039] [`ChecklistEntry`] onto the [048] [`ChecklistEntryView`],
/// preserving the upstream `code`, `title`, `category`, `location`, and the
/// explanatory `section_id`s verbatim.
fn view_from(entry: &ChecklistEntry) -> ChecklistEntryView {
    ChecklistEntryView {
        code: entry.code.clone(),
        title: entry.title.clone(),
        category: entry.category.clone(),
        location: entry.location.clone(),
        explanation_sections: entry
            .explanation_sections
            .iter()
            .map(|section| section.as_str().to_string())
            .collect(),
    }
}

/// Assemble the `CallToolResult` for a `get_api_checklist` response.
///
/// Mirrors [`get_section`](crate::mcp::get_section::result) and
/// [`search_docs`](crate::mcp::search_docs::result): the full
/// [`GetApiChecklistResponse`] serialized to JSON rides in `structured_content`
/// for structured-capable clients, and a compact human-readable summary is the
/// primary text content block. The response-assembled byte size is then enforced
/// against the [`MAX_RESPONSE_BYTES`] cap: the [037] page already bounds every
/// entry, so a real response fits and this is a defensive safety net against a
/// pathological snapshot, surfaced as a typed [`AppError`].
///
/// # Errors
///
/// Returns [`AppError::RuntimeStart`] when the response exceeds the response cap
/// or serialization fails.
pub fn result(resp: &GetApiChecklistResponse) -> Result<CallToolResult, AppError> {
    let structured = serde_json::to_value(resp).map_err(|e| {
        AppError::runtime_start(
            "encode get_api_checklist structured output",
            Some(Box::new(e)),
        )
    })?;
    let text = render_text(resp);
    // The cap is enforced on *both* rendered forms: the structured body carries
    // every field, so its serialized byte length is the binding measurement,
    // while the text rendering guards the "both within the cap" contract.
    let structured_bytes = serde_json::to_string(&structured)
        .map_err(|e| {
            AppError::runtime_start(
                "measure get_api_checklist structured output",
                Some(Box::new(e)),
            )
        })?
        .len();
    let byte_len = structured_bytes.max(text.len());
    if byte_len > MAX_RESPONSE_BYTES {
        return Err(AppError::runtime_start(
            format!(
                "get_api_checklist response ({byte_len} bytes) exceeds the \
                 {MAX_RESPONSE_BYTES}-byte response cap"
            ),
            None,
        ));
    }
    // The text summary is the primary content block; the full structured payload
    // rides in `structuredContent` for structured-capable clients.
    let mut call = CallToolResult::success(vec![ContentBlock::text(text)]);
    call.structured_content = Some(structured);
    Ok(call)
}

/// Render a compact, structured text summary of a `get_api_checklist` response.
///
/// Mirrors the sibling renderers: a header naming the snapshot, the category
/// (or "all"), the entry count, and the effective limit, then one block per
/// entry (its code, title, category, location, and explanation section ids),
/// plus a truncation notice when a `next_cursor` is present.
pub fn render_text(resp: &GetApiChecklistResponse) -> String {
    let category = match &resp.category {
        Some(category) => category.clone(),
        None => "all".to_string(),
    };
    let mut lines = Vec::with_capacity(resp.entries.len() + 4);
    lines.push(format!(
        "get_api_checklist — snapshot {}, category {}, {} entr{} (limit {})",
        resp.snapshot_id,
        category,
        resp.entries.len(),
        if resp.entries.len() == 1 { "y" } else { "ies" },
        resp.limit,
    ));
    if resp.truncated {
        lines.push(format!(
            "  (truncated; resume with next_cursor: {})",
            resp.next_cursor.as_deref().unwrap_or("?")
        ));
    }
    for (i, entry) in resp.entries.iter().enumerate() {
        lines.push(format!(
            "  [{}] {} {} ({}) @ {}",
            i + 1,
            entry.code,
            entry.title,
            entry.category,
            entry.location,
        ));
        if !entry.explanation_sections.is_empty() {
            lines.push(format!(
                "      explains: {}",
                entry.explanation_sections.join(", ")
            ));
        }
    }
    lines.join("\n")
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bundled::manifest;
    use crate::model::SectionId;
    use rmcp::model::{ErrorCode, ProtocolVersion};

    fn request(
        category: Option<&str>,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> GetApiChecklistRequest {
        GetApiChecklistRequest {
            category: category.map(str::to_string),
            limit,
            cursor,
        }
    }

    /// Build a snapshot whose checklist carries `n` entries spread across three
    /// categories, each with an empty `explanation_sections` (vacuously
    /// satisfiable by the [036] read, so no section must resolve). The codes are
    /// `C-000`..`C-(n-1)`, cycling through the three categories.
    fn multi_category_manifest(n: usize) -> SnapshotManifest {
        let mut m = manifest();
        // Start from a clean checklist so the count is exactly `n`.
        m.body.checklist.clear();
        let categories = ["Conversions and From", "Error Handling", "Naming"];
        for i in 0..n {
            m.body.checklist.push(ChecklistEntry {
                code: format!("C-{i:03}"),
                title: format!("Guideline {i}"),
                category: categories[i % categories.len()].to_string(),
                location: format!("C-{i:03}#C-{i:03}"),
                explanation_sections: vec![],
            });
        }
        m
    }

    // ----- AC1: an omitted category retrieves all entries across pages. -----

    #[test]
    fn tool_name_matches_the_registered_tool() {
        assert_eq!(TOOL_NAME, get_api_checklist_tool().name);
        assert_eq!(TOOL_NAME, "get_api_checklist");
    }

    #[test]
    fn input_schema_documents_category_limit_and_cursor() {
        // AC1: the input schema documents the request's fields. `category`,
        // `limit`, and `cursor` are all optional.
        let input = get_api_checklist_tool().input_schema;
        let props = input
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("input schema has `properties`");
        for field in ["category", "limit", "cursor"] {
            assert!(
                props.contains_key(field),
                "input schema is missing the `{field}` field"
            );
        }
        // Every field is optional: `category` is not in `required`.
        let required: Vec<&str> = input
            .get("required")
            .and_then(serde_json::Value::as_array)
            .map(|arr| arr.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default();
        assert!(
            required.is_empty(),
            "no field is required (all optional): {required:?}"
        );
    }

    #[test]
    fn omitted_category_returns_all_entries() {
        // AC1: with no category, entries from *every* category are returned (not
        // filtered to one). A limit that fits the corpus returns them all in a
        // single page; the cross-page walk is covered by
        // `omitted_category_walks_every_entry_across_pages`.
        let m = multi_category_manifest(6);
        let resp = checklist(&m, &request(None, Some(20), None)).expect("all categories");
        assert_eq!(
            resp.entries.len(),
            6,
            "an omitted category returns every entry"
        );
        assert_eq!(resp.category, None, "the response records the omission");
        assert_eq!(resp.snapshot_id, m.body.snapshot_id.as_str());
    }

    #[test]
    fn omitted_category_walks_every_entry_across_pages() {
        // AC1: an omitted category with a small limit paginates; concatenating
        // every page (following the [037] continuation cursor until it is
        // exhausted) yields the complete set, in order, with no gaps or
        // duplicates.
        let m = multi_category_manifest(23);
        let mut collected: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;

        loop {
            let resp =
                checklist(&m, &request(None, Some(5), cursor.take())).expect("the page resolves");
            // The effective limit is the clamped request limit (five).
            assert_eq!(resp.limit, 5);
            pages += 1;
            for entry in &resp.entries {
                collected.push(entry.code.clone());
            }
            if resp.truncated {
                cursor = Some(
                    resp.next_cursor
                        .clone()
                        .expect("a truncated page carries a next cursor"),
                );
            } else {
                assert!(
                    resp.next_cursor.is_none(),
                    "the final page carries no next cursor"
                );
                break;
            }
        }

        assert_eq!(collected.len(), 23, "every entry is walked across pages");
        assert!(pages > 1, "the walk spans more than one page");
        // The collected codes are exactly the full set, in order, with no
        // duplicates.
        let mut expected: Vec<String> = (0..23).map(|i| format!("C-{i:03}")).collect();
        expected.sort();
        collected.sort();
        assert_eq!(
            collected, expected,
            "the concatenated pages equal the full set"
        );
    }

    #[test]
    fn omitted_category_over_cap_paginates_without_loss() {
        // AC1: even when the response cap (not the count limit) truncates a page,
        // the continuation carries the remainder, so the concatenated pages equal
        // the full set. A body-dense entry forces the byte cap to bite.
        let mut m = manifest();
        m.body.checklist.clear();
        let n = 40;
        for i in 0..n {
            m.body.checklist.push(ChecklistEntry {
                code: format!("D-{i:03}"),
                title: "x".repeat(4000),
                category: "Heavy".to_string(),
                location: format!("Heavy#D-{i:03}"),
                explanation_sections: vec![],
            });
        }
        let mut collected = 0usize;
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let resp =
                checklist(&m, &request(None, None, cursor.take())).expect("the page resolves");
            collected += resp.entries.len();
            pages += 1;
            if resp.truncated {
                cursor = Some(
                    resp.next_cursor
                        .clone()
                        .expect("a truncated page carries a cursor"),
                );
            } else {
                assert!(resp.next_cursor.is_none());
                break;
            }
        }
        assert_eq!(collected, n, "every entry survives cap-based pagination");
        assert!(pages > 1, "the cap forces multiple pages");
    }

    // ----- AC2: a valid category filters; an invalid one is a typed error. -----

    #[test]
    fn a_valid_category_filters_to_that_category() {
        // AC2: a category that matches returns only that category's entries.
        let m = multi_category_manifest(9); // three per category
        let resp = checklist(&m, &request(Some("Error Handling"), None, None))
            .expect("a valid category resolves");
        assert_eq!(
            resp.entries.len(),
            3,
            "only the matching category is returned"
        );
        assert_eq!(resp.category, Some("Error Handling".to_string()));
        assert!(
            resp.entries.iter().all(|e| e.category == "Error Handling"),
            "every returned entry belongs to the requested category"
        );
        // The filtered page is not truncated for a three-entry category.
        assert!(!resp.truncated);
        assert!(resp.next_cursor.is_none());
    }

    #[test]
    fn a_valid_category_paginates_and_filters_together() {
        // AC2: a valid category and a small limit paginate *within* that
        // category only.
        let m = multi_category_manifest(9); // three per category
        let mut collected = Vec::new();
        let mut cursor = None;
        loop {
            let resp = checklist(&m, &request(Some("Naming"), Some(1), cursor.take()))
                .expect("the page resolves");
            assert!(
                resp.entries.iter().all(|e| e.category == "Naming"),
                "every page entry is the requested category"
            );
            for entry in &resp.entries {
                collected.push(entry.code.clone());
            }
            cursor = if resp.truncated {
                resp.next_cursor.clone()
            } else {
                assert!(resp.next_cursor.is_none());
                break;
            };
        }
        assert_eq!(
            collected.len(),
            3,
            "the category paginates to its three entries"
        );
    }

    #[test]
    fn an_unknown_category_is_a_typed_error_with_discoverable_alternatives() {
        // AC2: a category that matches no entry fails with the [048]
        // `UnknownCategory` mapping, surfacing the snapshot's discoverable
        // categories in the error `data` — never fabricating an entry.
        let m = multi_category_manifest(6);
        let err = checklist(&m, &request(Some("No Such Category"), None, None))
            .expect_err("an unknown category fails");
        assert!(
            matches!(err, GetApiChecklistError::UnknownCategory { .. }),
            "an unknown category is UnknownCategory"
        );
        if let GetApiChecklistError::UnknownCategory {
            requested,
            supported,
        } = &err
        {
            assert_eq!(requested, "No Such Category");
            // The discoverable alternatives are the snapshot's distinct, sorted
            // categories.
            let mut expected = vec!["Conversions and From", "Error Handling", "Naming"];
            expected.sort();
            assert_eq!(
                supported, &expected,
                "the error names the available categories"
            );
        }
    }

    #[test]
    fn unknown_category_maps_to_the_048_protocol_error() {
        // AC2: the [048] mapping classifies an unknown category as
        // `RESOURCE_NOT_FOUND` for a pre-`2026-07-28` peer and `INVALID_PARAMS`
        // for a newer peer, and surfaces the alternatives in `data`.
        let m = multi_category_manifest(6);
        let err = checklist(&m, &request(Some("Nope"), None, None))
            .expect_err("an unknown category fails");

        let pre = err.protocol_error(Some(&ProtocolVersion::V_2025_11_25));
        assert_eq!(pre.code, ErrorCode::RESOURCE_NOT_FOUND);
        let data = pre.data.expect("the error carries data");
        assert_eq!(data.get("category"), Some(&"Nope".into()));
        assert_eq!(
            data.get("categories"),
            Some(&serde_json::json!([
                "Conversions and From",
                "Error Handling",
                "Naming"
            ]))
        );

        // The version-aware upgrade: the same error is `INVALID_PARAMS` for a
        // `2026-07-28` peer.
        let post = err.protocol_error(Some(&ProtocolVersion::V_2026_07_28));
        assert_eq!(post.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn unknown_category_message_is_actionable_and_leak_free() {
        // AC2: the error message names the alternatives and leaks no internal
        // path or stack-trace.
        let m = multi_category_manifest(6);
        let err = checklist(&m, &request(Some("Nope"), None, None))
            .expect_err("an unknown category fails");
        let pe = err.protocol_error(None);
        let text = format!("{}{}", pe.message, pe.data.unwrap_or_default());
        assert!(
            text.contains("available categories"),
            "the message points at the alternatives: {text}"
        );
        assert!(text.contains("Conversions and From"));
        for forbidden in ["/src/", "at ", "panic", "backtrace", "src/mcp"] {
            assert!(
                !text.contains(forbidden),
                "error leaks `{forbidden}`: {text}"
            );
        }
    }

    #[test]
    fn an_empty_corpus_omitted_category_is_a_valid_empty_result() {
        // AC1: an omitted category over an empty checklist is a valid empty page
        // (not an error), with no next cursor.
        let mut m = manifest();
        m.body.checklist.clear();
        let resp = checklist(&m, &request(None, None, None)).expect("empty is not an error");
        assert!(resp.entries.is_empty());
        assert!(!resp.truncated);
        assert!(resp.next_cursor.is_none());
    }

    #[test]
    fn a_dangling_explanation_is_an_internal_error_not_a_fabrication() {
        // AC2: an entry whose explanation does not resolve fails with a typed
        // internal error (the snapshot's citation is inconsistent); no section
        // is fabricated.
        let mut m = manifest();
        m.body.checklist.clear();
        m.body.checklist.push(ChecklistEntry {
            code: "C-DANGLE".to_string(),
            title: "Dangling".to_string(),
            category: "Conversions and From".to_string(),
            location: "Conversions and From#C-DANGLE".to_string(),
            explanation_sections: vec![
                "api-guidelines/does-not-exist"
                    .parse::<SectionId>()
                    .expect("a well-formed but absent id"),
            ],
        });
        let err = checklist(&m, &request(Some("Conversions and From"), None, None))
            .expect_err("a dangling explanation fails");
        assert!(
            matches!(err, GetApiChecklistError::Dangling { .. }),
            "a dangling explanation is Dangling"
        );
        assert_eq!(
            err.protocol_error(None).code,
            ErrorCode::INTERNAL_ERROR,
            "a dangling citation is an internal (corpus-integrity) error"
        );
    }

    // ----- Continuations via the [037] cursor contract. -----

    #[test]
    fn a_cursor_resumes_the_page() {
        // A `next_cursor` resumes the page (the [037] cursor contract) and
        // advances past the entries already returned.
        let m = multi_category_manifest(6);
        let first = checklist(&m, &request(None, Some(2), None)).expect("first page");
        assert_eq!(first.entries.len(), 2);
        let cursor = first
            .next_cursor
            .clone()
            .expect("a truncated page carries a cursor");
        let second = checklist(&m, &request(None, Some(2), Some(cursor))).expect("resumed page");
        assert_eq!(second.entries.len(), 2);
        // The resumed entries are different from the first page's.
        let first_codes: std::collections::BTreeSet<String> =
            first.entries.iter().map(|e| e.code.clone()).collect();
        for entry in &second.entries {
            assert!(
                !first_codes.contains(&entry.code),
                "the resume advances past the first page"
            );
        }
        // Both pages were read from the same snapshot.
        assert_eq!(first.snapshot_id, second.snapshot_id);
    }

    #[test]
    fn a_cursor_minted_for_a_different_category_is_rejected() {
        // A continuation cursor minted for one category cannot be replayed against
        // another (the [037] filter-key binding).
        let m = multi_category_manifest(9);
        let first =
            checklist(&m, &request(Some("Naming"), Some(1), None)).expect("first page for Naming");
        let cursor = first
            .next_cursor
            .clone()
            .expect("a truncated page carries a cursor");
        // Resume "Naming" but the request now names a different category.
        let err = checklist(&m, &request(Some("Error Handling"), Some(1), Some(cursor)))
            .expect_err("a foreign-category cursor is rejected");
        assert!(
            matches!(
                err,
                GetApiChecklistError::Cursor(CursorError::Mismatch { .. })
            ),
            "a category-mismatched cursor is a Mismatch: {err:?}"
        );
    }

    #[test]
    fn a_foreign_operation_cursor_is_rejected() {
        // A cursor minted for a different operation (here `Search`) cannot be
        // replayed against `get_api_checklist` (the [037] operation binding).
        let m = multi_category_manifest(6);
        let foreign = crate::pagination::Cursor::mint(
            Operation::Search,
            &FilterKey::all(),
            &m.body.snapshot_id,
            0,
        )
        .to_token();
        let err = checklist(&m, &request(None, None, Some(foreign))).expect_err("foreign op fails");
        assert!(
            matches!(
                err,
                GetApiChecklistError::Cursor(CursorError::Mismatch { .. })
            ),
            "a foreign-operation cursor is a Mismatch: {err:?}"
        );
    }

    #[test]
    fn a_malformed_cursor_is_a_typed_error() {
        // A non-cursor token is a typed [`CursorError::Malformed`], never a panic.
        let m = multi_category_manifest(6);
        let err = checklist(&m, &request(None, None, Some("not-a-cursor".to_string())))
            .expect_err("a malformed cursor fails");
        assert!(
            matches!(err, GetApiChecklistError::Cursor(CursorError::Malformed(_))),
            "a malformed cursor is Malformed: {err:?}"
        );
        assert_eq!(
            err.protocol_error(Some(&ProtocolVersion::V_2025_11_25))
                .code,
            ErrorCode::INVALID_PARAMS,
            "a cursor failure is INVALID_PARAMS"
        );
    }

    // ----- The `CallToolResult` assembly + cap. -----

    #[test]
    fn result_fits_the_response_cap_for_a_page() {
        let m = multi_category_manifest(6);
        let resp = checklist(&m, &request(None, Some(20), None)).expect("resolves");
        let call = result(&resp).expect("the response fits the cap");
        assert!(!call.content.is_empty(), "a text block is rendered");
        assert!(
            call.structured_content.is_some(),
            "structured content is present"
        );
        assert_eq!(
            call.structured_content.as_ref().unwrap()["entries"]
                .as_array()
                .unwrap()
                .len(),
            6
        );
        assert_eq!(
            call.structured_content.as_ref().unwrap()["category"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn result_rejects_an_over_cap_response() {
        // A response whose serialized body alone exceeds the cap is rejected
        // defensively.
        let entries = (0..40)
            .map(|i| ChecklistEntryView {
                code: format!("C-{i:03}"),
                title: "x".repeat(5000),
                category: "Heavy".to_string(),
                location: "x".repeat(5000),
                explanation_sections: vec![],
            })
            .collect();
        let resp = GetApiChecklistResponse {
            category: Some("Heavy".to_string()),
            entries,
            snapshot_id: "seed".to_string(),
            limit: 40,
            truncated: true,
            next_cursor: Some("cursor".to_string()),
        };
        let err = result(&resp).expect_err("an over-cap response is rejected");
        assert!(
            err.to_string().contains("response cap"),
            "the error names the response cap: {err}"
        );
    }

    #[test]
    fn text_surface_summarizes_the_response() {
        let m = multi_category_manifest(3);
        let resp = checklist(&m, &request(None, None, None)).expect("resolves");
        let text = render_text(&resp);
        assert!(
            text.contains("get_api_checklist —"),
            "the text names the tool"
        );
        assert!(text.contains("C-000"), "the text names an entry");
        assert!(!text.contains("at "), "no stack-trace-like text");
        let call = result(&resp).expect("the response assembles");
        assert!(!call.content.is_empty(), "the call carries content");
    }

    #[test]
    fn the_read_is_deterministic() {
        // The same request over the same snapshot yields the same response.
        let m = multi_category_manifest(6);
        let a = checklist(&m, &request(Some("Naming"), None, None)).expect("resolves");
        let b = checklist(&m, &request(Some("Naming"), None, None)).expect("resolves");
        assert_eq!(a, b, "the same request yields the same response");
    }
}
