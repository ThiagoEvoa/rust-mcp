//! The `search_docs` tool surface.
//!
//! The [048] schema defines the `search_docs` request/response shapes and their
//! JSON schemas; this module is the **read surface** that answers a `search_docs`
//! request against a snapshot already assembled and activated by earlier tasks —
//! exactly the "wire it in" follow-on the [048] schema module and the
//! `list_sources` module both anticipate. It consumes the completed retrieval
//! seams rather than re-inventing them:
//!
//! * **Query + limit validation** — the request's `query`/`limit` are validated
//!   and the limit clamped by the [038] [`SearchRequest`]; a blank or
//!   oversized query is a typed [`SearchError`], never an empty result.
//! * **Source filtering** — the request's optional `source` restricts the result
//!   set by the [035] [`SourceFilter`]; an unknown source is a typed
//!   [`SearchError`].
//! * **Ranking** — the [034]/[035] [`SectionIndex`] ranks the validated, filtered
//!   query over the active snapshot's sections (BM25 plus exact-match boosts).
//! * **Bounded, cursor-paginated page** — the [037] [`paginate_items`] layer
//!   applies the byte cap, the count limit, and the continuation-cursor contract
//!   to the ranked stream, so a result is never truncated implicitly.
//! * **Excerpt + citation assembly** — each hit is resolved back to its full
//!   [`Section`] via [036] reads so the response can carry a bounded excerpt,
//!   the section id, provenance/compatibility citations, and a `next_cursor`.
//! * **Error mapping** — the [048] [`ProtocolError`] mapping turns a retrieval
//!   or cursor failure into an MCP `error` data object, leaking no internal path
//!   or stack-trace.
//!
//! The module is wired into the `mcp` namespace and the server's `call_tool`
//! dispatch now consumes its `search` resolution and `result` assembly through the
//! [048] `ProtocolError` mapping; the tests below exercise every branch.
//!
//! ## Snapshot-identity and no-network invariant
//!
//! Every value in a `search_docs` response is derived from the active snapshot's
//! sections (via the index) and the request — no network, no live lookup, no
//! filesystem. The response echoes the snapshot id it was read from, so a client
//! can detect drift and re-search; a cursor is bound to that same snapshot id,
//! source filter, and operation, so it cannot be replayed across a refresh or a
//! different request (the [037] cursor contract).
//!
//! ## No internal detail leaks
//!
//! Failures are mapped through the [048] [`ProtocolError`] mapping (delegating to
//! the [038] [`SearchError`] and the [037] [`CursorError`] mappings), so the
//! client never sees an internal path, stack-trace, or a message that does not
//! name the problem. The response text is a compact, structured summary, not a
//! stack trace.

use rmcp::model::{CallToolResult, ContentBlock};

use crate::error::AppError;
use crate::mcp::protocol_error::{ErrorClass, ProtocolError};
use crate::mcp::schema::{CitationView, SearchDocsRequest, SearchDocsResponse, SearchHitView};
use crate::model::{Section, SnapshotManifest};
use crate::pagination::{CursorError, FilterKey, Item, MAX_RESPONSE_BYTES, Operation};
use crate::search::{SearchError, SearchRequest, SectionIndex, SourceFilter};
use serde_json::Value;

/// The `search_docs` tool name, matching the [048] schema's `name` field.
pub const TOOL_NAME: &str = "search_docs";

/// The maximum number of characters a single hit's excerpt may carry.
///
/// Bounding the excerpt — rather than echoing a section's whole body — keeps the
/// `search_docs` response compact and within the [`MAX_RESPONSE_BYTES`] cap even
/// at the maximum hit limit (twenty), so the response-assembled cap check in
/// [`result`] is a defensive safety net rather than a routine failure.
const EXCERPT_MAX_CHARS: usize = 240;

/// The maximum number of citations a single hit carries.
///
/// A section's provenance (canonical URL, guideline codes, command keys, lint
/// ids, qualifications) is surfaced as citations; capping the count keeps the
/// response bounded against a pathological section with an unbounded list.
const MAX_CITATIONS_PER_HIT: usize = 12;

/// The ellipsis marker appended to a windowed excerpt that is a strict subset of
/// the section's body. A non-breaking-free, single-codepoint marker keeps the
/// excerpt a compact, deterministic string.
const EXCERPT_ELLIPSIS: &str = "…";

/// The error a `search_docs` request can surface, mapped through the [048]
/// [`ProtocolError`] mapping (see [`SearchDocsError::code`]/[`SearchDocsError::message`]).
///
/// The two variants mirror the two failure families the request can hit: a
/// query/source/limit validation failure ([038]/[035] [`SearchError`]) and a
/// malformed or mismatched continuation cursor ([037] [`CursorError`]). Each
/// variant delegates to its inner error's [`ProtocolError`] mapping, so the
/// sanitized message, error code, and optional data are produced by the [048]
/// seam — no path or stack-trace leaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchDocsError {
    /// A query, source, or limit validation failure ([038]/[035]).
    Query(SearchError),
    /// A malformed or mismatched continuation cursor ([037]).
    Cursor(CursorError),
}

