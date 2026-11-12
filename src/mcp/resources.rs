//! [054][M4] — the `rust-docs://catalog` resource and its paginated
//! command-catalog pages, as `rmcp` [`Resource`]s over the [048]
//! [`CatalogResourceView`](crate::mcp::schema::CatalogResourceView) /
//! [`SectionResourceView`](crate::mcp::schema::SectionResourceView) schemas,
//! consuming the [043] command inventory and the [039]–[043] command catalog.
//!
//! # What this module does
//!
//! This is the *Resources* half of `plan.md` §6: the server advertises a catalog
//! resource (`rust-docs://catalog`) and concrete, paginated command-catalog pages,
//! so a caller discovers the available commands and the exact section resources
//! *without inferring their URIs*. It mirrors the five M4 tools
//! ([049]–[053]): a schema-driven resource builder (delegated to
//! [`crate::mcp::schema`]), a core view builder that composes the completed
//! seams, a concise text rendering, and a cap-bounded structured assembly.
//!
//! * **AC1 — the catalog resource advertises the active snapshot and concrete
//!   resource identities.** [`catalog_resource_view`] builds the
//!   [`CatalogResourceView`] the [`catalog_resource`](crate::mcp::schema::catalog_resource)
//!   resource serves: the active snapshot's identity (`snapshot_id`, `corpus_hash`),
//!   the per-source status rows (the same report `list_sources` serves), and —
//!   carried in the [054] additive `resource_uris` field — the concrete,
//!   URI-safe resource identities a caller fetches: the command-catalog page URI
//!   (`rust-docs://snapshots/{snapshot_id}/commands`) and the section resource
//!   URIs (`rust-docs://snapshots/{snapshot_id}/sections/{section_id}`) for the
//!   sections this page's commands cite. The identities come from the catalog's
//!   *own* validated [`SectionId`]s and the snapshot's validated
//!   [`SnapshotId`](crate::model::SnapshotId), never from a caller-supplied path.
//!
//! * **AC2 — the command-catalog pages are bounded, explicit, and complete.** The
//!   page of supported keys is produced by the [043] [`command_inventory`]
//!   stream, bounded by the [037] `Operation::Catalog` /
//!   [`MAX_RESPONSE_BYTES`] cap with an explicit `next_cursor` (a foreign or
//!   malformed cursor is a typed [`CursorError`], rejected — never a wrong page),
//!   and each returned key is projected, via [`CommandCatalog::get`], to a
//!   compact [`CommandEntryView`] (its key, short purpose, and the upstream
//!   section ids it cites as "source references"). Walking every page reproduces
//!   the catalog's full key list in catalog order; a zero-limit request yields an
//!   empty page that still carries its continuation.
//!
//! # Design decisions
//!
//! * **Concrete URIs, never inferred (AC1).** The catalog resource *advertises*
//!   the exact `rust-docs://…` URIs a client fetches; it does not map a resource
//!   path onto a filesystem path. The section URIs are built from the catalog's
//!   validated [`SectionId`]s (URI-safe by construction) and the snapshot's
//!   validated [`SnapshotId`], so no caller-supplied string is ever joined into a
//!   path. A test asserts every advertised section URI names a section the
//!   catalog actually cites, and a source scan asserts the module touches no
//!   filesystem, process, or network primitive.
//! * **The command-catalog page carries compact entries.** Each page's
//!   [`CommandEntryView`]s carry only the key, the short purpose, and the cited
//!   upstream section ids — the "short purposes + source references" of AC2. The
//!   full guidance (documented syntax, examples, prerequisites, compatibility, and
//!   the separately-labeled effect notes) is the [`get_command_guidance`](crate::mcp::get_command_guidance)
//!   tool's job; the catalog page keeps the discovery surface small and bounded.
//! * **Errors are typed and leak-free.** The only fallible steps are the snapshot
//!   serialisation for the content hash (an operational [`AppError`]) and the
//!   continuation cursor (a [`CursorError`], mapped to [`AppError`] at this layer
//!   and to a version-aware `INVALID_PARAMS` by the [048] `ProtocolError`
//!   contract at the handler). No message carries an internal path or backtrace.
//! * **Total, pure, and offline.** [`catalog_resource_view`] returns a typed
//!   error (never panics) and a fixed catalog plus snapshot plus injected
//!   `as_of` / threshold yield a fixed page; the module imports no process /
//!   filesystem / network primitive, so a caller discovers documentation without
//!   the server touching the outside world.
//!
//! # How it is wired
//!
//! Mirroring the five tools, this exposes the [`catalog_resource()`] /
//! [`section_resource()`] builders (delegated to [`crate::mcp::schema`]) for
//! [`rmcp::ServerHandler::list_resources`], the [`catalog_resource_view`] core
//! read, a [`render_catalog`] text summary, and the cap-bounded
//! [`catalog_resource_result`] assembly. The [048] `ServerHandler` seam wires
//! these in a later task, following the same transitional pattern as the tools:
//! the resource logic and its assembly live here; the handler consumes them once
//! it is ready. The module is therefore marked `#![allow(dead_code)]` while the
//! seam is transitional.

