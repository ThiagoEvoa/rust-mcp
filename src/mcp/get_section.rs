//! [051][M4] — `get_section`, as a real `rmcp` `Tool` over the [036] section
//! read and the [037] continuation cursor, projecting to the [048]
//! `GetSectionRequest` / `GetSectionResponse` types and mapping errors through
//! the [048] `ProtocolError` contract.
//!
//! # What this module does
//!
//! `get_section` resolves a **server-issued** section id to the section's
//! complete record — Markdown body, heading-context breadcrumb, citations,
//! source freshness, and snapshot identity — served from one active snapshot
//! with no network. The caller supplies an id, never a URL (AC1): the id is a
//! key into the [036] `SectionIndex`, and a not-found / not-well-formed id is
//! a typed [`crate::mcp::protocol_error::ProtocolError`] (`RESOURCE_NOT_FOUND`
//! pre-2026), never a fabricated section.
//!
//! When the body exceeds the response cap, the section is **paginated through
//! the [037] continuation cursor** (`SectionContinuation`), never silently
//! truncated: the first page carries a `next_cursor` and a `truncated` flag, and
//! a resumed call re-clips the remainder from the cursor's absolute offset.
//!
//! # How it is wired
//!
//! Mirroring `list_sources` ([049]) and `search_docs` ([050]), this exposes:
//!
//! * [`TOOL_NAME`] — the stable name matching the [048] registered tool;
//! * [`get_section_tool`] — the [`rmcp::model::Tool`] builder (input/output
//!   schemas from the [048] types);
//! * [`get_section`] — the core read that resolves an id to a
//!   [`GetSectionResponse`], paginating through the [037] cursor;
//! * [`result`] — assembles the final [`rmcp::model::CallToolResult`]
//!   (structured content + human-readable text) and enforces the cap;
//! * [`GetSectionError`] — the tool's own typed error, mapped to the [048]
//!   `ProtocolError` class + sanitized message + optional data.
//!
//! The [048] `ServerHandler` seam wires this in a later task, following the
//! same transitional pattern as `list_sources` / `search_docs`: the tool logic
//! and its `CallToolResult` assembly live here; the handler consumes them once
//! it is ready. The module is therefore marked `#![allow(dead_code)]` while the
//! seam is transitional.
//!
//! # Design decisions
//!
//! * **The caller supplies an id, not a URL (AC1).** The `section_id` is parsed
//!   into a [`crate::model::SectionId`] and used as a key into the
//!   [`crate::model::SectionIndex`] — the server never reconstructs a source
//!   URL. The [048] input schema documents only `section_id` and the optional
//!   `cursor`; it carries no `url`, which this module's tests assert.
//! * **The body is paginated, never truncated (AC1/AC2).** Over the cap, the
//!   section is clipped by the [037] `clip_body` primitive, and the remainder is
//!   carried as a `SectionContinuation` cursor whose offset is an *absolute*
//!   byte offset into the section's full body — so a resumed call re-clips from
//!   the correct position. The concatenated pages equal the full body.
//! * **Continuation offsets are guarded.** The [037] cursor's offset is a byte
//!   offset into a `str`; this build mints only character-boundary offsets, but
//!   a client could forge a non-boundary offset. The slice is therefore guarded
//!   with `is_char_boundary` so external input can never cause a panic
//!   (per the repository's no-panic-on-external-input policy).
//! * **Freshness is the read's own verdict.** `get_section` passes the caller's
//!   `as_of` + `threshold` through to the [036] read, which computes the source
//!   freshness (`fresh` / `stale` / `unknown`); the tool reports that verdict
//!   verbatim and never invents one.
//! * **Citations are carried from the corpus read (AC2).** The section's
//!   [`crate::model::Citation`] entries are projected onto the [048]
//!   `CitationView`, keeping the section id and source on each, so the
//!   response's citations match the corpus read.
//! * **Errors are typed and leak-free.** A not-found / not-well-formed id maps
//!   to `RESOURCE_NOT_FOUND` (pre-2026 code `-32002`, code name
//!   `RESOURCE_NOT_FOUND`) with a sanitized, actionable message; a bad cursor
//!   delegates to the [037] `CursorError` mapping (`INVALID_PARAMS`).

#![allow(dead_code)]

use rmcp::model::{CallToolResult, ContentBlock, Tool};