impl From<SearchError> for SearchDocsError {
    fn from(error: SearchError) -> Self {
        Self::Query(error)
    }
}

impl From<CursorError> for SearchDocsError {
    fn from(error: CursorError) -> Self {
        Self::Cursor(error)
    }
}

impl ProtocolError for SearchDocsError {
    /// The logical error class, delegating to the inner error's class.
    fn error_class(&self) -> ErrorClass {
        match self {
            Self::Query(error) => error.error_class(),
            Self::Cursor(error) => error.error_class(),
        }
    }

    /// The sanitized, actionable message, delegating to the inner error.
    fn message(&self) -> String {
        match self {
            Self::Query(error) => error.message(),
            Self::Cursor(error) => error.message(),
        }
    }

    /// Any structured data attached to the error, delegating to the inner error.
    fn data(&self) -> Option<Value> {
        match self {
            Self::Query(error) => error.data(),
            Self::Cursor(error) => error.data(),
        }
    }
}

/// Answer a `search_docs` request against the active snapshot.
///
/// This composes the completed retrieval seams in dependency order:
///
/// 1. **Validate** the request's `query`/`limit` via the [038] [`SearchRequest`]
///    (a blank or oversized query is a typed [`SearchError`], never an empty
///    result), and **resolve** the optional `source` filter via the [035]
///    [`SourceFilter`] (an unknown source is a typed [`SearchError`]).
/// 2. **Rank** the validated, filtered query over the active snapshot's sections
///    with the [034]/[035] [`SectionIndex`] (BM25 plus exact-match boosts).
/// 3. **Paginate** the full ranked stream with the [037] [`crate::pagination::paginate_items`]
///    layer — applying the byte cap, the count limit, and the continuation-cursor
///    contract — so a result is never truncated implicitly.
/// 4. **Assemble** each returned hit into a [`SearchHitView`] by resolving its
///    section back through [036] reads (bounded excerpt + provenance/
///    compatibility citations), and attach the [038] "useful empty" guidance
///    when the search matches nothing.
///
/// The returned [`SearchDocsResponse`] echoes the snapshot id it was read from
/// and carries a `next_cursor` only when the page is truncated (more results
/// exist beyond the cap or the limit). An empty match is a *successful* search
/// with zero hits and the "useful empty" next-steps, never an error.
///
/// # Errors
///
/// Returns [`SearchDocsError::Query`] for a blank/oversized query or an unknown
/// source, and [`SearchDocsError::Cursor`] for a malformed or mismatched
/// continuation cursor.
pub fn search(
    manifest: &SnapshotManifest,
    request: &SearchDocsRequest,
) -> Result<SearchDocsResponse, SearchDocsError> {
    // [038] Validate the query and clamp the limit. A blank/oversized query is a
    // typed error, never an empty result; the limit is clamped to the public
    // window ([0, MAX_HIT_LIMIT]) by the shared contract.
    let req = SearchRequest::with_limit(&request.query, request.limit.map(|l| l as usize))
        .map_err(SearchDocsError::Query)?;

    // [035] Resolve the optional source filter. An unknown source is a typed
    // error; the matching `FilterKey` binds the [037] cursor to the same source
    // so a cursor minted for source `X` cannot be replayed against a different
    // source or tool.
    let (filter, filter_key) = match &request.source {
        None => (SourceFilter::all(), FilterKey::all()),
        // `SourceFilter::source` validates the id (a typed error on an unknown
        // source); the matching `FilterKey` binds the [037] cursor to the same
        // source. `FilterKey::source` is infallible, so it carries no `?`.
        Some(source) => (SourceFilter::source(source)?, FilterKey::source(source)),
    };

    // [034] Build the in-memory inverted index over the active snapshot's
    // sections. Indexing is pure and deterministic.
    let index = SectionIndex::build(manifest);

    // [034]/[035] The full ranked, source-filtered result set — used both for the
    // zero-hit decision and as the cursor-paginated stream ([037]).
    let ranked = index.search_in(req.query(), &filter);
    let no_match = ranked.is_empty();

    // [037] Cap-bound, cursor-bound page over the ranked stream. The cursor's
    // filter key must match this request's source filter (verified by the layer).
    let items: Vec<Item> = ranked.into_iter().map(Item::Hit).collect();
    let page = crate::pagination::paginate_items(
        Operation::Search,
        &filter_key,
        &manifest.body.snapshot_id,
        &items,
        Some(req.limit()),
        request.cursor.as_deref(),
    )?;

    // Assemble each returned hit into a view by resolving its section back
    // through the index (a hit always resolves — it was minted from this index —
    // but the mapping stays total and panic-free via `filter_map`).
    let hits = page
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Hit(hit) => hit_view(&index, hit, req.query()),
            _ => None,
        })
        .collect();

    // [038] The "useful empty" guidance, reused verbatim, only for a genuine
    // zero-hit search: a zero-limit or final page is a *match* (more results may
    // exist), not a no-match, so it carries no next-steps.
    let next_steps = if no_match {
        // The [038] useful-empty guidance is `Vec<&'static str>`; the response
        // carries owned `String`s, so map (never fabricate) into that shape.
        index
            .run_in(&req, &filter)
            .next_steps
            .into_iter()
            .map(String::from)
            .collect()
    } else {
        Vec::new()
    };

    Ok(SearchDocsResponse {
        snapshot_id: manifest.body.snapshot_id.as_str().to_string(),
        hits,
        limit: req.limit() as u32,
        truncated: page.truncated,
        next_cursor: page.next_cursor,
        next_steps,
    })
}

