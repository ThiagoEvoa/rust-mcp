//! Response-limit and snapshot-bound pagination — task `[037][M3]`.
//!
//! This module is the *response-shaping* layer that sits on top of the [034]
//! ranked [`crate::search::SectionIndex`] and the [036] section/checklist reads:
//! it turns a ranked/filtered result into a page whose **complete serialized
//! payload stays within the 32 KiB response cap** and whose continuation is a
//! **cursor bound to the operation, the filters, and the snapshot identity**.
//!
//! It is the seam `plan.md` §6 ("Retrieval and response rules") and §11
//! ("All tool responses respect the complete serialized size cap and make
//! continuation explicit") require, shared by `search_docs`, `get_api_checklist`,
//! and the section-continuation path (`get_section`), and anticipated by the
//! command catalog ([039]+). The MCP handlers that call it land in M4
//! ([048]+); until then the module is transitional (see the `#![allow(dead_code)]`
//! note), mirroring `crate::search` and `crate::reads`.
//!
//! ## Two acceptance criteria (`plan.md` §11, issue [#37])
//!
//! * **AC1 — the 32 KiB cap.** A page's *complete serialized* size — the JSON the
//!   server would actually send, *including* JSON escaping and the metadata
//!   envelope (snapshot identity, operation, count, truncation flag, next cursor)
//!   — stays within [`MAX_RESPONSE_BYTES`]. Content that would exceed the cap is
//!   paginated or clipped *with explicit truncation metadata*; a clipped example
//!   is never presented as complete.
//! * **AC2 — snapshot-bound cursors.** A cursor binds the operation, the filters,
//!   and the snapshot identity; a cursor minted for a different
//!   operation/filter/snapshot is rejected as a typed [`CursorError`], and a
//!   split section's continuation always carries the remainder, so a clipped
//!   example is never concealed.
//!
//! ## Total, pure, and offline
//!
//! Every entry point is **total** (returns, never panics): a malformed or
//! mismatched cursor, an over-bound query, or an over-bound body surfaces as a
//! typed error or an explicit continuation — never a panic or a silent clip. The
//! layer is **pure** (a fixed input yields a fixed page) and **offline** (no
//! network, filesystem, or process state): the only argument that decides a page
//! is the result it paginates plus the bound inputs. It is therefore testable
//! offline against hand-built fixtures, with no upstream ingestion or
//! real-client/agent trial.
//!
//! **Out of scope for [037]** (later work, deliberately *not* built here): the
//! command catalog ([039]+), the MCP tool/resource wiring ([048]+), and any
//! change to the [033]-[036] ranking, boosts, source filters, or reads — the
//! pagination layer *consumes* those seams and does not reshape them.

#![allow(dead_code)] // transitional: unwired until the [048]+ handlers

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{ChecklistEntry, SectionId, SnapshotId};
use crate::reads::SectionRead;
use crate::search::Hit;

// ---------------------------------------------------------------------------
// Response-cap and limit constants
// ---------------------------------------------------------------------------

/// The cap on a **complete serialized tool response**, in bytes.
///
/// `plan.md` §6: "Cap complete serialized tool responses at 32 KiB. Account for
/// JSON escaping and metadata." The cap is measured on the *actual* serialized
/// bytes a page would send (see [`ResponsePage::serialized_bytes`]) — not on a
/// raw body length — so JSON escaping and the metadata envelope all count. A page
/// that would exceed the cap is paginated or clipped *with explicit truncation
/// metadata* rather than silently truncated.
pub const MAX_RESPONSE_BYTES: usize = 32 * 1024;

/// The default number of search hits per response (`plan.md` §6: "Default to five
/// search hits").
pub const DEFAULT_HIT_LIMIT: usize = 5;

/// The hard cap on search hits per response (`plan.md` §6: "cap at twenty"). A
/// caller's requested limit is clamped to [`DEFAULT_HIT_LIMIT`..=MAX_HIT_LIMIT].
pub const MAX_HIT_LIMIT: usize = 20;

/// The cap on a search query, in UTF-8 bytes (`plan.md` §6: "Limit queries to
/// 1,000 UTF-8 bytes and reject blank queries").
pub const MAX_QUERY_BYTES: usize = 1000;

// ---------------------------------------------------------------------------
// Task [037] AC2: the cursor that binds operation / filters / snapshot
// ---------------------------------------------------------------------------

/// The operation a pagination cursor is minted for and must be resumed against.
///
/// `plan.md` §6 binds a cursor to its operation, so a cursor minted for
/// `search_docs` cannot be replayed against `get_api_checklist` and vice versa.
/// [`Operation::SectionContinuation`] is the `get_section` split-section path;
/// [`Operation::Catalog`] is the command-catalog stream ([039]+), reserved so the
/// cursor format already anticipates it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// The `search_docs` ranked-hit stream.
    Search,
    /// The `get_api_checklist` entry stream.
    Checklist,
    /// A split section's continuation stream (`get_section`).
    SectionContinuation,
    /// The command-catalog stream ([039]+); reserved by the cursor format.
    Catalog,
}

/// The canonical, comparable key of a call's **filter** — the part of a cursor's
/// binding that records *which* result stream the cursor belongs to.
///
/// A cursor is rejected as a mismatch unless this key (together with the
/// operation and snapshot) matches the call it is resumed against. The key is a
/// canonical string so two equivalent filters compare equal and two different
/// filters never do:
///
/// * [`FilterKey::all`] is the "no restriction" key (an omitted `source`, or all
///   checklist categories);
/// * a source restriction canonicalizes its ids the same way
///   [`crate::search::SourceFilter::sources`] does — sorted, de-duplicated,
///   comma-joined — so a filter the search layer restricts to is bound by the same
///   key;
/// * a checklist [`category`](FilterKey::category) and a
///   [`section`](FilterKey::section) continuation each get their own namespace, so
///   the three never collide with one another or with [`all`](FilterKey::all).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FilterKey(String);

impl FilterKey {
    /// The "no restriction" key: every source (an omitted `source`) or every
    /// checklist category.
    pub fn all() -> Self {
        Self("all".to_string())
    }

    /// The key for a single restricted source id.
    pub fn source(id: impl AsRef<str>) -> Self {
        Self(format!("source:{}", id.as_ref()))
    }

