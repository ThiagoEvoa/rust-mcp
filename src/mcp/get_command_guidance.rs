//! [053][M4] — `get_command_guidance`, as a real `rmcp` `Tool` over the
//! [039]–[043] source-backed command catalog, the [036] section read, and the
//! [037] continuation cursor, projecting to the [048]
//! `GetCommandGuidanceRequest` / `GetCommandGuidanceResponse` types and mapping
//! errors through the [048] `ProtocolError` contract.
//!
//! # What this module does
//!
//! `get_command_guidance` answers one canonical command key (e.g. `cargo test`)
//! with its **documentation** — the catalog entry's metadata (purpose, documented
//! syntax, documented examples, prerequisites, version/stability
//! qualifications) plus the **separately-labeled** maintainer effect notes, and
//! the source-backed sections the entry cites, served from one active snapshot
//! with no network. It is the fifth and final M4 tool and mirrors its four
//! siblings ([049] `list_sources`, [050] `search_docs`, [051] `get_section`,
//! [052] `get_api_checklist`): a stable tool name, a schema-driven tool builder,
//! a core resolution that composes the completed seams, a `CallToolResult`
//! assembler that enforces the response cap, and a `ProtocolError` mapping.
//!
//! The command key is a **documentation selector**, never a shell line
//! (AC2): the catalog validates it at the [007] `CommandKey` boundary, so a
//! shell operator is [`LookupError::InvalidKey`](crate::commands::LookupError)
//! and an unsupported key is [`LookupError::UnknownKey`](crate::commands::LookupError)
//! carrying the catalog's supported keys as discoverable alternatives. A
//! well-formed key whose cited section does not resolve in the snapshot is a
//! corpus-integrity failure, [`LookupError::UnresolvedSection`](crate::commands::LookupError),
//! mapped to `INTERNAL_ERROR` — no section is fabricated.
//!
//! When the command's sections exceed the response cap, they are **paginated
//! through the [037] continuation cursor** (`Operation::Catalog`, filtered by the
//! command key), never silently truncated: a clipped page carries a `next_cursor`
//! and a `truncated` flag, and a resumed call returns the remaining sections in
//! the same citation order.
//!
//! # How it is wired
//!
//! Mirroring the four siblings, this exposes:
//!
//! * [`TOOL_NAME`] — the stable name matching the [048] registered tool;
//! * [`get_command_guidance_tool`] — the [`rmcp::model::Tool`] builder (input /
//!   output schemas from the [048] types, delegated to
//!   [`crate::mcp::schema::get_command_guidance_tool`]);
//! * [`get_command_guidance`] — the core read that resolves a key to a
//!   [`GetCommandGuidanceResponse`], paginating through the [037] cursor;
//! * [`result`] — assembles the final [`rmcp::model::CallToolResult`]
//!   (structured content + human-readable text) and enforces the cap;
//! * [`render_text`] — the compact text summary that rides the content block.
//!
//! The [048] `ServerHandler` seam wires this in a later task, following the
//! same transitional pattern as the siblings: the tool logic and its
//! `CallToolResult` assembly live here; the handler consumes them once it is
//! ready. The module is therefore marked `#![allow(dead_code)]` while the seam
//! is transitional.
//!
//! # Design decisions
//!
//! * **The key is a selector, never a shell line (AC2).** The `command` field is
//!   validated as a [`crate::model::CommandKey`] by the [039]–[043] catalog: a
//!   shell operator is refused at the key boundary (`InvalidKey`), an unsupported
//!   key is refused with the supported keys (`UnknownKey`). No key is ever
//!   interpolated into a process; this module imports no process / filesystem /
//!   network primitive, and a test asserts that by scanning this module's source.
//! * **Upstream excerpts and effect notes stay separate (AC2).** The entry's
//!   documented `syntax` / `examples` / `prerequisites` / `compatibility`
//!   (upstream) are projected verbatim, and the maintainer [`EffectNote`]s are
//!   projected into their own `effect_notes` field with a snake-case `kind` —
//!   the two are never flattened together, so a caller can tell a documented
//!   option from a "possible effect" note that is *not* a permission control.
//! * **Citations are carried from the corpus read (AC2).** Each resolved section
//!   is projected onto the [048] [`SectionView`] the same way
//!   [`get_section`](crate::mcp::get_section) does, keeping the section id,
//!   source, body, heading breadcrumb, provenance, freshness, and the citations
//!   verbatim — the guidance's sections match the corpus read.
//! * **Errors are typed and leak-free (AC1).** Each [`CommandGuidanceError`]
//!   maps through the [048] `ProtocolError` contract: an unsupported key is
//!   `RESOURCE_NOT_FOUND` (upgraded to `INVALID_PARAMS` for a `2026-07-28` peer),
//!   a malformed / mismatched cursor is `INVALID_PARAMS`, and a dangling citation
//!   is `INTERNAL_ERROR`. The messages name the discoverable alternatives and
//!   leak no internal path or backtrace.
//! * **Freshness is the read's own verdict.** `get_command_guidance` passes the
//!   caller's `as_of` + `threshold` through to the [036] section read, which
//!   computes each section's source freshness; the tool reports that verdict
//!   verbatim and never invents one.
//!
//! # Security boundary (AC2)
//!
//! This module retrieves **documentation only**. It composes the [039]–[043]
//! catalog lookup, the [036] section read, and the [037] cursor — all of which
//! are pure, offline, and in-memory. It imports no process-spawning primitive,
//! no filesystem access, and no network client: the calling agent executes
//! `cargo` and the other commands through *its own* terminal tools, and this
//! server never does it on the caller's behalf. A test at the module boundary
//! scans this file's source for the forbidden primitives to keep that guarantee
//! honest.

