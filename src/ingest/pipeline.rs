//! [027] The explicit sync pipeline.
//!
//! This is the single boundary that turns the M1/M2 ingest machinery into a
//! published corpus snapshot: it discovers each source's document URLs, fetches
//! them through the registry-validated [`BoundClient`], extracts sections
//! (and, for the api-guidelines source, its checklist entries), assembles one
//! [`SnapshotManifest`], and publishes it atomically through
//! [`crate::corpus::publish_snapshot`].
//!
//! ## The two acceptance criteria
//!
//! * **AC1 — a successful sync reports per-source inventory changes and a new
//!   snapshot identity.** [`sync`] returns a [`SyncReport`] whose
//!   [`SyncReport::sources`] carries one [`SyncSourceResult`] per configured
//!   source — its document count, section / checklist / command counts, and the
//!   [`InventoryDiff`] against the previously published snapshot — and whose
//!   [`SyncReport::snapshot`] is the freshly minted identity of the published
//!   manifest. [`SyncReport::to_stderr`] renders that as a human-readable,
//!   stderr-only report.
//! * **AC2 — a fetch/parser/validation failure returns a typed error and
//!   publishes nothing.** A failure at *any* stage (a fetch that 404s, a page
//!   that fails to parse, a candidate that fails corpus validation) aborts the
//!   run with a [`SyncError`] *before* the atomic promotion in
//!   [`crate::corpus::publish_snapshot`], so the last valid snapshot is left
//!   untouched. No partial corpus is ever promoted.
//!
//! ## Why every source record is always present
//!
//! [`crate::corpus::validate_snapshot`] requires *every* one of the eight
//! approved collections ([`SourceCollection::ALL`]) to be present in the
//! manifest, so the pipeline always records all eight sources — even the ones a
//! given run did not ingest — and only the configured sources contribute
//! sections. A corpus with at least one section and all eight source records
//! clears the gate; a run that ingests nothing at all publishes nothing
//! (`EmptyCorpus`).
//!
//! ## Offline testability
//!
//! The live path discovers document URLs from the source registry and fetches
//! them through the [`RegistryGate`](crate::ingest::fetch). The
//! [`SyncConfig::direct`] override lets a run (and the loopback tests) pin the
//! exact document URLs per source, bypassing live discovery, so the whole
//! pipeline is exercisable with a loopback
//! [`FixtureServer`](crate::ingest::http_fixture) and no network.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Url;

use crate::corpus::{Publication, PublishError, load_published_snapshot, publish_snapshot};
use crate::ingest::api_guidelines;
use crate::ingest::books::{DiscoveryError, InventoryDiff, discover_inventory};
use crate::ingest::cargo;
use crate::ingest::clippy_book;
use crate::ingest::fetch::{BoundClient, FetchError, FetchPolicy, Fetched};
use crate::ingest::rustc;
use crate::ingest::rustdoc;
use crate::ingest::rustfmt;
use crate::ingest::rustup;
use crate::ingest::style_guide;
use crate::model::{
    Channel, CorpusHash, ManifestBody, Release, Section, SnapshotId, SnapshotManifest, Source,
    SourceId, Timestamp, ValidationSummary,
};
use crate::registry::SourceCollection;

/// Per-source document-URL overrides for an offline or pinned run: a source id
/// mapped to the exact document URLs to ingest, bypassing live book discovery.
///
/// In production this is `None` (the pipeline discovers URLs from the registry
/// through the [`RegistryGate`](crate::ingest::fetch)); in the loopback tests
/// it pins the fixture's served pages so the run touches no network.
pub type DirectSources = BTreeMap<String, Vec<Url>>;

/// A configured sync run.
///
/// A [`SyncConfig`] names the parser version to stamp into the snapshot, the set
/// of source ids to ingest and record, and the optional [`DirectSources`]
/// override. [`SyncConfig::production`] builds the all-eights, live-discovery
/// configuration the CLI wires up; a test builds a pinned one with `direct` set.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// The parser/fingerprint version stamped into the published snapshot's
    /// `parser_version` field.
    pub parser_version: String,
    /// The source ids to ingest and record.
    pub sources: Vec<SourceId>,
    /// Per-source document-URL overrides; `None` uses live book discovery.
    pub direct: Option<DirectSources>,
}

impl SyncConfig {
    /// The production configuration: every one of the eight approved collections,
    /// a build-derived parser version, and live discovery (no overrides).
    pub fn production() -> Self {
        Self {
            parser_version: parser_version(),
            sources: SourceCollection::ALL
                .iter()
                .map(|c| SourceId::new(c.id()).expect("a registry id is URI-safe"))
                .collect(),
            direct: None,
        }
    }
}

