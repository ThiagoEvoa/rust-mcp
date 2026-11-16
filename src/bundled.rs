//! The bundled documentation snapshot — task `[031]` M2 (issue #31, `plan.md` §10),
//! regenerated faithfully from upstream in issue #150.
//!
//! The binary ships a **validated, attributed** eight-source snapshot so a fresh
//! install serves the Rust toolchain documentation **without any network access**
//! (issue #31 acceptance criterion 1). The bundle is embedded at compile time
//! from [`JSON`] (committed at `data/bundled/snapshot.json`); the canonical
//! builder for that file is [`manifest`], and the drift test
//! `the_committed_bundle_matches_the_builder` keeps the two in lockstep.
//!
//! **Faithful, not hand-authored** (issue #150). The corpus is **not** written by
//! hand. It is generated at build time by [`crate::ingest::bundle::build_seed_corpus`],
//! which ingests the recorded upstream fixtures (`tests/fixtures/upstream/`) through
//! the *same* ingestion adapters and `parse_checklist` + `link_explanations`
//! stages the live pipeline uses, then assembles a
//! [`crate::model::SnapshotManifest`]. The result carries faithful upstream bodies
//! (the real C-CONV / C-CONV-TRAITS explanations, clippy lint details, the rustfmt
//! check-mode, …), deterministic content hashes, a parsed checklist, and per-source
//! provenance (commit / revision / ETag). Generation is deterministic and hermetic:
//! it reads the recorded fixtures from disk, makes no network call, and reproduces
//! byte-for-byte. See [`crate::ingest::bundle`] for the procedure and provenance.
//!
//! **Attribution** (acceptance criterion 2). Every captured document's licence
//! and origin are carried in [`ManifestBody::attribution`] ([`AttributionRef`]),
//! and the human-facing notice lives in `THIRD_PARTY_NOTICES.md`
//! (`docs/LICENSING.md` §8 template). All eight sources are MIT OR Apache-2.0
//! (`docs/LICENSING.md` §4); bundling is unconditionally permitted subject to
//! the attribution this module records.
//!
//! **First-sync fallback** (`docs/LICENSING.md` §5). If a source could not be
//! bundled, it is recorded in the manifest, marked for first sync, and the
//! bundle **omits it** (the last valid snapshot is never discarded). The set of
//! such sources today is [`FIRST_SYNC_REQUIRED`] — empty, because every source
//! is MIT OR Apache-2.0 and bundles cleanly. [`manifest_with_exclusions`] is the
//! seam that produces the partial, first-sync-required bundle a documented
//! exception would ship; the tests exercise it so the fallback is proven even
//! while no source currently triggers it.
//!
//! The builder assembles the **full ingested corpus**: one faithful section per
//! recorded fixture, the parsed checklist kept to the codes whose explanations
//! are shipped, and the command catalog derived from the sections. A full,
//! *current* corpus is produced by [`crate::ingest::sync`](crate::ingest) and
//! published to the data directory by the `sync` command; the bundle is the
//! offline seed that makes the *first* launch usable.

use crate::model::{
    AttributionRef, ChecklistEntry, ManifestBody, SchemaVersion, Section, SnapshotId,
    SnapshotManifest, Source, SourceId, Timestamp, ValidationSummary,
};
use crate::registry::SourceCollection;

/// The committed, embedded, compact-JSON snapshot, shipped inside the binary.
///
/// [`manifest`] is the canonical builder; the drift test
/// `the_committed_bundle_matches_the_builder` asserts this text equals
/// `manifest().to_json_string()`, so regenerating the file through the builder
/// is the only way to change it.
pub const JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/bundled/snapshot.json"
));

/// The snapshot identifier of the bundled seed.
pub const SNAPSHOT_ID: &str = "rust-docs-bundled-seed";

/// The fixed point in time the seed was assembled. Fixed so the bundle is
/// deterministic across builds; the live `sync` pipeline stamps a real time.
const CREATED_AT: &str = "2026-10-30T00:00:00Z";