#![allow(dead_code)]

use rmcp::model::{CallToolResult, ContentBlock, Tool};

use crate::commands::{CommandCatalog, CommandGuidanceError, command_guidance};
use crate::error::AppError;
use crate::mcp::schema::{
    CitationView, CommandEntryView, EffectNoteView, GetCommandGuidanceRequest,
    GetCommandGuidanceResponse, SectionView,
};
use crate::model::{EffectKind, EffectNote, Section, SnapshotManifest};
use crate::pagination::{MAX_RESPONSE_BYTES, clamp_query_limit};
use crate::provenance::{Instant, StaleThreshold};
use crate::reads::SectionRead;

// ---------------------------------------------------------------------------
// Tool name + builder
// ---------------------------------------------------------------------------

/// The [048] `rmcp` tool name for `get_command_guidance`.
pub const TOOL_NAME: &str = "get_command_guidance";

/// The [`rmcp::model::Tool`] for `get_command_guidance`, built from the [048]
/// `GetCommandGuidanceRequest` / `GetCommandGuidanceResponse` types so its input /
/// output JSON schemas agree with those types. Delegates to the [048] builder so
/// the advertised schema and the serialising type cannot drift.
pub fn get_command_guidance_tool() -> Tool {
    crate::mcp::schema::get_command_guidance_tool()
}

// ---------------------------------------------------------------------------
// View projection
// ---------------------------------------------------------------------------

/// Project a [039]–[043] [`CommandEntry`] onto the [048] [`CommandEntryView`],
/// keeping the upstream excerpts and the separately-labeled effect notes in
/// their own fields (AC2). The effect `kind` is rendered in the snake-case the
/// [048] view schema expects (`source_change`, `lockfile_change`, …), matching
/// the model's `#[serde(rename_all = "snake_case")]` encoding.
fn entry_view_from(entry: &crate::model::CommandEntry) -> CommandEntryView {
    CommandEntryView {
        key: entry.key.to_string(),
        purpose: entry.purpose.clone(),
        upstream_sections: entry
            .upstream_sections
            .iter()
            .map(|section| section.as_str().to_string())
            .collect(),
        syntax: entry.syntax.clone(),
        examples: entry.examples.clone(),
        prerequisites: entry.prerequisites.clone(),
        compatibility: entry.compatibility.clone(),
        effect_notes: entry.effect_notes.iter().map(effect_note_view).collect(),
    }
}

/// Project a maintainer [`EffectNote`] onto the [048] [`EffectNoteView`],
/// rendering the effect family as its snake-case label and keeping the human
/// note verbatim. The note describes a *possible* effect; it is never a
/// permission.
fn effect_note_view(note: &EffectNote) -> EffectNoteView {
    EffectNoteView {
        kind: effect_kind_label(&note.kind).to_string(),
        detail: note.detail.clone(),
    }
}

/// The snake-case wire label for an effect family, matching the model's
/// `#[serde(rename_all = "snake_case")]` encoding so the projected view agrees
/// with the [048] view schema.
fn effect_kind_label(kind: &EffectKind) -> &'static str {
    match kind {
        EffectKind::SourceChange => "source_change",
        EffectKind::LockfileChange => "lockfile_change",
        EffectKind::ArtifactWrite => "artifact_write",
        EffectKind::DependencyDownload => "dependency_download",
        EffectKind::ProjectCodeExecution => "project_code_execution",
        EffectKind::ToolchainChange => "toolchain_change",
        EffectKind::Publication => "publication",
    }
}

/// Project a [036] [`SectionRead`] onto the [048] [`SectionView`], keeping the
/// body verbatim and the context / citations / freshness / snapshot identity
/// faithful to the read (AC2). Mirrors
/// [`crate::mcp::get_section`]'s `SectionView` projection so the guidance's
/// section bodies match a direct `get_section` read.
fn section_view_from(read: &SectionRead) -> SectionView {
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

/// Label a source's freshness verdict for the [`SectionView`], mirroring
/// [`crate::mcp::get_section`].
fn verdict_label(stale: Option<bool>) -> &'static str {
    match stale {
        Some(true) => "stale",
        Some(false) => "fresh",
        None => "unknown",
    }
}

/// The maximum number of citations a single section carries, keeping the
/// response bounded (mirrors the [050] per-hit cap and the [051] per-section cap).
const MAX_CITATIONS: usize = 12;

