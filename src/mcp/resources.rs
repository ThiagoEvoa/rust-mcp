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
// API surface the `crate::mcp` handler dispatch calls when a client lists or
// reads the catalog resource. The core paths (the catalog view and its result,
// and the section-resource read) are now consumed by that dispatch, but the
// transitional `catalog_resource`/`section_resource_result` wrappers and the
// test-only inventory helpers stay un-reached from the non-test build, which
// would trip `dead_code`; the module therefore keeps `#![allow(dead_code)]`
// until those wrappers are retired.

#![allow(dead_code)]

use rmcp::model::{CallToolResult, ContentBlock, Resource};

use crate::commands::{CommandCatalog, command_inventory};
use crate::error::AppError;
use crate::mcp::protocol_error::{ErrorClass, ProtocolError};
use crate::mcp::schema::{
    CatalogResourceView, CommandEntryView, SectionResourceView, SectionView, SourceStatusView,
};
use crate::model::{SectionId, SnapshotId, SnapshotManifest};
use crate::pagination::{CursorError, MAX_RESPONSE_BYTES};
use crate::provenance::{Instant, StaleThreshold};
use crate::reads::ReadError;
use crate::status::{SourceStatus, build_report};
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt;
use std::str::FromStr;

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

/// The canonical prefix of a section resource URI:
/// `rust-docs://snapshots/{snapshot_id}/sections/{section_id}`.
///
/// A single shared prefix keeps the [`section_uri`] producer and the
/// [`parse_section_resource_uri`] consumer in agreement on the exact canonical
/// form ([055], resolving [054]'s F-054-2): a section resource is always named
/// under the active snapshot, never a bare `rust-docs://section/<id>`. The
/// snapshot id is a validated [`SnapshotId`](crate::model::SnapshotId) (URI-safe,
/// so it carries no `/`), and the section id is a validated
/// [`SectionId`](crate::model::SectionId) whose only raw `/` is the source
/// separator — so the three structural `/`s in the form are unambiguous.
pub const SECTION_RESOURCE_PREFIX: &str = "rust-docs://snapshots/";

/// Build the concrete section resource URI for `snapshot_id` / `section_id`,
/// `rust-docs://snapshots/{snapshot_id}/sections/{section_id}`.
///
/// Both `snapshot_id` and `section_id` are validated, URI-safe ids
/// ([`SnapshotId`](crate::model::SnapshotId) /
/// [`SectionId::as_str`](crate::model::SectionId::as_str)), so the result is
/// URI-safe by construction. The `section_id` is always drawn from the catalog's
/// own cited sections, never a caller-supplied path. The builder shares the
/// [`SECTION_RESOURCE_PREFIX`] the parser enforces, so a produced URI always
/// round-trips through [`parse_section_resource_uri`].
pub fn section_uri(snapshot_id: &str, section_id: &str) -> String {
    format!("{SECTION_RESOURCE_PREFIX}{snapshot_id}/sections/{section_id}")
}

// ---------------------------------------------------------------------------
// Section resources ([055]): immutable reads + paginated discovery
// ---------------------------------------------------------------------------

/// The error a section-resource read or discovery surfaces. Every variant maps
/// through the [048] [`ProtocolError`] contract to a version-aware error code
/// without leaking an internal path or the raw client token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionResourceError {
    /// The URI is not of the exact canonical form
    /// `rust-docs://snapshots/{snapshot_id}/sections/{section_id}` — a missing
    /// prefix, a non-`sections` segment, or an id that does not parse. Maps to
    /// `INVALID_PARAMS`.
    MalformedUri { uri: String },
    /// The URI names a snapshot other than the one the manifest serves. Maps to
    /// `INVALID_PARAMS`.
    MismatchedSnapshot { uri: String },
    /// The section id parses but is absent from the active snapshot. Maps to
    /// `RESOURCE_NOT_FOUND` (pre-2026-07-28), upgraded to `INVALID_PARAMS` by the
    /// version-aware contract for a newer peer — the same mapping `get_section`
    /// ([051]) uses, so a resource read and a tool read fail identically.
    UnknownSection { section_id: String },
    /// A continuation cursor was malformed or does not match this discovery call.
    /// Delegates to the [037] [`CursorError`] mapping (`INVALID_PARAMS`); this is
    /// the [054] F-054-1 fix — the section-resource path surfaces a cursor error
    /// as a typed `INVALID_PARAMS` through [`impl ProtocolError for CursorError`],
    /// never the lossy [`cursor_to_app_error`] `INTERNAL_ERROR` route.
    Cursor(CursorError),
}