/// The registry-validated HTTP client a production sync fetches through.
///
/// Wraps a [`FetchPolicy::default`] client in the
/// [`RegistryGate`](crate::ingest::fetch) (via [`BoundClient::new`]), so every
/// fetch is re-checked against the source registry on each hop — no arbitrary
/// source can reach the wire.
pub fn production_client() -> Result<BoundClient, SyncError> {
    BoundClient::new(FetchPolicy::default()).map_err(|e| SyncError::Client(Box::new(e)))
}

/// The per-source outcome of a sync, surfaced in the [`SyncReport`].
#[derive(Debug, Clone)]
pub struct SyncSourceResult {
    /// The source id (e.g. `cargo`).
    pub id: String,
    /// The number of documents fetched for this source.
    pub documents: usize,
    /// The number of sections extracted from this source.
    pub sections: usize,
    /// The number of checklist entries extracted from this source.
    pub checklist_entries: usize,
    /// The number of command entries extracted from this source.
    pub command_entries: usize,
    /// The inventory change against the previously published snapshot for this
    /// source (added / removed / unavailable documents).
    pub diff: InventoryDiff,
}

/// The result of a successful sync: the new snapshot identity, the publication,
/// and a per-source breakdown.
#[derive(Debug, Clone)]
pub struct SyncReport {
    /// The identity of the snapshot this run published.
    pub snapshot: SnapshotId,
    /// The atomic publication (the active snapshot path and the validation
    /// summary the candidate cleared).
    pub publication: Publication,
    /// One outcome per configured source, in the order `config.sources` named
    /// them.
    pub sources: Vec<SyncSourceResult>,
}

impl SyncReport {
    /// Render a human-readable, **stderr-only** report of the run: the new
    /// snapshot identity, the published totals, and each source's inventory
    /// change. The text is *returned* (not written) so the caller decides where
    /// to emit it — the CLI writes it to stderr, keeping stdout exclusive to MCP
    /// messages (plan §8/§9).
    pub fn to_stderr(&self) -> String {
        let mut out = String::from("rust-docs-mcp sync:\n");
        out.push_str(&format!("  snapshot: {}\n", self.snapshot.as_str()));
        out.push_str(&format!(
            "  published {} section(s), {} checklist entr(y/ies), {} command entr(y/ies)\n",
            self.publication.summary.sections,
            self.publication.summary.checklist_entries,
            self.publication.summary.command_entries,
        ));
        for source in &self.sources {
            out.push_str(&format!(
                "  [{}] {} document(s), {} section(s)",
                source.id, source.documents, source.sections
            ));
            if source.checklist_entries > 0 {
                out.push_str(&format!(
                    " {} checklist entr(y/ies)",
                    source.checklist_entries
                ));
            }
            if source.command_entries > 0 {
                out.push_str(&format!(" {} command entr(y/ies)", source.command_entries));
            }
            if !source.diff.added.is_empty() {
                out.push_str(&format!(" +{} added", source.diff.added.len()));
            }
            if !source.diff.removed.is_empty() {
                out.push_str(&format!(" -{} removed", source.diff.removed.len()));
            }
            if !source.diff.unavailable.is_empty() {
                out.push_str(&format!(" {} unavailable", source.diff.unavailable.len()));
            }
            out.push('\n');
        }
        out
    }
}

/// The outcome of ingesting one source's documents into a corpus.
///
/// Carries the fetched document URLs (recorded on the source's
/// `document_urls`) plus the extracted sections and checklist entries. Empty for
/// a source the run did not ingest.
#[derive(Debug, Clone, Default)]
struct IngestedSource {
    /// The document URLs that were fetched (recorded on the source record).
    urls: Vec<Url>,
    /// The sections extracted from this source.
    sections: Vec<Section>,
    /// The checklist entries extracted from this source (api-guidelines only).
    checklist: Vec<crate::model::ChecklistEntry>,
}

