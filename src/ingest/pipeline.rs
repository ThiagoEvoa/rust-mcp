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

use crate::commands::CommandCatalog;
use crate::corpus::{
    CompletenessReport, Publication, PublishError, check_completeness, load_published_snapshot,
    publish_snapshot,
};
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
    /// A test-only override for rustfmt's *live* discovery: when `Some`, these
    /// document URLs are used instead of [`rustfmt::approved_documents`], so a
    /// hermetic test can point rustfmt at a loopback fixture. `None` (the
    /// production default) discovers rustfmt from the two approved,
    /// commit-pinned raw documents. The override is read on every run, so it is
    /// not dead code; production sets it to `None`.
    pub rustfmt_urls: Option<Vec<Url>>,
    /// Whether this run must publish a *complete* production corpus
    /// ([`crate::corpus::check_completeness`]) — every required collection
    /// populated, the checklist/lint catalog/command catalog nonempty, and
    /// attribution present. When `true`, an incomplete assembled snapshot is
    /// rejected *before* publication, so a degraded corpus can never replace the
    /// last valid one (issue #151). `production` sets this to `true`; the
    /// dev/test configs leave it `false`, so an explicitly partial corpus may
    /// still be published for local iteration.
    pub require_complete: bool,
}

impl SyncConfig {
    /// The production configuration: every one of the eight approved collections,
    /// a build-derived parser version, and live discovery (no overrides). A
    /// production run must publish a *complete* corpus, so it sets
    /// `require_complete` to `true`: the publication gate rejects an incomplete
    /// snapshot rather than letting a degraded corpus replace the last valid one.
    pub fn production() -> Self {
        Self {
            parser_version: parser_version(),
            sources: SourceCollection::ALL
                .iter()
                .map(|c| SourceId::new(c.id()).expect("a registry id is URI-safe"))
                .collect(),
            direct: None,
            rustfmt_urls: None,
            require_complete: true,
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
/// a per-source breakdown, and the completeness verdict.
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
    /// The completeness verdict for the published snapshot ([`CompletenessReport`])
    /// — the production gate that rejects a degraded corpus (issue #151). On a
    /// successful run this is `complete`; a run that would publish an incomplete
    /// corpus fails instead of reaching here.
    pub completeness: CompletenessReport,
}

impl SyncReport {
    /// Render a human-readable, **stderr-only** report of the run: the new
    /// snapshot identity, the published totals, each source's inventory change,
    /// and the completeness verdict. The text is *returned* (not written) so the
    /// caller decides where to emit it — the CLI writes it to stderr, keeping
    /// stdout exclusive to MCP messages (plan §8/§9).
    pub fn to_stderr(&self) -> String {
        let mut out = String::from("rust-docs-mcp sync:\n");
        out.push_str(&format!("  snapshot: {}\n", self.snapshot.as_str()));
        out.push_str(&format!(
            "  published {} section(s), {} checklist entr(y/ies), {} command entr(y/ies)\n",
            self.publication.summary.sections,
            self.publication.summary.checklist_entries,
            self.publication.summary.command_entries,
        ));
        out.push_str(&format!("  completeness: {}\n", self.completeness.render()));
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
    /// The captured upstream documents — the raw bytes fetched for each URL plus
    /// the response metadata that identified them (the `ETag` / `Last-Modified`
    /// the upstream supplied, when any). A content identity is computed over
    /// these bytes, and the metadata is the freshness evidence carried onto the
    /// source record (issue #152). Empty for a source that was not ingested.
    captured: Vec<CapturedDocument>,
}

/// One captured upstream document: the bytes fetched plus the response metadata
/// that identified them. A source's content identity is a hash over its captured
/// bytes, and its `ETag` / `Last-Modified` are the freshness hints carried onto
/// the source record — neither is fabricated, so an upstream that supplies none
/// leaves them `None`.
#[derive(Debug, Clone)]
struct CapturedDocument {
    /// The URL this document was fetched from.
    url: Url,
    /// The raw bytes captured (the content identity is hashed over these).
    bytes: Vec<u8>,
    /// The upstream `ETag` header, when present.
    etag: Option<String>,
    /// The upstream `Last-Modified` header, when present (an HTTP-date / RFC 3339
    /// string as served).
    last_modified: Option<String>,
}

impl IngestedSource {
    /// The captured document bytes, concatenated in URL-stable order and
    /// NUL-separated, for a content identity that is independent of fetch order
    /// (mirroring `crate::ingest::bundle`'s captured-content hashing). An
    /// empty source hashes the empty inventory, which is still a stable, unique
    /// identity for that source.
    fn captured_bytes(&self) -> Vec<u8> {
        let mut docs: Vec<&CapturedDocument> = self.captured.iter().collect();
        docs.sort_by(|a, b| a.url.as_str().cmp(b.url.as_str()));
        let mut bytes = Vec::new();
        for doc in docs {
            bytes.extend_from_slice(&doc.bytes);
            bytes.push(0);
        }
        bytes
    }

    /// The content identity of this source: a hash over the captured bytes. A
    /// different body yields a different identity; an identical body keeps it
    /// stable — so a no-change refresh preserves content identity while the
    /// check time advances (issue #152).
    fn content_hash(&self) -> crate::model::CorpusHash {
        crate::model::CorpusHash::compute(self.captured_bytes())
    }

    /// The representative upstream `ETag` — the `ETag` of the URL-smallest
    /// captured document that carries one, when any do. `None` when no upstream
    /// supplied an ETag (preserved, never fabricated).
    fn representative_etag(&self) -> Option<String> {
        let mut docs: Vec<&CapturedDocument> = self.captured.iter().collect();
        docs.sort_by(|a, b| a.url.as_str().cmp(b.url.as_str()));
        docs.iter().find_map(|doc| doc.etag.clone())
    }

    /// The upstream-modification hint: the most-recent parseable `Last-Modified`
    /// across the captured documents, canonicalised to the RFC 3339 UTC grammar
    /// via [`crate::provenance::Instant::to_rfc3339`], when any supplied a
    /// parseable one. `None` when the upstream supplied none or none parsed — an
    /// honest "we don't know," never a fabricated value.
    fn upstream_modified(&self) -> Option<crate::model::Timestamp> {
        use crate::model::Timestamp;
        let latest = self
            .captured
            .iter()
            .filter_map(|doc| doc.last_modified.as_deref())
            .filter_map(crate::provenance::parse_date_time)
            .max();
        latest.map(|instant| {
            Timestamp::new(instant.to_rfc3339()).expect("a canonical RFC 3339 timestamp is valid")
        })
    }
}

/// Capture the bytes and response metadata of one fetched document, for the
/// content identity and the `ETag` / `Last-Modified` evidence carried onto the
/// source record. The caller borrows `fetched` for extraction; this clones only
/// the small metadata and the (already-buffered, capped) body.
fn capture_document(url: &Url, fetched: &crate::ingest::fetch::Fetched) -> CapturedDocument {
    CapturedDocument {
        url: url.clone(),
        bytes: fetched.body.clone(),
        etag: fetched.etag.clone(),
        last_modified: fetched.last_modified.clone(),
    }
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
                // Live discovery.
                //
                // rustfmt is discovered through its own adapter, not the generic
                // book discovery (issue #147): its human-facing `github.com`
                // entry is refused by the fetch boundary and the rendered
                // `rust-lang.github.io/rustfmt/` site is a JavaScript shell, so
                // `discover_inventory`'s navigation-page fetch fails for it.
                // rustfmt's only fetchable boundary is the two approved,
                // commit-pinned `raw.githubusercontent.com` Markdown documents
                // (`README.md` + `Configurations.md`). A test-only override
                // (`rustfmt_urls`) can point this at a loopback fixture;
                // production uses the approved documents.
                if collection == SourceCollection::Rustfmt {
                    let urls = config
                        .rustfmt_urls
                        .clone()
                        .unwrap_or_else(rustfmt::approved_documents);
                    let previous_urls = previous_document_urls(&previous, collection.id());
                    let diff = diff_against_previous(&urls, &previous_urls);
                    (urls, diff)
                } else {
                    // Every other source: fetch the source's entry page, read its
                    // navigation, and resolve the document URLs against the
                    // registry. No `expected` set is passed, so the diff is empty
                    // (the caller knows no canonical index here).
                    let base_url = reqwest::Url::parse(collection.entry_url())
                        .map_err(|e| SyncError::Discover(DiscoveryError::Fetch(e.to_string())))?;
                    let (inventory, diff) =
                        discover_inventory(client, &base_url, collection.id(), None).await?;
                    (inventory.urls, diff)
                }
            }
        };

        let ingested_source = ingest_source(collection, &urls, client).await?;
        ingested.insert(collection.id().to_string(), ingested_source.clone());
        results.push(SyncSourceResult {
            id: collection.id().to_string(),
            documents: urls.len(),
            sections: ingested_source.sections.len(),
            checklist_entries: ingested_source.checklist.len(),
            // [149] the command entries this source contributes to the unified
            // catalog: the distinct command keys its own sections stamp
            // (lightweight, no full catalog build).
            command_entries: crate::commands::count_command_keys(&ingested_source.sections),
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
        let previous_source = previous_source(&previous, collection.id());
        let source_record = build_source_record(
            collection,
            ingested.get(collection.id()),
            previous_source,
            &created_at,
            &config.parser_version,
        )?;
        source_records.push(source_record);
        if let Some(source) = ingested.get(collection.id()) {
            sections.extend(source.sections.clone());
            checklist.extend(source.checklist.clone());
        }
    }

    let mut manifest = SnapshotManifest {
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

    // [149] The unified, resolvable command catalog is derived from the sections
    // the snapshot actually carries: one entry per distinct command key the
    // sections stamp, each entry's citations re-pointed at those same sections.
    // `list_sources`, the catalog resource, and `get_command_guidance` all read
    // this *same* snapshot catalog, so they can no longer disagree about which
    // keys exist (the pre-fix split: a fixed `combined()` catalog over a
    // snapshot that carried no command entries).
    let command_catalog = CommandCatalog::from_sections(&manifest.body.sections);
    // AC4: `publish_snapshot` runs `validate_snapshot`, which rejects a snapshot
    // whose command citations dangle (`UnresolvedCommandReference`) *before* it
    // touches the corpus — so a broken citation is refused here, leaving the last
    // valid snapshot untouched, rather than surfacing to a client later as a
    // JSON-RPC `-32603`. The derived catalog's citations all resolve by
    // construction; the guard is defense-in-depth against a future drift.
    manifest.body.commands = command_catalog.into_entries();

    // Issue #151: the completeness gate runs *after* the integrity gate
    // (`validate_snapshot`, via `publish_snapshot`) and *before* the atomic
    // promotion. A production run (`require_complete`) refuses an incomplete
    // corpus — one that omits a required collection, cross-reference, or its
    // attribution — so a degraded snapshot can never replace the last valid
    // 8-source corpus. The gate is checked against the required production
    // inventory, with `previous` contributing removal reporting: a collection
    // that was populated before but is now empty is an explicit, justified
    // policy decision rather than a silent loss.
    let completeness = check_completeness(&manifest, previous.as_ref());
    if config.require_complete && !completeness.is_complete() {
        return Err(SyncError::IncompleteCorpus {
            report: completeness,
        });
    }

    // `publish_snapshot` validates the candidate *before* it touches the
    // filesystem and promotes it atomically, so a validation failure leaves the
    // last valid snapshot untouched (AC2).
    let publication = publish_snapshot(data_dir, &manifest)?;

    Ok(SyncReport {
        snapshot: manifest.body.snapshot_id,
        publication,
        sources: results,
        completeness,
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
                out.captured.push(capture_document(url, &fetched));
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
                out.captured.push(capture_document(url, &fetched));
                let sections =
                    rustfmt::ingest_markdown(&fetched).map_err(|e| extract_err(collection, e))?;
                out.sections.extend(sections);
            }
        }
        SourceCollection::ApiGuidelines => {
            // Issue #148: the checklist is parsed from the document identified by
            // *URL identity* (`…/checklist.html`, via `is_checklist_url`), not from
            // the first page the discovery books. Discovery may book `about.html`
            // before `checklist.html`, and the old "first page" selection parsed
            // the checklist from `about.html` (which carries none), so a valid
            // checklist published zero entries. Here we capture every page as a
            // chapter and remember the one that *is* the checklist, regardless of
            // order; the checklist is then parsed, required nonempty, and its
            // explanation links resolved against the captured sections.
            let mut checklist_page: Option<Fetched> = None;
            for url in urls {
                let fetched = client.fetch(url).await?;
                // Capture the bytes/metadata before `fetched` may be moved into
                // `checklist_page` below.
                out.captured.push(capture_document(url, &fetched));
                let chapter = api_guidelines::ingest_chapter(&fetched)
                    .map_err(|e| extract_err(collection, e))?;
                out.sections.extend(chapter);
                if api_guidelines::is_checklist_url(url) {
                    // There should be exactly one checklist document; a duplicate
                    // would be an upstream anomaly, so the last one wins
                    // deterministically.
                    checklist_page = Some(fetched);
                }
            }

            // The source is present (the `urls.is_empty()` guard above returns
            // early for an empty inventory), so its checklist must be found and
            // nonempty. A missing, empty, or unparseable checklist is a
            // validation failure that aborts the run — and, because publication
            // is atomic, leaves the last valid snapshot untouched — rather than
            // publishing a zero-entry checklist (issue #148). The continuation-
            // aware, ambiguity-checked explanation mapping is retained inside the
            // call (`link_explanations`).
            let checklist =
                api_guidelines::ingest_checklist_document(&out.sections, checklist_page.as_ref())
                    .map_err(|e| extract_err(collection, e))?;
            out.checklist = checklist;
        }
        // The remaining collections (style-guide, rustc, rustup, rustdoc, and
        // the clippy *book*) share the HTML chapter adapter.
        _ => {
            let (sections, captured) = ingest_book(collection, urls, client).await?;
            out.sections = sections;
            out.captured = captured;
        }
    }

    Ok(out)
}

/// Fetch and split each `url` through the HTML chapter adapter for `collection`,
/// accumulating the resulting sections and the captured documents (the bytes and
/// response metadata a content identity and freshness evidence are built from).
async fn ingest_book(
    collection: SourceCollection,
    urls: &[Url],
    client: &BoundClient,
) -> Result<(Vec<Section>, Vec<CapturedDocument>), SyncError> {
    let mut sections = Vec::new();
    let mut captured = Vec::new();
    for url in urls {
        let fetched = client.fetch(url).await?;
        captured.push(capture_document(url, &fetched));
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
    Ok((sections, captured))
}

/// Build the source record for `collection`, recording the ingested document
/// URLs and the capture's real provenance when the source was ingested, else an
/// (empty) placeholder record so the manifest always carries all eight sources.
///
/// Provenance (issue #152):
///
/// * `content_hash` is a hash over the captured document *bytes* when the source
///   was ingested — so distinct bodies hash distinctly and an unchanged body
///   hashes identically — rather than a constant over the collection id.
/// * `etag` / `upstream_modified` carry the capture's real ETag and
///   `Last-Modified` evidence when the upstream supplied it (else `None`).
/// * `fetched_at` (download time) and `upstream_modified` are preserved across a
///   no-change refresh (identical content hash to the prior record) so content
///   identity is stable; a content change records the fresh capture.
/// * `last_checked` (the check time) advances on every successful check, so a
///   no-change refresh updates the check time while preserving the content
///   identity; for a source the run did not ingest it preserves the last known
///   check (or stays unknown) rather than fabricating one.
/// * `release` keeps the source-specific revision/channel provenance
///   ([`release_for`], e.g. rustfmt's approved revision).
fn build_source_record(
    collection: SourceCollection,
    ingested: Option<&IngestedSource>,
    previous: Option<&Source>,
    now: &Timestamp,
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

    // Content identity: a hash over the captured document bytes when the source
    // was ingested, else a deterministic placeholder over the collection id so a
    // source the run did not fetch still carries a stable, non-empty identity.
    let content_hash = match ingested {
        Some(source) => source.content_hash(),
        None => CorpusHash::compute(collection.id().as_bytes()),
    };

    // A no-change refresh (identical captured content to the prior record)
    // preserves the download time and the upstream-modification hint; a content
    // change (or a first sync) records the fresh capture.
    let content_unchanged = previous.is_some_and(|prior| prior.content_hash == content_hash);

    // The upstream-modification hint: the capture's most recent `Last-Modified`
    // when present; on a no-change refresh the prior hint is preserved rather
    // than re-stamped (so the hint is stable across an unchanged refresh).
    let upstream_modified = if content_unchanged {
        previous
            .and_then(|prior| prior.upstream_modified.clone())
            .or_else(|| ingested.and_then(|source| source.upstream_modified()))
    } else {
        ingested.and_then(|source| source.upstream_modified())
    };

    // Download time: preserved on a no-change refresh, else the current check
    // time (the canonical RFC 3339 `now` supplied by the caller).
    let fetched_at = if content_unchanged {
        previous
            .map(|prior| prior.fetched_at.clone())
            .unwrap_or_else(|| now.clone())
    } else {
        now.clone()
    };

    // The check time advances on every successful check; for a source the run
    // did not ingest this run we preserve the last known check (or stay unknown)
    // rather than fabricate one — the explicit-unknown representation the issue
    // asks for.
    let last_checked = match ingested {
        Some(_) => Some(now.clone()),
        None => previous.and_then(|prior| prior.last_checked.clone()),
    };

    Ok(Source {
        id,
        title: collection.title().to_string(),
        entry_url,
        document_urls,
        last_checked,
        fetched_at,
        upstream_modified,
        etag: ingested.and_then(|source| source.representative_etag()),
        content_hash,
        release: release_for(collection),
        parser: parser_version.to_string(),
    })
}

/// The release/channel a source record carries. The registry does not yet expose
/// per-collection release metadata, so the stable channel is recorded as the
/// default for the book-shaped collections; unknown releases are preserved
/// through [`Release`]. The one exception is `rustfmt`: its documents are
/// commit-pinned (issue #147), so its record carries the approved upstream
/// revision ([`crate::ingest::rustfmt::APPROVED_REVISION`]) as `revision`
/// provenance — the "record an approved revision" part of the fix — while the
/// channel stays the transitional stable default.
fn release_for(collection: SourceCollection) -> Release {
    match collection {
        // rustfmt's documents are commit-pinned, so record the approved revision
        // as the corpus's provenance for this source. Per `plan.md` §5 a
        // commit-pinned rustfmt document is not automatically a stable release,
        // so the *revision* (not a toolchain version) is the meaningful
        // provenance here; the channel keeps the transitional default.
        SourceCollection::Rustfmt => Release {
            channel: Channel::Stable,
            documentation_release: None,
            revision: Some(rustfmt::APPROVED_REVISION.to_string()),
        },
        _ => Release::channel_only(Channel::Stable),
    }
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

/// The previous snapshot's [`Source`] record for `source_id`, when a prior
/// snapshot was published and carried a record for that source. Used to carry
/// download time, the upstream-modification hint, and the last check time
/// forward on a no-change refresh (issue #152: preserve evidence rather than
/// fabricate a new one, while still advancing the check time).
fn previous_source<'a>(
    previous: &'a Option<SnapshotManifest>,
    source_id: &str,
) -> Option<&'a Source> {
    previous.as_ref().and_then(|manifest| {
        manifest
            .body
            .sources
            .iter()
            .find(|source| source.id.as_str() == source_id)
    })
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

/// A "now" timestamp for the snapshot, derived from the system clock, as a
/// canonical RFC 3339 instant (issue #152). It round-trips through
/// [`crate::provenance::Instant::parse`] and matches the format a `Last-Modified`
/// hint is normalized to, so the snapshot's check time and the upstream-modified
/// hint share one representation (both parseable, both comparable).
fn now_timestamp() -> Timestamp {
    let instant = crate::provenance::Instant::from_system_time(SystemTime::now());
    Timestamp::new(instant.to_rfc3339()).expect("a canonical RFC 3339 timestamp is valid")
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
    /// The assembled snapshot is well-formed but not a *complete* production
    /// corpus — a required collection is empty, a cross-reference is missing,
    /// or attribution is absent (issue #151). This aborts *before* the atomic
    /// promotion, so the last valid 8-source snapshot is preserved and a
    /// degraded corpus can never replace it.
    IncompleteCorpus { report: CompletenessReport },
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
            Self::IncompleteCorpus { report } => {
                write!(
                    f,
                    "the assembled snapshot is not a complete production corpus ({} completeness gap(s)): {}",
                    report.problem_count(),
                    report.render()
                )
            }
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
            Self::IncompleteCorpus { .. } => None,
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
    use crate::corpus;
    use crate::ingest::fetch::{BoundClient, FetchPolicy, LoopbackGate};
    use crate::ingest::http_fixture::{Behavior, FixtureServer};
    use crate::model::SourceId;
    use crate::registry::SourceCollection;
    use std::collections::{BTreeMap, HashSet};
    use std::fs;
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
            rustfmt_urls: None,
            require_complete: false,
        }
    }

    /// An API Guidelines `about.html` page carrying an external `github.com`
    /// citation — the front matter whose external link must be serialized, not
    /// fetched (issue #146). Its nav links to `checklist.html`; the link is a
    /// citation the direct-URL override does not follow.
    fn api_guidelines_about_html() -> String {
        r#"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="checklist.html">Checklist</a></li></ol></nav>
  <main id="main-content">
    <h1 id="about">About the API Guidelines</h1>
    <p>To contribute, read the <a href="https://github.com/rust-lang/api-guidelines/blob/master/CONTRIBUTING.md">CONTRIBUTING guide</a> upstream.</p>
  </main>
</body></html>"#.to_string()
    }

    /// An API Guidelines `checklist.html` page with one entry (`C-CONV`, in the
    /// `Conversions and From` category) — the real checklist's shape: a nested
    /// `<ul>` of category `<li>`s, each with a `<strong>` group heading and a
    /// nested list of guideline `<li>`s linking to their code.
    fn api_guidelines_checklist_html() -> String {
        r#"<!DOCTYPE html>
<html><body>
  <main id="main-content">
    <h1 id="checklist">Checklist</h1>
    <ul>
      <li>
        <strong>Conversions and From</strong>
        <ul>
          <li><a href="conversions.html#c-conv">Prefer <code>From</code> over other methods (C-CONV)</a></li>
        </ul>
      </li>
    </ul>
  </main>
</body></html>"#.to_string()
    }

    /// A `checklist.html` page with **no** guideline entries — an empty/unparseable
    /// checklist layout, which the pipeline must reject rather than publish.
    fn api_guidelines_empty_checklist_html() -> String {
        r#"<!DOCTYPE html>
<html><body>
  <main id="main-content">
    <h1 id="checklist">Checklist</h1>
    <p>This page has no guideline entries.</p>
  </main>
</body></html>"#
            .to_string()
    }

    /// An API Guidelines `conversions.html` chapter whose `C-CONV` section explains
    /// the checklist's `C-CONV` entry — the explanatory section the mapping joins
    /// the entry to by code.
    fn api_guidelines_conversion_html() -> String {
        r#"<!DOCTYPE html>
<html><body>
  <main id="main-content">
    <h1 id="conversions">Conversions</h1>
    <h2 id="c-conv">Prefer From/TryFrom (C-CONV)</h2>
    <p>Implement <code>From</code> and <code>TryFrom</code> for conversions between types.</p>
  </main>
</body></html>"#
            .to_string()
    }

    /// A pinned `api-guidelines` config whose document URLs are `urls`, in the
    /// given order — so a test can drive discovery order explicitly.
    fn api_guidelines_config(urls: Vec<reqwest::Url>, parser_version: &str) -> SyncConfig {
        let mut direct: BTreeMap<String, Vec<reqwest::Url>> = BTreeMap::new();
        direct.insert("api-guidelines".to_string(), urls);
        SyncConfig {
            parser_version: parser_version.to_string(),
            sources: vec![SourceId::new("api-guidelines").expect("a registry id is URI-safe")],
            direct: Some(direct),
            rustfmt_urls: None,
            require_complete: false,
        }
    }

    /// The loopback base `http://{addr}/api-guidelines/`, used to build the
    /// about / checklist / conversion URLs a test pins.
    fn api_guidelines_base(server: &FixtureServer) -> String {
        format!("http://{}/api-guidelines", server.addr)
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
            rustfmt_urls: None,
            require_complete: false,
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

    /// AC1 (issue #151): a **production** sync whose live discovery is pinned to a
    /// single source (`style-guide`) assembles an incomplete corpus — seven of the
    /// eight collections carry no content — and must be **rejected at the
    /// publication gate** rather than overwriting the last valid snapshot. The
    /// previously published snapshot stays byte-for-byte intact.
    #[tokio::test]
    async fn ac151_a_production_sync_rejects_an_incomplete_corpus_and_preserves_the_prior() {
        // A single style-guide chapter, served from loopback — the only content
        // the production run will capture (the #151 bug: live discovery pinned to
        // one collection).
        let server = FixtureServer::start(vec![(
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
        let naming =
            reqwest::Url::parse(&format!("http://{}/style-guide/naming.html", server.addr))
                .expect("a valid loopback chapter URL");

        // 1. Establish a valid previous snapshot through the real pipeline (a dev
        //    sync, not yet gated), so there is something to preserve.
        let good_report = sync(&style_guide_config(naming.clone()), &data_dir, &client)
            .await
            .expect("the first dev sync publishes a valid snapshot");
        let prior_bytes = fs::read(corpus::published_snapshot_path(&data_dir))
            .expect("the prior snapshot is on disk");

        // 2. A production-style run: all eight collections are configured, but
        //    the `direct` override supplies URLs only for `style-guide`, so the
        //    other seven resolve to empty inventories — the exact shape the bug
        //    published. `require_complete` turns on the gate.
        let mut direct: BTreeMap<String, Vec<reqwest::Url>> = BTreeMap::new();
        direct.insert("style-guide".to_string(), vec![naming]);
        let production = SyncConfig {
            parser_version: "test/151".to_string(),
            sources: SourceCollection::ALL
                .iter()
                .map(|c| SourceId::new(c.id()).expect("a registry id is URI-safe"))
                .collect(),
            direct: Some(direct),
            rustfmt_urls: None,
            require_complete: true,
        };

        let err = sync(&production, &data_dir, &client)
            .await
            .expect_err("an incomplete production corpus must be rejected");
        assert!(
            matches!(err, SyncError::IncompleteCorpus { .. }),
            "the incomplete production sync must fail with IncompleteCorpus, got {err:?}"
        );
        let message = err.to_string();
        assert!(
            message.contains("completeness gap"),
            "the rejection must name the incompleteness, got: {message}"
        );

        // 3. AC1: the previously published snapshot is byte-for-byte intact —
        //    the rejected incomplete run must not have overwritten it.
        let still = fs::read(corpus::published_snapshot_path(&data_dir))
            .expect("the prior snapshot is still on disk");
        assert_eq!(
            still, prior_bytes,
            "the rejected incomplete sync must not overwrite the last valid snapshot"
        );
        let loaded =
            corpus::load_published_snapshot(&data_dir).expect("the prior snapshot still loads");
        assert_eq!(
            loaded.body.snapshot_id, good_report.snapshot,
            "the prior snapshot identity is preserved"
        );
    }

    /// AC1/AC2 (issue #147): a live (non-`direct`) sync discovers rustfmt
    /// through its own adapter — the two approved Markdown documents, here pointed
    /// at a loopback fixture via the test-only `rustfmt_urls` override — and
    /// never fetches a generic-book navigation page for it. The fixture receives
    /// exactly the two raw documents and nothing else, proving no book discovery
    /// ran for rustfmt.
    #[tokio::test]
    async fn rustfmt_live_discovery_routes_to_the_markdown_adapter_without_book_nav() {
        // Two approved rustfmt documents served from loopback, in fetch order:
        // `README.md` (a configuration section) then `Configurations.md` (the
        // option reference). Each starts with a preamble and carries a `## `
        // section with content, so every section has a non-empty body (the corpus
        // validator rejects an empty section body) and the adapter emits sections
        // rather than swallowing the body into prose.
        let readme = "rustfmt is a tool for formatting Rust code.\n\n## Configuration\n\nrustfmt reads its options from a `rustfmt.toml` file.\n";
        let configs = "The rustfmt configuration options reference.\n\n## Stable options\n\nA stable formatting option that is always available.\n";
        let server = FixtureServer::start(vec![
            (
                "/rustfmt/README.md".to_string(),
                Behavior::Text {
                    body: readme.as_bytes().to_vec(),
                    etag: Some("\"readme-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/markdown; charset=utf-8".to_string()),
                },
            ),
            (
                "/rustfmt/Configurations.md".to_string(),
                Behavior::Text {
                    body: configs.as_bytes().to_vec(),
                    etag: Some("\"configs-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/markdown; charset=utf-8".to_string()),
                },
            ),
        ]);
        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();

        // Point rustfmt's live discovery at the two loopback raw documents via
        // the test-only override (production uses `rustfmt::approved_documents`).
        let readme_url = reqwest::Url::parse(&format!("http://{}/rustfmt/README.md", server.addr))
            .expect("a valid loopback rustfmt README URL");
        let configs_url =
            reqwest::Url::parse(&format!("http://{}/rustfmt/Configurations.md", server.addr))
                .expect("a valid loopback rustfmt Configurations URL");
        let config = SyncConfig {
            parser_version: "test/147".to_string(),
            sources: vec![SourceId::new("rustfmt").expect("a registry id is URI-safe")],
            // Live discovery (no `direct` override), but rustfmt pointed at the
            // fixture — so the live-dispatch branch is exercised, not `direct`.
            direct: None,
            rustfmt_urls: Some(vec![readme_url, configs_url]),
            require_complete: false,
        };

        let report = sync(&config, &data_dir, &client)
            .await
            .expect("the rustfmt live sync succeeds through its own adapter");

        // The rustfmt result was ingested and emitted sections: the Markdown
        // adapter (not the HTML book adapter) ran.
        let rustfmt = report
            .sources
            .iter()
            .find(|s| s.id == "rustfmt")
            .expect("the rustfmt result is present");
        assert_eq!(rustfmt.documents, 2, "both approved documents were fetched");
        assert!(
            rustfmt.sections >= 2,
            "the Markdown adapter emitted sections, got {}",
            rustfmt.sections
        );

        // No generic-book navigation page was fetched for rustfmt: the fixture
        // received exactly the two raw documents and nothing else. A book
        // discovery would have fetched a navigation page first (a third request).
        assert_eq!(
            server.request_count(),
            2,
            "only the two approved raw documents were fetched; no book navigation page"
        );

        // The published snapshot actually carries the two rustfmt sections and
        // the two approved document URLs on the source record.
        let loaded = crate::corpus::load_published_snapshot(&data_dir)
            .expect("the snapshot is published and loadable");
        let record = loaded
            .body
            .sources
            .iter()
            .find(|s| s.id.as_str() == "rustfmt")
            .expect("the rustfmt source record is published");
        assert_eq!(
            record.document_urls.len(),
            2,
            "the record carries the two documents"
        );
        // The published snapshot is the full all-eight-source manifest (every
        // approved collection is always recorded); rustfmt is the source whose
        // routing this hermetic sync exercises.
        assert_eq!(
            loaded.body.sources.len(),
            8,
            "the published manifest records all eight approved sources"
        );
        // AC4 (the "record an approved revision" part): the rustfmt source record
        // carries the approved upstream commit as its revision provenance, so the
        // corpus records where the commit-pinned documents came from.
        assert_eq!(
            record.release.revision.as_deref(),
            Some(crate::ingest::rustfmt::APPROVED_REVISION),
            "the rustfmt record records the approved revision"
        );
        assert!(
            loaded
                .body
                .sections
                .iter()
                .any(|s| s.source_id.as_str() == "rustfmt"),
            "the published snapshot carries rustfmt sections"
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
    /// the document pages — proving no external target was requested.
    ///
    /// Since issue #148, the `api-guidelines` source also requires a nonempty
    /// checklist, so this fixture pins the real `about` + `checklist` + `conversions`
    /// inventory: `about` carries the citation, `checklist` carries the one
    /// `C-CONV` entry, and `conversions` explains it.
    #[tokio::test]
    async fn the_pipeline_proceeds_past_an_external_citation_and_requests_no_external_target() {
        let server = FixtureServer::start(vec![
            (
                "/api-guidelines/about.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_about_html().into_bytes(),
                    etag: Some("\"about-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            (
                "/api-guidelines/checklist.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_checklist_html().into_bytes(),
                    etag: Some("\"checklist-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            (
                "/api-guidelines/conversions.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_conversion_html().into_bytes(),
                    etag: Some("\"conversions-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
        ]);
        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();
        let base = api_guidelines_base(&server);
        let about_url = reqwest::Url::parse(&format!("{base}/about.html"))
            .expect("a valid loopback about-page URL");
        let checklist_url = reqwest::Url::parse(&format!("{base}/checklist.html"))
            .expect("a valid loopback checklist-page URL");
        let conversions_url = reqwest::Url::parse(&format!("{base}/conversions.html"))
            .expect("a valid loopback conversions-page URL");
        // The about page is first, mirroring the discovery order that book the
        // front matter before the checklist — the order #148 fixed.
        let config =
            api_guidelines_config(vec![about_url, checklist_url, conversions_url], "test/146");

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
        assert_eq!(api.documents, 3, "the three document pages were fetched");
        assert!(api.sections >= 1, "the pages yielded at least one section");

        // AC2: the fixture server received ONLY the three document pages — the
        // external `github.com` citation was serialized, never fetched.
        assert_eq!(
            server.request_count(),
            3,
            "no external target may be requested; only the three document pages were fetched"
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

        // Issue #148: the checklist the combined inventory publishes is nonempty
        // and its entry's explanation resolves.
        let c_conv = loaded
            .body
            .checklist
            .iter()
            .find(|e| e.code == "C-CONV")
            .expect("the published checklist carries the C-CONV entry");
        assert_eq!(
            c_conv.explanation_sections.len(),
            1,
            "C-CONV must resolve to exactly one explanatory section"
        );
    }

    // -- Issue #148: the api-guidelines checklist is parsed by document identity,
    //    not discovery order, and a missing/empty/malformed checklist fails the
    //    run rather than publishing a zero-entry checklist. --

    /// AC1: reordering the `about` / `checklist` / `conversions` inventory does
    /// not change the published checklist. The checklist is selected by its URL
    /// identity (`checklist.html`), so booking the front matter first (the order
    /// that triggered the bug) or last yields the same published entry and
    /// mapping. The two runs publish identical checklists.
    #[tokio::test]
    async fn the_checklist_is_selected_by_identity_regardless_of_discovery_order() {
        let server = FixtureServer::start(vec![
            (
                "/api-guidelines/about.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_about_html().into_bytes(),
                    etag: Some("\"about-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            (
                "/api-guidelines/checklist.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_checklist_html().into_bytes(),
                    etag: Some("\"checklist-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            (
                "/api-guidelines/conversions.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_conversion_html().into_bytes(),
                    etag: Some("\"conversions-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
        ]);
        let client = loopback_client().expect("the loopback client builds");
        let base = api_guidelines_base(&server);
        let about_url = reqwest::Url::parse(&format!("{base}/about.html"))
            .expect("a valid loopback about-page URL");
        let checklist_url = reqwest::Url::parse(&format!("{base}/checklist.html"))
            .expect("a valid loopback checklist-page URL");
        let conversions_url = reqwest::Url::parse(&format!("{base}/conversions.html"))
            .expect("a valid loopback conversions-page URL");

        // Run A: the buggy order — the front matter is booked before the checklist.
        let data_a = temp_data_dir();
        let report_a = sync(
            &api_guidelines_config(
                vec![
                    about_url.clone(),
                    checklist_url.clone(),
                    conversions_url.clone(),
                ],
                "test/148a",
            ),
            &data_a,
            &client,
        )
        .await
        .expect("the front-matter-first run publishes a nonempty checklist");

        // Run B: the checklist and front matter are booked in the opposite order.
        let data_b = temp_data_dir();
        let report_b = sync(
            &api_guidelines_config(vec![conversions_url, checklist_url, about_url], "test/148b"),
            &data_b,
            &client,
        )
        .await
        .expect("the front-matter-last run publishes a nonempty checklist");

        // Both runs publish, and their checklists are identical — the discovery
        // order did not change what the pipeline published.
        let loaded_a = crate::corpus::load_published_snapshot(&data_a)
            .expect("the front-matter-first snapshot is published and loadable");
        let loaded_b = crate::corpus::load_published_snapshot(&data_b)
            .expect("the front-matter-last snapshot is published and loadable");
        assert!(
            loaded_a.body.checklist == loaded_b.body.checklist,
            "the published checklist must be independent of discovery order"
        );
        assert!(!report_a.snapshot.as_str().is_empty());
        assert!(!report_b.snapshot.as_str().is_empty());

        // The published entry resolves its explanation, so a later retrieval can
        // reach the section that explains C-CONV.
        let c_conv = loaded_a
            .body
            .checklist
            .iter()
            .find(|e| e.code == "C-CONV")
            .expect("the published checklist carries the C-CONV entry");
        assert_eq!(
            c_conv.explanation_sections.len(),
            1,
            "C-CONV must resolve to exactly one explanatory section"
        );
    }

    /// AC3: when the source's pages are fetched but none of them is
    /// `checklist.html`, the run fails with a source-specific extraction error
    /// and the previously published snapshot is preserved — a missing checklist
    /// is a validation failure, not a published empty checklist.
    #[tokio::test]
    async fn a_missing_checklist_page_aborts_the_sync_and_preserves_the_prior_corpus() {
        let server = FixtureServer::start(vec![
            (
                "/api-guidelines/about.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_about_html().into_bytes(),
                    etag: Some("\"about-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            (
                "/api-guidelines/checklist.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_checklist_html().into_bytes(),
                    etag: Some("\"checklist-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            (
                "/api-guidelines/conversions.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_conversion_html().into_bytes(),
                    etag: Some("\"conversions-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
        ]);
        let client = loopback_client().expect("the loopback client builds");
        let base = api_guidelines_base(&server);
        let about_url = reqwest::Url::parse(&format!("{base}/about.html"))
            .expect("a valid loopback about-page URL");
        let checklist_url = reqwest::Url::parse(&format!("{base}/checklist.html"))
            .expect("a valid loopback checklist-page URL");
        let conversions_url = reqwest::Url::parse(&format!("{base}/conversions.html"))
            .expect("a valid loopback conversions-page URL");
        let data_dir = temp_data_dir();

        // Publish a known-good snapshot first (a nonempty, explained checklist).
        let good = sync(
            &api_guidelines_config(
                vec![
                    about_url.clone(),
                    checklist_url.clone(),
                    conversions_url.clone(),
                ],
                "test/148-good",
            ),
            &data_dir,
            &client,
        )
        .await
        .expect("the good sync publishes");
        assert!(!good.snapshot.as_str().is_empty());

        // Now a sync whose inventory omits the checklist page entirely. It must
        // fail and must NOT overwrite the good snapshot.
        let err = sync(
            &api_guidelines_config(vec![about_url, conversions_url], "test/148-missing"),
            &data_dir,
            &client,
        )
        .await
        .expect_err("a missing checklist page must fail the sync");
        assert!(
            matches!(
                err,
                SyncError::Extract { ref source, .. } if source.as_str() == "api-guidelines"
            ),
            "expected an api-guidelines extraction failure, got {err:?}"
        );

        // AC3: the previously published snapshot is preserved untouched.
        let still = crate::corpus::load_published_snapshot(&data_dir)
            .expect("the good snapshot is still published");
        assert_eq!(
            still.body.snapshot_id, good.snapshot,
            "a failed sync must not overwrite the last valid snapshot"
        );
        assert!(
            !still.body.checklist.is_empty(),
            "the preserved snapshot still carries the nonempty checklist"
        );
    }

    /// AC3: a `checklist.html` that carries no guideline entries (an empty or
    /// unparseable layout) fails the run with a source-specific extraction
    /// error rather than publishing a zero-entry checklist.
    #[tokio::test]
    async fn an_empty_checklist_page_aborts_the_sync() {
        let server = FixtureServer::start(vec![
            (
                "/api-guidelines/about.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_about_html().into_bytes(),
                    etag: Some("\"about-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            (
                "/api-guidelines/checklist.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_empty_checklist_html().into_bytes(),
                    etag: Some("\"checklist-empty\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
        ]);
        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();
        let base = api_guidelines_base(&server);
        let about_url = reqwest::Url::parse(&format!("{base}/about.html"))
            .expect("a valid loopback about-page URL");
        let checklist_url = reqwest::Url::parse(&format!("{base}/checklist.html"))
            .expect("a valid loopback checklist-page URL");

        let err = sync(
            &api_guidelines_config(vec![about_url, checklist_url], "test/148-empty"),
            &data_dir,
            &client,
        )
        .await
        .expect_err("an empty checklist page must fail the sync");
        assert!(
            matches!(
                err,
                SyncError::Extract { ref source, ref reason }
                    if source.as_str() == "api-guidelines" && reason.contains("checklist")
            ),
            "expected an api-guidelines checklist extraction failure, got {err:?}"
        );

        // No snapshot was published by the failed run.
        assert!(
            crate::corpus::load_published_snapshot(&data_dir).is_err(),
            "a failed sync must publish nothing"
        );
    }

    /// AC2 (negative): a nonempty checklist whose entry has no resolving
    /// explanation section fails validation. Here the `conversions.html` that
    /// explains `C-CONV` is absent, so the mapping is incomplete and the run
    /// aborts rather than publishing an entry with an empty target.
    #[tokio::test]
    async fn a_checklist_without_a_resolving_explanation_fails_validation() {
        let server = FixtureServer::start(vec![
            (
                "/api-guidelines/about.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_about_html().into_bytes(),
                    etag: Some("\"about-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            (
                "/api-guidelines/checklist.html".to_string(),
                Behavior::Text {
                    body: api_guidelines_checklist_html().into_bytes(),
                    etag: Some("\"checklist-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
        ]);
        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();
        let base = api_guidelines_base(&server);
        let about_url = reqwest::Url::parse(&format!("{base}/about.html"))
            .expect("a valid loopback about-page URL");
        let checklist_url = reqwest::Url::parse(&format!("{base}/checklist.html"))
            .expect("a valid loopback checklist-page URL");

        // `conversions.html` (the explanation) is deliberately absent, so
        // C-CONV has no matching section.
        let err = sync(
            &api_guidelines_config(vec![about_url, checklist_url], "test/148-noexplanation"),
            &data_dir,
            &client,
        )
        .await
        .expect_err("a checklist whose entry has no explanation must fail the sync");
        assert!(
            matches!(
                err,
                SyncError::Extract { ref source, .. } if source.as_str() == "api-guidelines"
            ),
            "expected an api-guidelines extraction failure, got {err:?}"
        );
        assert!(
            crate::corpus::load_published_snapshot(&data_dir).is_err(),
            "a failed sync must publish nothing"
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
            rustfmt_urls: None,
            require_complete: false,
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

    /// AC5 (opt-in, live, issue #147): the production rustfmt discovery fetches
    /// the two approved, commit-pinned `raw.githubusercontent.com` Markdown
    /// documents (`README.md` + `Configurations.md`) and ingests them through the
    /// Markdown adapter. This is the real-network verification that
    /// `rustfmt::approved_documents()` resolves at the pinned revision through the
    /// registry-gated client, rather than failing on the refused `github.com`
    /// entry. It is `#[ignore]`d and gated on `RUST_DOCS_MCP_LIVE_SYNC` so the
    /// hermetic suite never touches the network. Opt in with:
    ///
    /// ```sh
    /// RUST_DOCS_MCP_LIVE_SYNC=1 cargo test -- --ignored \
    ///   rustfmt_live_sync_ingests_the_approved_raw_documents
    /// ```
    #[tokio::test]
    #[ignore = "opt-in live sync — run with `RUST_DOCS_MCP_LIVE_SYNC=1 cargo test -- --ignored`"]
    async fn rustfmt_live_sync_ingests_the_approved_raw_documents() {
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
        // A focused live sync over just rustfmt, using the production discovery
        // (`rustfmt_urls: None` → `rustfmt::approved_documents()` at the pinned
        // revision). This must succeed through the registry-gated client rather
        // than failing on the refused `github.com` entry — the original bug.
        let config = SyncConfig {
            parser_version: "test/live-147".to_string(),
            sources: vec![SourceId::new("rustfmt").expect("a registry id is URI-safe")],
            direct: None,
            rustfmt_urls: None,
            require_complete: false,
        };
        let report = sync(&config, &data_dir, &client)
            .await
            .expect("the live rustfmt sync ingests the approved raw documents");
        assert!(
            !report.snapshot.as_str().is_empty(),
            "a snapshot must be published"
        );
        let rustfmt = report
            .sources
            .iter()
            .find(|s| s.id == "rustfmt")
            .expect("the rustfmt result is present");
        assert_eq!(rustfmt.documents, 2, "both approved documents were fetched");
        assert!(
            rustfmt.sections > 0,
            "the Markdown adapter emitted sections"
        );
    }

    // -- Issue #152: real source provenance (content hash, freshness timestamps,
    //    and upstream ETag / Last-Modified / pinned revision) is recorded during
    //    sync. These five acceptance tests exercise the fix over a loopback
    //    `FixtureServer` (no network) and through the real sync path. --

    /// A captured document for the content-identity tests: raw `bytes` plus the
    /// response metadata a content identity and freshness evidence are built
    /// from. `etag`/`last_modified` are optional upstream evidence.
    fn captured_doc(url: &str, bytes: &[u8], etag: Option<&str>) -> super::CapturedDocument {
        super::CapturedDocument {
            url: reqwest::Url::parse(url).expect("a valid capture URL"),
            bytes: bytes.to_vec(),
            etag: etag.map(str::to_string),
            last_modified: None,
        }
    }

    /// A minimal ingested source carrying exactly the given captured documents
    /// (no sections / checklist), so a test can drive `content_hash` / the
    /// representative `ETag` / `Last-Modified` directly over a captured set.
    fn ingested_with_captured(docs: Vec<super::CapturedDocument>) -> super::IngestedSource {
        let urls = docs.iter().map(|d| d.url.clone()).collect();
        super::IngestedSource {
            urls,
            sections: Vec::new(),
            checklist: Vec::new(),
            captured: docs,
        }
    }

    /// AC1 (issue #152): a content identity is computed over the captured
    /// document *bytes* — distinct bodies hash distinctly, an unchanged body
    /// hashes identically — never a constant per-source hash over the
    /// collection id. The identity is also independent of fetch order (the
    /// captured documents are URL-sorted before hashing).
    #[test]
    fn ac1_content_hash_distinguishes_bodies() {
        // An identical body hashes identically.
        let a = ingested_with_captured(vec![captured_doc("https://e/a", b"the body", None)])
            .content_hash();
        let b = ingested_with_captured(vec![captured_doc("https://e/a", b"the body", None)])
            .content_hash();
        assert_eq!(a, b, "an identical body must hash identically");

        // A different body hashes differently.
        let c =
            ingested_with_captured(vec![captured_doc("https://e/a", b"a different body", None)])
                .content_hash();
        assert_ne!(a, c, "a different body must hash differently");

        // The identity is order-independent: the captured documents are
        // URL-sorted before hashing, so two fetch orders of the same documents
        // hash identically.
        let multi = ingested_with_captured(vec![
            captured_doc("https://e/a", b"first", None),
            captured_doc("https://e/b", b"second", None),
        ])
        .content_hash();
        let multi_rev = ingested_with_captured(vec![
            captured_doc("https://e/b", b"second", None),
            captured_doc("https://e/a", b"first", None),
        ])
        .content_hash();
        assert_eq!(
            multi, multi_rev,
            "the content identity is independent of fetch order"
        );
    }

    /// AC2 (issue #152): a successful sync records a *parseable* `last_checked`
    /// (a real instant, not a raw counter) and a *fresh* verdict — the source's
    /// last check is the sync's "now", within the staleness threshold.
    #[tokio::test]
    async fn ac2_successful_sync_records_parseable_last_checked_and_fresh_verdict() {
        use crate::provenance::{StaleThreshold, evaluate_freshness};

        let server = FixtureServer::start(vec![(
            "/style-guide/naming.html".to_string(),
            Behavior::Text {
                body: one_section_html("naming", "Naming").into_bytes(),
                etag: Some("\"naming-1\"".to_string()),
                last_modified: Some("Wed, 09 Oct 2024 06:00:00 GMT".to_string()),
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();
        let url = reqwest::Url::parse(&format!("http://{}/style-guide/naming.html", server.addr))
            .expect("a valid loopback URL");
        let config = style_guide_config(url);

        sync(&config, &data_dir, &client)
            .await
            .expect("a successful sync publishes a snapshot");

        let loaded = crate::corpus::load_published_snapshot(&data_dir)
            .expect("the snapshot is published and loadable");
        let src = loaded
            .body
            .sources
            .iter()
            .find(|s| s.id.as_str() == "style-guide")
            .expect("the style-guide source record is present");

        // `last_checked` is recorded and is a real, parseable instant.
        let last_checked = src
            .last_checked
            .as_ref()
            .expect("a successful sync records a last_checked");
        crate::provenance::Instant::parse(last_checked.as_str())
            .expect("a recorded last_checked is a real, parseable instant");

        // ... and it yields a fresh verdict: the check is the sync's "now",
        // within the threshold.
        let as_of = crate::provenance::Instant::from_system_time(std::time::SystemTime::now());
        let verdict = evaluate_freshness(src, as_of, StaleThreshold::default());
        assert_eq!(
            verdict.stale,
            Some(false),
            "a just-synced source is fresh, not stale"
        );
    }

    /// AC3 (issue #152): a no-change refresh (a second sync over identical
    /// captured content) preserves the content *identity* — the content hash and
    /// the download time are unchanged — while the *check time advances* (the
    /// second check stamps a later-or-equal instant). The two syncs share a data
    /// directory, so the second reads the first's published snapshot as its
    /// `previous` and measures "unchanged" against it.
    #[tokio::test]
    async fn ac3_no_change_refresh_preserves_identity_and_advances_check_time() {
        let server = FixtureServer::start(vec![(
            "/style-guide/naming.html".to_string(),
            Behavior::Text {
                body: one_section_html("naming", "Naming").into_bytes(),
                etag: Some("\"naming-1\"".to_string()),
                last_modified: Some("Wed, 09 Oct 2024 06:00:00 GMT".to_string()),
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();
        let url = reqwest::Url::parse(&format!("http://{}/style-guide/naming.html", server.addr))
            .expect("a valid loopback URL");
        let config = style_guide_config(url);

        // The first sync publishes the snapshot.
        sync(&config, &data_dir, &client)
            .await
            .expect("the first sync publishes");
        let m1 = crate::corpus::load_published_snapshot(&data_dir)
            .expect("the first snapshot is loadable");
        let s1 = m1
            .body
            .sources
            .iter()
            .find(|s| s.id.as_str() == "style-guide")
            .expect("the style-guide record is present in the first snapshot");

        // The second sync reads the first's published snapshot as `previous` and
        // finds the content unchanged (the fixture serves identical bytes).
        sync(&config, &data_dir, &client)
            .await
            .expect("the second sync publishes");
        let m2 = crate::corpus::load_published_snapshot(&data_dir)
            .expect("the second snapshot is loadable");
        let s2 = m2
            .body
            .sources
            .iter()
            .find(|s| s.id.as_str() == "style-guide")
            .expect("the style-guide record is present in the second snapshot");

        // Content identity is preserved across the no-change refresh.
        assert_eq!(
            s1.content_hash, s2.content_hash,
            "a no-change refresh preserves the content identity"
        );
        assert_eq!(
            s1.fetched_at, s2.fetched_at,
            "a no-change refresh preserves the download time"
        );
        // The check time advanced: the second check stamped a later-or-equal
        // instant (it can only equal under the same clock instant, never go
        // backward).
        let lc1 = crate::provenance::Instant::parse(
            s1.last_checked
                .as_ref()
                .expect("the first sync recorded a check time")
                .as_str(),
        )
        .expect("the first check time parses");
        let lc2 = crate::provenance::Instant::parse(
            s2.last_checked
                .as_ref()
                .expect("the second sync recorded a check time")
                .as_str(),
        )
        .expect("the second check time parses");
        assert!(
            lc2 >= lc1,
            "the check time advanced (or held) on the no-change refresh"
        );
    }

    /// AC4 (issue #152): a sync retains the upstream `ETag` and `Last-Modified`
    /// evidence on the source record (the `ETag` is the URL-representative one;
    /// the `Last-Modified` is the most-recent parseable hint, normalized to UTC
    /// RFC 3339), and rustfmt's record carries its approved *pinned revision* as
    /// release provenance.
    #[tokio::test]
    async fn ac4_etag_last_modified_and_pinned_revision_retained() {
        // Only `README.md` carries the `ETag` / `Last-Modified` evidence, so the
        // representative `ETag` is unambiguous and the `Last-Modified` hint is
        // the one the upstream supplied.
        let readme = "rustfmt is a tool for formatting Rust code.\n\n## Configuration\n\nrustfmt reads its options from a `rustfmt.toml` file.\n";
        let configs = "The rustfmt configuration options reference.\n\n## Stable options\n\nA stable formatting option that is always available.\n";
        let server = FixtureServer::start(vec![
            (
                "/rustfmt/README.md".to_string(),
                Behavior::Text {
                    body: readme.as_bytes().to_vec(),
                    etag: Some("\"readme-42\"".to_string()),
                    last_modified: Some("Mon, 13 May 2024 14:30:00 GMT".to_string()),
                    content_type: Some("text/markdown; charset=utf-8".to_string()),
                },
            ),
            (
                "/rustfmt/Configurations.md".to_string(),
                Behavior::Text {
                    body: configs.as_bytes().to_vec(),
                    etag: None,
                    last_modified: None,
                    content_type: Some("text/markdown; charset=utf-8".to_string()),
                },
            ),
        ]);
        let client = loopback_client().expect("the loopback client builds");
        let data_dir = temp_data_dir();
        let readme_url = reqwest::Url::parse(&format!("http://{}/rustfmt/README.md", server.addr))
            .expect("a valid loopback URL");
        let configs_url =
            reqwest::Url::parse(&format!("http://{}/rustfmt/Configurations.md", server.addr))
                .expect("a valid loopback URL");
        let config = SyncConfig {
            parser_version: "test/152".to_string(),
            sources: vec![SourceId::new("rustfmt").expect("a registry id is URI-safe")],
            direct: None,
            rustfmt_urls: Some(vec![readme_url, configs_url]),
            require_complete: false,
        };

        sync(&config, &data_dir, &client)
            .await
            .expect("the rustfmt sync succeeds through its own adapter");
        let loaded = crate::corpus::load_published_snapshot(&data_dir)
            .expect("the snapshot is published and loadable");
        let rustfmt = loaded
            .body
            .sources
            .iter()
            .find(|s| s.id.as_str() == "rustfmt")
            .expect("the rustfmt source record is present");

        // The `ETag` the upstream supplied is retained on the record.
        assert_eq!(
            rustfmt.etag.as_deref(),
            Some("\"readme-42\""),
            "the captured ETag is retained on the record"
        );
        // The `Last-Modified` hint is retained and parseable, normalized to
        // UTC RFC 3339.
        let um = rustfmt
            .upstream_modified
            .as_ref()
            .expect("the Last-Modified hint is retained");
        let parsed =
            crate::provenance::Instant::parse(um.as_str()).expect("the hint is a real instant");
        assert_eq!(
            parsed.to_rfc3339(),
            "2024-05-13T14:30:00Z",
            "the HTTP-date Last-Modified normalizes to UTC RFC 3339"
        );
        // rustfmt's record carries its approved pinned revision as provenance.
        assert_eq!(
            rustfmt.release.revision.as_deref(),
            Some(crate::ingest::rustfmt::APPROVED_REVISION),
            "the pinned revision is retained on rustfmt's record"
        );
    }

    /// AC5 (issue #152): the sync's timestamps are canonical RFC 3339 and
    /// round-trip through `Instant::parse` — `now_timestamp` emits a parseable
    /// instant, a parsed instant's canonical form round-trips exactly, and an
    /// HTTP-date `Last-Modified` hint parses to the same instant its RFC 3339
    /// spelling would (so a captured hint and a `last_checked` share one
    /// representation).
    #[test]
    fn ac5_timestamps_round_trip_through_instant_parse() {
        use crate::provenance::Instant;

        // The sync's "now" stamp is RFC 3339 and round-trips.
        let now = super::now_timestamp();
        let parsed = Instant::parse(now.as_str()).expect("now_timestamp emits RFC 3339");
        assert_eq!(
            parsed.to_rfc3339(),
            now.as_str(),
            "the sync's stamp round-trips exactly"
        );

        // Parsed instants of the canonical grammar round-trip exactly, including
        // the pre-epoch and far-future boundaries.
        for literal in [
            "1970-01-01T00:00:00Z",
            "2024-05-13T14:30:00Z",
            "2037-12-31T23:59:59Z",
        ] {
            let i = Instant::parse(literal).expect("a valid RFC 3339 literal");
            assert_eq!(i.to_rfc3339(), literal, "round-trips: {literal}");
        }

        // An HTTP-date `Last-Modified` hint parses (via the dual-path
        // `parse_date_time`) to the same instant its RFC 3339 spelling does, so a
        // captured hint and a `last_checked` share one representation.
        let http = crate::provenance::parse_date_time("Mon, 13 May 2024 14:30:00 GMT")
            .expect("the HTTP-date hint parses");
        assert_eq!(
            http.to_rfc3339(),
            "2024-05-13T14:30:00Z",
            "the HTTP-date hint normalizes to the same instant as its RFC 3339 spelling"
        );
    }
}
