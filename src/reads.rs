//! Complete section and checklist reads — task `[036][M3]`.
//!
//! This module is the *read* side of retrieval: the internal operations the
//! `get_section` and `get_api_checklist` MCP tools (wired later, `[048]+`) call
//! to turn a **server-issued** [`SectionId`] and an **upstream checklist
//! category** into their full records from the active snapshot. It is the
//! complement of [`crate::search`], which produces the *ranked* [`SectionId`]
//! hits; here we resolve a chosen id to its complete record, and a category to
//! its complete checklist.
//!
//! The two reads, and the two acceptance criteria they implement
//! (`plan.md` §6 "get_section" / "get_api_checklist", issue [#36]):
//!
//! * [`get_section`] — resolve a [`SectionId`] to its full [`Section`], and
//!   compose the owning source's [`SourceFreshness`] and the snapshot identity
//!   (`plan.md` §5: "snapshot identity disambiguates section content across
//!   updates"). This is **AC1** — the read *preserves* the Markdown body, the
//!   heading breadcrumb, the citations (guideline/lint/command codes, the
//!   canonical URL, qualifications), the snapshot identity, and the freshness
//!   verdict. An unknown id returns [`ReadError::UnknownSection`] (**AC2**).
//! * [`get_api_checklist`] — resolve an upstream checklist `category` (or all
//!   categories when omitted) to its [`ChecklistEntry`]s, **verifying that every
//!   entry's explanation sections resolve to retrievable content** in the same
//!   snapshot. An unknown category returns [`ReadError::UnknownCategory`] and a
//!   dangling explanation [`ReadError::DanglingExplanation`] (**AC2**).
//!
//! ## Total, pure, and offline
//!
//! Both reads are **total** (they return, never panic) and **pure** (a fixed
//! snapshot and inputs yield a fixed result). They touch no network, no
//! filesystem, and no process state: a [`SnapshotManifest`] is the single
//! argument that decides everything. The retrieval is therefore testable
//! offline against the committed bundled snapshot and hand-built fixtures, with
//! no network or real-client trial.
//!
//! The section resolution reuses the tested in-memory [`SectionIndex`]
//! ([`crate::search`]) — its [`SectionIndex::get`] primitive turns "is this id
//! in the corpus?" into one lookup, so no manifest re-scan or re-parse is
//! needed. A long-running server should build the index once from the loaded
//! snapshot and reuse it; these reads build it per call so the *contract* stays
//! self-contained (the hot-path caching is the wiring task's concern).

// The module is the transitional read seam: its items are exercised by the tests
// below but are not yet reachable from `main`/`lib`'s public API, which the
// `get_section` / `get_api_checklist` handlers land (`[048]+`). Until then the
// whole module is `#[allow(dead_code)]`-covered, mirroring the precedent in
// `crate::search` (`SearchError`, `SectionIndex`).
#![allow(dead_code)]

use crate::model::{
    ChecklistEntry, SchemaVersion, Section, SectionId, SnapshotId, SnapshotManifest, Source,
    SourceId,
};
use crate::provenance::{Instant, SourceFreshness, StaleThreshold, evaluate_freshness};
use crate::search::SectionIndex;
use thiserror::Error;

/// A typed error from a section or checklist read.
///
/// Each variant names the *exact* failure a caller must react to, and the
/// message carries the offending value so the caller can correct or report it
/// from the error alone (`plan.md` §6: "Unknown categories, malformed IDs, and
/// unsupported source values produce clear validation errors"). Every variant is
/// exercised by the tests below; the whole module stays `#[allow(dead_code)]`
/// -covered until the `get_section` / `get_api_checklist` handlers land
/// (`[048]+`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReadError {
    /// A requested `section_id` resolves to no section in the active snapshot.
    ///
    /// The id is well-formed (it parsed) but the snapshot carries no such
    /// section — the caller requested content this snapshot does not have.
    #[error("section `{0}` is not present in the active snapshot")]
    UnknownSection(SectionId),
    /// A requested checklist `category` matches no entry in the active snapshot.
    ///
    /// The category string is well-formed but no [`ChecklistEntry`] belongs to
    /// it — the caller asked for a category the corpus does not have.
    #[error("no checklist entry belongs to category `{0}`")]
    UnknownCategory(String),
    /// A matched checklist entry names an explanation `section_id` that resolves
    /// to no retrievable section in the active snapshot.
    ///
    /// This is the read-side guard for `plan.md` §6 / §10: "every checklist
    /// explanation points to retrievable content." A valid corpus already
    /// satisfies this by construction (the api-guidelines stage validates the
    /// code→section mapping); the read *re-checks* it at query time so a
    /// hand-built or drifted snapshot can never hand a caller an unresolvable
    /// reference.
    #[error(
        "checklist entry `{code}` names an explanation section `{missing}` that is not retrievable in the active snapshot"
    )]
    DanglingExplanation {
        /// The checklist entry's upstream code (e.g. `C-CONV`).
        code: String,
        /// The explanation `section_id` that did not resolve.
        missing: SectionId,
    },
}