/// The sources a documented bundling exception (`docs/LICENSING.md` §5)
/// currently excludes from the bundle because they cannot be bundled.
///
/// **Empty.** Every source in [`SourceCollection::ALL`] is MIT OR Apache-2.0
/// (`docs/LICENSING.md` §4) and bundles cleanly, so no first-sync-required
/// exception applies today. When a source *cannot* be bundled, it is added here
/// (with a reason recorded in `docs/LICENSING.md` §5); [`manifest`] then omits
/// it and marks it for first sync instead of discarding the last valid snapshot.
pub const FIRST_SYNC_REQUIRED: &[&str] = &[];

/// The canonical, validated, attributed eight-source seed snapshot.
///
/// This is the bundle [`JSON`] embeds. It assembles the **full ingested
/// corpus** — one faithful section per recorded fixture, the parsed checklist
/// kept to the codes whose explanations are shipped, and the command catalog
/// derived from the sections — so a fresh install serves the toolchain
/// documentation offline. A full, *current* corpus is produced by
/// [`crate::ingest::sync`](crate::ingest) and published to the data directory
/// by the `sync` command; the bundle is the offline seed that makes the *first*
/// launch usable. It always validates ([`crate::corpus::validate_snapshot`]).
pub fn manifest() -> SnapshotManifest {
    manifest_with_exclusions(FIRST_SYNC_REQUIRED)
}

/// The seed corpus with `excluded` sources omitted, each recorded as a
/// first-sync-required marker (a documented exception per `docs/LICENSING.md`
/// §5). With `excluded` empty it is the full [`manifest`]; with a source id it
/// returns the **incomplete** manifest that fails validation on that source —
/// the partial snapshot the last-valid-corpus guarantee preserves.
///
/// `excluded` is the id of a source to omit. The omitted source and its section
/// drop out; any checklist entry or command cited only to an omitted section is
/// also dropped, so the result never carries an unresolvable reference.
pub fn manifest_with_exclusions(excluded: &[&str]) -> SnapshotManifest {
    use std::collections::HashSet;

    let excluded_set: HashSet<&str> = excluded.iter().copied().collect();

    // --- Seed corpus, generated from the recorded upstream fixtures via the real
    // ingestion adapters (issue #150). `build_seed_corpus()` is the canonical
    // seed: faithful bodies, deterministic content hashes, a parsed checklist, and
    // per-source provenance. `excluded` is the documented first-sync-required set
    // ([`FIRST_SYNC_REQUIRED`], empty today); a source in it — together with any
    // section or checklist entry that resolves only to it — is then dropped, so
    // the manifest never carries an unresolvable reference. ---
    let corpus = crate::ingest::bundle::build_seed_corpus()
        .expect("the faithful seed corpus builds from the recorded upstream fixtures");

    // The survivor set: every collection except the excluded ones.
    let surviving: HashSet<&str> = SourceCollection::ALL
        .iter()
        .map(|c| c.id())
        .filter(|id| !excluded_set.contains(*id))
        .collect();

    // Keep only the sections and sources whose source survives the exclusion.
    let sections: Vec<Section> = corpus
        .sections
        .into_iter()
        .filter(|s| surviving.contains(s.source_id.as_str()))
        .collect();
    let sources: Vec<Source> = corpus
        .sources
        .into_iter()
        .filter(|s| surviving.contains(s.id.as_str()))
        .collect();

    // Re-point the parsed checklist's explanation links at the surviving sections
    // and drop any entry whose explanation no longer resolves — so every shipped
    // checklist entry links to a section actually present in the snapshot.
    let section_ids: HashSet<&str> = sections.iter().map(|s| s.id.as_str()).collect();
    let checklist: Vec<ChecklistEntry> = corpus
        .checklist
        .into_iter()
        .map(|mut entry| {
            entry
                .explanation_sections
                .retain(|s| section_ids.contains(s.as_str()));
            entry
        })
        .filter(|e| !e.explanation_sections.is_empty())
        .collect();

    // --- Commands: [149] the unified catalog is derived from the sections the
    // snapshot actually carries — one entry per distinct command key those
    // sections stamp, carrying the canonical rich metadata (syntax, examples,
    // prerequisites, compatibility qualifications, effect notes) with the
    // citations re-pointed at the surviving sections. A key whose section was
    // excluded drops out with it, so the catalog and the section index never
    // disagree: every advertised key resolves in this snapshot (AC1/AC3). ---
    let commands = crate::commands::CommandCatalog::from_sections(&sections).into_entries();

    // --- Attribution: one record per surviving source (the licence/origin that
    // accompanies captured content, `docs/LICENSING.md` §4–§8). ---
    let attribution = SourceCollection::ALL
        .iter()
        .copied()
        .filter(|c| surviving.contains(c.id()))
        .map(attribution_for)
        .collect();

    // --- Validation summary: the real record counts plus the human-facing notes.
    let mut warnings = vec![
        "seed corpus: representative content shipped for an offline first launch; \
         run `rust-docs-mcp sync` to obtain the complete, current corpus."
            .to_string(),
    ];
    for source in excluded {
        warnings.push(format!(
            "first-sync-required: {source} — excluded from the bundled snapshot \
             (see docs/LICENSING.md §5); a local `sync` is required to obtain it."
        ));
    }
    let summary = ValidationSummary {
        documents: sources.iter().map(|s| s.document_urls.len()).sum(),
        sections: sections.len(),
        checklist_entries: checklist.len(),
        command_entries: commands.len(),
        warnings,
    };

    SnapshotManifest {
        schema_version: SchemaVersion::CURRENT,
        body: ManifestBody {
            snapshot_id: SnapshotId::new(SNAPSHOT_ID).expect("a URI-safe snapshot id"),
            created_at: Timestamp::new(CREATED_AT).expect("a valid timestamp"),
            parser_version: crate::ingest::bundle::SEED_PARSER_VERSION.to_string(),
            sources,
            sections,
            checklist,
            commands,
            attribution,
            validation: summary,
        },
    }
}