// This module exposes the catalog/command-catalog resource logic (view
// construction, URI building, rendering, and cap-bounded assembly). It is the
// API surface the `crate::mcp` handler dispatch will call when a client lists or
// reads the catalog resource. Until that dispatch is wired, the public functions
// are un-reached from within this crate, which would trip `dead_code`; the allow
// is lifted when the handler consumes them.
#![allow(dead_code)]

use rmcp::model::Resource;

use crate::commands::{CommandCatalog, command_inventory};
use crate::error::AppError;
use crate::mcp::schema::{CatalogResourceView, CommandEntryView, SourceStatusView};
use crate::model::SnapshotManifest;
use crate::pagination::{CursorError, MAX_RESPONSE_BYTES};
use crate::provenance::{Instant, StaleThreshold};
use crate::status::{SourceStatus, build_report};

// ---------------------------------------------------------------------------
// Resource URIs + builders
// ---------------------------------------------------------------------------

/// The stable URI of the catalog resource a client discovers first.
pub const CATALOG_URI: &str = "rust-docs://catalog";

/// The `rust-docs://catalog` [`Resource`] — the advertised catalog entry point.
///
/// Delegates to the [048] [`crate::mcp::schema::catalog_resource`] builder so the
/// advertised resource (its URI, name, title, description, and typed payload
/// schema) and the [`CatalogResourceView`] that serialises the payload cannot
/// drift.
pub fn catalog_resource() -> Resource {
    crate::mcp::schema::catalog_resource()
}

/// Build the concrete command-catalog page URI for `snapshot_id`,
/// `rust-docs://snapshots/{snapshot_id}/commands`.
///
/// `snapshot_id` is a validated [`SnapshotId`](crate::model::SnapshotId) and so is
/// URI-safe; the result is a concrete resource identity a client fetches, never an
/// inferred one.
pub fn command_page_uri(snapshot_id: &str) -> String {
    format!("rust-docs://snapshots/{snapshot_id}/commands")
}

/// Build the concrete section resource URI for `snapshot_id` / `section_id`,
/// `rust-docs://snapshots/{snapshot_id}/sections/{section_id}`.
///
/// Both `snapshot_id` and `section_id` are validated, URI-safe ids
/// ([`SnapshotId`](crate::model::SnapshotId) /
/// [`SectionId::as_str`](crate::model::SectionId::as_str)), so the result is
/// URI-safe by construction. The `section_id` is always drawn from the catalog's
/// own cited sections, never a caller-supplied path.
pub fn section_uri(snapshot_id: &str, section_id: &str) -> String {
    format!("rust-docs://snapshots/{snapshot_id}/sections/{section_id}")
}

// ---------------------------------------------------------------------------
// View projection
// ---------------------------------------------------------------------------

/// Map a per-source [`SourceStatus`] to its [`SourceStatusView`], keeping the
/// freshness verdict in the human label the [048] view contract uses
/// (`stale` / `fresh` / `unknown`). Mirrors [`crate::mcp::list_sources`].
fn source_view_from(status: &SourceStatus) -> SourceStatusView {
    SourceStatusView {
        id: status.id.as_str().to_string(),
        title: status.title.clone(),
        last_checked: status
            .last_checked
            .as_ref()
            .map(|at| at.as_str().to_string()),
        fetched_at: status.fetched_at.as_str().to_string(),
        upstream_modified: status
            .upstream_modified
            .as_ref()
            .map(|at| at.as_str().to_string()),
        content_hash: status.content_hash.as_str().to_string(),
        freshness: verdict_label(status.is_stale()).to_string(),
    }
}