    /// The key for a set of restricted source ids.
    ///
    /// Mirrors [`crate::search::SourceFilter::sources`]: the ids are sorted and
    /// de-duplicated, and an empty iterable yields [`all`](FilterKey::all) (an
    /// empty restriction is "no sources named", i.e. every source), so the key a
    /// search binds always agrees with the filter it restricts to.
    pub fn sources(ids: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut set: std::collections::BTreeSet<_> = std::collections::BTreeSet::new();
        for id in ids {
            set.insert(id.as_ref().to_string());
        }
        if set.is_empty() {
            FilterKey::all()
        } else {
            Self(format!(
                "source:{}",
                set.into_iter().collect::<Vec<_>>().join(",")
            ))
        }
    }

    /// The key for a checklist category restriction.
    pub fn category(category: impl AsRef<str>) -> Self {
        Self(format!("category:{}", category.as_ref()))
    }

    /// The key binding a section-continuation cursor to the section it continues.
    pub fn section(section_id: &SectionId) -> Self {
        Self(format!("section:{}", section_id.as_str()))
    }

    /// Whether this is the "no restriction" key.
    pub fn is_all(&self) -> bool {
        self.0 == "all"
    }

    /// Borrow the canonical key string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The cursor-binding format version, and the marker that distinguishes a
/// `rust-docs-mcp` cursor from any other JSON a client might echo.
///
/// A token that does not carry the marker (or carries a version this build does
/// not recognize) is rejected as [`CursorError::Malformed`] rather than silently
/// reinterpreted — `plan.md` §6 "reject malformed or mismatched cursors".
const CURSOR_KIND: &str = "rmcp-cursor";
const CURSOR_VERSION: u8 = 1;

/// The structured fields a cursor binds. Serialized to the opaque token a client
/// receives; parsed and checked on resume.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CursorBinding {
    /// The format marker ([`CURSOR_KIND`]); a foreign token fails to match it.
    kind: String,
    /// The binding format version; a version this build does not know is
    /// [`CursorError::Malformed`].
    version: u8,
    /// The operation the cursor belongs to.
    op: Operation,
    /// The canonical filter key (see [`FilterKey`]).
    filter: String,
    /// The snapshot identity the cursor was minted under.
    snapshot: String,
    /// The resume position: an item index for [`Operation::Search`] /
    /// [`Operation::Checklist`], or a byte offset into the body for
    /// [`Operation::SectionContinuation`].
    offset: usize,
}

/// An opaque, server-issued pagination cursor.
///
/// Opaque to the client: it is a token the server mints and the client echoes
/// back on the next page; the client must not construct or parse it. Its
/// structure ([`CursorBinding`]) binds the **operation**, **filter**, and
/// **snapshot identity** the page belongs to plus the resume **offset**.
///
/// A cursor is minted by [`Cursor::mint`] (which never fails) and validated by
/// [`Cursor::from_token`] / [`Cursor::verify`]: a token that is not a well-formed
/// `rust-docs-mcp` cursor of this version is [`CursorError::Malformed`], and a
/// well-formed token whose operation/filter/snapshot differ from the call it is
/// resumed against is [`CursorError::Mismatch`]. Both are typed errors — never a
/// panic and never a silent wrong page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    binding: CursorBinding,
}

/// A typed cursor failure — either a token this build cannot read, or a
/// well-formed token that does not match the call it is resumed against.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CursorError {
    /// The token is not a well-formed `rust-docs-mcp` cursor of the version this
    /// build recognizes: it is not valid JSON, carries the wrong marker, carries a
    /// version this build does not know, or has an unexpected shape.
    #[error("malformed pagination cursor: {0}")]
    Malformed(String),
    /// The token is a well-formed cursor, but its **operation**, **filter**, or
    /// **snapshot** differs from the call it is being resumed against. A cursor
    /// from a different operation/filter/snapshot cannot continue this call.
    #[error("cursor {field} mismatch: expected {expected:?}, found {found:?}")]
    Mismatch {
        /// Which bound field disagreed.
        field: MismatchField,
        /// The value the *call* expected (what the resume was checked against).
        expected: String,
        /// The value the *cursor* carried.
        found: String,
    },
}

/// Which bound field a [`CursorError::Mismatch`] reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MismatchField {
    /// The operation disagreed.
    Operation,
    /// The filter disagreed.
    Filter,
    /// The snapshot identity disagreed.
    Snapshot,
}

impl MismatchField {
    /// A short, human name for use in error messages.
    fn name(&self) -> &'static str {
        match self {
            MismatchField::Operation => "operation",
            MismatchField::Filter => "filter",
            MismatchField::Snapshot => "snapshot",
        }
    }
}

impl fmt::Display for MismatchField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl Cursor {
    /// Mint a cursor for `op`/`filter`/`snapshot_id` resuming at `offset`. Never
    /// fails: it only serializes a well-formed binding.
    pub fn mint(
        op: Operation,
        filter: &FilterKey,
        snapshot_id: &SnapshotId,
        offset: usize,
    ) -> Self {
        Cursor {
            binding: CursorBinding {
                kind: CURSOR_KIND.to_string(),
                version: CURSOR_VERSION,
                op,
                filter: filter.as_str().to_string(),
                snapshot: snapshot_id.as_str().to_string(),
                offset,
            },
        }
    }

    /// The opaque token the server hands to a client: the compact JSON of the
    /// binding. Total over every well-formed [`Cursor`] (our bindings always
    /// serialize), so it never panics.
    pub fn to_token(&self) -> String {
        // Our binding is always well-formed, so serialization cannot fail; the
        // `expect` documents that invariant rather than hiding a real error.
        serde_json::to_string(&self.binding)
            .expect("a well-formed CursorBinding always serializes to JSON")
    }

    /// Parse and validate a client-echoed token into a [`Cursor`].
    ///
    /// Returns [`CursorError::Malformed`] for anything that is not a well-formed
    /// `rust-docs-mcp` cursor of the current version (invalid JSON, wrong marker,
    /// an unknown version, or an unexpected shape). Never panics on any input.
    pub fn from_token(token: &str) -> Result<Self, CursorError> {
        let binding: CursorBinding =
            serde_json::from_str(token).map_err(|_| CursorError::Malformed(token.to_string()))?;
        if binding.kind != CURSOR_KIND {
            return Err(CursorError::Malformed(token.to_string()));
        }
        if binding.version != CURSOR_VERSION {
            return Err(CursorError::Malformed(format!(
                "cursor version {} is not supported (this build knows version {CURSOR_VERSION})",
                binding.version
            )));
        }
        Ok(Cursor { binding })
    }