/// The complete read of one section: its full record plus the snapshot identity
/// and the owning source's freshness.
///
/// This is exactly what `plan.md` §6 "get_section" promises to return — "Section
/// Markdown, heading context, citations, source freshness, snapshot ID, and
/// continuation information" — assembled into one value so the caller never
/// reconstructs the composition:
///
/// * **Markdown** — [`Section::body`];
/// * **heading context** — [`Section::doc_title`] and [`Section::heading_path`]
///   (the breadcrumb, most-to-least specific);
/// * **citations** — [`Section::guideline_codes`], [`Section::lint_ids`],
///   [`Section::command_keys`], [`Section::qualifications`], and
///   [`Section::canonical_url`];
/// * **continuation** — [`Section::continuation`] (`None` for an unsplit
///   section);
/// * **snapshot identity** — [`snapshot_id`](SectionRead::snapshot_id) and
///   [`schema_version`](SectionRead::schema_version);
/// * **freshness** — [`freshness`](SectionRead::freshness), the owning
///   source's [`SourceFreshness`] as of the evaluation instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionRead {
    /// The full section record, with its Markdown body, heading breadcrumb,
    /// citations, and continuation preserved verbatim.
    pub section: Section,
    /// The active snapshot's identity, so the caller can disambiguate content
    /// across updates (`plan.md` §5).
    pub snapshot_id: SnapshotId,
    /// The active snapshot's schema version.
    pub schema_version: SchemaVersion,
    /// The owning source's freshness, judged at `as_of` under `threshold`.
    pub freshness: SourceFreshness,
}

/// The complete read of an upstream checklist category: its verified entries
/// plus the snapshot identity.
///
/// This is exactly what `plan.md` §6 "get_api_checklist" promises to return —
/// "Checklist entries with original codes, titles, and explanatory section
/// IDs/URLs." Each entry keeps its upstream [`ChecklistEntry::code`],
/// [`ChecklistEntry::title`], and [`ChecklistEntry::explanation_sections`];
/// the read has already verified every explanation resolves, so the explanatory
/// ids are guaranteed retrievable (call [`get_section`] to fetch any of them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecklistRead {
    /// The category that was selected, or `None` when the caller asked for all
    /// categories.
    pub category: Option<String>,
    /// The matching checklist entries, each verified so its explanations resolve.
    pub entries: Vec<ChecklistEntry>,
    /// The active snapshot's identity.
    pub snapshot_id: SnapshotId,
    /// The active snapshot's schema version.
    pub schema_version: SchemaVersion,
}