/// Assemble the `CallToolResult` for a `search_docs` response.
///
/// The response is carried in **both** forms a client can consume:
///
/// * **Structured** — the full [`SearchDocsResponse`] serialized to JSON, so a
///   client can read the hits, the snapshot id, the effective limit, the
///   `truncated`/`next_cursor` pagination state, and the `next_steps` guidance
///   programmatically.
/// * **Text** — a compact human-readable summary of the same content, so a client
///   that prefers a text surface gets the headline result (the snapshot, the hit
///   count, the top hits with their excerpts and citations, and any next-steps).
///
/// The text summary is the primary content block and the structured payload rides
/// in `structuredContent`, mirroring [`list_sources`](crate::mcp::list_sources). The
/// response-assembled byte size is then enforced
/// against the [`MAX_RESPONSE_BYTES`] cap: the excerpt and citation bounds in
/// [`search`] keep a real response within the cap, so this is a defensive safety
/// net against a pathological snapshot, surfaced as a typed [`AppError`] (an
/// operational start failure — the response could not be produced within the
/// advertised bound).
///
/// # Errors
///
/// Returns [`AppError::RuntimeStart`] when the response exceeds the response cap
/// or serialization fails.
pub fn result(resp: &SearchDocsResponse) -> Result<CallToolResult, AppError> {
    let structured = serde_json::to_value(resp).map_err(|e| {
        AppError::runtime_start("encode search_docs structured output", Some(Box::new(e)))
    })?;
    let text = render_text(resp);
    // The cap is enforced on *both* rendered forms: the structured body carries
    // every field, so its serialized byte length is the binding measurement, while
    // the text rendering (a readable subset of the same data) guards the "both
    // within the cap" contract.
    let structured_bytes = serde_json::to_string(&structured)
        .map_err(|e| {
            AppError::runtime_start("measure search_docs structured output", Some(Box::new(e)))
        })?
        .len();
    let byte_len = structured_bytes.max(text.len());
    if byte_len > MAX_RESPONSE_BYTES {
        return Err(AppError::runtime_start(
            format!(
                "search_docs response ({byte_len} bytes) exceeds the {MAX_RESPONSE_BYTES}-byte response cap"
            ),
            None,
        ));
    }
    // The text summary is the primary content block; the full structured payload
    // rides in `structuredContent` for structured-capable clients. Both forms
    // carry the same data.
    let mut call = CallToolResult::success(vec![ContentBlock::text(text)]);
    call.structured_content = Some(structured);
    Ok(call)
}

/// Render a compact, structured text summary of a `search_docs` response.
///
/// Mirrors the `list_sources` rendering: a snapshot header (with the hit count,
/// the effective limit, and a truncation notice when a `next_cursor` is
/// present), then one block per hit (its title, section id, score, bounded
/// excerpt, and citations), then the next-steps guidance for a zero-hit search.
pub fn render_text(resp: &SearchDocsResponse) -> String {
    let mut lines = Vec::with_capacity(resp.hits.len() + 3);
    lines.push(format!(
        "search_docs — snapshot {}, {} hit(s), limit {}",
        resp.snapshot_id,
        resp.hits.len(),
        resp.limit,
    ));
    if resp.truncated {
        lines.push(format!(
            "  (truncated; resume with next_cursor: {})",
            resp.next_cursor.as_deref().unwrap_or("?")
        ));
    }
    for (i, hit) in resp.hits.iter().enumerate() {
        lines.push(format!(
            "  [{}] {} — {} ({:.3})",
            i + 1,
            hit.title,
            hit.section_id,
            hit.score,
        ));
        if !hit.excerpt.is_empty() {
            lines.push(format!("      {}", hit.excerpt));
        }
        for citation in &hit.citations {
            lines.push(render_citation(citation));
        }
    }
    for step in &resp.next_steps {
        lines.push(format!("  • {step}"));
    }
    lines.join("\n")
}