    /// Resume this cursor against the call identified by `op`/`filter`/`snapshot_id`,
    /// returning the resume offset.
    ///
    /// The cursor must already be well-formed (call [`from_token`] first, or mint
    /// it yourself). If any bound field differs, this returns a typed
    /// [`CursorError::Mismatch`] naming the field and both values; otherwise it
    /// returns the offset. Distinct from "no more results": a mismatch is a
    /// *foreign* cursor, while an exhausted stream is simply a call whose next
    /// cursor is `None`.
    pub fn verify(
        &self,
        op: Operation,
        filter: &FilterKey,
        snapshot_id: &SnapshotId,
    ) -> Result<usize, CursorError> {
        if self.binding.op != op {
            return Err(CursorError::Mismatch {
                field: MismatchField::Operation,
                expected: format!("{op:?}"),
                found: format!("{:?}", self.binding.op),
            });
        }
        if self.binding.filter != filter.as_str() {
            return Err(CursorError::Mismatch {
                field: MismatchField::Filter,
                expected: filter.as_str().to_string(),
                found: self.binding.filter.clone(),
            });
        }
        if self.binding.snapshot != snapshot_id.as_str() {
            return Err(CursorError::Mismatch {
                field: MismatchField::Snapshot,
                expected: snapshot_id.as_str().to_string(),
                found: self.binding.snapshot.clone(),
            });
        }
        Ok(self.binding.offset)
    }

    /// The resume offset this cursor was minted with.
    pub fn offset(&self) -> usize {
        self.binding.offset
    }

    /// The operation this cursor binds.
    pub fn operation(&self) -> Operation {
        self.binding.op
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_token())
    }
}

// ---------------------------------------------------------------------------
// Task [037] AC1: the 32 KiB cap, measured on the real serialized page
// ---------------------------------------------------------------------------

/// An item the [`paginate_items`] layer can paginate.
///
/// Each variant serializes to a stable, compact JSON object:
/// * [`Item::Hit`] wraps a [`crate::search::Hit`] (the `search_docs` stream),
/// * [`Item::Checklist`] wraps a [`crate::model::ChecklistEntry`]
///   (`get_api_checklist`),
/// * [`Item::Section`] wraps a [`crate::reads::SectionRead`] (the `get_section`
///   stream, already split so a clipped example is never concealed — see
///   [`clip_body`]).
///
/// The two large variants are boxed: a `SectionRead` can carry a section body
/// up to the response cap, so holding it unboxed would make every `Vec<Item>
///` page buffer as large as the biggest section. Boxing keeps the enum
/// pointer-sized per slot without adding a body copy — the bodies are already
/// heap-allocated — and the accessor arms auto-deref through the `Box`.
///
/// `Item` is the type the shared seam is parameterized over, so one
/// [`paginate_items`] serves all three streams ([034] hits, [036] checklist
/// entries, [036] section continuations) and the command catalog ([039]+).
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A `search_docs` hit.
    Hit(Hit),
    /// A `get_api_checklist` entry.
    Checklist(Box<ChecklistEntry>),
    /// A `get_section` read (one part of a possibly split section).
    Section(Box<SectionRead>),
}

impl Item {
    /// The item's compact JSON object, as the server would embed it in a page.
    ///
    /// Each variant is projected to a small, dedicated `Serialize` struct rather
    /// than requiring the source type to derive `Serialize`: the [034] [`Hit`] and
    /// the [036] [`SectionRead`] are internal read types, and widening them (or
    /// pulling [`Token`](crate::search::Token) into `Serialize`) would touch
    /// other milestones' contracts. A checklist [`ChecklistEntry`] is already
    /// `Serialize`, so it serializes directly. The projection carries exactly the
    /// fields a `search_docs` / `get_api_checklist` / `get_section` page must
    /// send, so the cap is measured on the real, escaping-and-metadata-bearing
    /// bytes.
    fn to_value(&self) -> serde_json::Value {
        match self {
            Item::Hit(h) => serde_json::to_value(HitValue {
                section_id: h.section_id.as_str().to_string(),
                score: h.score,
            })
            .expect("a projected Hit always serializes"),
            Item::Checklist(e) => {
                serde_json::to_value(e.as_ref()).expect("a ChecklistEntry always serializes")
            }
            Item::Section(s) => {
                let sec = &s.section;
                serde_json::to_value(SectionValue {
                    section_id: sec.id.as_str().to_string(),
                    source_id: sec.source_id.as_str().to_string(),
                    doc_title: sec.doc_title.clone(),
                    heading_path: sec.heading_path.clone(),
                    body: sec.body.clone(),
                    snapshot_id: s.snapshot_id.as_str().to_string(),
                })
                .expect("a projected SectionRead always serializes")
            }
        }
    }

    /// The section body for an [`Item::Section`] — the only item that may need
    /// clipping at the body level (see [`clip_body`]); `None` for the never-clipped
    /// [`Item::Hit`] / [`Item::Checklist`].
    pub fn body(&self) -> Option<&str> {
        match self {
            Item::Section(s) => Some(s.section.body.as_str()),
            _ => None,
        }
    }
}

/// The compact JSON projection of a [`crate::search::Hit`] for the page cap
/// measurement — the retrievable identity plus the ranking score. A dedicated
/// struct keeps the [034] [`Hit`] (and, transitively, [`Token`]) free of a
/// `Serialize` obligation.
#[derive(Serialize)]
struct HitValue {
    /// The retrievable section identity the hit resolves to.
    section_id: String,
    /// The deterministic BM25 score that ranks the hit.
    score: f64,
}

/// The compact JSON projection of a [`crate::reads::SectionRead`] for the page
/// cap measurement: the retrievable identity, the heading breadcrumb, the
/// Markdown body (the bulk the cap bounds and clips), and the snapshot identity.
///
/// This is a *projection* of the read, not the whole read — it carries exactly
/// the fields a `get_section` page must send, so the cap is measured on the real
/// escaping-and-metadata-bearing bytes without widening the [036] read's own type
/// to be `Serialize` (its [`crate::provenance::SourceFreshness`] is not
/// `Serialize`, and the read is a pure value a caller may hold by reference).
#[derive(Serialize)]
struct SectionValue {
    /// The section's identity.
    section_id: String,
    /// The source the section belongs to.
    source_id: String,
    /// The document title.
    doc_title: String,
    /// The heading breadcrumb, most-to-least specific.
    heading_path: Vec<String>,
    /// The Markdown body — the bulk the cap bounds and clips.
    body: String,
    /// The snapshot the read was served under.
    snapshot_id: String,
}