/// Resolve a server-issued [`SectionId`] to its full record, snapshot identity,
/// and the owning source's freshness, as of `as_of` under `threshold`.
///
/// **AC1 (preserve)**: the returned [`SectionRead`] carries the section's
/// Markdown body, heading breadcrumb, citations, and continuation *verbatim*,
/// together with the snapshot identity and the source's freshness — nothing is
/// recomputed, truncated, or guessed.
///
/// **AC2 (explicit error)**: an id that is well-formed but absent from the
/// snapshot returns [`ReadError::UnknownSection`] with that id, so a caller
/// distinguishes "not in this snapshot" from a successful read.
///
/// The read is **total and pure**: it never panics on a malformed or oversized
/// id (a failed id parse never reaches here — ids arrive already parsed from a
/// hit), and a fixed snapshot yields a fixed result. An empty corpus yields
/// `UnknownSection` (the id cannot be present), not a panic.
///
/// **Freshness composition.** Freshness is a property of the section's *source*,
/// so the read finds the source whose [`Source::id`] matches
/// [`Section::source_id`] and judges it with [`evaluate_freshness`]. When the
/// source is absent (only reachable for a hand-built, unvalidated snapshot), the
/// freshness is the honest *unknown* verdict — `stale: None` with no
/// `last_checked` — rather than a guessed one.
pub fn get_section(
    manifest: &SnapshotManifest,
    section_id: &SectionId,
    as_of: Instant,
    threshold: StaleThreshold,
) -> Result<SectionRead, ReadError> {
    // Resolve the id through the tested in-memory index. `build` never fails on
    // an empty corpus — it simply yields an empty index, so an absent id (the
    // only miss) maps to `UnknownSection`, and the read never panics.
    let section = SectionIndex::build(manifest)
        .get(section_id)
        .cloned()
        .ok_or(ReadError::UnknownSection(section_id.clone()))?;

    // Compose freshness from the owning source; an absent source (only reachable
    // for a hand-built snapshot) yields the honest "unknown" verdict.
    let freshness = match find_source(manifest, &section.source_id) {
        Some(source) => evaluate_freshness(source, as_of, threshold),
        None => SourceFreshness {
            as_of,
            last_checked: None,
            threshold,
            stale: None,
        },
    };

    Ok(SectionRead {
        section,
        snapshot_id: manifest.body.snapshot_id.clone(),
        schema_version: manifest.schema_version,
        freshness,
    })
}

/// Resolve an upstream checklist `category` to its entries, or all categories
/// when `category` is `None`, verifying that every entry's explanation sections
/// resolve to retrievable content in the same snapshot.
///
/// **AC1 (preserve)**: the returned [`ChecklistRead`] carries each entry's
/// original code, title, and explanatory section ids, plus the snapshot
/// identity.
///
/// **AC2 (explicit error + retrievability)**: an omitted-or-unknown category
/// that matches no entry returns [`ReadError::UnknownCategory`]; a matched entry
/// whose explanation ids include one that does not resolve returns
/// [`ReadError::DanglingExplanation`]. The read therefore *guarantees* the
/// "every checklist explanation points to retrievable content" property
/// (`plan.md` §6 / §10) for whatever it returns.
///
/// An omitted category (`None`) is "all categories", so an empty checklist is a
/// valid empty result — not an error.
pub fn get_api_checklist(
    manifest: &SnapshotManifest,
    category: Option<&str>,
) -> Result<ChecklistRead, ReadError> {
    // Filter by category (exact match on the upstream category string), or take
    // every entry when the category is omitted. `&str` is `Copy`, so `category`
    // stays usable for the unknown-category check below.
    let filtered: Vec<ChecklistEntry> = match category {
        None => manifest.body.checklist.clone(),
        Some(cat) => manifest
            .body
            .checklist
            .iter()
            .filter(|entry| entry.category == cat)
            .cloned()
            .collect(),
    };

    // A requested category that matched nothing is a clear, typed error — the
    // caller distinguishes "no such category" from a zero-entry result.
    if let Some(cat) = category
        && filtered.is_empty()
    {
        return Err(ReadError::UnknownCategory(cat.to_string()));
    }

    // Verify every matched entry's explanations resolve. An empty corpus has no
    // resolvable section, so any entry that names an explanation is dangling.
    // Building the index once amortizes the per-id lookups.
    let index = SectionIndex::build(manifest);
    for entry in &filtered {
        for reference in &entry.explanation_sections {
            if index.get(reference).is_none() {
                return Err(ReadError::DanglingExplanation {
                    code: entry.code.clone(),
                    missing: reference.clone(),
                });
            }
        }
    }

    Ok(ChecklistRead {
        category: category.map(str::to_string),
        entries: filtered,
        snapshot_id: manifest.body.snapshot_id.clone(),
        schema_version: manifest.schema_version,
    })
}