/// Run one explicit sync: discover (or take the pinned) document URLs for each
/// configured source, fetch and extract them, assemble a complete manifest
/// carrying all eight source records, and publish it atomically.
///
/// Returns the [`SyncReport`] on success. Any failure — a fetch that 404s or is
/// rejected by the registry, a page that fails to parse, or a candidate that
/// fails corpus validation — aborts the run with a [`SyncError`] *before* the
/// atomic promotion, so the previously published snapshot (if any) is left
/// intact (AC2).
pub async fn sync(
    config: &SyncConfig,
    data_dir: &Path,
    client: &BoundClient,
) -> Result<SyncReport, SyncError> {
    // Read the previously published snapshot once, up front, so each source's
    // inventory diff is measured against it. `None` means "no snapshot yet"
    // (the first sync), so every captured document is reported as `added`.
    let previous = load_previous(data_dir);

    let mut ingested: BTreeMap<String, IngestedSource> = BTreeMap::new();
    let mut results: Vec<SyncSourceResult> = Vec::with_capacity(config.sources.len());

    for source_id in &config.sources {
        // The source id must resolve to one of the eight approved collections;
        // an unconfigured id is a configuration error, reported before any work.
        let collection = SourceCollection::by_id(source_id.as_str()).ok_or_else(|| {
            SyncError::UnconfiguredSource {
                id: source_id.as_str().to_string(),
            }
        })?;

        // Resolve this source's document URLs and its inventory diff: the pinned
        // override (offline / pinned) when present, otherwise live discovery.
        let (urls, diff) = match &config.direct {
            Some(direct) => {
                let urls = direct.get(collection.id()).cloned().unwrap_or_default();
                let previous_urls = previous_document_urls(&previous, collection.id());
                let diff = diff_against_previous(&urls, &previous_urls);
                (urls, diff)
            }
            None => {
                // Live discovery: fetch the source's entry page, read its
                // navigation, and resolve the document URLs against the
                // registry. No `expected` set is passed, so the diff is empty
                // (the caller knows no canonical index here).
                let base_url = reqwest::Url::parse(collection.entry_url())
                    .map_err(|e| SyncError::Discover(DiscoveryError::Fetch(e.to_string())))?;
                let (inventory, diff) =
                    discover_inventory(client, &base_url, collection.id(), None).await?;
                (inventory.urls, diff)
            }
        };

        let ingested_source = ingest_source(collection, &urls, client).await?;
        ingested.insert(collection.id().to_string(), ingested_source.clone());
        results.push(SyncSourceResult {
            id: collection.id().to_string(),
            documents: urls.len(),
            sections: ingested_source.sections.len(),
            checklist_entries: ingested_source.checklist.len(),
            command_entries: 0,
            diff,
        });
    }

    // Assemble the manifest: a record for every approved source (ingested or
    // not) plus the collected sections and checklist entries.
    let created_at = now_timestamp();
    let snapshot_id = new_snapshot_id(&config.parser_version, &created_at);
    let mut sections = Vec::new();
    let mut checklist = Vec::new();
    let mut source_records: Vec<Source> = Vec::with_capacity(SourceCollection::ALL.len());

    for collection in SourceCollection::ALL {
        let source_record = build_source_record(
            collection,
            ingested.get(collection.id()),
            created_at.clone(),
            &config.parser_version,
        )?;
        source_records.push(source_record);
        if let Some(source) = ingested.get(collection.id()) {
            sections.extend(source.sections.clone());
            checklist.extend(source.checklist.clone());
        }
    }

    let manifest = SnapshotManifest {
        schema_version: crate::model::SchemaVersion::CURRENT,
        body: ManifestBody {
            snapshot_id,
            created_at,
            parser_version: config.parser_version.clone(),
            sources: source_records,
            sections,
            checklist,
            commands: Vec::new(),
            validation: ValidationSummary::default(),
            attribution: Vec::new(),
        },
    };

    // `publish_snapshot` validates the candidate *before* it touches the
    // filesystem and promotes it atomically, so a validation failure leaves the
    // last valid snapshot untouched (AC2).
    let publication = publish_snapshot(data_dir, &manifest)?;

    Ok(SyncReport {
        snapshot: manifest.body.snapshot_id,
        publication,
        sources: results,
    })
}

/// Fetch and extract one source's documents, dispatching to the source's own
/// extractor: the HTML book adapter for the book-shaped collections, the
/// rustfmt Markdown adapter, the cargo chapter + command-manual extractors, and
/// the api-guidelines chapter + checklist extractors. A fetch failure or a
/// parse failure surfaces as a [`SyncError`] that aborts the run.
async fn ingest_source(
    collection: SourceCollection,
    urls: &[Url],
    client: &BoundClient,
) -> Result<IngestedSource, SyncError> {
    let mut out = IngestedSource {
        urls: urls.to_vec(),
        ..Default::default()
    };
    if urls.is_empty() {
        return Ok(out);
    }

    match collection {
        SourceCollection::Cargo => {
            // A command-manual page yields command sections via
            // `ingest_command_page`; a guide chapter yields sections via
            // `ingest_chapter`. Both land as `Section`s.
            for url in urls {
                let fetched = client.fetch(url).await?;
                let sections = if cargo::is_command_page(url) {
                    cargo::ingest_command_page(&fetched).map_err(|e| extract_err(collection, e))?
                } else {
                    cargo::ingest_chapter(&fetched).map_err(|e| extract_err(collection, e))?
                };
                out.sections.extend(sections);
            }
        }
        SourceCollection::Rustfmt => {
            for url in urls {
                let fetched = client.fetch(url).await?;
                let sections =
                    rustfmt::ingest_markdown(&fetched).map_err(|e| extract_err(collection, e))?;
                out.sections.extend(sections);
            }
        }
        SourceCollection::ApiGuidelines => {
            // The checklist is parsed from the first captured page and its
            // explanation links resolved against the captured sections.
            let mut first: Option<Fetched> = None;
            for url in urls {
                let fetched = client.fetch(url).await?;
                let chapter = api_guidelines::ingest_chapter(&fetched)
                    .map_err(|e| extract_err(collection, e))?;
                out.sections.extend(chapter);
                if first.is_none() {
                    first = Some(fetched);
                }
            }
            if let Some(first) = first {
                let mut checklist = api_guidelines::ingest_checklist(&first);
                api_guidelines::link_explanations(&out.sections, &mut checklist)
                    .map_err(|e| extract_err(collection, e))?;
                out.checklist = checklist;
            }
        }
        // The remaining collections (style-guide, rustc, rustup, rustdoc, and
        // the clippy *book*) share the HTML chapter adapter.
        _ => {
            out.sections = ingest_book(collection, urls, client).await?;
        }
    }

    Ok(out)
}