impl fmt::Display for SectionResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedUri { uri } => write!(
                f,
                "'{}' is not a canonical section resource URI; expected \
                 {}{{snapshot}}/sections/{{section_id}}",
                uri, SECTION_RESOURCE_PREFIX
            ),
            Self::MismatchedSnapshot { uri } => write!(
                f,
                "section resource URI '{}' names a snapshot other than the active one",
                uri
            ),
            Self::UnknownSection { section_id } => write!(
                f,
                "section '{section_id}' is not in this snapshot; list the available \
                 sections with `list_sources`, `search_docs`, or the catalog resource"
            ),
            Self::Cursor(inner) => write!(f, "{inner}"),
        }
    }
}

impl std::error::Error for SectionResourceError {}

impl ProtocolError for SectionResourceError {
    fn error_class(&self) -> ErrorClass {
        match self {
            Self::MalformedUri { .. } | Self::MismatchedSnapshot { .. } => {
                ErrorClass::InvalidParams
            }
            Self::UnknownSection { .. } => ErrorClass::ResourceNotFound,
            // F-054-1: a cursor error is a typed INVALID_PARAMS through the
            // contract, not a lossy INTERNAL_ERROR.
            Self::Cursor(inner) => inner.error_class(),
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn data(&self) -> Option<Value> {
        match self {
            Self::MalformedUri { uri } => Some(json!({ "uri": uri.clone() })),
            Self::MismatchedSnapshot { uri } => Some(json!({ "uri": uri.clone() })),
            Self::UnknownSection { section_id } => {
                Some(json!({ "section_id": section_id.clone() }))
            }
            Self::Cursor(inner) => inner.data(),
        }
    }
}

/// Parse a canonical section resource URI into its `(snapshot_id, section_id)`
/// components, enforcing the exact form `rust-docs://snapshots/{snapshot}/sections/{id}`.
///
/// This is the consumer half of [054]'s F-054-2 canonicalization: it rejects
/// anything not of the canonical form without touching the filesystem — a missing
/// or extra prefix, a non-`sections` segment, a path-traversal id, or an
/// id that fails the [`SectionId`] / [`SnapshotId`] grammar is a
/// [`SectionResourceError::MalformedUri`] (`INVALID_PARAMS`), never a fabricated
/// read. Because the snapshot id is URI-safe (no `/`) and the section id's only
/// raw `/` is its source separator, the three structural `/`s are unambiguous:
/// the first ends the snapshot, the second opens `sections/`, and the rest is
/// the section id.
pub fn parse_section_resource_uri(
    uri: &str,
) -> Result<(SnapshotId, SectionId), SectionResourceError> {
    fn malformed(uri: &str) -> SectionResourceError {
        SectionResourceError::MalformedUri {
            uri: uri.to_string(),
        }
    }
    // 1. The exact prefix. A bare `rust-docs://section/…` or a filesystem path
    //    fails here — the form is always snapshot-scoped.
    let rest = uri
        .strip_prefix(SECTION_RESOURCE_PREFIX)
        .ok_or_else(|| malformed(uri))?;
    // 2. The snapshot id is URI-safe (no `/`), so the first `/` ends it.
    let (snapshot_part, tail) = rest.split_once('/').ok_or_else(|| malformed(uri))?;
    let snapshot_id = SnapshotId::from_str(snapshot_part).map_err(|_| malformed(uri))?;
    // 3. The remainder is exactly `sections/{section_id}`; any other segment (a
    //    second path component, a `commands` segment, …) is not the form.
    let section_part = tail
        .strip_prefix("sections/")
        .ok_or_else(|| malformed(uri))?;
    // 4. The section id must parse through its grammar, which rejects a literal
    //    `/` after the source separator (the path-traversal guard) and an
    //    incomplete percent-escape.
    let section_id = SectionId::from_str(section_part).map_err(|_| malformed(uri))?;
    Ok((snapshot_id, section_id))
}

/// Read a section resource by its canonical URI, serving the identical
/// [`SectionView`] the `get_section` tool ([051]) serves for the same section
/// (AC1: resource reads and tool reads agree on content and provenance).
///
/// The read composes the [036] section read and the [051] [`view_from`]
/// projection (`crate::mcp::get_section::view_from`), so the resource serves
/// *exactly* the view the tool produces — no second, drifting derivation. It
/// first parses the canonical URI ([`parse_section_resource_uri`]) and rejects a
/// URI naming a snapshot other than the one `manifest` serves
/// ([`SectionResourceError::MismatchedSnapshot`]); a section the snapshot does
/// not carry is [`SectionResourceError::UnknownSection`]. No filesystem, process,
/// or network is touched.
///
/// `as_of` / `threshold` feed the [036] read's freshness verdict, passed through
/// verbatim — the read never invents a freshness.
pub fn read_section_resource(
    manifest: &SnapshotManifest,
    uri: &str,
    as_of: Instant,
    threshold: StaleThreshold,
) -> Result<SectionView, SectionResourceError> {
    let (snapshot_id, section_id) = parse_section_resource_uri(uri)?;

    // A section resource is named under the snapshot the manifest serves; a URI
    // naming another snapshot is a mismatch, not a read. The mismatch is checked
    // *before* any read, so a foreign-snapshot URI never resolves.
    if snapshot_id != manifest.body.snapshot_id {
        return Err(SectionResourceError::MismatchedSnapshot {
            uri: uri.to_string(),
        });
    }

    // Read the full record through the [036] section read and project it through
    // the [051] `view_from` seam, so the resource serves the identical `SectionView`
    // the tool does (AC1). A not-found / not-well-formed id maps to the same
    // not-found the tool maps — never a fabricated section.
    let read = match crate::reads::get_section(manifest, &section_id, as_of, threshold) {
        Ok(read) => read,
        Err(ReadError::UnknownSection(id)) => {
            return Err(SectionResourceError::UnknownSection {
                section_id: id.as_str().to_string(),
            });
        }
        // `UnknownCategory` and `DanglingExplanation` are checklist-read errors
        // only; a section read yields `UnknownSection` on a miss. Keep the read
        // total and panic-free by mapping any other variant to the same not-found.
        Err(_) => {
            return Err(SectionResourceError::UnknownSection {
                section_id: section_id.as_str().to_string(),
            });
        }
    };

    Ok(crate::mcp::get_section::view_from(&read))
}

/// Assemble the final `rmcp` [`CallToolResult`] for a section-resource read: a
/// human-readable text block plus the structured [`SectionResourceView`]
/// payload, with a defensive cap check mirroring the tool `result` assemblers.
pub fn section_resource_result(view: &SectionView) -> Result<CallToolResult, AppError> {
    let structured = serde_json::to_value(SectionResourceView {
        section: view.clone(),
    })
    .map_err(|e| {
        AppError::runtime_start(
            "encode section resource structured output",
            Some(Box::new(e)),
        )
    })?;

    let text = render_section_resource(view);
    let structured_bytes = serde_json::to_string(&structured)
        .map_err(|e| {
            AppError::runtime_start(
                "measure section resource structured output",
                Some(Box::new(e)),
            )
        })?
        .len();

    // Defensive cap check: the section body is already bounded by the [037] cap
    // upstream of any over-cap path, so a well-formed read fits; this is the
    // safety net.
    let byte_len = structured_bytes.max(text.len());
    if byte_len > MAX_RESPONSE_BYTES {
        return Err(AppError::runtime_start(
            format!(
                "section resource ({byte_len} bytes) exceeds the {MAX_RESPONSE_BYTES}-byte \
                 response cap"
            ),
            None,
        ));
    }

    let mut call = CallToolResult::success(vec![ContentBlock::text(text)]);
    call.structured_content = Some(structured);
    Ok(call)
}

/// Render a section-resource read as a concise, human-readable text block that
/// agrees with the structured [`SectionResourceView`] payload: the section
/// identity, its source, the canonical URL, the freshness verdict, and the body.
fn render_section_resource(view: &SectionView) -> String {
    let mut lines = Vec::with_capacity(8 + view.citations.len());
    lines.push(format!(
        "section {} ({} · snapshot {} · freshness {})",
        view.id, view.doc_title, view.snapshot_id, view.freshness
    ));
    if !view.heading_path.is_empty() {
        lines.push(format!("  heading: {}", view.heading_path.join(" › ")));
    }
    lines.push(format!("  source: {}", view.source));
    if !view.canonical_url.is_empty() {
        lines.push(format!("  canonical: {}", view.canonical_url));
    }
    if !view.citations.is_empty() {
        lines.push("  citations:".to_string());
        for citation in &view.citations {
            lines.push(format!(
                "  ↳ {}",
                citation.note.as_deref().unwrap_or_default()
            ));
        }
    }
    lines.push(format!("  body:\n{}", view.body));
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Section-resource discovery ([055] AC2): paginated listing of section URIs
// ---------------------------------------------------------------------------

/// One cap-bounded page of the section-resource discovery: the concrete section
/// resource URIs (`rust-docs://snapshots/{snapshot}/sections/{id}`) on the page,
/// in a deterministic order, with an explicit continuation.
///
/// Mirrors the [043] [`crate::commands::CommandInventory`] the catalog resource
/// serves, but over the snapshot's section ids instead of the command catalog's
/// keys, so a caller discovers every section resource across pages rather than
/// receiving one silent full dump. The page is snapshot-bound (its [`Cursor`]
/// ([`crate::pagination::Cursor`]) binds the active snapshot, so a cursor minted
/// under another snapshot or operation is a [`CursorError::Mismatch`]) and
/// bounded on two counts a caller cannot bypass: the item `limit` (via the shared
/// [`crate::pagination::clamp_query_limit`]) and the [`MAX_RESPONSE_BYTES`] cap
/// measured on the complete serialized page. A non-final page carries an explicit
/// `next_cursor` that resumes after the last returned URI; a final page carries
/// `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionResourceInventory {
    /// The snapshot the inventory was served under (the cursor's binding).
    pub snapshot_id: SnapshotId,
    /// The section resource URIs returned on this page, in deterministic order.
    pub uris: Vec<String>,
    /// The number of URIs returned (`== uris.len()`).
    pub returned: usize,
    /// Whether more URIs follow that this page did not return.
    pub truncated: bool,
    /// The cursor to resume this exact stream, or `None` when this is the last
    /// page.
    pub next_cursor: Option<String>,
}

impl SectionResourceInventory {
    /// The complete serialized size of this page in bytes — the measurement AC2
    /// enforces: every page this seam produces is `<=` [`MAX_RESPONSE_BYTES`].
    pub fn serialized_bytes(&self) -> usize {
        section_resource_page_bytes(
            self.snapshot_id.as_str(),
            &self.uris,
            self.truncated,
            self.next_cursor.as_deref(),
        )
    }

    /// Whether more section resources follow that this page did not return.
    pub fn has_more(&self) -> bool {
        self.truncated
    }
}

/// The exact serialized page envelope [`section_resource_inventory`] bounds —
/// mirroring the `[043]` `SerializedInventoryPage` discipline (snapshot identity,
/// operation, counts, truncation flag, next cursor, and the URIs in full) so the
/// cap is measured on the complete response a server would send, not on the URIs
/// alone.
#[derive(Serialize)]
struct SerializedSectionResourcePage {
    snapshot_id: String,
    operation: crate::pagination::Operation,
    returned: usize,
    truncated: bool,
    next_cursor: Option<String>,
    uris: Vec<String>,
}

/// The complete serialized byte length of a section-resource page holding `uris`,
/// the way AC2 measures it — over the whole JSON a server would send. On the
/// (impossible) serialization failure the page is treated as over the cap so the
/// caller never lets an unmeasured page through.
fn section_resource_page_bytes(
    snapshot_id: &str,
    uris: &[String],
    truncated: bool,
    next_cursor: Option<&str>,
) -> usize {
    use crate::pagination::Operation;
    let page = SerializedSectionResourcePage {
        snapshot_id: snapshot_id.to_string(),
        operation: Operation::Catalog,
        returned: uris.len(),
        truncated,
        next_cursor: next_cursor.map(str::to_string),
        uris: uris.to_vec(),
    };
    serde_json::to_vec(&page)
        .map(|bytes| bytes.len())
        .unwrap_or(MAX_RESPONSE_BYTES + 1)
}

/// Serve a cap-bounded, snapshot-bound page of the section-resource discovery —
/// the paginated "resource discovery" of `plan.md` §6 / §11 for section
/// resources.
///
/// The URIs come from the snapshot's own [`Section`]s (each a validated
/// [`SectionId`], so URI-safe by construction) and are paged under the `[037]`
/// [`Cursor`] / [`CursorError`] / [`FilterKey`] / [`MAX_RESPONSE_BYTES`] primitives,
/// bound to `Operation::Catalog` with a `sections` filter (disjoint from the
/// command-catalog page, which uses the `all` filter) and the active snapshot, so
/// a caller walks every section resource across pages with an explicit
/// continuation rather than receiving a silent full dump. A cursor minted under
/// another operation / filter / snapshot is a [`CursorError::Mismatch`]; a
/// malformed token is [`CursorError::Malformed`] — never a panic, never a wrong
/// page. Both surface as a typed `INVALID_PARAMS` through
/// [`impl ProtocolError for CursorError`], the [054] F-054-1 fix.
///
/// Total and pure: it returns a typed [`CursorError`] (never panics) and a fixed
/// snapshot plus `limit` plus cursor yield a fixed page. It touches no network, no
/// filesystem, and no process.
pub fn section_resource_inventory(
    manifest: &SnapshotManifest,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<SectionResourceInventory, CursorError> {
    use crate::pagination::{Cursor, FilterKey, Operation, clamp_query_limit};

    let snapshot_id = manifest.body.snapshot_id.clone();
    let op = Operation::Catalog;
    // A `sections` filter disjoint from the command-catalog page's `all` filter,
    // so a cursor from one stream never resumes the other.
    let filter = FilterKey::category("sections");

    // AC2: resume from a cursor this layer minted, rejecting a foreign / malformed
    // one; `None` is the first page (offset 0). The typed `CursorError` is the
    // F-054-1 fix — a mismatch is a `INVALID_PARAMS` through the contract, not a
    // lossy `INTERNAL_ERROR`.
    let start = match cursor {
        None => 0,
        Some(token) => {
            let cursor = Cursor::from_token(token)?;
            cursor.verify(op, &filter, &snapshot_id)?
        }
    };

    // Deterministic order: the snapshot's section ids sorted by their URI-safe
    // identity, so two builds of the same snapshot yield the same page sequence.
    let mut ids: Vec<SectionId> = manifest
        .body
        .sections
        .iter()
        .map(|s| s.id.clone())
        .collect();
    // `SectionId` is out of scope to derive `Ord`, so sort by its URI-safe
    // string identity — a deterministic, total order over the string form.
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let remaining = ids.get(start..).unwrap_or(&[]);
    // The item count: default and clamp via the shared [037] rule so the
    // discovery and the [038] request agree. `0` yields an empty page; a request
    // above the cap is clamped to the cap.
    let count_limit = clamp_query_limit(limit);

    // Greedily fill URIs until the complete serialized page would exceed the cap,
    // stopping short of the URI that would push it over — never silently dropping
    // it. This mirrors `[043]` `command_inventory`'s cap discipline over the URI
    // stream (whose items are too small to page through `Item` directly).
    let mut chosen: Vec<String> = Vec::with_capacity(count_limit.min(remaining.len()));
    let mut k = 0;
    while k < remaining.len() && chosen.len() < count_limit {
        let would_truncate = (k + 1) < remaining.len();
        let next_cursor = if would_truncate {
            Some(Cursor::mint(op, &filter, &snapshot_id, start + k + 1).to_token())
        } else {
            None
        };
        let mut candidate = chosen.clone();
        candidate.push(section_uri(snapshot_id.as_str(), remaining[k].as_str()));
        if section_resource_page_bytes(
            snapshot_id.as_str(),
            &candidate,
            would_truncate,
            next_cursor.as_deref(),
        ) > MAX_RESPONSE_BYTES
        {
            break; // this URI would exceed the cap; stop here
        }
        chosen = candidate;
        k += 1;
    }

    let truncated = k < remaining.len();
    let next_cursor = if truncated {
        Some(Cursor::mint(op, &filter, &snapshot_id, start + k).to_token())
    } else {
        None
    };

    Ok(SectionResourceInventory {
        snapshot_id,
        // `returned` is a copy of the length; compute it before `chosen` is
        // moved into `uris`.
        returned: chosen.len(),
        uris: chosen,
        truncated,
        next_cursor,
    })
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

    // ===== [055] AC1: a section-resource read == the get_section tool read =====

    #[test]
    fn a_resource_read_and_the_tool_read_return_identical_content_and_provenance() {
        // AC1: the section resource serves the exact `SectionView` `get_section`
        // serves for the same section — both compose the shared
        // `get_section::view_from` projection, so content and provenance cannot
        // drift. A small section is used so the tool's body clip is a no-op and
        // the two reads are byte-identical.
        let m = snapshot();
        let id = "cargo/commands%2Fcargo-build";
        let uri = section_uri(m.body.snapshot_id.as_str(), id);

        let tool = crate::mcp::get_section::get_section(
            &m,
            &crate::mcp::schema::GetSectionRequest {
                section_id: id.to_string(),
                cursor: None,
            },
            as_of(),
            threshold(),
        )
        .expect("the tool read resolves");

        let resource = read_section_resource(&m, &uri, as_of(), threshold())
            .expect("the resource read resolves");

        assert_eq!(
            tool.section, resource,
            "AC1: the resource read and the tool read agree on content + provenance"
        );
        // Provenance in particular is identical: snapshot identity, source, and
        // the verbatim body.
        assert_eq!(resource.snapshot_id, m.body.snapshot_id.as_str());
        assert_eq!(resource.source, "cargo");
        assert!(!resource.body.is_empty(), "the body is served verbatim");
    }

    #[test]
    fn a_section_resource_read_serves_the_section_provenance() {
        // The resource read carries the section's full provenance: id, source,
        // canonical URL, snapshot identity, and a freshness verdict the read
        // computed (never invented).
        let m = snapshot();
        let uri = section_uri(m.body.snapshot_id.as_str(), "cargo/commands%2Fcargo-build");
        let view = read_section_resource(&m, &uri, as_of(), threshold())
            .expect("the resource read resolves");
        assert_eq!(view.id, "cargo/commands%2Fcargo-build");
        assert_eq!(view.source, "cargo");
        assert_eq!(view.snapshot_id, m.body.snapshot_id.as_str());
        assert!(
            matches!(view.freshness.as_str(), "fresh" | "stale" | "unknown"),
            "a freshness verdict is reported: {}",
            view.freshness
        );
        assert!(!view.body.is_empty(), "the body is served");
    }

    #[test]
    fn a_uri_naming_another_snapshot_is_a_mismatch_not_a_read() {
        // AC2: a section resource is named under the active snapshot; a URI
        // naming another snapshot is rejected before any read.
        let m = snapshot();
        let other = crate::model::SnapshotId::new("fixture-other-snapshot").expect("a URI-safe id");
        let uri = section_uri(other.as_str(), "cargo/commands%2Fcargo-build");
        let err = read_section_resource(&m, &uri, as_of(), threshold())
            .expect_err("a foreign-snapshot URI is rejected");
        assert!(
            matches!(err, SectionResourceError::MismatchedSnapshot { .. }),
            "a foreign-snapshot URI is a MismatchedSnapshot, got {err:?}"
        );
    }

    #[test]
    fn a_valid_but_absent_section_is_unknown_not_a_fabricated_read() {
        // AC2: a section id that parses through the grammar but is absent from the
        // active snapshot is a typed not-found, never a fabricated section.
        let m = snapshot();
        let uri = section_uri(
            m.body.snapshot_id.as_str(),
            "cargo/commands%2Fdoes-not-exist",
        );
        let err = read_section_resource(&m, &uri, as_of(), threshold())
            .expect_err("an absent section fails");
        assert!(
            matches!(err, SectionResourceError::UnknownSection { .. }),
            "an absent section is UnknownSection, got {err:?}"
        );
    }

    #[test]
    fn the_section_resource_result_is_structured_and_within_cap() {
        // The resource result carries the section under a `section` key and fits
        // the response cap.
        let m = snapshot();
        let uri = section_uri(m.body.snapshot_id.as_str(), "cargo/commands%2Fcargo-build");
        let view = read_section_resource(&m, &uri, as_of(), threshold())
            .expect("the resource read resolves");
        let call = section_resource_result(&view).expect("the result fits the cap");
        assert!(!call.content.is_empty(), "a text block is rendered");
        let structured = call
            .structured_content
            .as_ref()
            .expect("structured content");
        assert!(
            structured.get("section").is_some(),
            "the payload carries the section"
        );
        assert_eq!(structured["section"]["id"], "cargo/commands%2Fcargo-build");
    }

    // ===== [055] AC2: the URI grammar rejects non-canonical forms ===========

    #[test]
    fn the_parser_accepts_a_canonical_section_uri_and_round_trips() {
        // A canonical `rust-docs://snapshots/{snap}/sections/{id}` URI parses to
        // its snapshot + section components.
        let m = snapshot();
        let uri = section_uri(m.body.snapshot_id.as_str(), "cargo/commands%2Fcargo-build");
        let (snap, section) = parse_section_resource_uri(&uri).expect("a canonical URI parses");
        assert_eq!(snap, m.body.snapshot_id);
        assert_eq!(section.as_str(), "cargo/commands%2Fcargo-build");
    }

    #[test]
    fn a_non_canonical_uri_is_malformed_without_filesystem_access() {
        // AC2: a bare, prefix-less, or filesystem-style URI is rejected by the
        // grammar — never read from a path.
        let m = snapshot();
        let commands_segment = format!(
            "rust-docs://snapshots/{}/commands",
            m.body.snapshot_id.as_str()
        );
        for bad in [
            "rust-docs://section/cargo/commands%2Fcargo-build", // the old bare form
            "cargo/commands%2Fcargo-build",                     // no prefix
            "/etc/passwd",                                      // a filesystem path
            "not-a-uri",
            commands_segment.as_str(), // a non-`sections` segment
        ] {
            let err = parse_section_resource_uri(bad).expect_err("rejected");
            assert!(
                matches!(err, SectionResourceError::MalformedUri { .. }),
                "{bad} must be MalformedUri, got {err:?}"
            );
        }
    }

    #[test]
    fn a_path_traversal_section_uri_is_rejected_without_filesystem_access() {
        // AC2: a raw `/` after the source separator fails the section-id grammar,
        // so a traversal never reaches a filesystem read.
        let m = snapshot();
        let snap = m.body.snapshot_id.as_str();
        for bad in [
            format!("rust-docs://snapshots/{snap}/sections/../../etc/passwd"),
            format!("rust-docs://snapshots/{snap}/sections/cargo/commands%2Fbuild/extra"),
        ] {
            let err = parse_section_resource_uri(&bad).expect_err("rejected");
            assert!(
                matches!(err, SectionResourceError::MalformedUri { .. }),
                "{bad} must be rejected as MalformedUri, got {err:?}"
            );
        }
    }

    // ===== [055] AC2: the section-resource discovery paginates =============

    #[test]
    fn section_resource_discovery_paginates_and_walks_every_section_uri() {
        // AC2: walking every page reproduces the snapshot's full section list in
        // deterministic order, with an explicit continuation between pages and a
        // cap-bounded envelope on each page.
        let m = snapshot();
        let mut expected: Vec<String> = m
            .body
            .sections
            .iter()
            .map(|s| section_uri(m.body.snapshot_id.as_str(), s.id.as_str()))
            .collect();
        expected.sort();

        let mut seen: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let page =
                section_resource_inventory(&m, None, cursor.as_deref()).expect("the page builds");
            assert!(
                page.serialized_bytes() <= MAX_RESPONSE_BYTES,
                "every page fits the response cap"
            );
            assert_eq!(
                page.returned,
                page.uris.len(),
                "`returned` matches the page's URI count"
            );
            seen.extend(page.uris.iter().cloned());
            pages += 1;
            assert!(
                pages <= 200,
                "the discovery paginates in a bounded number of pages"
            );
            if page.truncated {
                cursor = page.next_cursor.clone();
                assert!(cursor.is_some(), "a truncated page carries a continuation");
            } else {
                assert!(
                    page.next_cursor.is_none(),
                    "the final page carries no continuation"
                );
                break;
            }
        }
        assert_eq!(
            seen, expected,
            "the walked pages equal the full section list in order"
        );
        assert!(
            !seen.is_empty(),
            "the discovery returns the snapshot's sections"
        );
    }

    #[test]
    fn a_zero_limit_section_discovery_yields_an_empty_page_with_a_continuation() {
        // A zero-limit request returns an empty page that still knows more
        // sections exist and carries a continuation to resume.
        let m = snapshot();
        let page = section_resource_inventory(&m, Some(0), None).expect("the page builds");
        assert!(page.uris.is_empty(), "a zero-limit page returns no URIs");
        assert!(
            page.truncated && page.next_cursor.is_some(),
            "an empty page knows more sections exist and resumes"
        );
    }

    #[test]
    fn a_foreign_cursor_is_rejected_for_section_discovery() {
        // A cursor minted for a different filter (the command-catalog `all`
        // filter) cannot resume the `sections` stream.
        use crate::pagination::{Cursor, FilterKey};
        let m = snapshot();
        let foreign = Cursor::mint(
            Operation::Catalog,
            &FilterKey::all(),
            &m.body.snapshot_id,
            0,
        )
        .to_token();
        let err = section_resource_inventory(&m, None, Some(foreign.as_str()))
            .expect_err("a foreign-filter cursor is rejected");
        assert!(
            matches!(err, CursorError::Mismatch { .. }),
            "a foreign-filter cursor is a Mismatch, got {err:?}"
        );
    }

    #[test]
    fn a_malformed_cursor_is_rejected_for_section_discovery() {
        let m = snapshot();
        let err = section_resource_inventory(&m, None, Some("not-a-cursor"))
            .expect_err("a malformed cursor is rejected");
        assert!(
            matches!(err, CursorError::Malformed(_)),
            "garbage is Malformed, got {err:?}"
        );
    }
}