/// Find the [`Source`] in a manifest whose [`Source::id`] is `source_id`, when
/// present.
///
/// This is the freshness lookup [`get_section`] composes: a section's freshness
/// is its *source's* freshness, so the read must locate the source a section
/// belongs to. An absent source is the only path to an "unknown" freshness
/// verdict, and it is only reachable for a hand-built, unvalidated snapshot —
/// a corpus that passed validation always carries every section's source.
fn find_source<'a>(manifest: &'a SnapshotManifest, source_id: &SourceId) -> Option<&'a Source> {
    manifest
        .body
        .sources
        .iter()
        .find(|source| &source.id == source_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundled::manifest;
    use crate::model::{Continuation, SectionBuilder, Url};

    /// A section id that is well-formed but absent from the committed corpus —
    /// the "known-but-missing" input that must produce `UnknownSection`.
    fn missing_section_id() -> SectionId {
        "api-guidelines/does-not-exist"
            .parse()
            .expect("well-formed id parses")
    }

    /// The `as_of` instant the seed sources were captured at (their
    /// `fetched_at`), used as the freshness evaluation point.
    fn seed_as_of() -> Instant {
        Instant::parse("2026-10-30T00:00:00Z").expect("valid instant")
    }

    /// A split-section fixture, built the way `src/model.rs` defines it: a
    /// [`Section`] whose `continuation` is `Some(Continuation { part, parts })`
    /// with a 1-based `part <= parts` (here, 2 of 3). Its derived id carries the
    /// `$c<part>.<parts>` discriminator, so it is distinguishable from every
    /// unsplit seed section (whose `continuation` is `None`).
    fn split_section() -> Section {
        SectionBuilder::new(
            SourceId::new("api-guidelines").expect("a registry id builds a SourceId"),
            "Split Document",
            "long-document",
            Url::new("https://rust-lang.github.io/api-guidelines/long-document.html")
                .expect("a valid URL"),
            "Part two of three of a split document; the continuation marks it as 2 of 3.",
        )
        .continuation(Continuation { part: 2, parts: 3 })
        .build()
        .expect("a well-formed split section")
    }

    // -- AC1: reads preserve Markdown, heading context, citations, snapshot
    //    identity, and freshness --------------------------------------------

    /// AC1: `get_section` returns the full record — Markdown body, the owning
    /// source, the snapshot identity, and a composed freshness verdict — for a
    /// known id in the committed corpus.
    #[test]
    fn get_section_preserves_the_full_record() {
        let id: SectionId = "api-guidelines/about".parse().expect("valid id");
        let read = get_section(&manifest(), &id, seed_as_of(), StaleThreshold::default())
            .expect("known id resolves");

        // Identity and the source the section belongs to are preserved.
        assert_eq!(read.section.id, id);
        assert_eq!(
            read.section.source_id,
            "api-guidelines"
                .parse::<SourceId>()
                .expect("valid source id")
        );

        // Markdown body is preserved verbatim (non-empty; the seed's about page).
        assert!(
            !read.section.body.is_empty(),
            "the Markdown body must be preserved, not truncated or dropped"
        );

        // Citations are preserved: the canonical URL and the guideline code the
        // about section carries.
        assert!(
            read.section
                .canonical_url
                .as_str()
                .contains("api-guidelines"),
            "the canonical citation URL must be preserved"
        );
        assert!(
            read.section
                .guideline_codes
                .iter()
                .any(|code| code == "C-CONV"),
            "the section's guideline-code citation must be preserved"
        );

        // Snapshot identity is carried alongside the record.
        assert_eq!(read.snapshot_id, manifest().body.snapshot_id);
        assert_eq!(read.schema_version, SchemaVersion::V1);

        // Freshness is composed from the source; the seed source has no
        // `last_checked`, so the honest verdict is "unknown", not a guess.
        assert_eq!(read.freshness.last_checked, None);
        assert_eq!(read.freshness.stale, None);
        assert_eq!(read.freshness.threshold, StaleThreshold::default());
    }

    /// AC1: heading context and qualifications are preserved *through* the read
    /// — enriching a section and reading it back yields the enriched record.
    #[test]
    fn get_section_preserves_heading_context_and_qualifications() {
        let mut m = manifest();
        let about = m
            .body
            .sections
            .iter_mut()
            .find(|section| section.source_id == "api-guidelines".parse().unwrap())
            .expect("seed carries the api-guidelines about section");
        about.heading_path = vec![
            "Rust API Guidelines".to_string(),
            "Conversions and From".to_string(),
        ];
        about.qualifications = vec!["Stable since Rust 1.0".to_string()];

        let id: SectionId = "api-guidelines/about".parse().expect("valid id");
        let read = get_section(&m, &id, seed_as_of(), StaleThreshold::default()).expect("resolves");

        assert_eq!(
            read.section.heading_path,
            vec![
                "Rust API Guidelines".to_string(),
                "Conversions and From".to_string()
            ]
        );
        assert_eq!(
            read.section.qualifications,
            vec!["Stable since Rust 1.0".to_string()]
        );
    }

    /// AC1 + freshness: the read composes a real stale verdict from a source
    /// whose `last_checked` predates the threshold window.
    #[test]
    fn get_section_reports_staleness_from_the_source() {
        // A last check 10 months before `as_of` is stale under the 7-day default.
        let mut stale = manifest();
        stale
            .body
            .sources
            .iter_mut()
            .find(|s| s.id == "api-guidelines".parse().unwrap())
            .expect("seed source present")
            .last_checked =
            Some(crate::model::Timestamp::new("2026-01-01T00:00:00Z").expect("valid ts"));
        let id: SectionId = "api-guidelines/about".parse().expect("valid id");
        let read =
            get_section(&stale, &id, seed_as_of(), StaleThreshold::default()).expect("resolves");
        assert_eq!(
            read.freshness.stale,
            Some(true),
            "a 10-month-old check is stale"
        );

        // A last check 1 day before `as_of` is fresh under the 7-day default.
        let mut fresh = manifest();
        fresh
            .body
            .sources
            .iter_mut()
            .find(|s| s.id == "api-guidelines".parse().unwrap())
            .expect("seed source present")
            .last_checked =
            Some(crate::model::Timestamp::new("2026-10-29T00:00:00Z").expect("valid ts"));
        let read =
            get_section(&fresh, &id, seed_as_of(), StaleThreshold::default()).expect("resolves");
        assert_eq!(
            read.freshness.stale,
            Some(false),
            "a 1-day-old check is fresh"
        );
    }

    // -- AC1: continuation is preserved through the read --------------------

    /// AC1: `get_section` preserves a split section's `continuation` *verbatim*.
    ///
    /// The read returns the full [`Section`] (it clones the record resolved
    /// through the [`crate::search::SectionIndex`]), so a `Continuation { part,
    /// parts }` round-trips through it untouched. Every seed section is unsplit
    /// (`continuation == None`), so this is the only test that exercises a
    /// non-`None` continuation through the read — the gap `F-036-3` names.
    #[test]
    fn get_section_preserves_the_continuation() {
        let split = split_section();
        let id = split.id.clone();

        // The split section's id carries the `$c<part>.<parts>` discriminator —
        // only a continuation section's id does — so resolving this id can only
        // land on the split section.
        assert!(
            id.as_str().contains("$c2.3"),
            "a split section's id carries the continuation discriminator"
        );

        // The read is total: push the split section into a snapshot and read it
        // back by its id, composing freshness from the owning source the seed
        // already carries.
        let mut m = manifest();
        m.body.sections.push(split.clone());
        let read = get_section(&m, &id, seed_as_of(), StaleThreshold::default()).expect("resolves");

        // AC1: the read returns the section verbatim, including the continuation
        // sub-field — nothing recomputed, truncated, or defaulted.
        assert_eq!(
            read.section, split,
            "the read must return the section verbatim"
        );
        assert_eq!(
            read.section.continuation,
            Some(Continuation { part: 2, parts: 3 }),
            "a split section's continuation must survive the read verbatim"
        );

        // Contrast: the unsplit seed sections carry no continuation through the
        // same read — the discrimination is real, not a default `None` that
        // every read would hand back regardless of the record's contents.
        let unsplit = get_section(
            &manifest(),
            &"api-guidelines/about".parse().expect("valid id"),
            seed_as_of(),
            StaleThreshold::default(),
        )
        .expect("a seed section resolves");
        assert_eq!(
            unsplit.section.continuation, None,
            "an unsplit section carries no continuation"
        );
    }

    // -- AC2: unknown ids / categories return explicit errors ---------------

    /// AC2: a well-formed but absent section id returns `UnknownSection` carrying
    /// that id — never a panic, never an empty "success".
    #[test]
    fn get_section_unknown_id_is_explicit() {
        let id = missing_section_id();
        let err = get_section(&manifest(), &id, seed_as_of(), StaleThreshold::default());
        assert_eq!(err, Err(ReadError::UnknownSection(id)));
    }

    /// AC2: a category that matches no entry returns `UnknownCategory` carrying
    /// the requested category.
    #[test]
    fn get_api_checklist_unknown_category_is_explicit() {
        let err = get_api_checklist(&manifest(), Some("No Such Category"));
        assert_eq!(
            err,
            Err(ReadError::UnknownCategory("No Such Category".to_string()))
        );
    }

    // -- AC1: get_api_checklist returns entries with their explanations -----

    /// AC1: a matching category returns its entries with original code, title,
    /// and explanatory section ids, plus the snapshot identity.
    #[test]
    fn get_api_checklist_returns_matching_entries() {
        let read =
            get_api_checklist(&manifest(), Some("Conversions and From")).expect("known category");
        assert_eq!(read.category, Some("Conversions and From".to_string()));
        assert_eq!(read.entries.len(), 1, "the seed has one checklist entry");

        let entry = &read.entries[0];
        assert_eq!(entry.code, "C-CONV");
        assert!(!entry.title.is_empty(), "the entry's title is preserved");
        assert_eq!(
            entry.explanation_sections.len(),
            1,
            "the entry's explanation is preserved"
        );

        // The explanation is retrievable: it resolves in the same snapshot the
        // read served from.
        let corpus = manifest();
        let index = SectionIndex::build(&corpus);
        let resolved = index
            .get(&entry.explanation_sections[0])
            .expect("explanation resolves");
        assert!(
            !resolved.body.is_empty(),
            "the explanation is retrievable content"
        );

        // Snapshot identity is carried alongside the entries.
        assert_eq!(read.snapshot_id, manifest().body.snapshot_id);
        assert_eq!(read.schema_version, SchemaVersion::V1);
    }

    /// AC1: an omitted category means *all* categories — every entry is
    /// returned, and the read's `category` field records the omission.
    #[test]
    fn get_api_checklist_none_means_all_categories() {
        let mut m = manifest();
        let extra = ChecklistEntry {
            code: "E-RESCUE".to_string(),
            title: "Panic-free error handling".to_string(),
            category: "Error Handling".to_string(),
            location: "Error Handling#E-RESCUE".to_string(),
            explanation_sections: vec!["api-guidelines/about".parse().expect("resolves")],
        };
        m.body.checklist.push(extra);

        let read = get_api_checklist(&m, None).expect("all categories");
        assert_eq!(read.category, None);
        assert_eq!(
            read.entries.len(),
            2,
            "omitted category returns every entry"
        );
        assert!(read.entries.iter().any(|e| e.category == "Error Handling"));
        assert!(
            read.entries
                .iter()
                .any(|e| e.category == "Conversions and From")
        );
    }

    /// AC1: a category that matches a subset returns only that subset.
    #[test]
    fn get_api_checklist_filters_to_the_category() {
        let mut m = manifest();
        m.body.checklist.push(ChecklistEntry {
            code: "E-RESCUE".to_string(),
            title: "Panic-free error handling".to_string(),
            category: "Error Handling".to_string(),
            location: "Error Handling#E-RESCUE".to_string(),
            explanation_sections: vec!["api-guidelines/about".parse().expect("resolves")],
        });

        let read = get_api_checklist(&m, Some("Error Handling")).expect("known category");
        assert_eq!(
            read.entries.len(),
            1,
            "only the matching category is returned"
        );
        assert_eq!(read.entries[0].code, "E-RESCUE");
    }

    // -- AC2: every checklist explanation points to retrievable content -----

    /// AC2: a matched entry whose explanation does not resolve fails the read
    /// with `DanglingExplanation` naming the entry and the missing section — the
    /// read never hands a caller an unresolvable reference.
    #[test]
    fn get_api_checklist_rejects_a_dangling_explanation() {
        let mut m = manifest();
        let dangling = missing_section_id();
        for entry in &mut m.body.checklist {
            entry.explanation_sections.push(dangling.clone());
        }

        let err = get_api_checklist(&m, Some("Conversions and From"));
        assert_eq!(
            err,
            Err(ReadError::DanglingExplanation {
                code: "C-CONV".to_string(),
                missing: dangling,
            })
        );
    }

    /// AC2 (boundary): a matched entry with no explanations is vacuously
    /// satisfiable — the read returns it rather than failing, since "every
    /// explanation points to retrievable content" holds for a set with none.
    #[test]
    fn get_api_checklist_allows_an_entry_without_explanations() {
        let mut m = manifest();
        for entry in &mut m.body.checklist {
            entry.explanation_sections.clear();
        }

        let read =
            get_api_checklist(&m, Some("Conversions and From")).expect("vacuously satisfiable");
        assert_eq!(read.entries[0].explanation_sections.len(), 0);
    }
}
