//! Corpus validation: the gate a *candidate* snapshot must clear before it may
//! replace the last valid corpus.
//!
//! Tasks `[029]`-`[030]` load the latest valid synchronized snapshot (or the
//! bundled fallback) and publish it atomically; task `[025]` (this module) is
//! the integrity check that publication runs *first*. `plan.md` §5 requires that
//! "all eight required collections must validate before publishing a complete
//! replacement snapshot" and that "corrupt snapshots … report actionable
//! errors"; §9 requires that "schema versions, checksums, IDs, and bounded
//! sizes" are validated "before loading a snapshot."
//!
//! The type layer (`crate::model`) deserializes every record field
//! *independently* and gates only the cheap, per-field invariants
//! (non-empty ids, URI-safety, shell-operator-free command keys, a non-empty
//! content hash). A corrupt or hand-edited corpus can still carry an
//! inconsistency the type layer cannot see — a `Section` whose stored
//! [`SectionId`](crate::model::SectionId) does not match the id
//! *re-derived* from its own fields, a `content_hash` that no longer describes
//! its content, a cross-reference to a section that does not exist, a missing
//! required source, or empty content. This module performs those cross-field,
//! cross-record, and corpus-wide checks and reports them as an actionable
//! [`ValidationReport`] rather than failing on the first problem or silently
//! passing.
//!
//! ## What is validated
//!
//! - **All eight required sources are present** (`crate::registry::SourceCollection::ALL`).
//!   Each required collection must appear exactly once; a missing collection is
//!   reported by id, and a duplicated source id is reported with its count.
//! - **Non-empty content**: the corpus must carry at least one section, and no
//!   section may have an empty (whitespace-only) body.
//! - **Unique identities**: no two sections may share a
//!   [`SectionId`](crate::model::SectionId).
//! - **Consistent identities**: each section's stored `id` must equal the id
//!   [`SectionId::derive`](crate::model::SectionId::derive) recomputes from the
//!   section's own `source_id`, `chapter_path`, `anchor`, and `continuation`.
//! - **Valid hashes**: each section's stored `content_hash` must equal the
//!   [`CorpusHash`](crate::model::CorpusHash) recomputed over its content
//!   ([`Section::expected_content_hash`](crate::model::Section::expected_content_hash)).
//! - **Resolvable cross-references**: every
//!   [`ChecklistEntry::explanation_sections`](crate::model::ChecklistEntry) and
//!   [`CommandEntry::upstream_sections`](crate::model::CommandEntry) target must
//!   name a section that exists in the snapshot.
//!
//! The result is a [`ValidationReport`]: a deterministic list of
//! [`ValidationProblem`]s plus a populated [`ValidationSummary`]. A snapshot is
//! publishable when the report has **no** problems. Nothing here panics on a
//! corrupt or malformed candidate — every failure is a typed, actionable
//! problem.
//!
//! ## Trust boundary and scope
//!
//! The corpus is *reference data* (`plan.md` §5/§9): this module only inspects
//! it. It performs **no** network call, **no** command execution, and **no**
//! filesystem or project access. It is pure and offline; the checks run against
//! an in-memory [`SnapshotManifest`](crate::model::SnapshotManifest). It does
//! **not** authenticate an untrusted corpus (a checksum detects corruption, not
//! tamperers — `plan.md` §9) and does **not** change the model's serialization
//! contract or the `c128-` content-hash algorithm; it *recomputes* the existing
//! hash and compares.

// Like `crate::model` and `crate::registry`, this foundational module's public
// items are not yet reached from within the crate: the publication path
// (`[029]`-`[030]`) and the MCP surface (`[048]`-`[059]`) are the consumers.
// The allow is lifted incrementally as they wire in; it is scoped to this
// module only, so a later unwired item re-appears in a dead-code check the
// moment it is written.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{ManifestBody, SchemaError, SectionId, SnapshotManifest, ValidationSummary};
use crate::registry::SourceCollection;

// ---------------------------------------------------------------------------
// The validation result
// ---------------------------------------------------------------------------

/// One actionable problem found while validating a candidate corpus.
///
/// Each variant names the record and the specific defect, so a report points an
/// operator at what to fix rather than emitting a bare "invalid." The variants
/// are the exact failure modes the acceptance criteria hinge on: missing
/// sources, empty content, duplicate or inconsistent identities, invalid
/// hashes, and unresolved cross-references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationProblem {
    /// A required source collection is absent from the snapshot.
    ///
    /// `plan.md` §5: all eight required collections must validate before
    /// publication.
    MissingSource {
        /// The required source id that is not present (e.g. `rustdoc`).
        source: String,
    },
    /// A required source id appears more than once in the snapshot.
    DuplicateSource {
        /// The duplicated source id.
        source: String,
        /// How many times it appears.
        occurrences: usize,
    },
    /// The snapshot carries no sections at all: a genuinely empty corpus is not
    /// a valid replacement.
    EmptyCorpus,
    /// A section's `body` is empty (or whitespace only): the section captured no
    /// content.
    EmptySectionBody {
        /// The offending section's id.
        section_id: String,
    },
    /// Two or more sections share the same deterministic
    /// [`SectionId`](crate::model::SectionId).
    DuplicateSectionId {
        /// The duplicated section id.
        section_id: String,
        /// How many sections carry it.
        occurrences: usize,
    },
    /// A section's stored `id` does not match the id re-derived from its own
    /// `source_id`, `chapter_path`, `anchor`, and `continuation`.
    ///
    /// `derived` is either the id that *was* recomputed or a note when the
    /// section's fields cannot form a valid id at all.
    SectionIdMismatch {
        /// The section's stored id.
        section_id: String,
        /// The id recomputed from the section's fields, or a note when none
        /// could be derived.
        derived: String,
    },
    /// A section's stored `content_hash` does not match the hash recomputed over
    /// its content: the hash is stale, decorative, or otherwise inconsistent.
    ContentHashMismatch {
        /// The offending section's id.
        section_id: String,
        /// The hash recomputed from the section's content.
        expected: String,
        /// The hash the section stored.
        found: String,
    },
    /// A checklist entry references a section id that does not exist in the
    /// snapshot: an unresolved explanation mapping.
    UnresolvedChecklistReference {
        /// The checklist entry's code whose reference is unresolved.
        code: String,
        /// The section id that was referenced but does not exist.
        section_id: String,
    },
    /// A command entry references a section id that does not exist in the
    /// snapshot: an unresolved citation mapping.
    UnresolvedCommandReference {
        /// The command key whose citation is unresolved.
        command: String,
        /// The section id that was referenced but does not exist.
        section_id: String,
    },
}

impl std::fmt::Display for ValidationProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSource { source } => {
                write!(f, "missing required source collection: {source}")
            }
            Self::DuplicateSource {
                source,
                occurrences,
            } => {
                write!(
                    f,
                    "duplicate source id `{source}` appears {occurrences} times"
                )
            }
            Self::EmptyCorpus => write!(f, "corpus contains no sections"),
            Self::EmptySectionBody { section_id } => {
                write!(f, "section `{section_id}` has empty content")
            }
            Self::DuplicateSectionId {
                section_id,
                occurrences,
            } => write!(
                f,
                "section id `{section_id}` is shared by {occurrences} sections"
            ),
            Self::SectionIdMismatch {
                section_id,
                derived,
            } => {
                write!(
                    f,
                    "section id `{section_id}` does not match the id `{derived}` derived from its fields"
                )
            }
            Self::ContentHashMismatch {
                section_id,
                expected,
                found,
            } => write!(
                f,
                "section `{section_id}` content hash mismatch: stored `{found}`, expected `{expected}`"
            ),
            Self::UnresolvedChecklistReference { code, section_id } => write!(
                f,
                "checklist code {code} references unknown section `{section_id}`"
            ),
            Self::UnresolvedCommandReference {
                command,
                section_id,
            } => write!(
                f,
                "command `{command}` cites unknown section `{section_id}`"
            ),
        }
    }
}

/// The outcome of validating a candidate corpus: the problems found (if any) and
/// the [`ValidationSummary`] the validator computes.
///
/// A snapshot is publishable exactly when [`is_valid`](Self::is_valid) is true —
/// that is, when no problem was found. The [`ValidationSummary`] is always
/// populated so a consumer can record what was checked even when the run failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// Every problem found, in a deterministic order. Empty means the corpus is
    /// valid.
    pub problems: Vec<ValidationProblem>,
    /// The counts of the records the validator inspected.
    pub summary: ValidationSummary,
}