use crate::error::AppError;
use crate::mcp::protocol_error::{ErrorClass, ProtocolError};
use crate::mcp::schema::{CitationView, GetSectionRequest, GetSectionResponse, SectionView};
use crate::model::{Section, SectionId, SnapshotManifest};
use crate::pagination::{Cursor, CursorError, FilterKey, MAX_RESPONSE_BYTES, Operation, clip_body};
use crate::provenance::{Instant, StaleThreshold};
use crate::reads::{ReadError, SectionRead};
use serde_json::{Value, json};
use std::str::FromStr;

// The [048] `rmcp` tool name for `get_section`.
pub const TOOL_NAME: &str = "get_section";

/// The [`rmcp::model::Tool`] for `get_section`, built from the [048]
/// `GetSectionRequest` / `GetSectionResponse` types so its input/output JSON
/// schemas agree with those types.
pub fn get_section_tool() -> Tool {
    crate::mcp::schema::get_section_tool()
}

/// The tool's own typed error.
///
/// * [`GetSectionError::UnknownSection`] — the section id is not present in the
///   active snapshot, or is not a well-formed id the snapshot can hold. Either
///   way the section is not retrievable, and no section is fabricated. Maps to
///   `RESOURCE_NOT_FOUND`.
/// * [`GetSectionError::Cursor`] — a continuation cursor is malformed or does
///   not match this call. Delegates to the [037] `CursorError` mapping
///   (`INVALID_PARAMS`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GetSectionError {
    /// The section id is not present in the active snapshot, or is not a
    /// well-formed id the snapshot can hold. Either way, the section is not
    /// retrievable, and no section is fabricated.
    UnknownSection { id: String },
    /// A continuation cursor is malformed or does not match this call.
    Cursor(CursorError),
}

impl ProtocolError for GetSectionError {
    fn error_class(&self) -> ErrorClass {
        match self {
            Self::UnknownSection { .. } => ErrorClass::ResourceNotFound,
            Self::Cursor(inner) => inner.error_class(),
        }
    }

    fn message(&self) -> String {
        match self {
            Self::UnknownSection { id } => format!(
                "section '{id}' is not in this snapshot; list the available sections with `list_sources` or `search_docs`"
            ),
            Self::Cursor(inner) => inner.message(),
        }
    }

    fn data(&self) -> Option<Value> {
        match self {
            Self::UnknownSection { id } => Some(json!({ "section_id": id.clone() })),
            Self::Cursor(inner) => inner.data(),
        }
    }
}

/// Resolve a `get_section` request to its [048] response, paginating the body
/// through the [037] continuation cursor.
///
/// `as_of` and `threshold` supply the source-freshness computation (the caller
/// passes "now" and the configured staleness threshold); the [036] read uses
/// them to produce the section's freshness verdict.
pub fn get_section(
    manifest: &SnapshotManifest,
    request: &GetSectionRequest,
    as_of: Instant,
    threshold: StaleThreshold,
) -> Result<GetSectionResponse, GetSectionError> {
    // 1. Resolve the caller-supplied, server-issued section id (AC1: the caller
    //    supplies an id, never a URL). A not-well-formed id maps to a typed
    //    not-found; no section is fabricated.
    let section_id = SectionId::from_str(request.section_id.as_str()).map_err(|_| {
        GetSectionError::UnknownSection {
            id: request.section_id.clone(),
        }
    })?;

    // 2. Read the full record through the [036] section read: the Markdown body,
    //    the heading breadcrumb (context), the citations, the snapshot identity,
    //    and the source's freshness — all preserved verbatim (AC2).
    let read = match crate::reads::get_section(manifest, &section_id, as_of, threshold) {
        Ok(read) => read,
        Err(ReadError::UnknownSection(id)) => {
            return Err(GetSectionError::UnknownSection {
                id: id.as_str().to_string(),
            });
        }
        // `UnknownCategory` and `DanglingExplanation` are only produced by the
        // checklist read, never by a section read; keep the read total and
        // panic-free by mapping them to the same not-found.
        Err(_) => {
            return Err(GetSectionError::UnknownSection {
                id: request.section_id.clone(),
            });
        }
    };

    // 3. Resolve the continuation offset, if any, through the [037] cursor
    //    contract. The offset is an absolute byte offset into the section's full
    //    body; this build mints only character-boundary offsets, but a client
    //    could forge a non-boundary offset, so guard the slice (never panic on
    //    external input).
    let filter = FilterKey::section(&section_id);
    let start = match &request.cursor {
        None => 0,
        Some(token) => {
            let cursor = Cursor::from_token(token).map_err(GetSectionError::Cursor)?;
            cursor
                .verify(
                    Operation::SectionContinuation,
                    &filter,
                    &manifest.body.snapshot_id,
                )
                .map_err(GetSectionError::Cursor)?
        }
    };
    if !read.section.body.is_char_boundary(start) {
        return Err(GetSectionError::Cursor(CursorError::Malformed(format!(
            "continuation offset {start} is not a character boundary"
        ))));
    }

    // 4. Clip the remainder to the response cap, carrying any remainder as a
    //    continuation cursor (AC1/AC2: paginated, never silently truncated).
    let part = clip_body(
        &read.section.body[start..],
        Operation::SectionContinuation,
        &filter,
        &manifest.body.snapshot_id,
    );

    // 5. Re-mint the continuation as an absolute offset into the full body, so a
    //    resumed call re-clips from the correct position.
    let next_cursor = part.next.map(|cursor| {
        Cursor::mint(
            Operation::SectionContinuation,
            &filter,
            &manifest.body.snapshot_id,
            start + cursor.offset(),
        )
        .to_token()
    });

    // 6. Project the read to the [048] response, substituting the clipped body
    //    and the (re-minted) continuation cursor.
    let mut section = view_from(&read);
    section.body = part.body;

    Ok(GetSectionResponse {
        section,
        next_cursor,
        truncated: part.truncated,
        schema_version: read.schema_version.as_u32().to_string(),
    })
}