/// The exact JSON the server sends for a page — the object whose *byte length* is
/// the AC1 measurement. Every field a client needs to resume counts: the snapshot
/// identity, the operation, the returned count, the truncation flag, whether the
/// next item is too large to fit, the next cursor, and each item in full.
#[derive(Serialize)]
struct SerializedPage {
    snapshot_id: String,
    operation: Operation,
    returned: usize,
    truncated: bool,
    next_item_oversized: bool,
    next_cursor: Option<String>,
    items: Vec<serde_json::Value>,
}

/// A single, cap-bounded page of an item stream.
///
/// A page holds at most the items that keep the *complete serialized* response
/// within [`MAX_RESPONSE_BYTES`], and always carries the metadata a client needs
/// to resume: the snapshot identity it was served under, the operation it belongs
/// to, whether it was clipped, whether the next item is too large to fit (and so
/// must be clipped via [`clip_body`]), and the next cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponsePage {
    /// The snapshot the page was served under (provenance for the resume).
    pub snapshot_id: String,
    /// The operation this page belongs to.
    pub operation: Operation,
    /// The items returned on this page (each fits within the cap).
    pub items: Vec<Item>,
    /// The number of items returned (`== items.len()`).
    pub returned: usize,
    /// Whether more items follow that this page did not return.
    pub truncated: bool,
    /// Whether the *next* item, on its own, exceeds the cap and must be clipped
    /// via [`clip_body`] rather than returned whole. Distinct from
    /// [`truncated`](ResponsePage::truncated): a page can be truncated with a
    /// fittable next item, or empty with an oversized next item.
    pub next_item_oversized: bool,
    /// The cursor to resume this exact stream (operation/filter/snapshot), or
    /// `None` when this page is the last.
    pub next_cursor: Option<String>,
}

impl ResponsePage {
    /// The **complete serialized size** of this page in bytes — the measurement
    /// AC1 enforces: every page this module produces is `<= MAX_RESPONSE_BYTES`.
    pub fn serialized_bytes(&self) -> usize {
        serialized_page_bytes(
            self.operation,
            &self.snapshot_id,
            &self.items,
            self.truncated,
            self.next_item_oversized,
            self.next_cursor.as_deref(),
        )
    }
}

/// The exact serialized byte length of a page that holds `items`, measured the
/// way AC1 requires — over the whole JSON the server would send, including the
/// metadata envelope (snapshot identity, operation, counts, truncation flag,
/// the oversized-next-item flag) and any next cursor.
fn serialized_page_bytes(
    op: Operation,
    snapshot_id: &str,
    items: &[Item],
    truncated: bool,
    next_item_oversized: bool,
    next_cursor: Option<&str>,
) -> usize {
    let page = SerializedPage {
        snapshot_id: snapshot_id.to_string(),
        operation: op,
        returned: items.len(),
        truncated,
        next_item_oversized,
        next_cursor: next_cursor.map(str::to_string),
        items: items.iter().map(Item::to_value).collect(),
    };
    // Our items always serialize; on the (impossible) failure, assume the page is
    // over the cap so the caller never lets an unmeasured page through.
    serde_json::to_vec(&page)
        .map(|bytes| bytes.len())
        .unwrap_or(MAX_RESPONSE_BYTES + 1)
}