impl ValidationReport {
    /// Whether the validated corpus is free of problems and therefore
    /// publishable.
    pub fn is_valid(&self) -> bool {
        self.problems.is_empty()
    }

    /// The number of problems found.
    pub fn problem_count(&self) -> usize {
        self.problems.len()
    }

    /// Fail with this report when the corpus is invalid, yielding the summary
    /// when it is valid.
    ///
    /// This is the publication seam (`plan.md` §5: "publish only a validated
    /// snapshot"): `validate_snapshot(&m).require_valid()?` aborts publication of
    /// a corrupt candidate while preserving the last valid corpus.
    pub fn require_valid(self) -> Result<ValidationSummary, Self> {
        if self.problems.is_empty() {
            Ok(self.summary)
        } else {
            Err(self)
        }
    }
}

// ---------------------------------------------------------------------------
// The validator
// ---------------------------------------------------------------------------

/// Validate a candidate [`SnapshotManifest`] and return a [`ValidationReport`].
///
/// This is the complete-corpus gate run before publication. It runs **every**
/// check and aggregates the findings, so a report names *all* the defects at
/// once rather than stopping at the first. The checks, in order:
///
/// 1. the eight required sources are present (each exactly once);
/// 2. per-section integrity — non-empty content, id-vs-fields consistency,
///    hash-vs-content consistency, and unique section identities;
/// 3. cross-references — every checklist and command target names a section that
///    exists.
///
/// The schema version is gated by the loader
/// ([`SnapshotManifest::from_json_str`](crate::model::SnapshotManifest::from_json_str))
/// *before* a manifest reaches here, and [`SchemaVersion`](crate::model::SchemaVersion)
/// has a single supported value, so this function does not re-check it. It never
/// panics on a malformed candidate: every failure is collected as a
/// [`ValidationProblem`].
pub fn validate_snapshot(manifest: &SnapshotManifest) -> ValidationReport {
    let body = &manifest.body;
    let mut problems: Vec<ValidationProblem> = Vec::new();

    check_sources(body, &mut problems);
    check_sections(body, &mut problems);
    check_cross_references(body, &mut problems);

    let summary = ValidationSummary {
        // "Documents captured" per `plan.md` §5's document-inventory notion: the
        // number of document URLs the snapshot's sources record.
        documents: body.sources.iter().map(|s| s.document_urls.len()).sum(),
        sections: body.sections.len(),
        checklist_entries: body.checklist.len(),
        command_entries: body.commands.len(),
        // This validator records only fatal defects (the `problems`); it raises
        // no non-fatal warnings, so `warnings` is empty.
        warnings: Vec::new(),
    };

    ValidationReport { problems, summary }
}

/// Check that every required source collection is present, and report any
/// missing or duplicated source ids.
///
/// Presence is measured against the fixed eight ids
/// ([`SourceCollection::ALL`]); a missing collection is reported by its id, and
/// any source id that appears more than once is reported with its occurrence
/// count.
fn check_sources(body: &ManifestBody, problems: &mut Vec<ValidationProblem>) {
    // Presence, in the registry's fixed order, so the "which are missing" report
    // is deterministic.
    let present: HashSet<&str> = body.sources.iter().map(|s| s.id.as_str()).collect();
    for collection in SourceCollection::ALL {
        let id = collection.id();
        if !present.contains(id) {
            problems.push(ValidationProblem::MissingSource {
                source: id.to_string(),
            });
        }
    }

    // Duplicates, reported in sorted id order for determinism.
    let counts = occurrence_counts(body.sources.iter().map(|s| s.id.as_str()));
    for (source, occurrences) in duplicate_ids(counts) {
        problems.push(ValidationProblem::DuplicateSource {
            source: source.to_string(),
            occurrences,
        });
    }
}

/// Check per-section integrity: non-empty content, id-vs-fields consistency,
/// hash-vs-content consistency, and unique section identities.
fn check_sections(body: &ManifestBody, problems: &mut Vec<ValidationProblem>) {
    // A genuinely empty snapshot must not pass, even though every per-section
    // loop below trivially succeeds on an empty list.
    if body.sections.is_empty() {
        problems.push(ValidationProblem::EmptyCorpus);
    }

    for section in &body.sections {
        // Non-empty content: a whitespace-only body captured nothing.
        if section.body.trim().is_empty() {
            problems.push(ValidationProblem::EmptySectionBody {
                section_id: section.id.as_str().to_string(),
            });
        }

        // Identity consistency: the stored id must equal the id re-derived from
        // the section's own fields. A section whose fields cannot form a valid id
        // at all is reported too (its fields are structurally corrupt).
        let continuation = section.continuation.as_ref().map(|c| (c.part, c.parts));
        match SectionId::derive(
            &section.source_id,
            &section.chapter_path,
            section.anchor.as_deref(),
            continuation,
        ) {
            Ok(derived) if derived != section.id => {
                problems.push(ValidationProblem::SectionIdMismatch {
                    section_id: section.id.as_str().to_string(),
                    derived: derived.as_str().to_string(),
                })
            }
            Ok(_) => {}
            Err(err) => problems.push(ValidationProblem::SectionIdMismatch {
                section_id: section.id.as_str().to_string(),
                derived: format!("<no valid id derivable: {err}>"),
            }),
        }

        // Hash integrity: the stored hash must match a recomputation over the
        // section's content. A decorative or stale hash is rejected.
        let expected = section.expected_content_hash();
        if expected != section.content_hash {
            problems.push(ValidationProblem::ContentHashMismatch {
                section_id: section.id.as_str().to_string(),
                expected: expected.as_str().to_string(),
                found: section.content_hash.as_str().to_string(),
            });
        }
    }

    // Unique identities: no two sections may share a deterministic id.
    let counts = occurrence_counts(body.sections.iter().map(|s| s.id.as_str()));
    for (section_id, occurrences) in duplicate_ids(counts) {
        problems.push(ValidationProblem::DuplicateSectionId {
            section_id: section_id.to_string(),
            occurrences,
        });
    }
}