/// Fetch and split each `url` through the HTML chapter adapter for `collection`,
/// accumulating the resulting sections.
async fn ingest_book(
    collection: SourceCollection,
    urls: &[Url],
    client: &BoundClient,
) -> Result<Vec<Section>, SyncError> {
    let mut sections = Vec::new();
    for url in urls {
        let fetched = client.fetch(url).await?;
        // Each arm maps its source-specific `IngestError` to a `SyncError` so
        // the match unifies to one result type.
        let chapter: Result<Vec<Section>, SyncError> = match collection {
            SourceCollection::StyleGuide => {
                style_guide::ingest_chapter(&fetched).map_err(|e| extract_err(collection, e))
            }
            SourceCollection::Rustc => {
                rustc::ingest_chapter(&fetched).map_err(|e| extract_err(collection, e))
            }
            SourceCollection::Rustup => {
                rustup::ingest_chapter(&fetched).map_err(|e| extract_err(collection, e))
            }
            SourceCollection::Rustdoc => {
                rustdoc::ingest_chapter(&fetched).map_err(|e| extract_err(collection, e))
            }
            SourceCollection::Clippy => {
                clippy_book::ingest_chapter(&fetched).map_err(|e| extract_err(collection, e))
            }
            // The other collections are handled by their own branch in
            // `ingest_source`, so they never reach the shared book adapter.
            SourceCollection::ApiGuidelines
            | SourceCollection::Cargo
            | SourceCollection::Rustfmt => {
                return Err(SyncError::UnconfiguredSource {
                    id: collection.id().to_string(),
                });
            }
        };
        sections.extend(chapter?);
    }
    Ok(sections)
}