/// Assemble the final `rmcp` `CallToolResult` for a `get_section` response.
///
/// Mirrors `search_docs::result` and `list_sources::result`: structured JSON
/// content + a human-readable text block, with a defensive cap check.
pub fn result(resp: &GetSectionResponse) -> Result<CallToolResult, AppError> {
    let structured = serde_json::to_value(resp).map_err(|e| {
        AppError::runtime_start("encode get_section structured output", Some(Box::new(e)))
    })?;

    let text = render_text(resp);
    let structured_bytes = serde_json::to_string(&structured)
        .map_err(|e| {
            AppError::runtime_start("measure get_section structured output", Some(Box::new(e)))
        })?
        .len();

    // Defensive cap check: the [037] `clip_body` already bounds the body part,
    // so a well-formed response fits; this is the safety net.
    let byte_len = structured_bytes.max(text.len());
    if byte_len > MAX_RESPONSE_BYTES {
        return Err(AppError::runtime_start(
            format!(
                "get_section response ({byte_len} bytes) exceeds the \
                 {MAX_RESPONSE_BYTES}-byte response cap"
            ),
            None,
        ));
    }

    let mut call = CallToolResult::success(vec![ContentBlock::text(text)]);
    call.structured_content = Some(structured);
    Ok(call)
}

// ===========================================================================
// Projections
// ===========================================================================

/// Project a [036] `SectionRead` onto the [048] `SectionView`, keeping the body
/// verbatim and the context / citations / freshness / snapshot identity
/// faithful to the read (AC2). The caller substitutes the clipped body.
fn view_from(read: &SectionRead) -> SectionView {
    let section = &read.section;
    SectionView {
        id: section.id.as_str().to_string(),
        source: section.source_id.as_str().to_string(),
        doc_title: section.doc_title.clone(),
        chapter_path: section.chapter_path.clone(),
        heading_path: section.heading_path.clone(),
        canonical_url: section.canonical_url.as_str().to_string(),
        body: section.body.clone(),
        snapshot_id: read.snapshot_id.as_str().to_string(),
        freshness: verdict_label(read.freshness.is_stale()).to_string(),
        content_hash: section.content_hash.as_str().to_string(),
        citations: citation_views(section),
    }
}

/// Label the source's freshness verdict for the `SectionView`.
fn verdict_label(stale: Option<bool>) -> &'static str {
    match stale {
        Some(true) => "stale",
        Some(false) => "fresh",
        None => "unknown",
    }
}

/// The maximum number of citations a single section carries, keeping the
/// response bounded (mirrors the [050] per-hit cap).
const MAX_CITATIONS: usize = 12;