/// Check that every checklist and command cross-reference resolves to a section
/// that exists in the snapshot.
fn check_cross_references(body: &ManifestBody, problems: &mut Vec<ValidationProblem>) {
    let section_ids: HashSet<&str> = body.sections.iter().map(|s| s.id.as_str()).collect();

    for entry in &body.checklist {
        for target in &entry.explanation_sections {
            if !section_ids.contains(target.as_str()) {
                problems.push(ValidationProblem::UnresolvedChecklistReference {
                    code: entry.code.clone(),
                    section_id: target.as_str().to_string(),
                });
            }
        }
    }

    for entry in &body.commands {
        for target in &entry.upstream_sections {
            if !section_ids.contains(target.as_str()) {
                problems.push(ValidationProblem::UnresolvedCommandReference {
                    command: entry.key.as_str().to_string(),
                    section_id: target.as_str().to_string(),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count how often each id string occurs, in a map.
fn occurrence_counts<'a, I>(ids: I) -> HashMap<&'a str, usize>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for id in ids {
        *counts.entry(id).or_default() += 1;
    }
    counts
}

/// The ids that occur more than once, as `(id, occurrences)` pairs in sorted-id
/// order, so duplicate reports are deterministic.
fn duplicate_ids(counts: HashMap<&str, usize>) -> Vec<(&str, usize)> {
    let mut dups: Vec<(&str, usize)> = counts.into_iter().filter(|(_, count)| *count > 1).collect();
    dups.sort_by(|a, b| a.0.cmp(b.0));
    dups
}

// ---------------------------------------------------------------------------
// Atomic publication — task [026]
// ---------------------------------------------------------------------------
//
// On-disk layout (decided for this task). The active snapshot is a **single,
// self-contained manifest file** — `corpus/manifest.json` — written as the
// compact, deterministic JSON of a whole [`SnapshotManifest`]
// ([`SnapshotManifest::to_json_string`]) and read back with
// ([`SnapshotManifest::from_json_str`]).
//
// `SnapshotManifest` already carries the entire corpus *inline* (every
// `Source`, `Section`, checklist, and command entry), so a
// manifest-plus-side-cars split would only add a partition/assemble round-trip
// with no benefit for the offline, one-snapshot-per-process MVP (`plan.md` §5
// "runtime behavior": the snapshot is fixed for the process lifetime). A single
// file is what makes the atomic swap a one-step [`std::fs::rename`].
//
// Layout inside the configured application data directory:
//
// ```text
// <data_dir>/corpus/
//   manifest.json                     the active, published, immutable snapshot
//   snapshot-<pid>-<n>-<nanos>.tmp    a transient candidate during a publish
// ```
//
// The published file has a **fixed name** the loader reads; a transient file
// always carries a *different*, process/counter-stamped name, so a leftover from
// an interrupted run can never be mistaken for a published snapshot. The writer
// lock ([`crate::writer_lock`], task `[028]`) lives at a *third*, distinct name
// (`sync.lock`) in the same directory.
//
// Atomic publication: the candidate is validated in memory
// ([`validate_snapshot`] / [`ValidationReport::require_valid`]) **first**, then
// serialized to the transient file, flushed, and `sync_all`-ed for durability,
// and **only then** atomically `rename`-d over `manifest.json`. A failed
// validation, a failed write, or an interrupted run therefore leaves the
// previous valid snapshot untouched — the rename never happens, or happens
// atomically over a fully-written candidate. Every transient and published path
// is built under the *canonicalized* corpus directory, so a `..` or symlink
// component in the data directory cannot carry a write outside it, and a
// `rename` replaces a symlink node rather than following it.
//
// **Serialization (task `[028]`).** The filesystem-mutating half of a publish
// (stale-temp cleanup, candidate write, and atomic promotion) runs under the
// `crate::writer_lock` writer lock, held for its duration by an RAII guard, so
// two syncs cannot promote concurrently or interleave a write+rename. A stale
// lock left by a crashed or interrupted sync is reclaimed by the next sync, so
// publication recovers cleanly. See `crate::writer_lock` for the design and its
// documented, best-effort, std-only staleness boundary.

/// The application-data sub-directory that holds the published corpus and its
/// transient publication files.
const CORPUS_SUBDIR: &str = "corpus";

/// The fixed file name of the active snapshot. The loader reads *only* this
/// name, so a transient publication file (which carries a different name) can
/// never be mistaken for a published snapshot.
const PUBLISHED_NAME: &str = "manifest.json";

/// The name prefix of a transient publication file.
const TEMP_PREFIX: &str = "snapshot-";
/// The name suffix of a transient publication file.
const TEMP_SUFFIX: &str = ".tmp";

/// A hard upper bound on a published snapshot's on-disk size (bytes), per
/// `plan.md` §9 ("bounded … sizes"). The loader rejects a file above it rather
/// than reading it into memory.
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// A monotonic, process-local counter that makes each transient publication file
/// name unique within a process.
static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The result of a successful, validated atomic publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publication {
    /// The path of the active snapshot after publication (the fixed published
    /// file inside the configured data directory).
    pub path: PathBuf,
    /// The validation summary the candidate cleared on its way in.
    pub summary: ValidationSummary,
}

/// A failure to publish or load a snapshot, each variant carrying enough context
/// to be actionable rather than a bare `io::Error` (`plan.md` §9).
#[derive(Debug)]
pub enum PublishError {
    /// The candidate failed complete-corpus validation; the previously published
    /// snapshot is preserved untouched.
    Invalid(ValidationReport),
    /// The candidate (or a published file) failed the schema-version or
    /// (de)serialization gate.
    Schema(SchemaError),
    /// A snapshot file exceeded the on-disk size bound ([`MAX_SNAPSHOT_BYTES`]).
    TooLarge {
        /// The file's size in bytes.
        bytes: usize,
        /// The on-disk size bound that was exceeded.
        limit: usize,
    },
    /// A filesystem failure during write, flush, or atomic promotion, tagged with
    /// where it happened.
    Io {
        /// Short, human description of the operation that failed.
        context: String,
        /// The underlying I/O cause, preserved for the error chain.
        source: io::Error,
    },
    /// The sync writer lock could not be acquired because another sync holds a
    /// live, unstale lock ([`crate::writer_lock`]); this publish did not run, so
    /// the previously published snapshot is untouched. The caller may retry.
    Lock(crate::writer_lock::LockError),
}

impl PublishError {
    /// Build an [`PublishError::Io`] that records *where* a filesystem failure
    /// happened.
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Build an [`PublishError::TooLarge`] against the current size bound.
    pub fn too_large(bytes: usize) -> Self {
        Self::TooLarge {
            bytes,
            limit: MAX_SNAPSHOT_BYTES,
        }
    }
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(report) => {
                write!(
                    f,
                    "candidate snapshot is not publishable: {} problem(s)",
                    report.problem_count()
                )?;
                for problem in &report.problems {
                    writeln!(f, "  - {problem}")?;
                }
                Ok(())
            }
            Self::Schema(error) => write!(f, "schema or serialization error: {error}"),
            Self::TooLarge { bytes, limit } => write!(
                f,
                "snapshot is {bytes} bytes, exceeding the {limit}-byte on-disk limit"
            ),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::Lock(err) => write!(f, "could not acquire the sync writer lock: {err}"),
        }
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Lock(err) => Some(err),
            _ => None,
        }
    }
}

impl From<SchemaError> for PublishError {
    fn from(error: SchemaError) -> Self {
        Self::Schema(error)
    }
}

impl From<ValidationReport> for PublishError {
    fn from(report: ValidationReport) -> Self {
        Self::Invalid(report)
    }
}

impl From<io::Error> for PublishError {
    fn from(error: io::Error) -> Self {
        Self::io("io operation failed", error)
    }
}

impl From<crate::writer_lock::LockError> for PublishError {
    fn from(error: crate::writer_lock::LockError) -> Self {
        Self::Lock(error)
    }
}

/// Resolve the corpus sub-directory under `data_dir`, creating it and
/// canonicalizing it so a candidate can never escape the configured data
/// directory.
///
/// `create_dir_all` makes the data directory and the `corpus` sub-directory if
/// they are absent, and `canonicalize` resolves any `..` or symlink components
/// in the *data-directory prefix* to their real location. Every path this module
/// builds is then joined under the returned canonical directory, so a write can
/// never land outside the data directory the caller configured.
fn corpus_dir(data_dir: &Path) -> Result<PathBuf, PublishError> {
    fs::create_dir_all(data_dir)?;
    let parent = data_dir.join(CORPUS_SUBDIR);
    fs::create_dir_all(&parent)?;
    let canonical = parent
        .canonicalize()
        .map_err(|e| PublishError::io("resolving corpus directory", e))?;
    Ok(canonical)
}

/// The fixed path of the active snapshot under `data_dir` (the un-canonicalized
/// form; use [`corpus_dir`] when a containment-safe location is required).
fn published_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CORPUS_SUBDIR).join(PUBLISHED_NAME)
}

/// Build the next unique transient publication path *inside* `corpus_dir`.
///
/// The name is stamped with the process id, a process-local counter, and the
/// current nanosecond timestamp, so two publications in the same process never
/// collide on a temp file and a stale leftover from a different run is never
/// reused by mistake.
fn next_temp_path(corpus_dir: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!(
        "{TEMP_PREFIX}{}-{counter}-{nanos}{TEMP_SUFFIX}",
        std::process::id(),
    );
    corpus_dir.join(name)
}

/// Whether a snapshot of `bytes` bytes clears the on-disk size bound.
fn within_size_limit(bytes: usize) -> bool {
    bytes <= MAX_SNAPSHOT_BYTES
}

/// Write `bytes` to `path` and flush them to the underlying storage, so the
/// candidate is durable *before* it is ever renamed into place.
///
/// On any write/flush failure the partially written file (if any) is removed, so
/// no partial artifact escapes the data directory. A `sync_all` failure is
/// tolerated — the bytes were still fully written, so promotion over a complete
/// file is correct — it only means cross-crash durability is unguaranteed.
fn write_candidate(path: &Path, bytes: &[u8]) -> Result<(), PublishError> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| PublishError::io("opening candidate file", e))?;
    if let Err(e) = file.write_all(bytes) {
        let _ = fs::remove_file(path);
        return Err(PublishError::io("writing candidate file", e));
    }
    if let Err(e) = file.flush() {
        let _ = fs::remove_file(path);
        return Err(PublishError::io("flushing candidate file", e));
    }
    let _ = file.sync_all();
    Ok(())
}