/// Build the source record for `collection`, recording the ingested document
/// URLs when the source was ingested, else an (empty) placeholder record so the
/// manifest always carries all eight sources.
fn build_source_record(
    collection: SourceCollection,
    ingested: Option<&IngestedSource>,
    fetched_at: Timestamp,
    parser_version: &str,
) -> Result<Source, SyncError> {
    let id = SourceId::new(collection.id())?;
    let entry_url = crate::model::Url::new(collection.entry_url())?;
    let document_urls = ingested
        .map(|source| {
            source
                .urls
                .iter()
                .map(|url| crate::model::Url::new(url.as_str()))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    Ok(Source {
        id,
        title: collection.title().to_string(),
        entry_url,
        document_urls,
        last_checked: None,
        fetched_at,
        upstream_modified: None,
        content_hash: CorpusHash::compute(collection.id().as_bytes()),
        release: release_for(collection),
        parser: parser_version.to_string(),
    })
}

/// The release/channel a source record carries. The registry does not yet expose
/// per-collection release metadata, so the stable channel is recorded as the
/// default; unknown releases are preserved through [`Release`].
fn release_for(_collection: SourceCollection) -> Release {
    Release::channel_only(Channel::Stable)
}

/// Read the previously published snapshot, if any. A missing or unreadable
/// published snapshot (the first sync, or a corrupt previous) is treated as "no
/// previous", so the new sync's inventory diff reports every document `added`.
fn load_previous(data_dir: &Path) -> Option<SnapshotManifest> {
    load_published_snapshot(data_dir).ok()
}

/// The previous snapshot's document URLs for `source_id`, as a set of URL
/// strings; empty when there is no previous snapshot or no record for the source.
fn previous_document_urls(previous: &Option<SnapshotManifest>, source_id: &str) -> HashSet<String> {
    match previous {
        Some(manifest) => manifest
            .body
            .sources
            .iter()
            .find(|source| source.id.as_str() == source_id)
            .map(|source| {
                source
                    .document_urls
                    .iter()
                    .map(|url| url.as_str().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        None => HashSet::new(),
    }
}

/// Compute the [`InventoryDiff`] of `current` document URLs against the
/// `previous` set: URLs in `current` but not `previous` are `added`; URLs in
/// `previous` but not `current` are `removed`. (Nothing is probed for
/// `unavailable` on the pinned path, so that list is empty here.)
fn diff_against_previous(current: &[Url], previous: &HashSet<String>) -> InventoryDiff {
    let current_set: HashSet<&str> = current.iter().map(Url::as_str).collect();
    InventoryDiff {
        added: current
            .iter()
            .filter(|url| !previous.contains(url.as_str()))
            .map(Url::as_str)
            .map(str::to_string)
            .collect(),
        removed: previous
            .iter()
            .filter(|url| !current_set.contains(url.as_str()))
            .cloned()
            .collect(),
        unavailable: Vec::new(),
    }
}

/// Map a source-specific extraction error to the pipeline's [`SyncError`] with
/// the source id and a human-readable reason. Any extractor's error type
/// implements [`std::fmt::Display`], so one helper serves them all.
fn extract_err(collection: SourceCollection, reason: impl std::fmt::Display) -> SyncError {
    SyncError::Extract {
        source: collection.id().to_string(),
        reason: reason.to_string(),
    }
}

/// The parser version stamped into the snapshot, derived from the build.
fn parser_version() -> String {
    format!("rust-docs-mcp/{}", env!("CARGO_PKG_VERSION"))
}

/// A "now" timestamp for the snapshot, derived from the system clock. A numeric
/// nanoseconds-since-epoch string is a valid [`Timestamp`] (non-empty, ASCII,
/// no whitespace).
fn now_timestamp() -> Timestamp {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Timestamp::new(nanos.to_string()).expect("a numeric timestamp is valid")
}

/// A fresh, unique snapshot identity, combining the parser version and a
/// monotonic clock so two syncs never mint the same identity. The version and
/// timestamp are sanitised to the URI-safe unreserved set, since
/// [`SnapshotId::new`] rejects any other byte.
fn new_snapshot_id(parser_version: &str, created_at: &Timestamp) -> SnapshotId {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let version = sanitize_uri_safe(parser_version);
    let created = sanitize_uri_safe(created_at.as_str());
    SnapshotId::new(format!("{version}-{created}-{nanos}"))
        .expect("a sanitized, version-stamped snapshot id is URI-safe")
}

/// Map every byte outside the URI-safe unreserved set (`A-Z`, `a-z`, `0-9`,
/// `-`, `_`, `.`, `~`) to `-`, so a version-stamped snapshot id is always valid.
fn sanitize_uri_safe(s: &str) -> String {
    s.bytes()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
                c as char
            } else {
                '-'
            }
        })
        .collect()
}

/// A sync failure. A fetch / discovery / extraction / validation failure aborts
/// the run *before* the atomic promotion, so the last valid snapshot is
/// preserved (AC2).
#[derive(Debug)]
pub enum SyncError {
    /// Building the production client failed (e.g. the registry is unreachable).
    Client(Box<FetchError>),
    /// A per-document fetch failed (transport, boundary rejection, retry cap,
    /// a non-success status, ...).
    Fetch(Box<FetchError>),
    /// A source's document URLs could not be discovered on the live path.
    Discover(DiscoveryError),
    /// A source's content failed extraction or parsing.
    Extract {
        /// The source id whose extraction failed.
        source: String,
        /// The extractor's own error, rendered for the human report.
        reason: String,
    },
    /// A configured source id is not one of the eight approved collections.
    UnconfiguredSource {
        /// The source id that did not resolve.
        id: String,
    },
    /// A schema / id construction failure (e.g. an invalid source id or URL).
    Schema(crate::model::SchemaError),
    /// The candidate snapshot failed validation or could not be published; the
    /// previously published snapshot is preserved.
    Publish(PublishError),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Client(err) => write!(f, "building the fetch client failed: {err}"),
            Self::Fetch(err) => write!(f, "a fetch failed: {err}"),
            Self::Discover(err) => write!(f, "discovery failed: {err}"),
            Self::Extract { source, reason } => {
                write!(f, "extracting source `{source}` failed: {reason}")
            }
            Self::UnconfiguredSource { id } => {
                write!(f, "source id `{id}` is not one of the approved collections")
            }
            Self::Schema(err) => write!(f, "a schema validation failed: {err}"),
            Self::Publish(err) => write!(f, "publishing the snapshot failed: {err}"),
        }
    }
}

impl std::error::Error for SyncError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Client(err) => Some(err),
            Self::Fetch(err) => Some(err),
            Self::Discover(err) => Some(err),
            Self::Schema(err) => Some(err),
            Self::Publish(err) => Some(err),
            Self::Extract { .. } | Self::UnconfiguredSource { .. } => None,
        }
    }
}

impl From<FetchError> for SyncError {
    fn from(err: FetchError) -> Self {
        Self::Fetch(Box::new(err))
    }
}