// --- Attribution -----------------------------------------------------------

/// The attribution record for a collection. All eight sources are MIT OR
/// Apache-2.0 (`docs/LICENSING.md` §4); the notice points at the section of
/// `THIRD_PARTY_NOTICES.md` that carries the licence and origin.
fn attribution_for(collection: SourceCollection) -> AttributionRef {
    let short = collection
        .title()
        .split_whitespace()
        .next()
        .unwrap_or("Source");
    AttributionRef {
        source: SourceId::new(collection.id()).expect("a registry id builds a SourceId"),
        licenses: vec!["MIT OR Apache-2.0".to_string()],
        // The upstream repository that owns the content (`docs/LICENSING.md` §2
        // "Upstream repo"), not the rendered entry point.
        repository: collection.source_repo().to_string(),
        required_notice: format!(
            "{short} upstream content (MIT OR Apache-2.0), attributed in THIRD_PARTY_NOTICES.md"
        ),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{ValidationProblem, validate_snapshot};

    #[test]
    fn the_committed_bundle_matches_the_builder() {
        // The committed, embedded bundle must equal the canonical builder output.
        // To regenerate `data/bundled/snapshot.json` after a corpus change, run
        // the ignored `emit_bundle_json` seam (it is `#[ignore]`d so it never
        // runs in the normal suite): `cargo test --locked -- --ignored
        // bundled::tests::emit_bundle_json`. This test guards drift.
        assert_eq!(
            JSON,
            manifest()
                .to_json_string()
                .expect("the manifest serializes"),
            "the committed bundle has drifted from the builder; regenerate data/bundled/snapshot.json"
        );
    }

    #[test]
    fn the_bundled_snapshot_validates() {
        let report = validate_snapshot(&manifest());
        assert!(
            report.is_valid(),
            "the bundled snapshot must validate: {report:?}"
        );
    }

    #[test]
    fn the_bundled_snapshot_covers_all_eight_sources() {
        let m = manifest();
        for collection in SourceCollection::ALL {
            let id = collection.id();
            assert!(
                m.body.sources.iter().any(|s| s.id.as_str() == id),
                "the bundled snapshot must include the {id} source"
            );
            assert!(
                m.body.attribution.iter().any(|a| a.source.as_str() == id),
                "the bundled snapshot must attribute the {id} source"
            );
        }
    }

    #[test]
    fn the_bundled_snapshot_carries_attribution() {
        // Acceptance criterion 2: content is accompanied by its attribution.
        let m = manifest();
        assert_eq!(
            m.body.attribution.len(),
            8,
            "every source must carry an attribution record"
        );
        for record in &m.body.attribution {
            assert!(
                !record.licenses.is_empty(),
                "an attribution record must name a licence"
            );
            assert!(
                !record.required_notice.trim().is_empty(),
                "an attribution record must carry a required notice"
            );
        }
    }

    #[test]
    fn the_bundled_json_parses_and_validates() {
        // The *embedded* bytes (not just the builder) must load through the
        // same gate the runtime uses, proving the shipped artifact is sound.
        let parsed = SnapshotManifest::from_json_str(JSON).expect("the embedded JSON parses");
        assert!(
            validate_snapshot(&parsed).is_valid(),
            "the embedded JSON must validate"
        );
    }

    #[test]
    fn first_sync_fallback_omits_an_excluded_source() {
        // docs/LICENSING.md §5: a source that cannot be bundled is omitted and
        // recorded as first-sync-required; the result is the incomplete
        // snapshot the last-valid-corpus guarantee preserves.
        let m = manifest_with_exclusions(&["clippy"]);
        assert!(
            !m.body.sources.iter().any(|s| s.id.as_str() == "clippy"),
            "an excluded source must be omitted from the bundle"
        );
        // The first-sync-required marker is recorded.
        let warnings = m.body.validation.warnings.clone();
        assert!(
            warnings
                .iter()
                .any(|w| w.starts_with("first-sync-required: clippy")),
            "an excluded source must be marked for first sync: {warnings:?}"
        );
        // The incomplete manifest fails validation on the missing source.
        let report = validate_snapshot(&m);
        assert!(!report.is_valid());
        assert!(
            report.problems.iter().any(
                |p| matches!(p, ValidationProblem::MissingSource { source } if source == "clippy")
            ),
            "an excluded source must surface as a MissingSource validation problem"
        );
    }

    #[test]
    fn the_real_manifest_has_no_excluded_sources() {
        // The shipped bundle excludes nothing today: every source is MIT OR
        // Apache-2.0 and bundles cleanly.
        assert!(FIRST_SYNC_REQUIRED.is_empty());
        let report = validate_snapshot(&manifest());
        assert!(
            report.is_valid(),
            "the full bundled snapshot must validate: {report:?}"
        );
    }

    #[test]
    fn excluded_source_drops_only_its_own_references() {
        // Excluding a source with no checklist/command pointing at it must not
        // orphan any reference or otherwise break the survivor set.
        let m = manifest_with_exclusions(&["clippy"]);
        // The C-CONV checklist entry still resolves (it points at api-guidelines).
        assert!(
            m.body.checklist.iter().any(|c| c.code == "C-CONV"),
            "unrelated checklist entries must survive an unrelated exclusion"
        );
        // The cargo/rustup commands still resolve.
        assert!(
            m.body
                .commands
                .iter()
                .any(|c| c.key.as_str() == "cargo build"),
            "unrelated commands must survive an unrelated exclusion"
        );
    }

    // A dev-time regeneration seam for `data/bundled/snapshot.json`. Ignored so
    // it never runs (or writes) in the normal suite; run it explicitly after a
    // corpus change, then let `the_committed_bundle_matches_the_builder` guard
    // the regenerated file.
    #[test]
    #[ignore = "writes data/bundled/snapshot.json; run explicitly to regenerate"]
    fn emit_bundle_json() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/data/bundled/snapshot.json");
        std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/data/bundled"))
            .expect("create the bundle directory");
        let json = manifest()
            .to_json_string()
            .expect("the manifest serializes");
        let bytes = json.len();
        std::fs::write(path, json).expect("write the bundled snapshot");
        eprintln!("wrote {bytes} bytes to data/bundled/snapshot.json");
    }
}