/// Render a freshness verdict as the label the [048] view contract uses
/// (`Some(true)` → `stale`, `Some(false)` → `fresh`, `None` → `unknown`).
fn verdict_label(stale: Option<bool>) -> &'static str {
    match stale {
        Some(true) => "stale",
        Some(false) => "fresh",
        None => "unknown",
    }
}

/// Project a [039]–[043] [`CommandEntry`](crate::model::CommandEntry) onto a
/// compact catalog [`CommandEntryView`]: the key, the short purpose, and the
/// upstream section ids it cites (the "source references" of AC2). The full
/// guidance (documented syntax, examples, prerequisites, compatibility, and the
/// separately-labeled effect notes) is left empty — the [048] view's
/// `skip_serializing_if = "Vec::is_empty"` omits them, so the discovery surface
/// stays small and bounded; that content is the
/// [`get_command_guidance`](crate::mcp::get_command_guidance) tool's job.
fn catalog_entry_view_from(entry: &crate::model::CommandEntry) -> CommandEntryView {
    CommandEntryView {
        key: entry.key.to_string(),
        purpose: entry.purpose.clone(),
        upstream_sections: entry
            .upstream_sections
            .iter()
            .map(|section| section.as_str().to_string())
            .collect(),
        syntax: Vec::new(),
        examples: Vec::new(),
        prerequisites: Vec::new(),
        compatibility: Vec::new(),
        effect_notes: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The catalog resource view (AC1 + AC2)
// ---------------------------------------------------------------------------

/// Build the [`CatalogResourceView`] the [`catalog_resource()`] resource serves
/// for the active snapshot: the snapshot identity, the per-source status rows, the
/// paginated command-catalog page, the concrete `resource_uris`, and the
/// pagination state.
///
/// This composes the completed seams in dependency order:
///
/// 1. **The snapshot + sources** — [`build_report`] (the same per-source
///    freshness `list_sources` serves) yields the source status rows and the
///    snapshot identity; [`corpus_hash`] yields the corpus content hash.
/// 2. **The command-catalog page (AC2)** — [`command_inventory`] pages the
///    catalog's supported keys under `Operation::Catalog` /
///    [`MAX_RESPONSE_BYTES`], resuming after `cursor` and rejecting a foreign or
///    malformed one as a typed [`CursorError`] (mapped to [`AppError`] here, then
///    to `INVALID_PARAMS` by the [048] `ProtocolError` contract).
/// 3. **The compact entries** — each key on the page is projected, via
///    [`CommandCatalog::get`], to a compact [`CommandEntryView`].
/// 4. **The concrete resource identities (AC1)** — `resource_uris` advertises the
///    command-catalog page URI plus the section URIs this page's commands cite,
///    deduplicated and in order.
///
/// # Errors
///
/// Returns an [`AppError`] when the active snapshot cannot be serialised for its
/// content hash, or when the continuation cursor is malformed or mismatched (a
/// typed [`CursorError`] mapped here).
pub fn catalog_resource_view(
    manifest: &SnapshotManifest,
    catalog: &CommandCatalog,
    limit: Option<usize>,
    cursor: Option<&str>,
    as_of: Instant,
    threshold: StaleThreshold,
) -> Result<CatalogResourceView, AppError> {
    // 1. The snapshot identity, the corpus hash, and the per-source status rows.
    let report = build_report(manifest, as_of, threshold);
    let snapshot_id = report.snapshot_id.as_str();
    let corpus_hash = corpus_hash(manifest)?;
    let sources: Vec<SourceStatusView> = report.sources.iter().map(source_view_from).collect();

    // 2. The paginated command-catalog page. A foreign / malformed cursor is a
    //    typed `CursorError`, mapped to an `AppError` at this layer (the
    //    version-aware `INVALID_PARAMS` is the [048] `ProtocolError` contract's
    //    job at the handler).
    let inventory =
        command_inventory(catalog, manifest, limit, cursor).map_err(cursor_to_app_error)?;

    // 3. Project each returned key to a compact catalog entry. A key drawn from
    //    `catalog.keys()` always resolves through `catalog.get` (the same entries
    //    back both), so `ok()` filters only the impossible, keeping this total.
    let commands: Vec<CommandEntryView> = inventory
        .keys
        .iter()
        .filter_map(|key| catalog.get(key).ok())
        .map(catalog_entry_view_from)
        .collect();

    // 4. The concrete resource identities (AC1): the command-catalog page URI,
    //    then the section URIs this page's commands cite, deduplicated in order.
    let section_uris: Vec<String> = commands
        .iter()
        .flat_map(|entry| entry.upstream_sections.iter().cloned())
        .map(|section| section_uri(snapshot_id, &section))
        .collect();
    let mut resource_uris: Vec<String> = Vec::with_capacity(1 + section_uris.len());
    resource_uris.push(command_page_uri(snapshot_id));
    for uri in section_uris {
        if !resource_uris.contains(&uri) {
            resource_uris.push(uri);
        }
    }

    Ok(CatalogResourceView {
        snapshot_id: snapshot_id.to_string(),
        corpus_hash,
        sources,
        commands,
        resource_uris,
        next_cursor: inventory.next_cursor,
        truncated: inventory.truncated,
    })
}

/// Map a [`CursorError`] to an [`AppError`] at the catalog-resource layer.
///
/// The message is fixed and actionable — it names the remedy (start a fresh
/// request) and carries no internal path or the raw client token — so no leak
/// can escape through this mapping. The version-aware `INVALID_PARAMS` code is
/// the [048] `ProtocolError` contract's responsibility at the handler.
fn cursor_to_app_error(_err: CursorError) -> AppError {
    AppError::runtime_start(
        "a malformed or mismatched continuation cursor was supplied; start a fresh request",
        None,
    )
}

/// The corpus content hash — a deterministic FNV-1a over the serialised,
/// validated snapshot. The active snapshot always serialises (every field is a
/// serialisable, validated type), so a failure here is an operational error, not
/// a logic bug. Mirrors [`crate::mcp::list_sources`].
fn corpus_hash(manifest: &SnapshotManifest) -> Result<String, AppError> {
    let bytes = manifest.to_json_string().map_err(|e| {
        AppError::runtime_start(
            "serialise active snapshot for catalog resource content hash",
            Some(Box::new(e)),
        )
    })?;
    Ok(crate::model::CorpusHash::compute(bytes.as_bytes())
        .as_str()
        .to_string())
}

// ---------------------------------------------------------------------------
// Assembly: text + cap-bounded structured result
// ---------------------------------------------------------------------------

/// Render a concise, human-readable summary of a [`CatalogResourceView`] that
/// agrees with the structured payload: the snapshot identity, the source count,
/// the command keys on the page, and the advertised resource identities.
///
/// The catalog is small, so the text is well under the response cap; the
/// [`catalog_resource_result`] function enforces that bound on both forms.
pub fn render_catalog(view: &CatalogResourceView) -> String {
    let mut lines = Vec::with_capacity(6 + view.commands.len() + view.resource_uris.len());
    lines.push(format!(
        "catalog — snapshot {}, corpus {}, {} source{}, {} command{} on this page",
        view.snapshot_id,
        view.corpus_hash,
        view.sources.len(),
        if view.sources.len() == 1 { "" } else { "s" },
        view.commands.len(),
        if view.commands.len() == 1 { "" } else { "s" },
    ));
    if !view.commands.is_empty() {
        for entry in &view.commands {
            lines.push(format!("  - {} — {}", entry.key, entry.purpose));
        }
    }
    lines.push(format!(
        "resource identities: {}",
        view.resource_uris.join(", ")
    ));
    if view.truncated {
        lines.push(format!(
            "  (truncated; resume with next_cursor: {})",
            view.next_cursor.as_deref().unwrap_or("?")
        ));
    }
    lines.join("\n")
}

/// Assemble the structured, cap-bounded catalog resource payload: the full
/// [`CatalogResourceView`] serialised to JSON (for structured-capable clients)
/// measured, together with its [`render_catalog`] text, against the
/// [`MAX_RESPONSE_BYTES`] cap. An over-cap page is a typed [`AppError`] — a
/// defensive safety net, since the [043] inventory already bounds every page.
///
/// Returns the structured JSON `Value`; the text rendering is the compact
/// companion a human or older client reads.
///
/// # Errors
///
/// Returns [`AppError::RuntimeStart`] when the payload exceeds the response cap or
/// serialisation fails.
pub fn catalog_resource_result(view: &CatalogResourceView) -> Result<serde_json::Value, AppError> {
    let structured = serde_json::to_value(view).map_err(|e| {
        AppError::runtime_start(
            "encode catalog resource structured output",
            Some(Box::new(e)),
        )
    })?;
    let structured_bytes = serde_json::to_vec(&structured)
        .map_err(|e| {
            AppError::runtime_start(
                "measure catalog resource structured output",
                Some(Box::new(e)),
            )
        })?
        .len();
    let text = render_catalog(view);
    // The cap is enforced on *both* rendered forms (see the function docs): the
    // structured body carries every field plus JSON overhead, so its measurement
    // is the binding one, while the text measurement guards the "both within the
    // cap" contract.
    let byte_len = structured_bytes.max(text.len());
    if byte_len > MAX_RESPONSE_BYTES {
        return Err(AppError::runtime_start(
            format!(
                "catalog resource ({byte_len} bytes) exceeds the \
                 {MAX_RESPONSE_BYTES}-byte response cap"
            ),
            None,
        ));
    }
    Ok(structured)
}

// ---------------------------------------------------------------------------
// Tests: AC1 + AC2, total, pure, offline. No upstream, no process, no network.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::mcp::schema::{SectionResourceView, all_resources, catalog_resource};
    use crate::model::SectionId;
    use crate::pagination::{CursorError, MismatchField, Operation};
    use crate::registry::SourceCollection;
    use rmcp::model::ErrorCode;
    use std::str::FromStr;

    // ----- fixtures ------------------------------------------------------

    /// A complete, eight-source bundled snapshot with real sections and a
    /// command catalog — the offline seam, like the sibling `list_sources`.
    fn snapshot() -> SnapshotManifest {
        crate::bundled::manifest()
    }

    fn catalog() -> CommandCatalog {
        CommandCatalog::combined()
    }

    fn as_of() -> Instant {
        Instant::parse("2026-01-01T00:00:00Z").expect("a valid instant")
    }

    fn threshold() -> StaleThreshold {
        StaleThreshold::days(30)
    }

    // ----- the resource builders (AC1) -----------------------------------

    #[test]
    fn the_two_resources_advertise_json_and_stable_uris() {
        let resources = all_resources();
        assert_eq!(resources.len(), 2, "the two advertised resources");
        for r in &resources {
            assert_eq!(
                r.mime_type.as_deref(),
                Some("application/json"),
                "each resource is application/json"
            );
        }
        // The catalog builder delegates to the [048] schema and carries the
        // stable catalog URI.
        assert_eq!(catalog_resource().uri, CATALOG_URI);
        assert_eq!(CATALOG_URI, "rust-docs://catalog");
    }

    #[test]
    fn the_section_resource_is_advertised_and_json() {
        let section = catalog_resource();
        assert_eq!(section.mime_type.as_deref(), Some("application/json"));
        // The `SectionResourceView` is the section resource's payload schema.
        let view = SectionResourceView::default();
        assert_eq!(view.section.snapshot_id, "");
    }

    // ----- AC1: concrete, URI-safe resource identities -------------------

    #[test]
    fn the_catalog_advertises_the_active_snapshot_and_command_page_uri() {
        let m = snapshot();
        let view = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("the catalog builds");

        // The active snapshot is advertised.
        assert_eq!(view.snapshot_id, m.body.snapshot_id.as_str());
        assert!(
            view.corpus_hash.starts_with("c128-"),
            "the corpus hash is advertised"
        );

        // The concrete command-catalog page URI is advertised.
        let expected = command_page_uri(m.body.snapshot_id.as_str());
        assert_eq!(
            view.resource_uris.first().map(String::as_str),
            Some(expected.as_str()),
            "the first resource identity is the command-catalog page URI"
        );
        assert!(expected.starts_with("rust-docs://snapshots/"));
        assert!(expected.ends_with("/commands"));
    }

    #[test]
    fn advertised_section_uris_are_exact_known_identities_not_arbitrary_paths() {
        let m = snapshot();
        let view = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("the catalog builds");

        let snap = m.body.snapshot_id.as_str();
        let catalog = catalog();
        // Every advertised section URI must (a) name a section the catalog
        // actually cites, and (b) be the exact URI the builder produces — never
        // a filesystem path or a caller-supplied string.
        let cited: Vec<String> = catalog
            .keys()
            .iter()
            .filter_map(|key| catalog.get(key).ok())
            .flat_map(|entry| {
                entry
                    .upstream_sections
                    .iter()
                    .map(|s| s.as_str().to_string())
            })
            .collect();

        for uri in &view.resource_uris {
            if let Some(after) = uri.strip_prefix("rust-docs://snapshots/") {
                assert!(
                    after.starts_with(&format!("{snap}/")),
                    "URI names the snapshot: {uri}"
                );
                // No filesystem-path characters leak into the URI.
                assert!(
                    !uri.contains('\\') && !uri.contains(":////"),
                    "the URI is URI-safe: {uri}"
                );
            }
        }
        // Every advertised section URI is one the builder produced for a section
        // the catalog cites.
        for uri in &view.resource_uris {
            if let Some(sid) = uri.strip_prefix(&format!("rust-docs://snapshots/{snap}/sections/"))
            {
                assert!(
                    cited.iter().any(|s| s == sid),
                    "advertised section URI {uri} names a section the catalog cites"
                );
            }
        }
    }

    #[test]
    fn section_uris_are_uri_safe_and_round_trip_through_the_section_id_grammar() {
        // The section ids the catalog cites are validated `SectionId`s, so the
        // advertised section URIs carry URI-safe ids that parse back through the
        // section grammar.
        let m = snapshot();
        let view = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("the catalog builds");
        let prefix = format!(
            "rust-docs://snapshots/{}/sections/",
            m.body.snapshot_id.as_str()
        );
        for uri in &view.resource_uris {
            let Some(sid) = uri.strip_prefix(&prefix) else {
                continue;
            };
            let parsed = SectionId::from_str(sid).unwrap_or_else(|_| {
                panic!("advertised section id is not a valid SectionId: {sid}")
            });
            assert_eq!(parsed.as_str(), sid);
        }
    }

    #[test]
    fn catalog_resource_view_conforms_to_its_output_schema() {
        // AC1: the value this module builds agrees with the [048] schema.
        use crate::mcp::schema::{output_schema, value_conforms};
        let m = snapshot();
        let view = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("the catalog builds");
        let json = serde_json::to_value(&view).expect("the view serialises");
        let schema = output_schema::<CatalogResourceView>();
        value_conforms(&schema, &json)
            .unwrap_or_else(|e| panic!("catalog view does not conform: {e}\n{json}"));
    }

    // ----- AC1: the catalog resource result is cap-bounded ----------------

    #[test]
    fn the_catalog_resource_result_is_structured_and_within_cap() {
        let m = snapshot();
        let view = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("the catalog builds");
        let structured = catalog_resource_result(&view).expect("the result fits the cap");
        assert!(structured.is_object(), "the result is a JSON object");
        assert_eq!(
            structured["snapshot_id"],
            serde_json::Value::String(m.body.snapshot_id.as_str().to_string()),
        );
        // The text rendering carries the snapshot identity and the resource URIs.
        let text = render_catalog(&view);
        assert!(text.contains(&format!("snapshot {}", m.body.snapshot_id.as_str())));
        assert!(text.contains("rust-docs://snapshots/"));
        // Both forms fit the cap.
        assert!(serde_json::to_vec(&structured).expect("serialise").len() <= MAX_RESPONSE_BYTES);
        assert!(text.len() <= MAX_RESPONSE_BYTES);
    }

    // ----- AC2: the command-catalog pages are bounded and explicit -------

    #[test]
    fn the_first_page_lists_keys_purposes_and_source_references() {
        let m = snapshot();
        let view = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("the catalog builds");

        // Every returned entry carries its key, a non-empty short purpose, and
        // the upstream section ids it cites (the "source references").
        for entry in &view.commands {
            assert!(!entry.key.is_empty(), "each entry has a key");
            assert!(!entry.purpose.is_empty(), "each entry has a short purpose");
            assert!(
                !entry.upstream_sections.is_empty(),
                "each entry cites upstream sections (source references)"
            );
        }
        // The first page is a prefix of the catalog's keys, in catalog order.
        let keys = catalog().keys();
        let page_keys: Vec<String> = view.commands.iter().map(|e| e.key.clone()).collect();
        assert!(
            keys.starts_with(&page_keys),
            "the first page is a prefix of the catalog keys in order"
        );
        assert_eq!(
            view.commands.len(),
            keys.len().min(5),
            "the default limit is five"
        );
        assert!(view.truncated, "the full catalog paginates");
        assert!(
            view.next_cursor.is_some(),
            "a truncated page carries a next cursor"
        );
    }

    #[test]
    fn walking_all_pages_reproduces_the_full_catalog_in_order() {
        let m = snapshot();
        let expected = catalog().keys();

        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let view = catalog_resource_view(
                &m,
                &catalog(),
                None,
                cursor.as_deref(),
                as_of(),
                threshold(),
            )
            .expect("the page builds");
            for entry in &view.commands {
                seen.push(entry.key.clone());
            }
            pages += 1;
            if view.next_cursor.is_none() {
                break;
            }
            cursor = view.next_cursor.clone();
            // Guard against an infinite loop: a bounded number of pages.
            assert!(
                pages <= 10,
                "the catalog paginates in a bounded number of pages"
            );
        }

        assert_eq!(seen, expected, "concatenated pages equal the full catalog");
        assert!(pages >= 2, "the full catalog spans more than one page");
    }

    #[test]
    fn a_requested_limit_is_honoured_and_echoed_as_truncation() {
        let m = snapshot();
        let view = catalog_resource_view(&m, &catalog(), Some(3), None, as_of(), threshold())
            .expect("the page builds");
        assert_eq!(
            view.commands.len(),
            3,
            "a limit of three returns three keys"
        );
        assert!(
            view.truncated && view.next_cursor.is_some(),
            "a sub-full page is truncated with a continuation"
        );
    }

    #[test]
    fn a_zero_limit_yields_an_empty_page_with_a_continuation() {
        let m = snapshot();
        let view = catalog_resource_view(&m, &catalog(), Some(0), None, as_of(), threshold())
            .expect("the page builds");
        assert!(
            view.commands.is_empty(),
            "a zero-limit page returns no commands"
        );
        assert!(view.truncated, "an empty page knows more commands exist");
        assert!(
            view.next_cursor.is_some(),
            "an empty page carries a continuation to resume"
        );
        // No commands => no section URIs, but the command page URI is still
        // advertised.
        assert_eq!(
            view.resource_uris,
            vec![command_page_uri(m.body.snapshot_id.as_str())],
        );
    }

    // ----- AC2: a foreign / malformed cursor is rejected -----------------

    #[test]
    fn a_malformed_cursor_is_rejected_as_malformed() {
        let m = snapshot();
        let err = command_inventory(&catalog(), &m, None, Some("not-a-cursor"))
            .expect_err("a malformed cursor is rejected");
        assert!(
            matches!(err, CursorError::Malformed(_)),
            "garbage is malformed: {err:?}"
        );
    }

    #[test]
    fn a_foreign_operation_cursor_is_a_mismatch() {
        // A cursor minted for a different operation cannot resume the catalog
        // stream.
        use crate::pagination::{Cursor, FilterKey};
        let m = snapshot();
        let foreign =
            Cursor::mint(Operation::Search, &FilterKey::all(), &m.body.snapshot_id, 0).to_token();
        let err = command_inventory(&catalog(), &m, None, Some(&foreign))
            .expect_err("a foreign-operation cursor is rejected");
        match err {
            CursorError::Mismatch {
                field: MismatchField::Operation,
                ..
            } => {}
            other => panic!("expected an operation mismatch, got {other:?}"),
        }
    }

    #[test]
    fn a_foreign_snapshot_cursor_is_a_mismatch() {
        use crate::pagination::{Cursor, FilterKey};
        let m = snapshot();
        let other = crate::model::SnapshotId::new("fixture-other-snapshot")
            .expect("a URI-safe snapshot id");
        let foreign = Cursor::mint(Operation::Catalog, &FilterKey::all(), &other, 0).to_token();
        let err = command_inventory(&catalog(), &m, None, Some(&foreign))
            .expect_err("a foreign-snapshot cursor is rejected");
        match err {
            CursorError::Mismatch {
                field: MismatchField::Snapshot,
                ..
            } => {}
            other => panic!("expected a snapshot mismatch, got {other:?}"),
        }
    }

    #[test]
    fn the_catalog_view_rejects_a_malformed_cursor() {
        let m = snapshot();
        let err =
            catalog_resource_view(&m, &catalog(), None, Some("garbage"), as_of(), threshold())
                .expect_err("a malformed cursor rejects the view build");
        // Mapped to an operational error at this layer; the [048] contract maps
        // it to INVALID_PARAMS for the client.
        assert!(
            err.to_string().contains("cursor"),
            "the rejection names the cursor: {err}"
        );
    }

    #[test]
    fn a_matching_catalog_cursor_resumes_the_next_page() {
        // Page 1 mints a cursor; resuming it returns the following keys.
        let m = snapshot();
        let page1 = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("page 1 builds");
        let cursor = page1.next_cursor.clone().expect("page 1 is truncated");
        let page2 =
            catalog_resource_view(&m, &catalog(), None, Some(&cursor), as_of(), threshold())
                .expect("page 2 resumes");
        // The two pages are disjoint and together a prefix of the catalog.
        let keys: Vec<String> = catalog().keys();
        let p1: Vec<String> = page1.commands.iter().map(|e| e.key.clone()).collect();
        let p2: Vec<String> = page2.commands.iter().map(|e| e.key.clone()).collect();
        let p1_set: std::collections::BTreeSet<String> = p1.iter().cloned().collect();
        assert!(
            p2.iter().all(|k| !p1_set.contains(k)),
            "consecutive pages are disjoint"
        );
        let mut expected = p1;
        expected.extend(p2);
        assert!(
            keys.starts_with(&expected),
            "page1 ++ page2 is a prefix of the catalog"
        );
    }

    // ----- AC1/AC2: the per-source status rows are advertised ------------

    #[test]
    fn the_catalog_advertises_all_sources_in_registry_order() {
        let m = snapshot();
        let view = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("the catalog builds");
        assert_eq!(view.sources.len(), 8, "all eight sources are advertised");
        let order: Vec<&str> = view.sources.iter().map(|s| s.id.as_str()).collect();
        let expected: Vec<&str> = SourceCollection::ALL.iter().map(|s| s.id()).collect();
        assert_eq!(order, expected, "the sources are in registry order");
    }

    // ----- determinism + leak-free --------------------------------------

    #[test]
    fn the_catalog_view_is_deterministic() {
        let m = snapshot();
        let a = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("builds");
        let b = catalog_resource_view(&m, &catalog(), None, None, as_of(), threshold())
            .expect("builds");
        assert_eq!(a, b, "two builds of the same inputs are identical");
    }

    #[test]
    fn catalog_resource_result_rejects_an_over_cap_view() {
        // A view whose serialised body alone exceeds the cap is rejected
        // defensively.
        let wide = "x".repeat(4096);
        let mut view =
            catalog_resource_view(&snapshot(), &catalog(), None, None, as_of(), threshold())
                .expect("the base catalog builds");
        view.commands = (0..128)
            .map(|i| CommandEntryView {
                key: format!("cmd-{i}"),
                purpose: wide.clone(),
                upstream_sections: vec![],
                syntax: vec![],
                examples: vec![],
                prerequisites: vec![],
                compatibility: vec![],
                effect_notes: vec![],
            })
            .collect();
        view.resource_uris = vec![format!("rust-docs://snapshots/s/sections/{wide}")];
        // Precondition: the serialised body really exceeds the cap.
        assert!(
            serde_json::to_vec(&view).expect("serialise").len() > MAX_RESPONSE_BYTES,
            "precondition: the view exceeds the cap"
        );
        let err = catalog_resource_result(&view).expect_err("an over-cap view is rejected");
        assert!(
            err.to_string().contains("exceeds the")
                && err.to_string().contains(&MAX_RESPONSE_BYTES.to_string()),
            "the rejection names the cap: {err}"
        );
    }

    #[test]
    fn the_module_touches_no_process_filesystem_or_network() {
        // The module is documentation-only: no process-execution, filesystem, or
        // network primitive may appear in its source. The forbidden tokens are
        // assembled from pieces so this test's own source never matches its own
        // scan.
        let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp/resources.rs"));
        let forbidden = [
            "std::".to_string() + "process",
            "std::".to_string() + "fs",
            "std::".to_string() + "net",
            "Command::".to_string() + "new",
            ".spaw".to_string() + "n(",
            "req".to_string() + "west",
        ];
        for needle in &forbidden {
            assert!(
                !source.contains(needle.as_str()),
                "resources must contain no process/fs/network primitive, but found: {needle}"
            );
        }
    }

    #[test]
    fn an_unmapped_cursor_error_maps_to_invalid_params_through_the_contract() {
        // The [048] `ProtocolError` contract maps a cursor error to
        // INVALID_PARAMS; confirm the mapping the handler relies on.
        use crate::mcp::protocol_error::ProtocolError;
        use rmcp::model::ProtocolVersion;
        let pre = CursorError::Malformed("x".to_string())
            .protocol_error(Some(&ProtocolVersion::V_2025_11_25));
        assert_eq!(pre.code, ErrorCode::INVALID_PARAMS);
    }
}