impl From<DiscoveryError> for SyncError {
    fn from(err: DiscoveryError) -> Self {
        Self::Discover(err)
    }
}

impl From<PublishError> for SyncError {
    fn from(err: PublishError) -> Self {
        Self::Publish(err)
    }
}

impl From<crate::model::SchemaError> for SyncError {
    fn from(err: crate::model::SchemaError) -> Self {
        Self::Schema(err)
    }
}

// ---------------------------------------------------------------------------
// Offline acceptance tests — plan §11 / task [027] / issue #27
//
// Both criteria are exercised over a loopback `FixtureServer` with no network:
// a pinned `SyncConfig.direct` supplies the document URLs, and a
// `LoopbackGate` client lets the fetch layer reach `127.0.0.1`.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    #![allow(clippy::result_large_err)]

    use super::{SyncConfig, SyncError, diff_against_previous, new_snapshot_id, sync};
    use crate::ingest::fetch::{BoundClient, FetchPolicy, LoopbackGate};
    use crate::ingest::http_fixture::{Behavior, FixtureServer};
    use crate::model::SourceId;
    use std::collections::{BTreeMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// A fast loopback fetch policy: no retries and zero backoff so a 404 or a
    /// transient failure fast-fails instead of sleeping.
    fn fast_policy() -> FetchPolicy {
        FetchPolicy {
            transient_backoff: Duration::ZERO,
            retry_after_max: Duration::ZERO,
            max_retries: 0,
            ..FetchPolicy::default()
        }
    }

    fn loopback_client() -> Result<BoundClient, crate::ingest::fetch::FetchError> {
        BoundClient::with_gate(fast_policy(), Arc::new(LoopbackGate))
    }

    /// A unique, fresh data directory per call, so parallel tests never collide
    /// and each starts with no published snapshot.
    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_data_dir() -> PathBuf {
        let n = NEXT_DIR.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("rust-mcp-027-sync-{n}"));
        // Start from no published snapshot.
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A style-guide chapter page with a single `<h2>` section, served from
    /// loopback. `style_guide::ingest_chapter` splits it into at least one
    /// section carrying the `id` anchor.
    fn one_section_html(id: &str, heading: &str) -> String {
        format!(
            r##"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="index.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="top">Rust Style Guide</h1>
    <h2 id="{id}">{heading}</h2>
    <p>This section explains <code>{heading}</code> with a concrete example.</p>
    <pre><code class="language-rust">fn {id}_example() {{}}</code></pre>
  </main>
</body></html>"##
        )
    }

    /// A pinned style-guide config whose single document URL is `url`.
    fn style_guide_config(url: reqwest::Url) -> SyncConfig {
        let mut direct: BTreeMap<String, Vec<reqwest::Url>> = BTreeMap::new();
        direct.insert("style-guide".to_string(), vec![url]);
        SyncConfig {
            parser_version: "test/1".to_string(),
            sources: vec![SourceId::new("style-guide").expect("a registry id is URI-safe")],
            direct: Some(direct),
        }
    }

    #[tokio::test]
    async fn ac1_a_successful_sync_reports_sections_an_inventory_change_and_a_new_identity() {
        // Two style-guide chapters, each served from loopback, each with one
        // section — so the run captures at least two sections.
        let server = FixtureServer::start(vec![
            (
                "/style-guide/naming.html".to_string(),
                Behavior::Text {
                    body: one_section_html("naming", "Naming").into_bytes(),
                    etag: Some("\"naming-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            (
                "/style-guide/formatting.html".to_string(),
                Behavior::Text {
                    body: one_section_html("formatting", "Formatting").into_bytes(),
                    etag: Some("\"formatting-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
        ]);

        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();
        let naming =
            reqwest::Url::parse(&format!("http://{}/style-guide/naming.html", server.addr))
                .expect("a valid loopback chapter URL");
        let formatting = reqwest::Url::parse(&format!(
            "http://{}/style-guide/formatting.html",
            server.addr
        ))
        .expect("a valid loopback chapter URL");

        let mut direct: BTreeMap<String, Vec<reqwest::Url>> = BTreeMap::new();
        direct.insert("style-guide".to_string(), vec![naming, formatting]);
        let config = SyncConfig {
            parser_version: "test/1".to_string(),
            sources: vec![SourceId::new("style-guide").expect("a registry id is URI-safe")],
            direct: Some(direct),
        };

        let report = sync(&config, &data_dir, &client)
            .await
            .expect("the offline sync succeeds");

        // AC1: at least two sections were captured and published.
        assert!(
            report.publication.summary.sections >= 2,
            "expected at least two sections, got {}",
            report.publication.summary.sections
        );

        // AC1: the per-source result reports an inventory change (the first sync
        // adds every captured document).
        let style = report
            .sources
            .iter()
            .find(|s| s.id == "style-guide")
            .expect("the style-guide result is present");
        assert_eq!(style.documents, 2, "both documents were fetched");
        assert!(
            !style.diff.added.is_empty(),
            "the first sync must report the captured documents as added"
        );
        assert!(
            style.diff.removed.is_empty(),
            "nothing is removed on the first sync"
        );

        // AC1: a new snapshot identity, actually published and loadable.
        assert!(
            !report.snapshot.as_str().is_empty(),
            "the snapshot must carry an identity"
        );
        let loaded = crate::corpus::load_published_snapshot(&data_dir)
            .expect("the snapshot is published and loadable");
        assert_eq!(
            loaded.body.snapshot_id, report.snapshot,
            "the loaded snapshot carries the reported identity"
        );
        assert_eq!(
            loaded.body.sources.len(),
            8,
            "the published manifest records all eight approved sources"
        );
    }

    #[tokio::test]
    async fn ac2_a_fetch_failure_aborts_without_publishing_a_partial_snapshot() {
        // First, publish a known-good snapshot.
        let good_server = FixtureServer::start(vec![(
            "/style-guide/naming.html".to_string(),
            Behavior::Text {
                body: one_section_html("naming", "Naming").into_bytes(),
                etag: Some("\"naming-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();
        let good_url = reqwest::Url::parse(&format!(
            "http://{}/style-guide/naming.html",
            good_server.addr
        ))
        .expect("a valid loopback chapter URL");
        let good_config = style_guide_config(good_url);

        let good_report = sync(&good_config, &data_dir, &client)
            .await
            .expect("the first (good) sync publishes");
        let good_id = good_report.snapshot.clone();

        // Now a second sync whose pinned document URL 404s. It must fail and
        // must NOT overwrite the good snapshot.
        let bad_server = FixtureServer::start(vec![(
            "/style-guide/missing.html".to_string(),
            Behavior::NotFound,
        )]);
        let bad_url = reqwest::Url::parse(&format!(
            "http://{}/style-guide/missing.html",
            bad_server.addr
        ))
        .expect("a valid loopback chapter URL");
        let bad_config = style_guide_config(bad_url);

        let err = sync(&bad_config, &data_dir, &client)
            .await
            .expect_err("a 404 fetch must fail the sync");
        assert!(
            matches!(err, SyncError::Fetch(_)),
            "expected a fetch error, got {err:?}"
        );

        // AC2: the previously published snapshot is preserved untouched.
        let still = crate::corpus::load_published_snapshot(&data_dir)
            .expect("the good snapshot is still published");
        assert_eq!(
            still.body.snapshot_id, good_id,
            "a failed sync must not overwrite the last valid snapshot"
        );
    }

    /// The pinned-path inventory diff reports `current` URLs absent from
    /// `previous` as added, and `previous` URLs absent from `current` as
    /// removed.
    #[test]
    fn the_pinned_path_diffs_against_the_previous_snapshot() {
        let current = vec![
            reqwest::Url::parse("https://doc.rust-lang.org/a.html").unwrap(),
            reqwest::Url::parse("https://doc.rust-lang.org/b.html").unwrap(),
        ];
        let previous: HashSet<String> =
            HashSet::from(["https://doc.rust-lang.org/a.html".to_string()]);
        let diff = diff_against_previous(&current, &previous);
        assert_eq!(
            diff.added,
            vec!["https://doc.rust-lang.org/b.html".to_string()]
        );
        assert!(diff.removed.is_empty());
        assert!(diff.unavailable.is_empty());
    }

    /// A fresh snapshot identity is non-empty and embeds a URI-safe form of the
    /// parser version, so two syncs with different versions never mint the same
    /// identity. The version `test/1` is sanitised to `test-1` (the `/` is not
    /// URI-safe), and the timestamp is appended for uniqueness.
    #[test]
    fn a_snapshot_identity_is_nonempty_and_version_stamped() {
        let created_at = crate::model::Timestamp::new("1").expect("a numeric timestamp is valid");
        let id = new_snapshot_id("test/1", &created_at);
        assert!(!id.as_str().is_empty());
        assert!(id.as_str().contains("test-1"));
        // A different version mints a different, still-URI-safe identity.
        let other = new_snapshot_id("test/2", &created_at);
        assert_ne!(id, other);
    }

    // -- Issue #146: the pipeline *proceeds past* an API Guidelines page that
    //    carries an external citation, and requests no external target. --

    /// AC2/AC5 (hermetic): the pipeline proceeds past an API Guidelines page that
    /// carries an external `github.com` citation. Before #146 the run aborted at
    /// this page (the citation was treated as an unauthorized fetch and the whole
    /// sync failed with no manifest). Now the citation is serialized, the run
    /// publishes a snapshot that retains it, and the fixture server received only
    /// the document page — proving no external target was requested.
    #[tokio::test]
    async fn the_pipeline_proceeds_past_an_external_citation_and_requests_no_external_target() {
        let about_html = r##"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="checklist.html">Checklist</a></li></ol></nav>
  <main id="main-content">
    <h1 id="about">About the API Guidelines</h1>
    <p>To contribute, read the <a href="https://github.com/rust-lang/api-guidelines/blob/master/CONTRIBUTING.md">CONTRIBUTING guide</a> upstream.</p>
  </main>
</body></html>"##;
        let server = FixtureServer::start(vec![(
            "/api-guidelines/about.html".to_string(),
            Behavior::Text {
                body: about_html.as_bytes().to_vec(),
                etag: Some("\"about-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();
        let about_url =
            reqwest::Url::parse(&format!("http://{}/api-guidelines/about.html", server.addr))
                .expect("a valid loopback about-page URL");

        let mut direct: BTreeMap<String, Vec<reqwest::Url>> = BTreeMap::new();
        direct.insert("api-guidelines".to_string(), vec![about_url]);
        let config = SyncConfig {
            parser_version: "test/146".to_string(),
            sources: vec![SourceId::new("api-guidelines").expect("a registry id is URI-safe")],
            direct: Some(direct),
        };

        // AC5: the run proceeds past the page and publishes a new snapshot.
        let report = sync(&config, &data_dir, &client)
            .await
            .expect("a citation must not abort the sync");
        assert!(
            !report.snapshot.as_str().is_empty(),
            "a snapshot must be published"
        );
        let api = report
            .sources
            .iter()
            .find(|s| s.id == "api-guidelines")
            .expect("the api-guidelines result is present");
        assert_eq!(api.documents, 1, "the one about page was fetched");
        assert!(
            api.sections >= 1,
            "the about page yielded at least one section"
        );

        // AC2: the fixture server received ONLY the document page — the external
        // `github.com` citation was serialized, never fetched.
        assert_eq!(
            server.request_count(),
            1,
            "no external target may be requested; only the about page was fetched"
        );

        // The published snapshot actually carries the citation.
        let loaded = crate::corpus::load_published_snapshot(&data_dir)
            .expect("the snapshot is published and loadable");
        let bodies: String = loaded
            .body
            .sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            bodies.contains(
                "https://github.com/rust-lang/api-guidelines/blob/master/CONTRIBUTING.md"
            ),
            "the published snapshot must retain the external citation"
        );
    }

    /// AC5 (opt-in, live): the production sync proceeds past the API Guidelines
    /// `about` page that carries the `github.com` CONTRIBUTING citation. This is
    /// the real-network verification of the fix and the explicit acceptance
    /// check "the sync proceeds past this page." It is `#[ignore]`d and gated on
    /// the `RUST_DOCS_MCP_LIVE_SYNC` environment variable so the hermetic suite
    /// never touches the network. Opt in with:
    ///
    /// ```sh
    /// RUST_DOCS_MCP_LIVE_SYNC=1 cargo test -- --ignored \
    ///   the_pipeline_proceeds_live_past_the_contributing_citation
    /// ```
    #[tokio::test]
    #[ignore = "opt-in live sync — run with `RUST_DOCS_MCP_LIVE_SYNC=1 cargo test -- --ignored`; see docs/CI.md"]
    async fn the_pipeline_proceeds_live_past_the_contributing_citation() {
        // The env-var guard keeps the live path out of the hermetic suite; a bare
        // `--ignored` run is a fast no-op unless the operator explicitly opts in.
        if std::env::var_os("RUST_DOCS_MCP_LIVE_SYNC").is_none() {
            eprintln!(
                "[live-sync] RUST_DOCS_MCP_LIVE_SYNC is unset — skipping the live sync \
                 (fast no-op). Run with `RUST_DOCS_MCP_LIVE_SYNC=1 cargo test -- --ignored`."
            );
            return;
        }

        let client = super::production_client().expect("the production client builds");
        let data_dir = temp_data_dir();
        // A focused live sync over just the real api-guidelines collection must
        // now proceed past the about page (which carries the github.com citation)
        // rather than aborting on it — the original reported failure mode.
        let config = SyncConfig {
            parser_version: "test/live-146".to_string(),
            sources: vec![SourceId::new("api-guidelines").expect("a registry id is URI-safe")],
            direct: None,
        };
        let report = sync(&config, &data_dir, &client)
            .await
            .expect("the live sync proceeds past the external citation");
        assert!(
            !report.snapshot.as_str().is_empty(),
            "a snapshot must be published"
        );
        let api = report
            .sources
            .iter()
            .find(|s| s.id == "api-guidelines")
            .expect("the api-guidelines result is present");
        assert!(
            api.documents > 0,
            "the api-guidelines collection must yield at least one document"
        );
    }
}