/// Derive a section's citations for the [048] [`SectionView`] from its fields,
/// exactly as [`crate::mcp::get_section`] does for a hit: the canonical upstream
/// document (provenance) plus a note citation for each applicable guideline
/// code, command key, Clippy lint id, and stability/version qualification. The
/// result is capped at [`MAX_CITATIONS`]. These citations match the corpus read
/// (AC2), so the guidance's sections agree with a direct `get_section` read.
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

/// Build a note-only citation for `section_id` / `source`, mirroring
/// [`crate::mcp::get_section`].
fn note_citation(section_id: &str, source: &str, note: String) -> CitationView {
    CitationView {
        section_id: section_id.to_string(),
        source: source.to_string(),
        url: None,
        title: None,
        note: Some(note),
    }
}

// ---------------------------------------------------------------------------
// The core read
// ---------------------------------------------------------------------------

/// Resolve a `get_command_guidance` request to its [048] response, serving the
/// command's entry and its cited sections and paginating over-cap section bodies
/// through the [037] continuation cursor.
///
/// This composes the completed seams in dependency order:
///
/// 1. **Resolve + page** the command through the [039]–[043] [`command_guidance`]
///    seam, which validates the key at the [007] boundary (a shell operator is
///    `InvalidKey`, an unsupported key is `UnknownKey` — AC2), proves every
///    cited section resolves in `manifest` (a dangling citation is
///    `UnresolvedSection` — AC1/AC2), composes each resolved section into a
///    [036] [`SectionRead`], and pages them through the [037] cursor under
///    `Operation::Catalog` bound to the command key.
/// 2. **Project** the entry to the [048] [`CommandEntryView`] (upstream excerpts
///    and the separately-labeled effect notes in their own fields — AC2) and the
///    returned section reads to [`SectionView`]s (bodies + citations verbatim).
/// 3. **Echo** the snapshot identity and the effective limit (default five,
///    clamped to twenty) so a client knows what window the page used.
///
/// `as_of` and `threshold` feed the [036] section-read freshness; the entry
/// metadata and effect notes are returned whole on every page, and only the
/// section *content* is clipped across continuations.
///
/// # Errors
///
/// Returns a [`CommandGuidanceError`] — which maps through the [048]
/// `ProtocolError` contract — for a malformed or unsupported key
/// (`RESOURCE_NOT_FOUND` / `INVALID_PARAMS` with the supported keys), a
/// dangling citation (`INTERNAL_ERROR`), or a malformed / mismatched
/// continuation cursor (`INVALID_PARAMS`).
pub fn get_command_guidance(
    catalog: &CommandCatalog,
    manifest: &SnapshotManifest,
    request: &GetCommandGuidanceRequest,
    as_of: Instant,
    threshold: StaleThreshold,
) -> Result<GetCommandGuidanceResponse, CommandGuidanceError> {
    // The effective limit: `None` uses the default (five), a value is clamped to
    // the cap (twenty), and `0` yields an empty page. The [037] seam applies the
    // same clamp internally, so the echoed limit matches the page used.
    let limit = request.limit.map(|limit| limit as usize);

    // 1. Resolve + page through the [039]–[043] seam. The `?` propagates the
    //    typed error, which the caller maps through the [048] `ProtocolError`
    //    contract.
    let guidance = command_guidance(
        catalog,
        manifest,
        as_of,
        threshold,
        &request.command,
        limit,
        request.cursor.as_deref(),
    )?;

    // 2. Project the entry and the returned section reads onto the [048] views.
    let entry = entry_view_from(&guidance.entry);
    let sections = guidance.sections().map(section_view_from).collect();

    // 3. Echo the snapshot identity and the effective limit.
    let applied = clamp_query_limit(limit);

    Ok(GetCommandGuidanceResponse {
        entry,
        sections,
        snapshot_id: guidance.snapshot_id.as_str().to_string(),
        limit: applied as u32,
        truncated: guidance.page.truncated,
        next_cursor: guidance.page.next_cursor,
    })
}

// ---------------------------------------------------------------------------
// `CallToolResult` assembly + cap
// ---------------------------------------------------------------------------