/// Render one citation as a compact, structured text line.
fn render_citation(citation: &CitationView) -> String {
    let mut parts = vec![format!("↳ {}:", citation.source)];
    if let Some(note) = &citation.note {
        parts.push(note.clone());
    }
    if let Some(title) = &citation.title {
        parts.push(format!("({title})"));
    }
    if let Some(url) = &citation.url {
        parts.push(url.to_string());
    }
    parts.join(" ")
}

/// Assemble a [`SearchHitView`] for a hit by resolving its section back through
/// the index.
///
/// Returns `None` (and the hit is skipped) only if the section cannot be
/// resolved — which is unreachable in practice, since the hit's `section_id` was
/// minted from this very index — but keeping the mapping total makes `search`
/// panic-free.
fn hit_view(index: &SectionIndex, hit: &crate::search::Hit, query: &str) -> Option<SearchHitView> {
    let section = index.get(&hit.section_id)?;
    Some(SearchHitView {
        section_id: hit.section_id.as_str().to_string(),
        source: section.source_id.as_str().to_string(),
        title: section.doc_title.clone(),
        excerpt: excerpt(&section.body, query),
        score: hit.score,
        citations: build_citations(section),
    })
}

/// Build a hit's citations from its section's provenance and compatibility
/// metadata, capping the count at [`MAX_CITATIONS_PER_HIT`].
///
/// Each citation carries the hit's `section_id` and `source` (redundant but the
/// [`CitationView`] contract requires them), plus:
///
/// * the canonical URL (provenance), as a citation with a `url`/`title`/note;
/// * each applicable API-guideline code, command key, and Clippy lint id (the
///   "applicable guideline codes/command keys/lint IDs" the plan keeps on a
///   Source);
/// * each stability/version qualification (the "compatibility metadata").
///
/// Citing these (rather than only the URL) keeps the *applicable guidance* and
/// *compatibility metadata* explicit per hit, as the plan requires, while the
/// cap keeps the response bounded.
fn build_citations(section: &Section) -> Vec<CitationView> {
    let section_id = section.id.as_str().to_string();
    let source = section.source_id.as_str().to_string();
    let mut out = Vec::with_capacity(1 + 3 + section.qualifications.len());

    // Provenance: the canonical upstream document.
    out.push(CitationView {
        section_id: section_id.clone(),
        source: source.clone(),
        url: Some(section.canonical_url.as_str().to_string()),
        title: Some(section.doc_title.clone()),
        note: Some("canonical upstream document".to_string()),
    });

    // Applicable guideline codes / command keys / lint ids.
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
    // Compatibility metadata: stability/version qualifications.
    for qualification in &section.qualifications {
        out.push(note_citation(
            &section_id,
            &source,
            format!("qualification: {qualification}"),
        ));
    }

    out.truncate(MAX_CITATIONS_PER_HIT);
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

/// Produce a bounded excerpt of `body` centered on the first query term.
///
/// The excerpt is at most [`EXCERPT_MAX_CHARS`] characters wide, centered on the
/// first case-insensitive occurrence of any query term (or a leading window when
/// no term occurs), with runs of whitespace collapsed to single spaces and an
/// [`EXCERPT_ELLIPSIS`] marker added at each end the window is a strict subset of
/// the body. A `Vec<char>` cursor keeps the scan total and panic-free over
/// multi-byte UTF-8 input.
fn excerpt(body: &str, query: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let total = chars.len();
    if total == 0 {
        return String::new();
    }

    let center = first_term_offset(&chars, query);
    let half = EXCERPT_MAX_CHARS / 2;
    let start = center.saturating_sub(half);
    let end = (start + EXCERPT_MAX_CHARS).min(total);
    let window: String = chars[start..end].iter().collect();
    let collapsed = collapse_ws(&window);

    let mut out = String::new();
    if !collapsed.is_empty() {
        if start > 0 {
            out.push_str(EXCERPT_ELLIPSIS);
        }
        out.push_str(&collapsed);
        if end < total {
            out.push_str(EXCERPT_ELLIPSIS);
        }
    }
    out
}

/// The char offset of the first query term's first (case-insensitive) occurrence
/// in `body`, or `0` when no term occurs (a leading window).
fn first_term_offset(body: &[char], query: &str) -> usize {
    let mut best: Option<usize> = None;
    for token in crate::search::tokenize(query) {
        let needle: Vec<char> = token.as_str().to_lowercase().chars().collect();
        if needle.is_empty() {
            continue;
        }
        if let Some(offset) = find_first(body, &needle) {
            best = Some(match best {
                Some(existing) => existing.min(offset),
                None => offset,
            });
        }
    }
    best.unwrap_or(0)
}

/// The char offset of `needle`'s first case-insensitive occurrence in `body`, or
/// `None`. A char-level (not byte-level) scan keeps multi-byte input safe.
fn find_first(body: &[char], needle: &[char]) -> Option<usize> {
    let needle_len = needle.len();
    if needle_len == 0 || body.len() < needle_len {
        return None;
    }
    body.windows(needle_len).position(|window| {
        window
            .iter()
            .zip(needle.iter())
            .all(|(body_char, needle_char)| body_char.to_lowercase().eq(needle_char.to_lowercase()))
    })
}

/// Collapse runs of whitespace to single spaces and trim the ends.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws && !out.is_empty() {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundled::manifest;
    use crate::mcp::protocol_error;
    use crate::mcp::schema::search_docs_tool;
    use crate::pagination::{Cursor, CursorError, FilterKey, MismatchField, Operation};
    use rmcp::model::ProtocolVersion;

    /// The [048] input schema documents `query`, `source`, `limit`, and `cursor`
    /// (AC1: the schema documents the request's behaviour).
    #[test]
    fn input_schema_documents_query_source_limit_cursor() {
        let schema = search_docs_tool().input_schema;
        let props = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("input schema has `properties`");
        for field in ["query", "source", "limit", "cursor"] {
            assert!(
                props.contains_key(field),
                "input schema is missing the `{field}` field"
            );
        }
        // `query` is required; the others are optional (no `required` entry).
        let required: Vec<&str> = schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .map(|arr| arr.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default();
        assert_eq!(required, vec!["query"]);
    }

    /// The [048] output schema documents the response envelope fields a client
    /// reads (AC1).
    #[test]
    fn output_schema_documents_response_envelope() {
        let schema = search_docs_tool()
            .output_schema
            .expect("search_docs advertises an output schema");
        let props = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("output schema has `properties`");
        for field in [
            "snapshot_id",
            "hits",
            "limit",
            "truncated",
            "next_cursor",
            "next_steps",
        ] {
            assert!(
                props.contains_key(field),
                "output schema is missing the `{field}` field"
            );
        }
        // Each hit's shape is referenced (not inlined) from `hits.items` into the
        // schema's `$defs` (schemars keeps named structs as references). Resolve
        // the `items` node — a `$ref`, or an inline object — to its property bag.
        let items = props
            .get("hits")
            .and_then(|h| h.get("items"))
            .expect("output schema documents a `hits` array");
        let hit_props =
            if let Some(ref_str) = items.get("$ref").and_then(serde_json::Value::as_str) {
                let name = ref_str
                    .strip_prefix("#/$defs/")
                    .or_else(|| ref_str.strip_prefix("#/definitions/"))
                    .unwrap_or(ref_str);
                schema
                    .get("$defs")
                    .or_else(|| schema.get("definitions"))
                    .and_then(|defs| defs.get(name))
            } else {
                Some(items)
            }
            .and_then(|shape| {
                shape
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
            })
            .expect("output schema documents the hit shape");
        for field in [
            "section_id",
            "source",
            "title",
            "excerpt",
            "score",
            "citations",
        ] {
            assert!(
                hit_props.contains_key(field),
                "hit schema is missing the `{field}` field"
            );
        }
    }

    /// AC1/AC2: a blank query is a typed validation error (the [038] contract),
    /// never a silent empty result.
    #[test]
    fn blank_query_is_a_validated_error() {
        let m = manifest();
        let req = SearchDocsRequest {
            query: "    \n\t ".to_string(),
            source: None,
            limit: None,
            cursor: None,
        };
        let err = search(&m, &req).unwrap_err();
        assert_eq!(err, SearchDocsError::Query(SearchError::BlankQuery));
    }

    /// AC1/AC2: an oversized query is a typed validation error (the [038]
    /// `MAX_QUERY_BYTES` contract).
    #[test]
    fn oversized_query_is_a_validated_error() {
        use crate::pagination::MAX_QUERY_BYTES;
        let m = manifest();
        let req = SearchDocsRequest {
            query: "q".repeat(MAX_QUERY_BYTES + 1),
            source: None,
            limit: None,
            cursor: None,
        };
        let err = search(&m, &req).unwrap_err();
        match err {
            SearchDocsError::Query(SearchError::QueryTooLarge { limit, .. }) => {
                assert_eq!(limit, MAX_QUERY_BYTES);
            }
            other => panic!("expected QueryTooLarge, got {other:?}"),
        }
    }

    /// AC1/AC2: an unknown source is a typed validation error (the [035] source
    /// contract), not an empty result.
    #[test]
    fn unknown_source_is_a_validated_error() {
        let m = manifest();
        let req = SearchDocsRequest {
            query: "cargo build".to_string(),
            source: Some("frobnicate".to_string()),
            limit: None,
            cursor: None,
        };
        let err = search(&m, &req).unwrap_err();
        match err {
            SearchDocsError::Query(SearchError::UnsupportedSource(src)) => {
                assert_eq!(src, "frobnicate");
            }
            other => panic!("expected UnsupportedSource, got {other:?}"),
        }
    }

    /// AC2: a hit retains its excerpt, section id, provenance citation, and
    /// compatibility metadata (command keys + qualifications surfaced as
    /// citations), and the response echoes the snapshot id.
    #[test]
    fn hit_retains_excerpt_id_citations_and_metadata() {
        let m = manifest();
        let req = SearchDocsRequest {
            query: "cargo build".to_string(),
            source: Some("cargo".to_string()),
            limit: Some(5),
            cursor: None,
        };
        let resp = search(&m, &req).expect("cargo build search succeeds");
        assert_eq!(resp.hits.len(), 1);
        let hit = &resp.hits[0];
        // ID + source + title.
        assert!(hit.section_id.starts_with("cargo/"));
        assert_eq!(hit.source, "cargo");
        assert_eq!(hit.title, "The Cargo Book");
        // A bounded, non-empty excerpt.
        assert!(!hit.excerpt.is_empty());
        assert!(hit.excerpt.chars().count() <= EXCERPT_MAX_CHARS + 2);
        // Provenance (canonical URL) + compatibility metadata (command key +
        // qualification) are surfaced as citations.
        assert!(
            hit.citations.iter().any(|c| c
                .url
                .as_deref()
                .is_some_and(|u| u.starts_with("https://doc.rust-lang.org/cargo/"))),
            "a canonical-URL citation is present: {:?}",
            hit.citations
        );
        assert!(
            hit.citations
                .iter()
                .any(|c| c.note.as_deref() == Some("command key: cargo build")),
            "a command-key citation is present: {:?}",
            hit.citations
        );
        assert!(
            hit.citations.iter().any(|c| c
                .note
                .as_deref()
                .map(|n| n.starts_with("qualification: "))
                .unwrap_or(false)),
            "a qualification (compatibility) citation is present: {:?}",
            hit.citations
        );
        // The response echoes the snapshot id it was read from.
        assert_eq!(resp.snapshot_id, m.body.snapshot_id.as_str());
        // No truncation for a single-hit page → no next cursor.
        assert!(!resp.truncated);
        assert!(resp.next_cursor.is_none());
        assert!(resp.next_steps.is_empty());
    }

    /// AC2: a truncated page carries a `next_cursor` (the [037] pagination
    /// contract) and the effective (clamped) limit.
    #[test]
    fn truncated_page_carries_a_next_cursor() {
        let m = manifest();
        // "the" matches many sections; a limit of one truncates.
        let req = SearchDocsRequest {
            query: "the".to_string(),
            source: None,
            limit: Some(1),
            cursor: None,
        };
        let resp = search(&m, &req).expect("search succeeds");
        assert!(resp.hits.len() == 1, "a one-item page returns one hit");
        assert!(resp.truncated, "a truncated page reports truncation");
        assert!(
            resp.next_cursor.is_some(),
            "a truncated page carries a next cursor"
        );
        assert_eq!(resp.limit, 1);
    }

    /// AC2: a `next_cursor` resumes the page (the [037] cursor contract) and
    /// yields the next hit.
    #[test]
    fn cursor_resumes_the_page() {
        let m = manifest();
        let first = search(
            &m,
            &SearchDocsRequest {
                query: "the".to_string(),
                source: None,
                limit: Some(1),
                cursor: None,
            },
        )
        .expect("first page");
        let cursor = first
            .next_cursor
            .clone()
            .expect("a truncated first page carries a cursor");
        assert_eq!(first.hits.len(), 1);
        let first_id = first.hits[0].section_id.clone();

        let second = search(
            &m,
            &SearchDocsRequest {
                query: "the".to_string(),
                source: None,
                limit: Some(1),
                cursor: Some(cursor),
            },
        )
        .expect("resumed page");
        assert_eq!(second.hits.len(), 1, "the resumed page returns one hit");
        // The resumed hit is a *different* section than the first page's.
        assert_ne!(
            second.hits[0].section_id, first_id,
            "the resumed page advances past the first hit"
        );
        // Both pages were read from the same snapshot.
        assert_eq!(first.snapshot_id, second.snapshot_id);
    }

    /// AC2: a cursor minted for a different operation cannot be replayed against
    /// `search_docs` (the [037] foreign-cursor contract).
    #[test]
    fn foreign_cursor_is_rejected() {
        let m = manifest();
        // A cursor minted for a *different* operation (here `Checklist`, not
        // `Search`) cannot be replayed against `search_docs`: the [037] cursor
        // contract rejects it as an operation mismatch.
        let foreign = Cursor::mint(
            Operation::Checklist,
            &FilterKey::all(),
            &m.body.snapshot_id,
            5,
        );
        let req = SearchDocsRequest {
            query: "the".to_string(),
            source: None,
            limit: Some(1),
            cursor: Some(foreign.to_token()),
        };
        let err = search(&m, &req).unwrap_err();
        // The mismatch names the field and both values: the call expected the
        // `Search` operation but the cursor carried `Checklist`.
        assert_eq!(
            err,
            SearchDocsError::Cursor(CursorError::Mismatch {
                field: MismatchField::Operation,
                expected: "Search".to_string(),
                found: "Checklist".to_string(),
            })
        );
    }

    /// AC2: a malformed cursor is a typed error (the [037] malformed-cursor
    /// contract), never a panic.
    #[test]
    fn malformed_cursor_is_a_typed_error() {
        let m = manifest();
        let req = SearchDocsRequest {
            query: "the".to_string(),
            source: None,
            limit: Some(1),
            cursor: Some("not-a-valid-cursor".to_string()),
        };
        let err = search(&m, &req).unwrap_err();
        // A non-cursor token is a typed [`CursorError::Malformed`] (the [037]
        // malformed-cursor contract), never a panic. The inner reason echoes the
        // offending token, so match the variant rather than pin the message.
        assert!(matches!(
            err,
            SearchDocsError::Cursor(CursorError::Malformed(_))
        ));
    }

    /// AC2: a query that matches nothing is a *successful* search with zero hits,
    /// the [038] "useful empty" next-steps, and no next cursor — not an error.
    #[test]
    fn no_match_is_a_useful_empty_result() {
        let m = manifest();
        let req = SearchDocsRequest {
            query: "zzz-no-such-term-xyzzy".to_string(),
            source: None,
            limit: None,
            cursor: None,
        };
        let resp = search(&m, &req).expect("a no-match search is not an error");
        assert!(resp.hits.is_empty(), "a no-match search returns no hits");
        assert!(
            resp.next_cursor.is_none(),
            "a no-match search carries no next cursor"
        );
        assert!(!resp.truncated, "a no-match search is not truncated");
        assert!(
            !resp.next_steps.is_empty(),
            "a no-match search carries the [038] useful-empty next-steps"
        );
        assert!(
            resp.next_steps
                .iter()
                .any(|s| s.contains("broader") || s.contains("fewer terms")),
            "the useful-empty guidance suggests adjusting the query: {:?}",
            resp.next_steps
        );
        assert_eq!(resp.snapshot_id, m.body.snapshot_id.as_str());
    }

    /// AC2: a zero-limit query that *does* match is a match, not a no-match — it
    /// returns zero hits but no "useful empty" next-steps (more results exist).
    #[test]
    fn zero_limit_is_a_match_not_a_no_match() {
        let m = manifest();
        let req = SearchDocsRequest {
            query: "the".to_string(),
            source: None,
            limit: Some(0),
            cursor: None,
        };
        let resp = search(&m, &req).expect("search succeeds");
        assert!(resp.hits.is_empty(), "a zero-limit page returns no hits");
        assert!(
            resp.next_steps.is_empty(),
            "a zero-limit match carries no next-steps: it is a match, not a no-match"
        );
        assert!(resp.truncated, "more results exist beyond the zero limit");
        assert!(resp.next_cursor.is_some());
    }

    /// AC2: a no-match result is a *successful* empty page. On the wire it omits
    /// the `next_cursor` (the [038] "empty → null cursor" contract, gated by
    /// `skip_serializing_if` on `None`) while still carrying the useful-empty
    /// `next_steps` guidance, which a no-match always populates.
    #[test]
    fn no_match_omits_next_cursor_on_the_wire() {
        let m = manifest();
        let resp = search(
            &m,
            &SearchDocsRequest {
                query: "zzz-no-such-term-xyzzy".to_string(),
                source: None,
                limit: None,
                cursor: None,
            },
        )
        .expect("a no-match search is not an error");
        let json = serde_json::to_value(&resp).expect("serialize the response");
        assert_eq!(
            json.get("hits").and_then(serde_json::Value::as_array),
            Some(&Vec::<_>::new())
        );
        // `next_cursor` is `skip_serializing_if`-gated on `None`, so a no-match
        // (a null cursor) omits it entirely on the wire.
        assert!(json.get("next_cursor").is_none(), "next_cursor is absent");
        // A no-match still carries the [038] useful-empty `next_steps`, so it is
        // present and non-empty on the wire (the guidance is not omitted).
        assert!(
            json.get("next_steps")
                .and_then(serde_json::Value::as_array)
                .map(|steps| !steps.is_empty())
                .unwrap_or(false),
            "a no-match carries the useful-empty next_steps on the wire: {json:?}"
        );
    }

    /// AC2: a full page's assembled response stays within the response cap.
    #[test]
    fn result_fits_the_response_cap() {
        let m = manifest();
        let resp = search(
            &m,
            &SearchDocsRequest {
                query: "the".to_string(),
                source: None,
                limit: Some(20),
                cursor: None,
            },
        )
        .expect("search succeeds");
        let call = result(&resp).expect("the response fits the cap");
        let bytes = call
            .structured_content
            .as_ref()
            .map(serde_json::Value::to_string)
            .map(|s| s.len());
        assert!(
            bytes.map(|n| n <= MAX_RESPONSE_BYTES).unwrap_or(true),
            "the structured response fits the cap: {bytes:?} bytes"
        );
    }

    /// AC2: a response that would exceed the cap is rejected (the cap is
    /// enforced on the assembled response, not silently truncated).
    #[test]
    fn result_rejects_an_over_cap_response() {
        let mut hits = Vec::new();
        for i in 0..40 {
            hits.push(SearchHitView {
                section_id: format!("cargo#chapter-{i}"),
                source: "cargo".to_string(),
                title: "Cargo Book".to_string(),
                excerpt: "x".repeat(EXCERPT_MAX_CHARS),
                score: 1.0,
                citations: vec![CitationView {
                    section_id: format!("cargo#chapter-{i}"),
                    source: "cargo".to_string(),
                    url: Some("https://doc.rust-lang.org/cargo/".to_string()),
                    title: Some("Cargo Book".to_string()),
                    note: Some("x".repeat(400)),
                }],
            });
        }
        let resp = SearchDocsResponse {
            snapshot_id: "seed".to_string(),
            hits,
            limit: 40,
            truncated: true,
            next_cursor: Some("cursor".to_string()),
            next_steps: vec![],
        };
        let err = result(&resp).unwrap_err();
        assert!(
            err.to_string().contains("response cap"),
            "the cap error names the cap: {err}"
        );
    }

    /// AC2: searching the same request twice yields an identical response
    /// (determinism, no network).
    #[test]
    fn search_is_deterministic() {
        let m = manifest();
        let req = SearchDocsRequest {
            query: "cargo build".to_string(),
            source: None,
            limit: Some(5),
            cursor: None,
        };
        let a = search(&m, &req).expect("search succeeds");
        let b = search(&m, &req).expect("search succeeds");
        assert_eq!(a, b, "the same request yields the same response");
    }

    /// AC2: the [048] error mapping sanitizes a cursor failure and leaks no
    /// internal path or stack-trace.
    #[test]
    fn error_mapping_leaks_no_internal_detail() {
        let cases: Vec<SearchDocsError> = vec![
            SearchDocsError::Cursor(CursorError::Malformed("not-a-valid-cursor".to_string())),
            SearchDocsError::Cursor(CursorError::Mismatch {
                field: MismatchField::Filter,
                expected: "all".to_string(),
                found: "source:frobnicate".to_string(),
            }),
            SearchDocsError::Query(SearchError::UnsupportedSource("frobnicate".to_string())),
            SearchDocsError::Query(SearchError::BlankQuery),
        ];
        for version in [
            Some(ProtocolVersion::V_2025_06_18),
            Some(ProtocolVersion::V_2026_07_28),
            None,
        ] {
            for err in &cases {
                // `map` consumes the error by value; clone each case so every
                // version is checked against the same set.
                let data = protocol_error::map(err.clone(), version.as_ref());
                let text = format!("{}{}", data.message, data.data.unwrap_or_default());
                for forbidden in [
                    "/src/",
                    "at ",
                    "panic",
                    "SearchError",
                    "CursorError",
                    "search_docs.rs",
                ] {
                    assert!(
                        !text.contains(forbidden),
                        "error {err:?} at {version:?} leaks `{forbidden}`: {text}"
                    );
                }
            }
        }
    }

    /// The text surface summarizes the response without a stack trace.
    #[test]
    fn text_surface_summarizes_the_response() {
        let m = manifest();
        let resp = search(
            &m,
            &SearchDocsRequest {
                query: "cargo build".to_string(),
                source: None,
                limit: Some(5),
                cursor: None,
            },
        )
        .expect("search succeeds");
        let text = render_text(&resp);
        assert!(text.contains("search_docs —"), "the text names the tool");
        assert!(text.contains("Cargo Book"), "the text names the top hit");
        assert!(!text.contains("at "), "no stack-trace-like text");
        let call = result(&resp).expect("the response assembles");
        assert!(!call.content.is_empty(), "the call carries content");
    }

    /// The [048] tool name matches this module's `TOOL_NAME`.
    #[test]
    fn tool_name_matches_schema() {
        assert_eq!(search_docs_tool().name, TOOL_NAME);
        assert_eq!(TOOL_NAME, "search_docs");
    }
}