/// Paginate `items` into a cap-bounded [`ResponsePage`] for `op`/`filter`/
/// `snapshot_id`, resuming after the `cursor` (or from the start when `cursor` is
/// `None`). This is the single seam shared by `search_docs`, `get_api_checklist`,
/// and section continuations.
///
/// The page is bounded on two counts a client cannot bypass:
/// * a **limit** — the requested item count, defaulting to
///   [`DEFAULT_HIT_LIMIT`] and clamped to [`MAX_HIT_LIMIT`] (`0` yields an empty
///   page, any request above the cap is clamped to the cap);
/// * the **byte cap** — the page stops adding items once the *complete
///   serialized* page would exceed [`MAX_RESPONSE_BYTES`].
///
/// A non-empty, non-final page returns a [`next_cursor`](ResponsePage::next_cursor)
/// bound to this exact operation/filter/snapshot, resuming after the last returned
/// item; a final page returns `None`.
///
/// When the *next* item would not fit on its own, the page stops short of it and
/// sets [`next_item_oversized`](ResponsePage::next_item_oversized): the item is
/// **not** returned and **not** silently truncated — the caller serves it across
/// continuations via [`clip_body`], which is the "paginate/clip rather than
/// silently truncate" rule of AC1.
///
/// `cursor` must be a token this layer minted (or `None` for the first page): a
/// well-formed token for a **different** operation/filter/snapshot is a typed
/// [`CursorError::Mismatch`], and a malformed token is [`CursorError::Malformed`]
/// — never a panic, never a wrong page.
pub fn paginate_items(
    op: Operation,
    filter: &FilterKey,
    snapshot_id: &SnapshotId,
    items: &[Item],
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<ResponsePage, CursorError> {
    // AC2: resolve the resume cursor (rejecting a foreign/malformed one) to its
    // offset. `None` is the first page (offset 0).
    let start = match cursor {
        None => 0,
        Some(token) => {
            let c = Cursor::from_token(token)?;
            c.verify(op, filter, snapshot_id)?
        }
    };

    let remaining = items.get(start..).unwrap_or(&[]);
    // Effective item count: default to five, clamp to [0, 20]. A request of 0
    // yields an empty page; a request above the cap is clamped to the cap.
    let count_limit = limit.unwrap_or(DEFAULT_HIT_LIMIT).min(MAX_HIT_LIMIT);

    // Greedily fill up to `count_limit` items, stopping when the next item would
    // push the *complete serialized* page over the cap.
    let mut chosen: Vec<Item> = Vec::with_capacity(count_limit.min(remaining.len()));
    let mut k = 0;
    while k < remaining.len() && chosen.len() < count_limit {
        let would_truncate = (k + 1) < remaining.len();
        let next_cursor = if would_truncate {
            Some(Cursor::mint(op, filter, snapshot_id, start + k + 1).to_token())
        } else {
            None
        };
        let mut candidate = chosen.clone();
        candidate.push(remaining[k].clone());
        // The cap gate measures the candidate with the largest flag spelling
        // (`"false"` out-serializes `"true"`), so a candidate that passes here
        // also passes once the final page's `next_item_oversized` is filled in.
        if serialized_page_bytes(
            op,
            snapshot_id.as_str(),
            &candidate,
            would_truncate,
            false,
            next_cursor.as_deref(),
        ) > MAX_RESPONSE_BYTES
        {
            break; // this item would exceed the cap; stop here
        }
        chosen = candidate;
        k += 1;
    }

    let truncated = k < remaining.len();
    let next_cursor = if truncated {
        Some(Cursor::mint(op, filter, snapshot_id, start + k).to_token())
    } else {
        None
    };
    // Is the very next item, alone, too large to fit? If so, the caller clips it
    // via `clip_body` (AC1: paginate/clip, never silently truncate).
    let next_item_oversized = if truncated {
        let single = [remaining[k].clone()];
        let single_cursor = Some(Cursor::mint(op, filter, snapshot_id, start + k + 1).to_token());
        serialized_page_bytes(
            op,
            snapshot_id.as_str(),
            &single,
            true,
            false,
            single_cursor.as_deref(),
        ) > MAX_RESPONSE_BYTES
    } else {
        false
    };

    let returned = chosen.len();
    Ok(ResponsePage {
        snapshot_id: snapshot_id.as_str().to_string(),
        operation: op,
        items: chosen,
        returned,
        truncated,
        next_item_oversized,
        next_cursor,
    })
}

// ---------------------------------------------------------------------------
// Task [037] AC1/AC2: clip an over-cap body, carrying the remainder
// ---------------------------------------------------------------------------

/// Room reserved within the cap for a body part's metadata — the envelope plus the
/// continuation cursor token — so a clipped body part's *complete serialized*
/// size stays within the cap even though [`clip_body`] bounds only the body.
const BODY_METADATA_RESERVE: usize = 1024;

/// The first page of a body that is too large for one response: the leading
/// `body` that fits under the cap, plus a continuation that carries the remainder.
///
/// `truncated` is `true` with a [`next`](BodyPart::next) cursor exactly when a
/// remainder follows; resuming that cursor (slicing the body from the cursor's
/// offset and clipping again) yields the rest, and the concatenation of every
/// part is the original body. This is the "a split section must not conceal a
/// clipped example; the next part must carry the remainder" rule
/// (`plan.md` §6): a clipped example is never lost, and the continuation is bound
/// to the section's operation/filter/snapshot so a continuation from another
/// section/snapshot is rejected by [`Cursor::verify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyPart {
    /// The body returned on this part (bounded so its complete serialized size
    /// stays within the cap).
    pub body: String,
    /// Whether a remainder follows.
    pub truncated: bool,
    /// The continuation carrying the remainder (a cursor whose offset is the byte
    /// position of the remainder within the original body), or `None` when this
    /// part is the last.
    pub next: Option<Cursor>,
}

impl BodyPart {
    /// The complete serialized size of this body part (body + the continuation
    /// cursor + a small envelope) — the measurement the cap applies to.
    pub fn serialized_size(&self) -> usize {
        let page = SerializedBodyPart {
            body: self.body.clone(),
            truncated: self.truncated,
            next_cursor: self.next.as_ref().map(Cursor::to_token),
        };
        serde_json::to_vec(&page)
            .map(|b| b.len())
            .unwrap_or(MAX_RESPONSE_BYTES + 1)
    }
}

#[derive(Serialize)]
struct SerializedBodyPart {
    body: String,
    truncated: bool,
    next_cursor: Option<String>,
}

/// Clip `body` into its first cap-bounded part, returning a continuation that
/// carries the remainder when there is one.
///
/// The cut is on a **character boundary** (never splitting a multi-byte UTF-8
/// code point), and the returned body is bounded so the part's *complete
/// serialized* size stays within the cap. When a remainder exists, `next` is a
/// cursor bound to `op`/`filter`/`snapshot_id` whose offset is the byte position
/// of the remainder; resuming it re-clips the tail. Total: any body — empty,
/// single-line, multi-line, multi-byte — clips without panicking.
pub fn clip_body(
    body: &str,
    op: Operation,
    filter: &FilterKey,
    snapshot_id: &SnapshotId,
) -> BodyPart {
    let max_body = MAX_RESPONSE_BYTES.saturating_sub(BODY_METADATA_RESERVE);
    let cut = char_boundary_at_or_below(body, max_body);
    if cut >= body.len() {
        BodyPart {
            body: body.to_string(),
            truncated: false,
            next: None,
        }
    } else {
        BodyPart {
            body: body[..cut].to_string(),
            truncated: true,
            next: Some(Cursor::mint(op, filter, snapshot_id, cut)),
        }
    }
}