/// Derive a section's citations for the [048] `SectionView` from its fields, as
/// [050] does for a hit: the canonical upstream document (provenance) plus a
/// note citation for each applicable guideline code, command key, Clippy lint id,
/// and stability/version qualification. The result is capped at
/// [`MAX_CITATIONS`]. These are the "citations" the plan's `get_section`
/// contract requires, and they are derived from the section's fields — i.e. they
/// match the corpus read.
fn citation_views(section: &Section) -> Vec<CitationView> {
    let section_id = section.id.as_str().to_string();
    let source = section.source_id.as_str().to_string();
    let mut out = Vec::with_capacity(
        1 + section.guideline_codes.len()
            + section.command_keys.len()
            + section.lint_ids.len()
            + section.qualifications.len(),
    );

    // Provenance: the canonical upstream document.
    out.push(CitationView {
        section_id: section_id.clone(),
        source: source.clone(),
        url: Some(section.canonical_url.as_str().to_string()),
        title: Some(section.doc_title.clone()),
        note: Some("canonical upstream document".to_string()),
    });

    for code in &section.guideline_codes {
        out.push(note_citation(
            &section_id,
            &source,
            format!("guideline code: {code}"),
        ));
    }
    for key in &section.command_keys {
        out.push(note_citation(
            &section_id,
            &source,
            format!("command key: {key}"),
        ));
    }
    for lint in &section.lint_ids {
        out.push(note_citation(
            &section_id,
            &source,
            format!("lint id: {lint}"),
        ));
    }
    for qualification in &section.qualifications {
        out.push(note_citation(
            &section_id,
            &source,
            format!("qualification: {qualification}"),
        ));
    }

    out.truncate(MAX_CITATIONS);
    out
}

/// Build a note-only citation for `section_id`/`source`.
fn note_citation(section_id: &str, source: &str, note: String) -> CitationView {
    CitationView {
        section_id: section_id.to_string(),
        source: source.to_string(),
        url: None,
        title: None,
        note: Some(note),
    }
}

// ===========================================================================
// Rendering
// ===========================================================================

/// Render a `get_section` response as a human-readable text block for the
/// `CallToolResult` content.
pub fn render_text(resp: &GetSectionResponse) -> String {
    let s = &resp.section;
    let mut lines = Vec::with_capacity(12);
    lines.push(format!(
        "get_section — {} ({id} · snapshot {snapshot} · schema v{version} · freshness {freshness})",
        s.doc_title,
        id = s.id,
        snapshot = s.snapshot_id,
        version = resp.schema_version,
        freshness = s.freshness,
    ));
    if !s.heading_path.is_empty() {
        lines.push(format!("  heading: {}", s.heading_path.join(" › ")));
    }
    lines.push(format!("  source: {}", s.source));
    if !s.canonical_url.is_empty() {
        lines.push(format!("  canonical: {}", s.canonical_url));
    }
    if !s.citations.is_empty() {
        lines.push("  citations:".to_string());
        for citation in &s.citations {
            lines.push(render_citation(citation));
        }
    }
    if resp.truncated {
        lines.push(format!(
            "  (truncated; resume with next_cursor: {})",
            resp.next_cursor.as_deref().unwrap_or("?"),
        ));
    }
    lines.push(format!("  body:\n{}", s.body));
    lines.join("\n")
}

