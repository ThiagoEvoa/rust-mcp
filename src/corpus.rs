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

use crate::model::{ManifestBody, SectionId, SnapshotManifest, ValidationSummary};
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
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Channel, ChecklistEntry, CommandEntry, CorpusHash, EffectKind, EffectNote, Release,
        SectionBuilder, SnapshotId, Source, SourceId, Timestamp, Url,
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
        // ignores the id) stays consistent, isolating the id check.
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
}