/// The largest index `<= max` that is a character boundary of `s`.
///
/// Total: walks back from `max` to the nearest char boundary (`0` is always one),
/// so it never returns a position that would split a multi-byte code point.
fn char_boundary_at_or_below(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

// ---------------------------------------------------------------------------
// Tests: total, pure, offline. No upstream, no real-client/agent trial.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SchemaVersion, SectionBuilder, SourceId, Url};
    use crate::provenance::{Instant, SourceFreshness, StaleThreshold};

    // The tests build real read types -- a [`Hit`], a real
    // [`ChecklistEntry`], and a [`SectionRead`] assembled through the
    // [`SectionBuilder`] -- so the pagination contract is asserted against the
    // exact types the [034]/[036] read types produce. A "thin" section carries
    // a small Markdown body; a "huge" one overflows the cap.
    //
    // Tests are organized by acceptance criterion; the cursor is verified against
    // a `MismatchField`, and each page is verified to stay within the cap.
    fn snap(id: &str) -> SnapshotId {
        SnapshotId::new(id).expect("a uri-safe snapshot id builds")
    }

    /// Build a well-formed section id under a fixed test source, from a
    /// URI-safe `label` used as the chapter path (the [036]/[007] grammar).
    fn section(label: &str) -> SectionId {
        SectionId::derive(
            &SourceId::new("cargo").expect("a registry id builds"),
            label,
            None,
            None,
        )
        .expect("a well-formed section id")
    }

    fn hit(label: &str, score: f64) -> Hit {
        Hit {
            section_id: section(label),
            score,
        }
    }

    fn entry(label: &str, category: &str) -> ChecklistEntry {
        ChecklistEntry {
            code: label.to_string(),
            title: format!("title for {label}"),
            category: category.to_string(),
            location: format!("{category}#{label}"),
            explanation_sections: vec![section("about")],
        }
    }

    /// A complete section read carrying the given Markdown `body` under
    /// `snapshot_id`, built through the real [`SectionBuilder`] so its `Section`
    /// has a consistent identity and content hash, and the freshness is the
    /// honest "unknown" verdict.
    fn read(label: &str, body: &str, snapshot_id: &str) -> SectionRead {
        let section = SectionBuilder::new(
            SourceId::new("cargo").expect("a registry id builds"),
            "Test doc",
            label,
            Url::new("https://doc.rust-lang.org/cargo/").expect("a valid URL"),
            body.to_string(),
        )
        .build()
        .expect("a well-formed section builds");
        SectionRead {
            section,
            snapshot_id: snap(snapshot_id),
            schema_version: SchemaVersion::V1,
            freshness: SourceFreshness {
                as_of: Instant::parse("2026-10-30T00:00:00Z").expect("a valid instant"),
                last_checked: None,
                threshold: StaleThreshold::default(),
                stale: None,
            },
        }
    }

    // --- AC2: the cursor binds operation / filter / snapshot --------------

    #[test]
    fn a_cursor_round_trips_through_its_token() {
        let c = Cursor::mint(Operation::Search, &FilterKey::all(), &snap("snap-a"), 7);
        let token = c.to_token();
        let back = Cursor::from_token(&token).expect("a minted token is well-formed");
        assert_eq!(back, c, "mint -> token -> parse must be identity");
        assert!(
            token.contains(CURSOR_KIND),
            "the token carries the marker: {token}"
        );
        assert_eq!(c.offset(), 7);
        assert_eq!(c.operation(), Operation::Search);
    }

    #[test]
    fn a_matching_cursor_verifies_to_its_offset() {
        let c = Cursor::mint(
            Operation::Checklist,
            &FilterKey::category("Error Handling"),
            &snap("snap-b"),
            3,
        );
        let offset = c
            .verify(
                Operation::Checklist,
                &FilterKey::category("Error Handling"),
                &snap("snap-b"),
            )
            .expect("an identical binding verifies");
        assert_eq!(offset, 3);
    }

    #[test]
    fn a_cursor_from_a_different_operation_is_a_mismatch() {
        let c = Cursor::mint(Operation::Search, &FilterKey::all(), &snap("snap"), 1);
        let err = c
            .verify(Operation::Checklist, &FilterKey::all(), &snap("snap"))
            .expect_err("a search cursor cannot resume a checklist call");
        assert_eq!(
            err,
            CursorError::Mismatch {
                field: MismatchField::Operation,
                expected: "Checklist".to_string(),
                found: "Search".to_string(),
            }
        );
    }

    #[test]
    fn a_cursor_from_a_different_filter_is_a_mismatch() {
        let c = Cursor::mint(
            Operation::Search,
            &FilterKey::source("cargo"),
            &snap("snap"),
            1,
        );
        let err = c
            .verify(
                Operation::Search,
                &FilterKey::source("clippy"),
                &snap("snap"),
            )
            .expect_err("a cargo-filtered cursor cannot resume a clippy call");
        match err {
            CursorError::Mismatch { field, .. } => assert_eq!(field, MismatchField::Filter),
            other => panic!("expected a filter mismatch, got {other:?}"),
        }
    }

    #[test]
    fn a_cursor_from_a_different_snapshot_is_a_mismatch() {
        let c = Cursor::mint(Operation::Search, &FilterKey::all(), &snap("snap-a"), 1);
        let err = c
            .verify(Operation::Search, &FilterKey::all(), &snap("snap-b"))
            .expect_err("a cursor from another snapshot cannot resume this one");
        match err {
            CursorError::Mismatch { field, .. } => assert_eq!(field, MismatchField::Snapshot),
            other => panic!("expected a snapshot mismatch, got {other:?}"),
        }
    }

    #[test]
    fn a_section_continuation_cursor_binds_its_section() {
        // A continuation for section A cannot be resumed against section B.
        let c = Cursor::mint(
            Operation::SectionContinuation,
            &FilterKey::section(&section("guides-book")),
            &snap("snap"),
            100,
        );
        let err = c
            .verify(
                Operation::SectionContinuation,
                &FilterKey::section(&section("reference")),
                &snap("snap"),
            )
            .expect_err("a continuation for one section cannot resume another");
        assert!(matches!(
            err,
            CursorError::Mismatch {
                field: MismatchField::Filter,
                ..
            }
        ));
    }

    #[test]
    fn a_non_json_token_is_malformed() {
        let err = Cursor::from_token("not-json-at-all").expect_err("garbage is malformed");
        assert!(matches!(err, CursorError::Malformed(_)));
    }

    #[test]
    fn a_json_object_without_our_marker_is_malformed() {
        // Valid JSON, but not a `rust-docs-mcp` cursor: the marker must match.
        let err = Cursor::from_token(r#"{"op":"search","offset":0}"#)
            .expect_err("a foreign object is malformed");
        assert!(matches!(err, CursorError::Malformed(_)));
    }

    #[test]
    fn a_cursor_of_an_unknown_version_is_malformed() {
        // A well-formed token of an old/future version is rejected, not reinterpreted.
        let old = CursorBinding {
            kind: CURSOR_KIND.to_string(),
            version: 99,
            op: Operation::Search,
            filter: "all".to_string(),
            snapshot: "snap".to_string(),
            offset: 0,
        };
        let token = serde_json::to_string(&old).unwrap();
        let err = Cursor::from_token(&token).expect_err("an unknown version is malformed");
        assert!(matches!(err, CursorError::Malformed(_)));
    }

    #[test]
    fn cursor_tokens_are_deterministic() {
        let a = Cursor::mint(
            Operation::Search,
            &FilterKey::sources(["cargo", "clippy"]),
            &snap("s"),
            2,
        );
        let b = Cursor::mint(
            Operation::Search,
            &FilterKey::sources(["clippy", "cargo"]),
            &snap("s"),
            2,
        );
        // The order of the ids does not matter: the canonical key sorts them.
        assert_eq!(a.to_token(), b.to_token());
        // Repeated minting of the same inputs is byte-identical.
        assert_eq!(a.to_token(), a.to_token());
    }

    // --- AC2: paginate_items rejects a foreign/malformed cursor -----------

    #[test]
    fn paginate_items_rejects_a_foreign_cursor() {
        let items = vec![Item::Hit(hit("ch1", 1.0))];
        let other =
            Cursor::mint(Operation::Checklist, &FilterKey::all(), &snap("snap"), 0).to_token();
        let err = paginate_items(
            Operation::Search,
            &FilterKey::all(),
            &snap("snap"),
            &items,
            None,
            Some(&other),
        )
        .expect_err("a checklist cursor cannot resume a search page");
        assert!(matches!(
            err,
            CursorError::Mismatch {
                field: MismatchField::Operation,
                ..
            }
        ));
    }

    #[test]
    fn paginate_items_rejects_a_malformed_cursor() {
        let items = vec![Item::Hit(hit("ch1", 1.0))];
        let err = paginate_items(
            Operation::Search,
            &FilterKey::all(),
            &snap("snap"),
            &items,
            None,
            Some("garbage"),
        )
        .expect_err("a malformed cursor is a typed error");
        assert!(matches!(err, CursorError::Malformed(_)));
    }

    // --- AC1: the default and the limit cap -------------------------------

    #[test]
    fn paginate_items_defaults_to_five_hits_and_truncates() {
        let items: Vec<Item> = (0..30)
            .map(|i| Item::Hit(hit("ch1", 1.0 + i as f64 * 0.01)))
            .collect();
        let page = paginate_items(
            Operation::Search,
            &FilterKey::all(),
            &snap("snap"),
            &items,
            None,
            None,
        )
        .expect("pagination is total");
        assert_eq!(page.returned, DEFAULT_HIT_LIMIT, "the default is five hits");
        assert!(page.truncated, "thirty hits truncate after the first five");
        assert!(
            page.next_cursor.is_some(),
            "a truncated page returns a cursor"
        );
        assert!(!page.next_item_oversized, "small hits are not oversized");
    }

    #[test]
    fn paginate_items_clamps_a_limit_above_the_cap() {
        let items: Vec<Item> = (0..50).map(|_| Item::Hit(hit("ch1", 1.0))).collect();
        let page = paginate_items(
            Operation::Search,
            &FilterKey::all(),
            &snap("snap"),
            &items,
            Some(100),
            None,
        )
        .expect("pagination is total");
        assert_eq!(
            page.returned, MAX_HIT_LIMIT,
            "a 100 limit is clamped to the cap"
        );
        assert!(page.truncated);
    }

    #[test]
    fn paginate_items_treats_a_zero_limit_as_an_empty_page() {
        let items = vec![Item::Hit(hit("ch1", 1.0))];
        let page = paginate_items(
            Operation::Search,
            &FilterKey::all(),
            &snap("snap"),
            &items,
            Some(0),
            None,
        )
        .expect("pagination is total");
        assert_eq!(page.returned, 0);
        assert!(page.items.is_empty());
        assert!(page.truncated, "an empty page still knows more items exist");
        assert!(
            page.next_cursor.is_some(),
            "a zero-limit page returns a cursor to resume"
        );
    }

    #[test]
    fn a_final_page_reports_no_truncation_and_no_cursor() {
        let items: Vec<Item> = (0..3).map(|_| Item::Hit(hit("ch1", 1.0))).collect();
        let page = paginate_items(
            Operation::Search,
            &FilterKey::all(),
            &snap("snap"),
            &items,
            None,
            None,
        )
        .expect("pagination is total");
        assert_eq!(page.returned, 3);
        assert!(!page.truncated);
        assert!(
            page.next_cursor.is_none(),
            "the last page has no next cursor"
        );
        assert!(!page.next_item_oversized);
    }

    // --- AC1: every page stays within the 32 KiB cap ---------------------

    #[test]
    fn every_page_stays_within_the_response_cap() {
        // Ten items of ~5 KiB each: enough that the default of five would exceed
        // the cap, forcing the byte cap to cut the page short.
        let big = "x".repeat(5 * 1024);
        let items: Vec<Item> = (0..10)
            .map(|i| Item::Section(Box::new(read(&format!("s{i}"), &big, "snap"))))
            .collect();
        // Walk the whole stream via the cursor and assert every page fits.
        let mut cursor: Option<String> = None;
        let mut total = 0usize;
        let mut pages = 0usize;
        loop {
            let page = paginate_items(
                Operation::Search,
                &FilterKey::all(),
                &snap("snap"),
                &items,
                None,
                cursor.as_deref(),
            )
            .expect("pagination is total");
            assert!(
                page.serialized_bytes() <= MAX_RESPONSE_BYTES,
                "page {pages} is {} bytes, over the {} byte cap",
                page.serialized_bytes(),
                MAX_RESPONSE_BYTES
            );
            total += page.returned;
            pages += 1;
            if page.truncated {
                cursor = page.next_cursor;
            } else {
                break;
            }
            assert!(pages <= 100, "the walk must terminate");
        }
        assert_eq!(total, 10, "the walk returns every item exactly once");
        assert!(pages >= 2, "ten 5 KiB items need at least two pages");
    }

    #[test]
    fn a_single_over_cap_item_is_not_returned_but_flagged_oversized() {
        // A body larger than the cap cannot be returned whole: the page is empty,
        // truncated, and flags the next item oversized so the caller clips it.
        let huge = "y".repeat(MAX_RESPONSE_BYTES + 4096);
        let items = vec![Item::Section(Box::new(read("huge", &huge, "snap")))];
        let page = paginate_items(
            Operation::Search,
            &FilterKey::all(),
            &snap("snap"),
            &items,
            None,
            None,
        )
        .expect("pagination is total");
        assert!(
            page.items.is_empty(),
            "an over-cap item is not returned whole"
        );
        assert!(page.truncated, "the item remains, to be clipped");
        assert!(
            page.next_item_oversized,
            "the over-cap item is flagged for clipping, not silently dropped"
        );
        assert!(page.next_cursor.is_some());
    }

    // --- AC1/AC2: clip_body carries the remainder (no concealed example) --

    #[test]
    fn clip_body_carries_the_remainder_so_nothing_is_concealed() {
        let body = "z".repeat(MAX_RESPONSE_BYTES + 3000);
        // Walk the body through its continuation until it is whole.
        let mut offset = 0usize;
        let mut reassembled = String::new();
        let mut parts = 0usize;
        loop {
            let tail = &body[offset..];
            let part = clip_body(
                tail,
                Operation::SectionContinuation,
                &FilterKey::section(&section("big")),
                &snap("snap"),
            );
            reassembled.push_str(&part.body);
            parts += 1;
            // Each part's complete serialized size stays within the cap.
            assert!(
                part.serialized_size() <= MAX_RESPONSE_BYTES,
                "part {parts} is {} bytes, over the cap",
                part.serialized_size()
            );
            if part.truncated {
                let next = part.next.expect("a truncated part has a continuation");
                // The continuation is bound to the same section/snapshot.
                assert_eq!(next.offset(), offset + part.body.len());
                assert!(
                    next.verify(
                        Operation::SectionContinuation,
                        &FilterKey::section(&section("big")),
                        &snap("snap")
                    )
                    .is_ok()
                );
                offset = next.offset();
                assert!(parts <= 100, "the walk must terminate");
            } else {
                break;
            }
        }
        assert_eq!(reassembled, body, "the parts reassemble the original body");
        assert!(parts >= 2, "an over-cap body yields at least two parts");
    }

    #[test]
    fn clip_body_never_splits_a_multibyte_code_point() {
        // '€' is 3 bytes; a cut on a char boundary keeps every code point intact.
        let body = "€".repeat(MAX_RESPONSE_BYTES + 1000);
        let part = clip_body(
            &body,
            Operation::SectionContinuation,
            &FilterKey::section(&section("big")),
            &snap("snap"),
        );
        assert!(part.truncated);
        assert!(
            part.body.chars().all(|c| c == '€'),
            "the cut is on a char boundary: no split code point"
        );
        assert!(part.body.len() < body.len(), "the body was clipped");
        // Reassembling the clipped head with the resumed tail is the original.
        let tail = &body[part.next.expect("a truncated part continues").offset()..];
        assert_eq!(part.body + tail, body);
    }

    #[test]
    fn clip_body_reports_no_continuation_when_it_fits() {
        let body = "small body".to_string();
        let part = clip_body(
            &body,
            Operation::SectionContinuation,
            &FilterKey::section(&section("s")),
            &snap("snap"),
        );
        assert!(!part.truncated);
        assert!(part.next.is_none(), "a fitting body has no continuation");
        assert_eq!(part.body, body);
    }

    #[test]
    fn an_empty_body_clips_to_one_part() {
        let part = clip_body(
            "",
            Operation::SectionContinuation,
            &FilterKey::all(),
            &snap("snap"),
        );
        assert!(!part.truncated);
        assert!(part.body.is_empty());
        assert!(part.next.is_none());
    }

    #[test]
    fn a_continuation_from_another_snapshot_is_rejected() {
        // A body clipped under snapshot A cannot be resumed under snapshot B.
        let body = "q".repeat(MAX_RESPONSE_BYTES + 1000);
        let part = clip_body(
            &body,
            Operation::SectionContinuation,
            &FilterKey::section(&section("big")),
            &snap("snap-a"),
        );
        let cursor = part.next.expect("the part is truncated");
        let err = cursor
            .verify(
                Operation::SectionContinuation,
                &FilterKey::section(&section("big")),
                &snap("snap-b"),
            )
            .expect_err("a continuation from snapshot A cannot resume under B");
        assert!(matches!(
            err,
            CursorError::Mismatch {
                field: MismatchField::Snapshot,
                ..
            }
        ));
    }

    #[test]
    fn paginate_then_resume_with_the_returned_cursor_is_consistent() {
        // Walk the whole stream through the cursor each page hands back: a page
        // returned under the default five-item limit of twelve items spans
        // several pages, and every page's cursor is accepted by the next.
        let items: Vec<Item> = (0..12)
            .map(|i| Item::Hit(hit("ch1", 1.0 + i as f64)))
            .collect();
        let mut cursor: Option<String> = None;
        let mut all: Vec<f64> = Vec::new();
        let mut pages = 0usize;
        loop {
            let page = paginate_items(
                Operation::Search,
                &FilterKey::all(),
                &snap("snap"),
                &items,
                None,
                cursor.as_deref(),
            )
            .expect("a page is total");
            for it in &page.items {
                match it {
                    Item::Hit(h) => all.push(h.score),
                    other => panic!("expected hits, got {other:?}"),
                }
            }
            pages += 1;
            if page.truncated {
                cursor = page.next_cursor;
            } else {
                break;
            }
            assert!(pages <= 100, "the walk must terminate");
        }
        // The pages together return every item exactly once, in order.
        assert_eq!(all, (0..12).map(|i| 1.0 + i as f64).collect::<Vec<_>>());
        assert!(
            pages >= 2,
            "twelve items at the default limit need at least two pages"
        );
    }

    // --- AC1/AC2: a checklist entry stream paginates within the cap ------

    /// A `get_api_checklist` entry stream paginates under a category filter:
    /// the default limit truncates, every page stays within the cap, the
    /// returned cursor is bound to the same operation/filter/snapshot, and the
    /// walk returns every entry exactly once.
    #[test]
    fn a_checklist_entry_stream_paginates_and_binds_its_filter() {
        let entries: Vec<Item> = (0..8)
            .map(|i| {
                Item::Checklist(Box::new(entry(
                    &format!("C-{i:03}"),
                    "Conversions and From",
                )))
            })
            .collect();
        let filter = FilterKey::category("Conversions and From");
        let mut cursor: Option<String> = None;
        let mut total = 0usize;
        loop {
            let page = paginate_items(
                Operation::Checklist,
                &filter,
                &snap("snap"),
                &entries,
                None,
                cursor.as_deref(),
            )
            .expect("checklist pagination is total");
            assert!(
                page.serialized_bytes() <= MAX_RESPONSE_BYTES,
                "a checklist page stays within the cap"
            );
            total += page.returned;
            if page.truncated {
                cursor = page.next_cursor;
            } else {
                break;
            }
        }
        assert_eq!(total, 8, "the walk returns every entry exactly once");
    }
}