/// Assemble the [`CallToolResult`] for a `get_command_guidance` response.
///
/// Mirrors [`get_api_checklist`](crate::mcp::get_api_checklist::result) and
/// [`get_section`](crate::mcp::get_section::result): the full
/// [`GetCommandGuidanceResponse`] serialized to JSON rides in `structured_content`
/// for structured-capable clients, and a compact human-readable summary is the
/// primary text content block. The response-assembled byte size is then enforced
/// against the [`MAX_RESPONSE_BYTES`] cap: the [037] page already bounds every
/// section, so a real response fits and this is a defensive safety net against a
/// pathological snapshot, surfaced as a typed [`AppError`].
///
/// # Errors
///
/// Returns [`AppError::RuntimeStart`] when the response exceeds the response cap
/// or serialization fails.
pub fn result(resp: &GetCommandGuidanceResponse) -> Result<CallToolResult, AppError> {
    let structured = serde_json::to_value(resp).map_err(|e| {
        AppError::runtime_start(
            "encode get_command_guidance structured output",
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
                "measure get_command_guidance structured output",
                Some(Box::new(e)),
            )
        })?
        .len();
    let byte_len = structured_bytes.max(text.len());
    if byte_len > MAX_RESPONSE_BYTES {
        return Err(AppError::runtime_start(
            format!(
                "get_command_guidance response ({byte_len} bytes) exceeds the \
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

/// Render a compact, structured text summary of a `get_command_guidance`
/// response.
///
/// Mirrors the sibling renderers: a header naming the command, the snapshot, the
/// section count, and the effective limit; then the entry's purpose, documented
/// syntax / examples / prerequisites / compatibility (upstream), and the
/// separately-labeled effect notes; then one block per resolved section (its id,
/// title, and body); plus a truncation notice when a `next_cursor` is present.
pub fn render_text(resp: &GetCommandGuidanceResponse) -> String {
    let entry = &resp.entry;
    let mut lines = Vec::with_capacity(
        8 + entry.syntax.len()
            + entry.examples.len()
            + entry.prerequisites.len()
            + entry.compatibility.len()
            + entry.effect_notes.len()
            + resp.sections.len() * 2,
    );
    lines.push(format!(
        "get_command_guidance — {} (snapshot {}, {} section{}, limit {})",
        entry.key,
        resp.snapshot_id,
        resp.sections.len(),
        if resp.sections.len() == 1 { "" } else { "s" },
        resp.limit,
    ));
    lines.push(format!("  purpose: {}", entry.purpose));
    if !entry.syntax.is_empty() {
        lines.push("  syntax:".to_string());
        for syntax in &entry.syntax {
            lines.push(format!("    {syntax}"));
        }
    }
    if !entry.examples.is_empty() {
        lines.push("  examples:".to_string());
        for example in &entry.examples {
            lines.push(format!("    {example}"));
        }
    }
    if !entry.prerequisites.is_empty() {
        lines.push("  prerequisites:".to_string());
        for prerequisite in &entry.prerequisites {
            lines.push(format!("    {prerequisite}"));
        }
    }
    if !entry.compatibility.is_empty() {
        lines.push("  compatibility:".to_string());
        for qualification in &entry.compatibility {
            lines.push(format!("    {qualification}"));
        }
    }
    // The effect notes are labeled separately from the upstream excerpts so a
    // caller can tell a documented option from a "possible effect" that is not a
    // permission control (AC2).
    if !entry.effect_notes.is_empty() {
        lines.push("  effect notes (possible effects, not permissions):".to_string());
        for note in &entry.effect_notes {
            lines.push(format!("    [{}] {}", note.kind, note.detail));
        }
    }
    if resp.truncated {
        lines.push(format!(
            "  (truncated; resume with next_cursor: {})",
            resp.next_cursor.as_deref().unwrap_or("?")
        ));
    }
    for (i, section) in resp.sections.iter().enumerate() {
        lines.push(format!(
            "  section [{}] {} ({}, freshness {})",
            i + 1,
            section.doc_title,
            section.id,
            section.freshness,
        ));
        if !section.body.is_empty() {
            for body_line in section.body.lines() {
                lines.push(format!("    {body_line}"));
            }
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

    use crate::commands::LookupError;
    use crate::mcp::protocol_error::ProtocolError;
    use crate::model::{
        ManifestBody, SchemaVersion, Section, SectionBuilder, SectionId, SnapshotId,
        SnapshotManifest, SourceId, Timestamp, Url,
    };
    use rmcp::model::{ErrorCode, ProtocolVersion};
    use serde_json::json;

    // ----- fixtures: hand-built snapshots that make the cited ids resolvable -----

    /// Build a snapshot carrying exactly the given sections, with the other
    /// collections empty — the offline "a hand-built snapshot makes the cited
    /// ids resolvable" seam (mirrors the [039]–[043] guidance fixtures).
    fn manifest(snapshot_id: &str, sections: Vec<Section>) -> SnapshotManifest {
        SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: ManifestBody {
                snapshot_id: SnapshotId::new(snapshot_id).expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections,
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        }
    }

    /// Build a section under `source` / `chapter` the way the catalog derives its
    /// cited ids, so a fixture section's derived id matches the id a catalog entry
    /// cites and resolution succeeds.
    fn section(source: &str, chapter: &str, body: &str) -> Section {
        SectionBuilder::new(
            SourceId::new(source).expect("a registry source id"),
            format!("{chapter} section"),
            chapter,
            Url::new(format!("https://doc.rust-lang.org/{source}/{chapter}.html"))
                .expect("a valid canonical URL"),
            body.to_string(),
        )
        .build()
        .expect("a well-formed section")
    }

    /// A snapshot with one small `cargo new` section — the "a single command's
    /// content fits in one page" case.
    fn small_snapshot() -> SnapshotManifest {
        manifest(
            "fixture-053-small",
            vec![section(
                "cargo",
                "commands/cargo-new",
                "A short, faithful excerpt from the Cargo Book.",
            )],
        )
    }

    /// A snapshot with two small sections the `cargo clippy` entry cites — the
    /// "a multi-section command that fits in one page" case.
    fn multi_small_snapshot() -> SnapshotManifest {
        manifest(
            "fixture-053-multi",
            vec![
                section("clippy", "usage", "Clippy usage excerpt."),
                section("clippy", "configuration", "Clippy configuration excerpt."),
            ],
        )
    }

    /// A snapshot with the two `cargo test` sections, each with a 20 KiB body so
    /// the pair overflows the response cap and a guidance lookup must page it.
    fn huge_snapshot(id: &str) -> SnapshotManifest {
        manifest(
            id,
            vec![
                section("cargo", "commands/cargo-test", &"x".repeat(20_000)),
                section("rustdoc", "documentation-tests", &"y".repeat(20_000)),
            ],
        )
    }

    fn as_of() -> Instant {
        Instant::parse("2026-01-01T00:00:00Z").expect("a valid instant")
    }

    fn threshold() -> StaleThreshold {
        StaleThreshold::default()
    }

    fn request(
        command: &str,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> GetCommandGuidanceRequest {
        GetCommandGuidanceRequest {
            command: command.to_string(),
            limit,
            cursor,
        }
    }

    fn catalog() -> CommandCatalog {
        CommandCatalog::combined()
    }

    // ----- AC1: supported keys return source-backed sections + metadata -----

    #[test]
    fn tool_name_matches_the_registered_tool() {
        assert_eq!(TOOL_NAME, get_command_guidance_tool().name);
        assert_eq!(TOOL_NAME, "get_command_guidance");
    }

    #[test]
    fn a_supported_key_returns_source_backed_sections_metadata_and_effect_notes() {
        let m = small_snapshot();
        let resp = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo new", None, None),
            as_of(),
            threshold(),
        )
        .expect("`cargo new` resolves in the fixture snapshot");

        // The entry's metadata rides on the page, kept distinct from the upstream
        // sections and the effect notes (AC2).
        assert_eq!(resp.entry.key, "cargo new", "the key round-trips");
        assert!(
            !resp.entry.syntax.is_empty(),
            "documented syntax is present"
        );
        assert!(
            !resp.entry.examples.is_empty(),
            "documented examples are present"
        );
        assert!(
            !resp.entry.prerequisites.is_empty(),
            "prerequisites are present"
        );
        assert!(
            !resp.entry.compatibility.is_empty(),
            "version/stability qualifications are present"
        );
        assert!(
            !resp.entry.effect_notes.is_empty(),
            "separately-labeled effect notes are present"
        );

        // The source-backed section content is retrievable, non-fabricated, and
        // snapshot-bound.
        assert_eq!(resp.sections.len(), 1, "`cargo new` cites one section");
        let read = &resp.sections[0];
        assert!(
            !read.body.is_empty(),
            "the section body is source-backed, not fabricated"
        );
        assert_eq!(
            read.source, "cargo",
            "the section is the real upstream source"
        );
        assert_eq!(
            read.snapshot_id,
            m.body.snapshot_id.as_str(),
            "the read is snapshot-bound"
        );
        assert_eq!(
            resp.snapshot_id,
            m.body.snapshot_id.as_str(),
            "the response carries the snapshot identity"
        );

        // No continuation: the one section fits, so the page is not truncated.
        assert!(
            !resp.truncated,
            "a single fitting section needs no continuation"
        );
        assert!(
            resp.next_cursor.is_none(),
            "the final page carries no next cursor"
        );
        assert_eq!(resp.limit, 5, "the default limit (five) is echoed");
    }

    #[test]
    fn a_multi_section_command_that_fits_has_no_continuation() {
        let m = multi_small_snapshot();
        let resp = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo clippy", None, None),
            as_of(),
            threshold(),
        )
        .expect("`cargo clippy` resolves in the fixture snapshot");

        assert_eq!(
            resp.sections.len(),
            2,
            "both cited sections fit on one page"
        );
        assert!(
            !resp.truncated,
            "a page that fits all sections is not truncated"
        );
        assert!(
            resp.next_cursor.is_none(),
            "the final page carries no next cursor"
        );
    }

    // ----- AC1: over-cap pages paginate with an explicit continuation. -----

    #[test]
    fn a_clipped_guidance_pages_with_an_explicit_continuation_and_reassembles() {
        let m = huge_snapshot("fixture-053-huge");

        // Page 1: the first section, with an explicit continuation and a
        // truncation reason (no silent truncation).
        let page1 = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo test", None, None),
            as_of(),
            threshold(),
        )
        .expect("`cargo test` resolves");
        assert!(
            page1.truncated,
            "a clipped response reports truncation, never silently truncating"
        );
        let next = page1
            .next_cursor
            .clone()
            .expect("a clipped page carries an explicit next cursor");
        assert!(
            page1
                .sections
                .iter()
                .all(|s| s.body.len() <= MAX_RESPONSE_BYTES),
            "each returned section fits on its page"
        );

        // Page 2: resume the explicit cursor; the remainder is returned whole.
        let page2 = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo test", None, Some(next)),
            as_of(),
            threshold(),
        )
        .expect("the continuation cursor resumes the page");
        assert!(!page2.truncated, "the final page is not truncated");
        assert!(
            page2.next_cursor.is_none(),
            "the final page carries no next cursor"
        );

        // Reassembling the pages reproduces the whole command's cited sections,
        // in citation order, with each body intact.
        let ids: Vec<String> = page1
            .sections
            .iter()
            .map(|s| s.id.clone())
            .chain(page2.sections.iter().map(|s| s.id.clone()))
            .collect();
        assert_eq!(
            ids.len(),
            2,
            "both cited sections are returned, none dropped"
        );
        assert!(
            page1
                .sections
                .iter()
                .chain(page2.sections.iter())
                .all(|s| s.body.len() == 20_000),
            "no section body is silently truncated across pages"
        );
    }

    #[test]
    fn a_clipped_guidance_can_be_limited_per_page() {
        // A non-default limit is honored and echoed, and still paginates.
        let m = huge_snapshot("fixture-053-huge-limit");
        let page1 = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo test", Some(1), None),
            as_of(),
            threshold(),
        )
        .expect("`cargo test` resolves with a limit of one");
        assert_eq!(page1.limit, 1, "the requested limit is echoed");
        assert_eq!(
            page1.sections.len(),
            1,
            "only one section fits on the first page"
        );
        assert!(page1.truncated, "a second section remains");
    }

    // ----- AC1/AC2: rejection at the key / citation boundary. -----

    #[test]
    fn an_unsupported_key_is_rejected_with_discoverable_alternatives() {
        let m = small_snapshot();
        let err = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo foobar", None, None),
            as_of(),
            threshold(),
        )
        .expect_err("`cargo foobar` is not in the catalog");
        match err {
            CommandGuidanceError::Lookup(LookupError::UnknownKey {
                requested,
                supported,
            }) => {
                assert_eq!(requested, "cargo foobar");
                assert_eq!(
                    supported,
                    catalog().keys(),
                    "the alternatives are exactly the catalog's supported keys"
                );
                assert!(
                    supported.contains(&"cargo new".to_string()),
                    "the alternatives name a supported key"
                );
            }
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn a_shell_operator_key_is_refused_not_executed() {
        // AC2: a shell operator in the key is refused at the boundary; it is never
        // interpolated into a process. A clean key still resolves for contrast.
        let m = small_snapshot();
        for bad in [
            "cargo new ; rm -rf /",
            "cargo new | xargs -n1 echo",
            "cargo metadata $(whoami)",
            "cargo init && echo pwned",
        ] {
            let err = get_command_guidance(
                &catalog(),
                &m,
                &request(bad, None, None),
                as_of(),
                threshold(),
            )
            .expect_err("a shell-operator key is refused");
            match err {
                CommandGuidanceError::Lookup(LookupError::InvalidKey {
                    requested,
                    reason: _,
                }) => {
                    assert_eq!(requested, bad, "the refused request is preserved");
                }
                other => panic!("expected InvalidKey for `{bad}`, got {other:?}"),
            }
        }
        // A clean key still resolves: refusal is specific to operators.
        let ok = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo new", None, None),
            as_of(),
            threshold(),
        )
        .expect("a clean key still resolves");
        assert_eq!(ok.entry.key, "cargo new");
    }

    #[test]
    fn a_dangling_citation_is_not_fabricated() {
        // AC1/AC2: a well-formed key whose cited section does not resolve is an
        // `UnresolvedSection` — no section is fabricated.
        let m = manifest("fixture-053-empty", vec![]);
        let err = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo new", None, None),
            as_of(),
            threshold(),
        )
        .expect_err("no section resolves in an empty snapshot");
        match err {
            CommandGuidanceError::Lookup(LookupError::UnresolvedSection { key, section }) => {
                assert_eq!(key, "cargo new");
                let expected = SectionId::derive(
                    &SourceId::new("cargo").expect("a registry source id"),
                    "commands/cargo-new",
                    None,
                    None,
                )
                .expect("a well-formed section id");
                assert_eq!(
                    section, expected,
                    "the unresolvable citation is named, not invented"
                );
            }
            other => panic!("expected UnresolvedSection, got {other:?}"),
        }
    }

    // ----- AC1: the [048] `ProtocolError` mapping. -----

    #[test]
    fn an_unknown_key_maps_to_resource_not_found_then_invalid_params() {
        let m = small_snapshot();
        let err = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo foobar", None, None),
            as_of(),
            threshold(),
        )
        .expect_err("an unknown key fails");

        // Pre-`2026-07-28`: `RESOURCE_NOT_FOUND` with the supported keys.
        let pre = err.protocol_error(Some(&ProtocolVersion::V_2025_11_25));
        assert_eq!(pre.code, ErrorCode::RESOURCE_NOT_FOUND);
        let data = pre.data.expect("the error carries the supported keys");
        assert_eq!(data.get("requested"), Some(&json!("cargo foobar")));
        assert!(
            data.get("supported")
                .and_then(|s| s.as_array())
                .map(|a| a.iter().any(|k| k.as_str() == Some("cargo new")))
                .unwrap_or(false),
            "the supported keys are surfaced as discoverable alternatives"
        );

        // The version-aware upgrade: the same error is `INVALID_PARAMS` for a
        // `2026-07-28` peer.
        let post = err.protocol_error(Some(&ProtocolVersion::V_2026_07_28));
        assert_eq!(post.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn an_invalid_key_maps_to_invalid_params() {
        let m = small_snapshot();
        let err = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo new ; rm -rf /", None, None),
            as_of(),
            threshold(),
        )
        .expect_err("a shell-operator key is refused");
        let pre = err.protocol_error(Some(&ProtocolVersion::V_2025_11_25));
        assert_eq!(pre.code, ErrorCode::INVALID_PARAMS);
        let data = pre.data.expect("the error carries the refused request");
        assert_eq!(data.get("requested"), Some(&json!("cargo new ; rm -rf /")));
    }

    #[test]
    fn an_unresolved_section_maps_to_internal_error() {
        let m = manifest("fixture-053-empty", vec![]);
        let err = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo new", None, None),
            as_of(),
            threshold(),
        )
        .expect_err("a dangling citation fails");
        // `INTERNAL_ERROR` is not part of the `2026-07-28` upgrade, so it is
        // unchanged for both peers.
        let pre = err.protocol_error(Some(&ProtocolVersion::V_2025_11_25));
        assert_eq!(pre.code, ErrorCode::INTERNAL_ERROR);
        let post = err.protocol_error(Some(&ProtocolVersion::V_2026_07_28));
        assert_eq!(post.code, ErrorCode::INTERNAL_ERROR);
        let data = pre.data.expect("the error names the offending citation");
        assert_eq!(data.get("key"), Some(&json!("cargo new")));
        assert!(
            data.get("section").is_some(),
            "the missing section is named"
        );
    }

    #[test]
    fn a_malformed_cursor_maps_to_invalid_params() {
        let m = small_snapshot();
        let err = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo new", None, Some("not-a-cursor".to_string())),
            as_of(),
            threshold(),
        )
        .expect_err("a malformed cursor is rejected");
        match err {
            CommandGuidanceError::Cursor(_) => {}
            other => panic!("expected a Cursor error, got {other:?}"),
        }
        let pre = err.protocol_error(Some(&ProtocolVersion::V_2025_11_25));
        assert_eq!(pre.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn an_unknown_key_message_is_actionable_and_leak_free() {
        let m = small_snapshot();
        let err = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo foobar", None, None),
            as_of(),
            threshold(),
        )
        .expect_err("an unknown key fails");
        let pe = err.protocol_error(None);
        let text = format!("{}{}", pe.message, pe.data.unwrap_or_default());
        assert!(
            text.contains("cargo foobar"),
            "the message names the refused request: {text}"
        );
        assert!(
            text.contains("cargo new"),
            "the message points at a discoverable alternative: {text}"
        );
        for forbidden in ["/src/", "at 0x", "panic", "backtrace", "src/mcp"] {
            assert!(
                !text.contains(forbidden),
                "the error leaks no internal path or backtrace ({forbidden}): {text}"
            );
        }
    }

    // ----- AC1: snapshot-bound cursors. -----

    #[test]
    fn a_cursor_is_snapshot_bound() {
        // A cursor minted under one snapshot cannot resume a call under another.
        let m_a = huge_snapshot("fixture-053-a");
        let m_b = huge_snapshot("fixture-053-b");
        let page1 = get_command_guidance(
            &catalog(),
            &m_a,
            &request("cargo test", None, None),
            as_of(),
            threshold(),
        )
        .expect("the first page is served under snapshot a");
        let cursor = page1
            .next_cursor
            .clone()
            .expect("a non-final page carries a cursor");

        let err = get_command_guidance(
            &catalog(),
            &m_b,
            &request("cargo test", None, Some(cursor)),
            as_of(),
            threshold(),
        )
        .expect_err("a cursor from another snapshot is rejected");
        match err {
            CommandGuidanceError::Cursor(inner) => {
                assert!(
                    inner.to_string().contains("snapshot"),
                    "the mismatch names the snapshot field: {inner}"
                );
            }
            other => panic!("expected a Cursor mismatch, got {other:?}"),
        }
    }

    // ----- AC2: upstream excerpts and effect notes stay separate. -----

    #[test]
    fn effect_notes_are_kept_separate_from_upstream_content() {
        let m = small_snapshot();
        let resp = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo new", None, None),
            as_of(),
            threshold(),
        )
        .expect("`cargo new` resolves");

        // The upstream excerpts and the effect notes occupy distinct fields, and
        // an effect note carries a snake-case `kind` and a human `detail`.
        assert!(
            resp.entry.syntax.iter().any(|s| !s.is_empty()),
            "upstream syntax is present"
        );
        let note = resp
            .entry
            .effect_notes
            .first()
            .expect("at least one effect note");
        assert!(!note.kind.is_empty(), "the effect note has a kind label");
        assert!(
            note.kind
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_'),
            "the effect kind is snake-case: {}",
            note.kind
        );
        assert!(!note.detail.is_empty(), "the effect note has a detail");
    }

    #[test]
    fn effect_kind_labels_match_the_model_serialisation() {
        // The snake-case labels this module emits must agree with the model's
        // `#[serde(rename_all = "snake_case")]` encoding, so the projected view
        // agrees with the [048] view schema.
        for kind in [
            EffectKind::SourceChange,
            EffectKind::LockfileChange,
            EffectKind::ArtifactWrite,
            EffectKind::DependencyDownload,
            EffectKind::ProjectCodeExecution,
            EffectKind::ToolchainChange,
            EffectKind::Publication,
        ] {
            let serialized = serde_json::to_value(&kind)
                .expect("an effect kind serialises")
                .as_str()
                .expect("an effect kind serialises to a string")
                .to_string();
            assert_eq!(
                effect_kind_label(&kind),
                serialized.as_str(),
                "the label matches the model's snake-case serialisation"
            );
        }
    }

    // ----- The `CallToolResult` assembly + cap. -----

    #[test]
    fn result_fits_the_response_cap_for_a_page() {
        let m = small_snapshot();
        let resp = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo new", None, None),
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
        let structured = call.structured_content.as_ref().unwrap();
        assert_eq!(
            structured["entry"]["key"],
            json!("cargo new"),
            "the structured payload carries the entry key"
        );
        assert_eq!(
            structured["sections"].as_array().unwrap().len(),
            1,
            "the structured payload carries one section"
        );
    }

    #[test]
    fn result_rejects_an_over_cap_response() {
        // A response whose serialized body alone exceeds the cap is rejected
        // defensively.
        let resp = GetCommandGuidanceResponse {
            entry: CommandEntryView {
                key: "cargo new".to_string(),
                purpose: String::new(),
                upstream_sections: vec![],
                syntax: vec![],
                examples: vec![],
                prerequisites: vec![],
                compatibility: vec![],
                effect_notes: vec![],
            },
            sections: vec![SectionView {
                id: "cargo/commands/cargo-new/1".to_string(),
                source: "cargo".to_string(),
                doc_title: String::new(),
                chapter_path: String::new(),
                heading_path: vec![],
                canonical_url: String::new(),
                body: "x".repeat(40_000),
                snapshot_id: "seed".to_string(),
                freshness: "unknown".to_string(),
                content_hash: String::new(),
                citations: vec![],
            }],
            snapshot_id: "seed".to_string(),
            limit: 5,
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
        let m = small_snapshot();
        let resp = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo new", None, None),
            as_of(),
            threshold(),
        )
        .expect("resolves");
        let text = render_text(&resp);
        assert!(
            text.contains("get_command_guidance —"),
            "the text names the tool"
        );
        assert!(text.contains("cargo new"), "the text names the command");
        assert!(
            text.contains("effect notes"),
            "the text labels the effect notes"
        );
        assert!(!text.contains("at 0x"), "no stack-trace-like text");
        let call = result(&resp).expect("the response assembles");
        assert!(!call.content.is_empty(), "the call carries content");
    }

    // ----- The lookup is pure and deterministic. -----

    #[test]
    fn the_guidance_is_pure_and_deterministic() {
        let m = huge_snapshot("fixture-053-huge-determinism");
        let a = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo test", None, None),
            as_of(),
            threshold(),
        )
        .expect("resolves");
        let b = get_command_guidance(
            &catalog(),
            &m,
            &request("cargo test", None, None),
            as_of(),
            threshold(),
        )
        .expect("resolves");
        assert_eq!(a, b, "two lookups of the same inputs are identical");
    }

    // ----- AC2: no process-execution / filesystem / network path. -----

    #[test]
    fn command_guidance_never_executes_or_touches_fs_or_network() {
        // The module is documentation-only: no process-execution, filesystem, or
        // network primitive may appear anywhere in this module's source. The
        // forbidden tokens are assembled from pieces so this test's own source
        // never matches its own scan.
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/mcp/get_command_guidance.rs"
        ));
        let forbidden = [
            "std::".to_string() + "process", // the standard-library process module
            "std::".to_string() + "fs",      // the standard-library filesystem module
            "std::".to_string() + "net",     // the standard-library network module
            "Command::".to_string() + "new", // the standard-library command constructor
            ".spaw".to_string() + "n(",      // the spawn method call
            ".exe".to_string() + "c(",       // the exec method call
        ];
        for needle in forbidden {
            assert!(
                !source.contains(needle.as_str()),
                "get_command_guidance must contain no process/fs/network primitive, but found: {needle}"
            );
        }
    }
}