/// Remove any transient publication files left by an interrupted prior run, so a
/// crash does not leave a partial candidate lying in the data directory.
///
/// Only files carrying the transient prefix *and* suffix are removed — never the
/// published [`PUBLISHED_NAME`] or any other file — and removal is best-effort:
/// a concurrent writer's in-progress file or a transient OS error must not abort
/// a publication. This is the "recover cleanly after an interrupted sync" step.
fn cleanup_stale_temps(corpus_dir: &Path) -> Result<(), PublishError> {
    let entries = match fs::read_dir(corpus_dir) {
        Ok(entries) => entries,
        // A missing directory is not an error for a cleanup pass: the caller
        // just created it.
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // Two transient families are swept: the candidate temps
        // (`snapshot-<pid>-<n>-<nanos>.tmp`) and the writer-lock stamp temps
        // (`sync.lock.stamp-<pid>-<ns>-<n>`), the latter left by a process
        // killed mid-stamp (task `[028]`). Both are safe to remove here because
        // this pass runs *inside the held writer lock*, so no live writer is
        // stamping; any temp present is an orphan from an earlier interrupted run.
        if (name.starts_with(TEMP_PREFIX) && name.ends_with(TEMP_SUFFIX) && name != PUBLISHED_NAME)
            || crate::writer_lock::is_stamp_temp(name)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

/// Publish `manifest` as the active snapshot under `data_dir`, atomically.
///
/// The candidate is validated **first** (the [025] complete-corpus gate), then
/// written to a transient file *inside* the corpus directory, flushed, and
/// `sync_all`-ed for durability, and **only then** atomically `rename`-d over
/// the fixed published file. The guarantees, each exercised by the tests below:
///
/// - **A successful publish is loadable** ([`load_published_snapshot`]) — the
///   published file is the exact bytes the loader parses back into an equal
///   [`SnapshotManifest`].
/// - **A failed or interrupted publish preserves the previous valid snapshot** —
///   validation runs before any write, and promotion is a single atomic rename
///   over a fully-written candidate, so the old file is never partially
///   overwritten.
/// - **Nothing escapes the data directory** — the transient and published paths
///   are built under the canonical corpus directory, and a leftover temp is
///   cleaned up, never promoted.
/// - **Concurrent syncs are serialized** (task `[028]`) — the write+rename runs
///   under the `crate::writer_lock` writer lock held for its duration, so two
///   syncs cannot promote concurrently; a stale lock from a crashed or
///   interrupted sync is reclaimed by the next sync.
///
/// Returns the [`Publication`] (the published path and the cleared
/// [`ValidationSummary`]). A live, unstale lock held by another sync surfaces as
/// [`PublishError::Lock`] and leaves the previously published snapshot untouched.
pub fn publish_snapshot(
    data_dir: &Path,
    manifest: &SnapshotManifest,
) -> Result<Publication, PublishError> {
    // 1. Gate on the complete-corpus validation in memory, before touching the
    //    filesystem. An invalid candidate is rejected here so the previously
    //    published snapshot is not even read or rewritten.
    let summary = validate_snapshot(manifest).require_valid()?;

    // 2. Serialize the validated candidate deterministically, then bound its
    //    size before writing it anywhere.
    let bytes = manifest.to_json_string()?.into_bytes();
    if !within_size_limit(bytes.len()) {
        return Err(PublishError::too_large(bytes.len()));
    }

    // 3. Resolve the containment-safe corpus directory and recover from any
    //    earlier interrupted run.
    let corpus = corpus_dir(data_dir)?;

    // 4. Acquire the sync writer lock (reclaiming a stale one left by a crashed
    //    or interrupted sync) and hold it, via an RAII guard, through the
    //    write+rename below. This serializes concurrent writers (task `[028]`):
    //    two syncs cannot promote concurrently or interleave a write+rename. The
    //    lock is released when `_guard` drops at the end of this function, on
    //    every exit path (success or error). A live, unstale lock held by another
    //    sync surfaces as `PublishError::Lock` and leaves the previous snapshot
    //    untouched.
    let _guard = crate::writer_lock::acquire(&corpus).map_err(PublishError::from)?;

    // 5. Clean up transient candidates left by an interrupted run, so a crash does
    //    not leave a partial artifact lying in the data directory.
    cleanup_stale_temps(&corpus)?;

    // 6. Write the candidate to a transient file inside the corpus directory and
    //    flush it durably, without yet promoting it.
    let temp = next_temp_path(&corpus);
    write_candidate(&temp, &bytes)?;

    // 7. Only now — candidate fully written, validated, and durable — promote it
    //    atomically over the active snapshot. `rename` within the same directory
    //    is atomic, so a reader never observes a half-written snapshot; a failure
    //    here leaves the previous valid snapshot in place and the temp removed.
    let target = corpus.join(PUBLISHED_NAME);
    fs::rename(&temp, &target).map_err(|e| {
        let _ = fs::remove_file(&temp);
        PublishError::io("promoting candidate snapshot", e)
    })?;

    Ok(Publication {
        path: target,
        summary,
    })
}

/// Load the active snapshot published under `data_dir` back into a
/// [`SnapshotManifest`], gated on the schema version, the on-disk size bound, and
/// JSON/UTF-8 integrity.
///
/// A corrupt, truncated, non-UTF-8, oversized, or unsupported-version published
/// file fails here (never a panic, never a mistaken load). Only the fixed
/// [`PUBLISHED_NAME`] is read: a transient candidate file is never loaded, so a
/// leftover from an interrupted run cannot masquerade as a published snapshot.
pub fn load_published_snapshot(data_dir: &Path) -> Result<SnapshotManifest, PublishError> {
    let path = published_path(data_dir);
    let bytes = fs::read(&path).map_err(|e| PublishError::io("reading published snapshot", e))?;
    if !within_size_limit(bytes.len()) {
        return Err(PublishError::too_large(bytes.len()));
    }
    let json = String::from_utf8(bytes).map_err(|e| {
        PublishError::Schema(SchemaError::Json(format!(
            "published snapshot is not valid UTF-8: {e}"
        )))
    })?;
    SnapshotManifest::from_json_str(&json).map_err(PublishError::from)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Channel, ChecklistEntry, CommandEntry, Continuation, CorpusHash, EffectKind, EffectNote,
        Release, SectionBuilder, SnapshotId, Source, SourceId, Timestamp, Url,
    };

    /// One `Source` per required collection, with a single recorded document URL.
    ///
    /// The `id` is the collection's canonical id (always URI-safe, so it always
    /// builds a [`SourceId`]); the remaining fields carry a valid, non-empty
    /// shape so the only thing a test mutates is the field under test.
    fn source_for(collection: SourceCollection) -> Source {
        let id = collection.id();
        Source {
            id: SourceId::new(id).expect("a registry id must build a SourceId"),
            title: collection.title().to_string(),
            entry_url: Url::new(collection.entry_url())
                .expect("a registry entry URL must be valid"),
            document_urls: vec![
                Url::new(collection.entry_url()).expect("a registry entry URL must be valid"),
            ],
            last_checked: None,
            fetched_at: Timestamp::new("2026-10-30T00:00:00Z").unwrap(),
            upstream_modified: None,
            content_hash: CorpusHash::compute(b"fixture"),
            release: Release::channel_only(Channel::Stable),
            parser: "test/1".into(),
        }
    }

    /// A ghost section id: well-formed, but one no section in a complete corpus
    /// carries, so it resolves to nothing.
    fn ghost_section_id() -> SectionId {
        SectionId::derive(
            &SourceId::new("cargo").unwrap(),
            "ghost/does-not-exist",
            None,
            None,
        )
        .unwrap()
    }

    /// A complete, valid snapshot: all eight sources, two well-formed sections,
    /// one checklist entry and one command entry, each cross-referencing a
    /// section that exists. Every section's id and content hash are the
    /// builder-derived (consistent) values, so this passes validation.
    fn complete_snapshot() -> SnapshotManifest {
        // Two well-formed sections, built through `SectionBuilder` so their ids
        // and content hashes are the consistent, recomputed values.
        let first = SectionBuilder::new(
            SourceId::new("cargo").unwrap(),
            "The cargo book",
            "commands/index",
            Url::new("https://doc.rust-lang.org/cargo/commands/index.html#build").unwrap(),
            "# Build\n\nRun `cargo build` to compile without linking.",
        )
        .anchor("build")
        .heading_path(["Building"])
        .guideline_codes(["C-CONV"])
        .command_keys(["cargo build"])
        .lint_ids(["clippy::unwrap_used"])
        .qualifications(["stable"])
        .build()
        .expect("a well-formed section builds");

        let second = SectionBuilder::new(
            SourceId::new("rustc").unwrap(),
            "The rustc book",
            "basics",
            Url::new("https://doc.rust-lang.org/rustc/basics.html").unwrap(),
            "# Basics\n\nThe compiler turns source into a binary.",
        )
        .build()
        .expect("a well-formed section builds");

        let first_id = first.id.clone();

        let mut sections = vec![first];
        sections.push(second);

        SnapshotManifest {
            schema_version: crate::model::SchemaVersion::CURRENT,
            body: ManifestBody {
                snapshot_id: SnapshotId::new("rust-docs-test").unwrap(),
                created_at: Timestamp::new("2026-10-30T00:00:00Z").unwrap(),
                parser_version: "test/1".into(),
                sources: SourceCollection::ALL
                    .iter()
                    .copied()
                    .map(source_for)
                    .collect(),
                sections,
                checklist: vec![ChecklistEntry {
                    code: "C-CONV".into(),
                    title: "Prefer `From`/`Into`".into(),
                    category: "Conversions and From".into(),
                    location: "Conversions and From#C-CONV".into(),
                    explanation_sections: vec![first_id.clone()],
                }],
                commands: vec![CommandEntry {
                    key: crate::model::CommandKey::new("cargo build").unwrap(),
                    purpose: "Compile the project".into(),
                    upstream_sections: vec![first_id],
                    syntax: vec!["cargo build [OPTIONS]".into()],
                    examples: vec![],
                    prerequisites: vec![],
                    compatibility: vec!["stable".into()],
                    effect_notes: vec![EffectNote {
                        kind: EffectKind::ProjectCodeExecution,
                        detail: "compiles the project's own code".into(),
                    }],
                }],
                validation: ValidationSummary::default(),
                attribution: vec![],
            },
        }
    }

    // -- Acceptance criterion 1: all eight collections must pass. --

    /// The complete, well-formed corpus passes and its summary counts the
    /// records it inspected.
    #[test]
    fn a_complete_corpus_passes_validation() {
        let report = validate_snapshot(&complete_snapshot());
        assert!(
            report.is_valid(),
            "a complete corpus must pass, found {:#?}",
            report.problems
        );
        assert_eq!(report.summary.sections, 2);
        assert_eq!(report.summary.checklist_entries, 1);
        assert_eq!(report.summary.command_entries, 1);
        // Eight sources, each recording one document URL.
        assert_eq!(report.summary.documents, 8);
    }

    /// Removing any one of the eight required sources is rejected, naming the
    /// missing collection.
    #[test]
    fn each_missing_required_source_is_reported() {
        for missing in SourceCollection::ALL {
            let mut snap = complete_snapshot();
            snap.body.sources.retain(|s| s.id.as_str() != missing.id());

            let report = validate_snapshot(&snap);
            assert!(!report.is_valid(), "missing `{}` must fail", missing.id());
            assert!(
                report
                    .problems
                    .iter()
                    .any(|p| matches!(p, ValidationProblem::MissingSource { source } if source == missing.id())),
                "must name the missing source `{}`, got {:#?}",
                missing.id(),
                report.problems
            );
        }
    }

    /// An empty source list misses all eight required collections at once.
    #[test]
    fn an_empty_source_list_misses_all_eight() {
        let mut snap = complete_snapshot();
        snap.body.sources.clear();
        let report = validate_snapshot(&snap);
        let missing: Vec<&str> = report
            .problems
            .iter()
            .filter_map(|p| match p {
                ValidationProblem::MissingSource { source } => Some(source.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(missing.len(), 8, "all eight must be reported missing");
    }

    // -- Acceptance criterion 2, dimension: empty content. --

    /// A section whose body is empty or whitespace-only is rejected.
    #[test]
    fn an_empty_section_body_is_rejected() {
        let mut snap = complete_snapshot();
        snap.body.sections[0].body = "   \n\t  ".to_string();
        let report = validate_snapshot(&snap);
        assert!(
            report
                .problems
                .iter()
                .any(|p| matches!(p, ValidationProblem::EmptySectionBody { .. })),
            "an empty body must be reported: {:#?}",
            report.problems
        );
        assert!(!report.is_valid());
    }

    /// A corpus with no sections at all is rejected as a genuinely empty corpus.
    #[test]
    fn an_empty_corpus_is_rejected() {
        let mut snap = complete_snapshot();
        snap.body.sections.clear();
        let report = validate_snapshot(&snap);
        assert!(
            report.problems.contains(&ValidationProblem::EmptyCorpus),
            "an empty corpus must be reported: {:#?}",
            report.problems
        );
    }

    // -- Acceptance criterion 2, dimension: duplicate identities. --

    /// Two sections sharing one id is rejected as a duplicate identity.
    #[test]
    fn a_duplicate_section_id_is_rejected() {
        let mut snap = complete_snapshot();
        snap.body.sections.push(snap.body.sections[0].clone());
        let report = validate_snapshot(&snap);
        let dup = report
            .problems
            .iter()
            .find(|p| matches!(p, ValidationProblem::DuplicateSectionId { .. }));
        assert!(
            dup.is_some(),
            "a duplicate section id must be reported: {:#?}",
            report.problems
        );
        assert!(!report.is_valid());
    }

    /// Two sources sharing one id is rejected as a duplicate source.
    #[test]
    fn a_duplicate_source_id_is_rejected() {
        let mut snap = complete_snapshot();
        snap.body.sources.push(snap.body.sources[0].clone());
        let report = validate_snapshot(&snap);
        assert!(
            report
                .problems
                .iter()
                .any(|p| matches!(p, ValidationProblem::DuplicateSource { .. })),
            "a duplicate source id must be reported: {:#?}",
            report.problems
        );
        assert!(!report.is_valid());
    }

    // -- Acceptance criterion 2, dimension: broken internal mappings. --

    /// A checklist entry that references a non-existent section is reported.
    #[test]
    fn an_unresolved_checklist_reference_is_rejected() {
        let mut snap = complete_snapshot();
        snap.body.checklist[0]
            .explanation_sections
            .push(ghost_section_id());
        let report = validate_snapshot(&snap);
        let unresolved = report
            .problems
            .iter()
            .find(|p| matches!(p, ValidationProblem::UnresolvedChecklistReference { .. }));
        assert!(
            unresolved.is_some(),
            "an unresolved checklist reference must be reported: {:#?}",
            report.problems
        );
        // The report names the offending reference.
        let ValidationProblem::UnresolvedChecklistReference { section_id, .. } =
            unresolved.expect("expected an UnresolvedChecklistReference")
        else {
            panic!("expected an UnresolvedChecklistReference");
        };
        assert_eq!(section_id, &ghost_section_id().as_str().to_string());
        assert!(!report.is_valid());
    }

    /// A command entry that cites a non-existent section is reported.
    #[test]
    fn an_unresolved_command_reference_is_rejected() {
        let mut snap = complete_snapshot();
        snap.body.commands[0]
            .upstream_sections
            .push(ghost_section_id());
        let report = validate_snapshot(&snap);
        let unresolved = report
            .problems
            .iter()
            .find(|p| matches!(p, ValidationProblem::UnresolvedCommandReference { .. }));
        assert!(
            unresolved.is_some(),
            "an unresolved command reference must be reported: {:#?}",
            report.problems
        );
        assert!(!report.is_valid());
    }

    // -- Acceptance criterion 2, dimension: invalid hashes / id-vs-fields. --

    /// A section whose content changed after its hash was fixed (so the stored
    /// hash no longer describes the content) is rejected.
    #[test]
    fn a_stale_content_hash_is_rejected() {
        let mut snap = complete_snapshot();
        // Append to the body without recomputing the hash: the stored hash now
        // describes the pre-edit content.
        snap.body.sections[0]
            .body
            .push_str("\n\nedited after hashing");
        let report = validate_snapshot(&snap);
        let mismatch = report
            .problems
            .iter()
            .find(|p| matches!(p, ValidationProblem::ContentHashMismatch { .. }));
        assert!(
            mismatch.is_some(),
            "a stale content hash must be reported: {:#?}",
            report.problems
        );
        // The report carries the recomputed (expected) hash and the stored one.
        let ValidationProblem::ContentHashMismatch {
            expected, found, ..
        } = mismatch.expect("expected a ContentHashMismatch")
        else {
            panic!("expected a ContentHashMismatch");
        };
        assert_ne!(expected, found, "the expected and found hashes differ");
        assert!(!report.is_valid());
    }

    /// A section whose stored id does not match the id derived from its fields
    /// is rejected.
    #[test]
    fn a_section_id_that_mismatches_its_fields_is_rejected() {
        let mut snap = complete_snapshot();
        // Replace the stored id with one derived from a *different* anchor, so it
        // no longer matches the section's own fields. The content hash (which
        // ignores the id) stays consistent, isolating the id-vs-fields check.
        // Swapping the id also orphans the checklist and command entries that
        // still reference the section's original id, so the report additionally
        // carries unresolved cross-reference problems; those are asserted below.
        let wrong = SectionId::derive(
            &SourceId::new("cargo").unwrap(),
            "commands/index",
            Some("test"),
            None,
        )
        .unwrap();
        snap.body.sections[0].id = wrong.clone();
        let report = validate_snapshot(&snap);
        let mismatch = report
            .problems
            .iter()
            .find(|p| matches!(p, ValidationProblem::SectionIdMismatch { .. }));
        assert!(
            mismatch.is_some(),
            "an id that mismatches its fields must be reported: {:#?}",
            report.problems
        );
        let ValidationProblem::SectionIdMismatch { section_id, .. } =
            mismatch.expect("expected a SectionIdMismatch")
        else {
            panic!("expected a SectionIdMismatch");
        };
        assert_eq!(section_id, &wrong.as_str().to_string());
        // Swapping the id orphans the checklist and command entries that still
        // reference the section's original id, so both surface as unresolved.
        assert!(
            report
                .problems
                .iter()
                .any(|p| matches!(p, ValidationProblem::UnresolvedChecklistReference { .. })),
            "the swapped id orphans the checklist reference: {:#?}",
            report.problems
        );
        assert!(
            report
                .problems
                .iter()
                .any(|p| matches!(p, ValidationProblem::UnresolvedCommandReference { .. })),
            "the swapped id orphans the command reference: {:#?}",
            report.problems
        );
        assert!(!report.is_valid());
    }

    // -- Acceptance criterion 2, dimension: un-derivable ids. --

    /// A section whose fields cannot form a valid id at all — an empty chapter
    /// path, as a corrupt or deserialized corpus may carry — is reported with a
    /// `derived` note, because `SectionId::derive` returns `Err` for it.
    #[test]
    fn a_section_with_an_empty_chapter_path_reports_a_no_id_derivable_mismatch() {
        let mut snap = complete_snapshot();
        // An empty chapter path cannot form a section id, so `SectionId::derive`
        // returns `Err`. Recompute the hash so the content check stays green,
        // isolating the failure to the un-derivable id itself.
        snap.body.sections[0].chapter_path = String::new();
        snap.body.sections[0].recompute_content_hash();
        let report = validate_snapshot(&snap);
        let ValidationProblem::SectionIdMismatch { derived, .. } = report
            .problems
            .iter()
            .find(|p| matches!(p, ValidationProblem::SectionIdMismatch { .. }))
            .expect("an empty chapter path must yield a SectionIdMismatch")
        else {
            panic!("expected a SectionIdMismatch");
        };
        // The un-derivable id is surfaced as a note rather than a concrete id.
        assert!(
            derived.contains("no valid id derivable"),
            "the derived note must explain the un-derivable id, got {derived:?}"
        );
        assert!(!report.is_valid());
    }

    /// A section whose continuation is structurally invalid — as a corrupt or
    /// deserialized corpus may carry — is likewise reported with a `derived`
    /// note, because `SectionId::derive` rejects the continuation.
    #[test]
    fn a_section_with_an_invalid_continuation_reports_a_no_id_derivable_mismatch() {
        let mut snap = complete_snapshot();
        // `SectionBuilder` validates the continuation, so an invalid one is
        // injected directly: part 0 is outside the 1-based range `derive`
        // allows. The content hash ignores the continuation, so it stays green.
        snap.body.sections[0].continuation = Some(Continuation { part: 0, parts: 1 });
        let report = validate_snapshot(&snap);
        let ValidationProblem::SectionIdMismatch { derived, .. } = report
            .problems
            .iter()
            .find(|p| matches!(p, ValidationProblem::SectionIdMismatch { .. }))
            .expect("an invalid continuation must yield a SectionIdMismatch")
        else {
            panic!("expected a SectionIdMismatch");
        };
        assert!(
            derived.contains("no valid id derivable"),
            "the derived note must explain the un-derivable id, got {derived:?}"
        );
        assert!(!report.is_valid());
    }

    // -- Robustness / aggregate-report behavior. --

    /// Validation aggregates every defect at once rather than stopping at the
    /// first, and is deterministic across repeated runs.
    #[test]
    fn a_corrupt_corpus_reports_every_defect_and_is_deterministic() {
        let mut snap = complete_snapshot();
        // Remove a required source.
        snap.body.sources.retain(|s| s.id.as_str() != "rustdoc");
        // Empty one section's body.
        snap.body.sections[0].body = "   ".to_string();
        // Add an unresolved cross-reference.
        snap.body.checklist[0]
            .explanation_sections
            .push(ghost_section_id());

        let first = validate_snapshot(&snap);
        let second = validate_snapshot(&snap);
        assert_eq!(first, second, "the report must be deterministic");

        assert!(
            first
                .problems
                .iter()
                .any(|p| matches!(p, ValidationProblem::MissingSource { .. })),
            "missing source present"
        );
        assert!(
            first
                .problems
                .iter()
                .any(|p| matches!(p, ValidationProblem::EmptySectionBody { .. })),
            "empty body present"
        );
        assert!(
            first
                .problems
                .iter()
                .any(|p| matches!(p, ValidationProblem::UnresolvedChecklistReference { .. })),
            "unresolved reference present"
        );
        assert!(first.problem_count() >= 3, "all defects aggregated");
        // `require_valid` surfaces the populated report as the failure.
        assert!(validate_snapshot(&snap).require_valid().is_err());
    }

    /// `require_valid` yields the summary for a valid corpus.
    #[test]
    fn require_valid_yields_the_summary_for_a_valid_corpus() {
        let summary = validate_snapshot(&complete_snapshot())
            .require_valid()
            .expect("a complete corpus is valid");
        assert_eq!(summary.sections, 2);
        assert_eq!(summary.checklist_entries, 1);
        assert_eq!(summary.command_entries, 1);
    }

    // Guard the test helper itself: a registry entry URL is always valid and
    // every registry id is URI-safe, so `source_for` never panics in a test.
    #[test]
    fn every_registry_source_is_uri_safe_and_has_a_valid_entry_url() {
        for collection in SourceCollection::ALL {
            SourceId::new(collection.id())
                .unwrap_or_else(|e| panic!("id {} must be URI-safe: {e}", collection.id()));
            Url::new(collection.entry_url())
                .unwrap_or_else(|e| panic!("entry url for {} must be valid: {e}", collection.id()));
        }
    }

    // -------------------------------------------------------------------
    // Atomic publication — task [026]
    // -------------------------------------------------------------------

    /// A process-local counter so each test gets a unique data directory even
    /// when cargo runs tests in parallel.
    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A RAII guard that removes its data directory on drop, so a test's temp
    /// files never leak into the shared temp location and no real user data
    /// directory is ever touched.
    struct TempDataDir(PathBuf);

    impl Drop for TempDataDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Create a fresh, unique, and existing data directory under the OS temp dir.
    fn temp_data_dir() -> TempDataDir {
        let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rust-docs-mcp-pub-{}-{nanos}-{n}",
            std::process::id(),
        ));
        fs::create_dir_all(&path).expect("create the temp data dir");
        TempDataDir(path)
    }

    /// Assert the corpus directory carries no leftover transient publication
    /// files (only the fixed published file may remain, if any).
    fn assert_no_stale_temps(data_dir: &Path) {
        let corpus = data_dir.join(CORPUS_SUBDIR);
        let stale: Vec<PathBuf> = fs::read_dir(&corpus)
            .map(|rd| {
                rd.flatten()
                    .filter(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .map(|name| {
                                name.starts_with(TEMP_PREFIX) && name.ends_with(TEMP_SUFFIX)
                            })
                            .unwrap_or(false)
                    })
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            stale.is_empty(),
            "no transient publication files should remain: {stale:?}",
        );
    }

    /// **AC1**: a successful publish produces a loadable, immutable manifest —
    /// the published file lives at the fixed path inside the data directory and
    /// loads back into an equal [`SnapshotManifest`].
    #[test]
    fn a_successful_publish_produces_a_loadable_snapshot() {
        let dir = temp_data_dir();
        let manifest = complete_snapshot();

        let publication =
            publish_snapshot(&dir.0, &manifest).expect("a valid snapshot must publish");

        // The published file is at the fixed name, contained within the data
        // directory (canonicalized, so it holds on platforms whose temp dir is a
        // symlink).
        let canonical_root = dir.0.canonicalize().expect("the data dir exists");
        assert!(
            publication.path.starts_with(&canonical_root),
            "the published path must be within the data directory: {:?}",
            publication.path,
        );
        // The fixed published file exists under the data directory.
        let published = dir.0.join(CORPUS_SUBDIR).join(PUBLISHED_NAME);
        assert!(published.exists(), "the fixed published file must exist");
        // It loads back into a snapshot equal to the one published.
        let loaded = load_published_snapshot(&dir.0).expect("the published snapshot must load");
        assert_eq!(
            loaded, manifest,
            "the loaded snapshot must equal the published one"
        );
        // The cleared validation summary carried through to the result.
        assert_eq!(publication.summary.sections, 2);
        assert_eq!(publication.summary.checklist_entries, 1);
        assert_eq!(publication.summary.command_entries, 1);
        // A successful publish leaves no transient file behind.
        assert_no_stale_temps(&dir.0);
    }

    /// **AC2a**: a failed (invalid) publish preserves the previously published
    /// valid snapshot byte-for-byte and leaves no partial artifact behind.
    #[test]
    fn a_failed_publish_preserves_the_previous_valid_snapshot() {
        let dir = temp_data_dir();
        let good = complete_snapshot();
        publish_snapshot(&dir.0, &good).expect("the first publish must succeed");

        let published = published_path(&dir.0);
        let before_bytes = fs::read(&published).expect("the published file exists");
        let before = load_published_snapshot(&dir.0).expect("the published snapshot loads");

        // Attempt to publish an *invalid* candidate (an empty section body). It
        // must fail and must not touch the previous snapshot.
        let mut bad = complete_snapshot();
        bad.body.sections[0].body = "   \n".to_string();
        let err = publish_snapshot(&dir.0, &bad).expect_err("an invalid candidate must fail");
        assert!(
            matches!(err, PublishError::Invalid(_)),
            "an invalid candidate must surface as `Invalid`, got {err:?}",
        );

        // The previous valid snapshot is byte-for-byte unchanged and still
        // loadable, and no transient artifact was left by the failed attempt.
        let after_bytes = fs::read(&published).expect("the published file is still present");
        assert_eq!(
            before_bytes, after_bytes,
            "a failed publish must not modify the published file",
        );
        let after = load_published_snapshot(&dir.0).expect("the previous snapshot still loads");
        assert_eq!(before, after, "the previous snapshot must be unchanged");
        assert_no_stale_temps(&dir.0);
    }

    /// **AC2b**: a transient candidate left by an interrupted run is never
    /// mistaken for a published snapshot, and a clean publish recovers it.
    #[test]
    fn a_leftover_temp_is_not_mistaken_for_a_published_snapshot() {
        let dir = temp_data_dir();
        let good = complete_snapshot();
        publish_snapshot(&dir.0, &good).expect("publish a valid snapshot");

        // Simulate an interrupted prior run that left a partial candidate in the
        // corpus directory, under a name distinct from the published file.
        let corpus = dir.0.join(CORPUS_SUBDIR);
        let stale = corpus.join("snapshot-4242-0-0.tmp");
        fs::write(&stale, b"{ a partial, unfinished candidate ...").expect("write the stale temp");
        assert!(stale.exists(), "the stale temp exists before load");

        // Loading reads the *published* snapshot, never the leftover temp, and
        // returns the originally published manifest.
        let loaded = load_published_snapshot(&dir.0).expect("the published snapshot loads");
        assert_eq!(
            loaded, good,
            "a stale temp must not be loaded as a snapshot"
        );

        // A clean publish recovers: the stale temp is removed, and the published
        // snapshot is intact.
        publish_snapshot(&dir.0, &good).expect("re-publish");
        assert!(
            !stale.exists(),
            "the stale temp must be cleaned up on a clean publish"
        );
        assert_eq!(
            load_published_snapshot(&dir.0).expect("re-loads"),
            good,
            "the published snapshot must remain intact after cleanup",
        );
    }

    /// **AC2b / containment**: a candidate is always written *inside* the data
    /// directory, even when the data directory is expressed with a `..` segment
    /// — it can never escape to a sibling or absolute path outside it.
    #[test]
    fn a_candidate_stays_within_the_data_directory() {
        let dir = temp_data_dir();
        let real = dir.0.canonicalize().expect("the data dir exists");

        // Express the data directory with a `..` segment that still resolves to
        // the real location: `dir/../<name>` canonicalizes back to `dir` itself,
        // so the corpus directory must remain under the *real* data dir, not a
        // sibling reached by climbing to the parent first.
        let name = dir.0.file_name().expect("the data dir has a name");
        let dotted = dir.0.join("..").join(name);

        let corpus = corpus_dir(&dotted).expect("the corpus dir resolves");
        let temp = next_temp_path(&corpus);

        // The transient path is inside the corpus directory, and the corpus
        // directory is inside the *canonical* data directory — never a sibling.
        assert!(
            temp.starts_with(&corpus),
            "the temp must be inside the corpus dir: {temp:?}",
        );
        assert!(
            corpus.starts_with(&real),
            "the corpus dir must be under the data dir: corpus={corpus:?} real={real:?}",
        );
        // The `..`-expressed data directory resolves to the real location, not
        // outside it.
        assert_eq!(
            corpus.canonicalize().expect("the corpus dir exists"),
            real.join(CORPUS_SUBDIR),
        );

        // A publish through the `..`-expressed path still lands inside the data
        // directory and is loadable from it.
        publish_snapshot(&dotted, &complete_snapshot()).expect("publish through a `..` dir");
        assert!(
            load_published_snapshot(&dir.0).is_ok(),
            "loads from the data dir"
        );
        assert_no_stale_temps(&dir.0);
    }

    /// **AC2a**: a second valid publish atomically replaces the previous one, so
    /// the active snapshot is the latest published one and the old file is not
    /// left dangling.
    #[test]
    fn a_second_valid_publish_replaces_the_previous() {
        let dir = temp_data_dir();
        let first = complete_snapshot();
        publish_snapshot(&dir.0, &first).expect("first publish");

        let mut second = complete_snapshot();
        second.body.snapshot_id =
            SnapshotId::new("rust-docs-2026-11-01").expect("a valid snapshot id");
        publish_snapshot(&dir.0, &second).expect("second publish");

        let loaded = load_published_snapshot(&dir.0).expect("the latest snapshot loads");
        assert_eq!(
            loaded.body.snapshot_id.as_str(),
            "rust-docs-2026-11-01",
            "the active snapshot must be the latest published one",
        );
        // The single fixed published file remains, with no versioned leftovers.
        assert_no_stale_temps(&dir.0);
    }

    /// **AC4**: a truncated (partial) published file is detected by the loader's
    /// schema/JSON gate rather than loaded or panicking.
    #[test]
    fn a_truncated_published_file_fails_to_load() {
        let dir = temp_data_dir();
        publish_snapshot(&dir.0, &complete_snapshot()).expect("publish a valid snapshot");

        // Overwrite the published file with truncated JSON.
        fs::write(published_path(&dir.0), b"{\"schema_version\":1,\"snap")
            .expect("overwrite the published file");

        let err = load_published_snapshot(&dir.0).expect_err("a truncated file must not load");
        assert!(
            matches!(err, PublishError::Schema(_)),
            "a truncated file must surface as a schema error, got {err:?}",
        );
    }

    /// **AC4**: a non-UTF-8 published file is detected by the loader rather than
    /// panicking or misloading.
    #[test]
    fn a_non_utf8_published_file_fails_to_load() {
        let dir = temp_data_dir();
        publish_snapshot(&dir.0, &complete_snapshot()).expect("publish a valid snapshot");

        fs::write(published_path(&dir.0), [0xff_u8, 0xfe, 0x00, 0x01])
            .expect("overwrite with non-UTF-8 bytes");

        let err = load_published_snapshot(&dir.0).expect_err("a non-UTF-8 file must not load");
        assert!(
            matches!(err, PublishError::Schema(_)),
            "a non-UTF-8 file must surface as a schema error, got {err:?}",
        );
    }

    /// **AC4**: the on-disk size bound is enforced by the loader's predicate.
    #[test]
    fn the_on_disk_size_bound_is_enforced() {
        assert!(
            within_size_limit(MAX_SNAPSHOT_BYTES),
            "a file at the limit is within it"
        );
        assert!(
            !within_size_limit(MAX_SNAPSHOT_BYTES + 1),
            "a file one byte over the limit is rejected",
        );
    }

    // -------------------------------------------------------------------
    // Writer-lock serialization — task [028]
    // -------------------------------------------------------------------

    /// **AC1 (publish level):** two syncs publishing to the *same* data directory
    /// are serialized by the writer lock, so neither overwrites the other's
    /// candidate and the published snapshot is always a complete, loadable
    /// manifest. The two publishes run on two threads that start together (a
    /// barrier); after both finish, the active snapshot is exactly one of the two
    /// candidates — never a torn, interleaved write.
    #[test]
    fn concurrent_publishes_serialize_without_corrupting_the_corpus() {
        use std::sync::{Arc, Barrier};

        let dir = temp_data_dir();
        let data = Arc::new(dir.0.clone());
        let barrier = Arc::new(Barrier::new(2));

        // Two distinct, valid candidates, each a complete corpus.
        let first = Arc::new({
            let mut m = complete_snapshot();
            m.body.snapshot_id =
                SnapshotId::new("rust-docs-concurrent-a").expect("a valid snapshot id");
            m
        });
        let second = Arc::new({
            let mut m = complete_snapshot();
            m.body.snapshot_id =
                SnapshotId::new("rust-docs-concurrent-b").expect("a valid snapshot id");
            m
        });

        let join_a = {
            let data = Arc::clone(&data);
            let first = Arc::clone(&first);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                publish_until_ok(&data, &first)
            })
        };
        let join_b = {
            let data = Arc::clone(&data);
            let second = Arc::clone(&second);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                publish_until_ok(&data, &second)
            })
        };
        let a = join_a.join().expect("writer A finishes");
        let b = join_b.join().expect("writer B finishes");

        // Both writers eventually published — serialized by the lock — and
        // neither panicked or left an error. (Each `Publication` names the active
        // file, which is the shared output, so identity is checked via the load
        // below.)
        let _ = (a, b);

        // The active snapshot is exactly one of the two candidates: a complete,
        // loadable manifest, never a torn/interleaved write.
        let loaded = load_published_snapshot(&dir.0).expect("the published snapshot loads");
        assert!(
            loaded.body.snapshot_id.as_str() == "rust-docs-concurrent-a"
                || loaded.body.snapshot_id.as_str() == "rust-docs-concurrent-b",
            "the active snapshot must be one of the two published candidates, got {}",
            loaded.body.snapshot_id.as_str()
        );
        // No transient candidate was left behind and none is mistaken for the
        // published snapshot.
        assert_no_stale_temps(&dir.0);
    }

    /// Publish `candidate`, retrying on a clean `WriterBusy` refusal (a live writer
    /// holds the lock) until it succeeds. Used by the concurrency test so both
    /// writers eventually publish — the lock serializes them rather than letting
    /// their writes interleave.
    fn publish_until_ok(data: &Path, candidate: &SnapshotManifest) -> Publication {
        use crate::writer_lock::LockError;
        for _ in 0..500 {
            match publish_snapshot(data, candidate) {
                Ok(pubn) => return pubn,
                Err(PublishError::Lock(LockError::WriterBusy(_))) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(other) => panic!("unexpected publish error: {other:?}"),
            }
        }
        panic!("publish did not succeed within the retry bound");
    }

    /// **AC1 (busy path through publish):** while a live writer holds the lock, a
    /// publish to the same data directory is refused with
    /// [`PublishError::Lock`]/`WriterBusy` and leaves the last valid snapshot
    /// untouched — the refused publish writes no candidate and corrupts nothing.
    #[test]
    fn a_publish_is_refused_while_the_writer_lock_is_held() {
        use crate::writer_lock::{LockError, acquire};

        let dir = temp_data_dir();
        let good = complete_snapshot();
        publish_snapshot(&dir.0, &good).expect("publish a last valid snapshot");
        let good_id = good.body.snapshot_id.clone();
        let before = load_published_snapshot(&dir.0).expect("load the last valid snapshot");

        // A live writer holds the lock for the duration of this test.
        let corpus = corpus_dir(&dir.0).expect("resolve the corpus dir");
        let guard = acquire(&corpus).expect("a live writer acquires the lock");

        // A second publish is refused while the lock is held.
        let mut next = complete_snapshot();
        next.body.snapshot_id = SnapshotId::new("rust-docs-busy").expect("a valid snapshot id");
        let err = publish_snapshot(&dir.0, &next)
            .expect_err("a publish must be refused while the lock is held");
        assert!(
            matches!(err, PublishError::Lock(LockError::WriterBusy(_))),
            "a held lock must refuse the publish with WriterBusy, got {err:?}"
        );

        // The last valid snapshot is untouched, and the refused publish wrote no
        // candidate.
        drop(guard);
        let after = load_published_snapshot(&dir.0).expect("the last valid snapshot still loads");
        assert_eq!(
            after.body.snapshot_id, good_id,
            "a refused publish must not overwrite the last valid snapshot"
        );
        assert_eq!(
            before, after,
            "the last valid snapshot is byte-for-byte unchanged"
        );
        assert_no_stale_temps(&dir.0);
    }

    /// **AC2 (publish level):** a later sync recovers after an interrupted write —
    /// a stale writer lock and a partial candidate left by a crashed sync are
    /// reclaimed and cleaned up, a new valid snapshot is published, and the last
    /// valid corpus is never corrupted or blocked by the stale lock.
    #[test]
    fn a_later_publish_recovers_after_an_interrupted_write() {
        use crate::writer_lock::{LOCK_NAME, dead_lock_bytes};

        let dir = temp_data_dir();

        // 1. Establish a last-valid snapshot.
        let first = complete_snapshot();
        publish_snapshot(&dir.0, &first).expect("the first sync publishes");
        let last_valid = load_published_snapshot(&dir.0).expect("load the last valid snapshot");

        // 2. Simulate an interrupted write: a stale writer lock (a dead holder)
        //    and a partial candidate left in the corpus directory.
        let corpus = corpus_dir(&dir.0).expect("resolve the corpus dir");
        let lock = corpus.join(LOCK_NAME);
        let partial = corpus.join("snapshot-4242-0-0.tmp");
        std::fs::write(&lock, dead_lock_bytes()).expect("leave a stale lock");
        std::fs::write(&partial, b"{ a partial, unfinished candidate")
            .expect("leave a partial candidate");
        assert!(
            lock.exists() && partial.exists(),
            "the stale artifacts exist"
        );

        // A stale lock must not prevent reading the last valid corpus.
        let still = load_published_snapshot(&dir.0)
            .expect("the last valid snapshot loads through a stale lock");
        assert_eq!(
            still, last_valid,
            "a stale lock must not corrupt or block the last valid corpus"
        );

        // 3. A later sync reclaims the stale lock, cleans the partial candidate,
        //    and publishes a new valid snapshot.
        let mut next = complete_snapshot();
        next.body.snapshot_id =
            SnapshotId::new("rust-docs-recovered").expect("a valid snapshot id");
        publish_snapshot(&dir.0, &next).expect("the later sync recovers and publishes");

        // The new snapshot is the active, loadable corpus.
        let recovered = load_published_snapshot(&dir.0).expect("the recovered snapshot loads");
        assert_eq!(
            recovered.body.snapshot_id.as_str(),
            "rust-docs-recovered",
            "the recovered sync must publish the new snapshot"
        );
        // The partial candidate is gone, no transient file remains, and the lock
        // was released after the publish.
        assert!(
            !partial.exists(),
            "the partial candidate must be cleaned up"
        );
        assert_no_stale_temps(&dir.0);
        assert!(
            !lock.exists(),
            "the writer lock must be released after a publish"
        );
    }
}