/// Render one citation as a compact, structured text line.
fn render_citation(citation: &CitationView) -> String {
    let mut parts = vec![format!("  ↳ {}:", citation.source)];
    if let Some(note) = &citation.note {
        parts.push(note.clone());
    }
    if let Some(title) = &citation.title {
        parts.push(format!("({title})"));
    }
    if let Some(url) = &citation.url {
        parts.push(url.clone());
    }
    parts.join(" ")
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::{SectionBuilder, SourceId, Url};
    use rmcp::model::{ErrorCode, ProtocolVersion};

    fn as_of() -> Instant {
        Instant::parse("2026-10-30T00:00:00Z").expect("as-of instant")
    }

    fn threshold() -> StaleThreshold {
        StaleThreshold::default()
    }

    fn manifest() -> SnapshotManifest {
        crate::bundled::manifest()
    }

    fn request(section_id: impl Into<String>, cursor: Option<String>) -> GetSectionRequest {
        GetSectionRequest {
            section_id: section_id.into(),
            cursor,
        }
    }

    // ----- AC1: the caller supplies a server-issued id, not a URL. -----

    #[test]
    fn tool_name_matches_the_registered_tool() {
        assert_eq!(TOOL_NAME, get_section_tool().name);
    }

    #[test]
    fn input_schema_documents_section_id_not_a_url() {
        // AC1: resolution uses a server-issued id; the input schema documents
        // `section_id` (and the optional `cursor`) and carries no URL field, so
        // a caller never constructs a source URL.
        let input = get_section_tool().input_schema;
        let props = input
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("input schema has properties");
        assert!(props.contains_key("section_id"), "section_id is documented");
        assert!(
            props.contains_key("cursor"),
            "the continuation cursor is documented"
        );
        for url_field in ["url", "source_url", "uri", "canonical_url"] {
            assert!(
                !props.contains_key(url_field),
                "input schema must not document a `{url_field}` field (AC1)"
            );
        }
    }

    #[test]
    fn caller_supplied_server_issued_id_resolves() {
        // AC1: a caller-supplied, server-issued id resolves to its section
        // without the caller constructing a URL.
        let m = manifest();
        let resp = get_section(
            &m,
            &request("cargo/commands%2Fcargo-build", None),
            as_of(),
            threshold(),
        )
        .expect("the id resolves");
        assert_eq!(resp.section.id, "cargo/commands%2Fcargo-build");
        assert_eq!(resp.section.source, "cargo");
        assert!(!resp.section.body.is_empty(), "the body is served");
        assert!(!resp.truncated, "a small section is not truncated");
        assert!(
            resp.next_cursor.is_none(),
            "a small section has no continuation"
        );
        assert_eq!(resp.schema_version, "1");
    }

    // ----- AC2: the returned fields match the corpus read. -----

    #[test]
    fn returned_fields_match_the_corpus_read() {
        // AC2: markdown, heading context, citations, freshness, and snapshot
        // identity in the response match the [036] corpus read verbatim.
        let m = manifest();
        let id = "api-guidelines/about";
        let read =
            crate::reads::get_section(&m, &SectionId::from_str(id).unwrap(), as_of(), threshold())
                .expect("the [036] read resolves");

        let resp =
            get_section(&m, &request(id, None), as_of(), threshold()).expect("the tool resolves");
        let s = &resp.section;

        // Markdown body: verbatim from the read.
        assert_eq!(s.body, read.section.body, "body matches the read");
        // Heading context: the breadcrumb from the read.
        assert_eq!(
            s.heading_path, read.section.heading_path,
            "heading context matches"
        );
        // Canonical URL / doc title / source: verbatim from the read.
        assert_eq!(s.canonical_url, read.section.canonical_url.as_str());
        assert_eq!(s.doc_title, read.section.doc_title);
        assert_eq!(s.source, read.section.source_id.as_str());
        // Snapshot identity: the read's snapshot id.
        assert_eq!(s.snapshot_id, read.snapshot_id.as_str());
        // Freshness: the read's verdict.
        assert_eq!(
            s.freshness,
            verdict_label(read.freshness.is_stale()).to_string()
        );
        // Citations: derived from the read's section fields (one provenance
        // citation plus a note per applicable code/key/lint/qualification).
        let expected_notes = read.section.guideline_codes.len()
            + read.section.command_keys.len()
            + read.section.lint_ids.len()
            + read.section.qualifications.len();
        assert_eq!(
            s.citations.len(),
            1 + expected_notes,
            "one provenance citation plus the note citations"
        );
        // The first citation is the canonical upstream document.
        assert_eq!(
            s.citations[0].url.as_deref(),
            Some(read.section.canonical_url.as_str()),
            "the first citation is the canonical URL"
        );
    }

    #[test]
    fn citations_match_the_corpus_read() {
        // AC2: a section's citations are derived from its fields (the corpus
        // read's data), matching [050]'s derivation.
        let mut m = manifest();
        let section = SectionBuilder::new(
            SourceId::new("cargo").expect("cargo"),
            "Cargo Build",
            "citations-test",
            Url::new("https://doc.rust-lang.org/cargo/commands/cargo-build").expect("url"),
            "# cargo build\n\nbody text",
        )
        .guideline_codes(["C-CONV".to_string(), "C-CONV-2".to_string()])
        .qualifications(["stable".to_string()])
        .build()
        .expect("section builds");
        let id = section.id.as_str().to_string();
        m.body.sections.push(section);

        let resp = get_section(&m, &request(&id, None), as_of(), threshold()).expect("resolves");
        let citations = &resp.section.citations;
        // One provenance citation + two guideline-code notes + one qualification
        // note.
        assert_eq!(citations.len(), 4, "the derived citation count");
        // The provenance citation carries the canonical URL + title.
        assert_eq!(
            citations[0].url.as_deref(),
            Some("https://doc.rust-lang.org/cargo/commands/cargo-build"),
            "the first citation is the canonical document"
        );
        assert!(
            citations
                .iter()
                .any(|c| c.note.as_deref() == Some("guideline code: C-CONV")),
            "a guideline-code note is present"
        );
        assert!(
            citations
                .iter()
                .any(|c| c.note.as_deref() == Some("qualification: stable")),
            "a qualification note is present"
        );
    }

    // ----- AC2: a missing / malformed id maps to resource-not-found. -----

    #[test]
    fn unknown_section_maps_to_resource_not_found() {
        let m = manifest();
        let err = get_section(
            &m,
            &request("api-guidelines/does-not-exist", None),
            as_of(),
            threshold(),
        )
        .expect_err("a missing id fails");
        assert!(
            matches!(err, GetSectionError::UnknownSection { .. }),
            "a missing id is UnknownSection"
        );
        let pe = err.protocol_error(Some(&ProtocolVersion::V_2025_11_25));
        assert_eq!(pe.code, ErrorCode::RESOURCE_NOT_FOUND);
        assert_eq!(
            pe.data.expect("data").get("section_id"),
            Some(&"api-guidelines/does-not-exist".into())
        );
        // The version-aware upgrade: a missing section is INVALID_PARAMS for a
        // peer on 2026-07-28, but RESOURCE_NOT_FOUND for an older peer.
        assert_eq!(
            err.protocol_error(Some(&ProtocolVersion::V_2026_07_28))
                .code,
            ErrorCode::INVALID_PARAMS
        );
    }

    #[test]
    fn malformed_section_id_maps_to_resource_not_found() {
        // A not-well-formed id (a bare string with no `/`) is a typed not-found,
        // not a fabricated section.
        let m = manifest();
        let err = get_section(&m, &request("notanid", None), as_of(), threshold())
            .expect_err("a malformed id fails");
        assert!(
            matches!(err, GetSectionError::UnknownSection { .. }),
            "a malformed id is UnknownSection"
        );
        if let GetSectionError::UnknownSection { id } = &err {
            assert_eq!(id, "notanid");
        }
        let pe = err.protocol_error(Some(&ProtocolVersion::V_2025_11_25));
        assert_eq!(pe.code, ErrorCode::RESOURCE_NOT_FOUND);
    }

    // ----- AC1/AC2: over-cap sections paginate via the [037] cursor. -----

    #[test]
    fn over_cap_section_truncates_and_resumes_via_cursor() {
        // A section whose body exceeds the cap is paginated through the [037]
        // continuation cursor; the concatenated pages equal the full body.
        let full = "x".repeat(100 * 1024);
        let mut m = manifest();
        let big = SectionBuilder::new(
            SourceId::new("cargo").expect("cargo"),
            "Big Doc",
            "big",
            Url::new("https://doc.rust-lang.org/cargo/big").expect("url"),
            full.clone(),
        )
        .build()
        .expect("section builds");
        let id = big.id.as_str().to_string();
        m.body.sections.push(big);

        // First page: truncated, with a continuation cursor.
        let first = get_section(&m, &request(&id, None), as_of(), threshold())
            .expect("first page resolves");
        assert!(first.truncated, "the first page is truncated");
        let cursor = first
            .next_cursor
            .clone()
            .expect("a continuation cursor is carried");
        assert!(
            first.section.body.len() <= MAX_RESPONSE_BYTES,
            "a page fits the cap"
        );

        // Resume until the body is fully drained, and assert the pages
        // concatenate to the full body.
        let mut collected = first.section.body.clone();
        let mut cursor = Some(cursor);
        let mut pages = 1;
        while let Some(token) = cursor.clone() {
            let next = get_section(&m, &request(&id, Some(token)), as_of(), threshold())
                .expect("the page resolves");
            collected.push_str(&next.section.body);
            pages += 1;
            cursor = next.next_cursor;
        }
        assert_eq!(
            collected, full,
            "the concatenated pages equal the full body"
        );
        assert!(pages > 1, "the body was paginated across pages");
    }

    #[test]
    fn foreign_cursor_is_rejected() {
        // A continuation cursor minted for one section cannot be reused for
        // another (the cursor binds the section via the [037] filter).
        let big_body = "x".repeat(100 * 1024);
        let mut m = manifest();
        let big = SectionBuilder::new(
            SourceId::new("cargo").expect("cargo"),
            "Big Doc",
            "big",
            Url::new("https://doc.rust-lang.org/cargo/big").expect("url"),
            big_body,
        )
        .build()
        .expect("section builds");
        let big_id = big.id.as_str().to_string();
        m.body.sections.push(big);

        // A small, unrelated section.
        let small = SectionBuilder::new(
            SourceId::new("rustc").expect("rustc"),
            "Small Doc",
            "small",
            Url::new("https://doc.rust-lang.org/rustc/small").expect("url"),
            "tiny",
        )
        .build()
        .expect("section builds");
        let small_id = small.id.as_str().to_string();
        m.body.sections.push(small);

        // Mint a cursor from a page of "big", then try to resume "small" with it.
        let first = get_section(&m, &request(&big_id, None), as_of(), threshold())
            .expect("first page resolves");
        let cursor = first.next_cursor.clone().expect("a cursor");
        let err = get_section(&m, &request(&small_id, Some(cursor)), as_of(), threshold())
            .expect_err("a foreign cursor is rejected");
        assert!(
            matches!(err, GetSectionError::Cursor(CursorError::Mismatch { .. })),
            "a foreign cursor is a Mismatch"
        );
    }

    #[test]
    fn malformed_cursor_is_rejected() {
        let m = manifest();
        let err = get_section(
            &m,
            &request(
                "cargo/commands%2Fcargo-build",
                Some("not-a-cursor".to_string()),
            ),
            as_of(),
            threshold(),
        )
        .expect_err("a malformed cursor fails");
        assert!(
            matches!(err, GetSectionError::Cursor(CursorError::Malformed(_))),
            "a malformed cursor is Malformed"
        );
        assert_eq!(
            err.protocol_error(Some(&ProtocolVersion::V_2025_11_25))
                .code,
            ErrorCode::INVALID_PARAMS
        );
    }

    // ----- The `CallToolResult` assembly + cap. -----

    #[test]
    fn result_fits_the_response_cap_for_a_section() {
        let m = manifest();
        let resp = get_section(
            &m,
            &request("cargo/commands%2Fcargo-build", None),
            as_of(),
            threshold(),
        )
        .expect("resolves");
        let call = result(&resp).expect("the response fits the cap");
        assert!(!call.content.is_empty(), "a text block is rendered");
        assert!(
            call.structured_content.is_some(),
            "structured content is present"
        );
        assert_eq!(
            call.structured_content.as_ref().unwrap()["section"]["id"],
            "cargo/commands%2Fcargo-build"
        );
    }

    #[test]
    fn result_rejects_an_over_cap_response() {
        // A response whose body alone exceeds the cap is rejected defensively.
        let mut resp = get_section(
            &manifest(),
            &request("cargo/commands%2Fcargo-build", None),
            as_of(),
            threshold(),
        )
        .expect("resolves");
        resp.section.body = "x".repeat(500 * 1024);
        let err = result(&resp).expect_err("an over-cap response is rejected");
        assert!(
            err.to_string().contains("response cap"),
            "the error names the response cap"
        );
    }

    // ----- Leak-free error messages. -----

    #[test]
    fn not_found_message_is_actionable_and_leak_free() {
        let err = GetSectionError::UnknownSection {
            id: "cargo/commands%2Fcargo-build".to_string(),
        };
        let msg = err.message();
        assert!(
            msg.contains("not in this snapshot"),
            "the message is actionable: {msg}"
        );
        // No internal paths or stack traces.
        assert!(!msg.contains("src/"));
        assert!(!msg.contains("at "));
        assert!(!msg.contains("/home/"));
    }
}
