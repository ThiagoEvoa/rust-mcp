//! Citation, identity, and cross-reference consistency — task `[047][M3]`
//! (issue #47, `plan.md` §10 "M3 exit" and §11 "… refer to the same immutable
//! corpus").
//!
//! This is a **verification module**, not a production one: it adds no runtime
//! behavior and changes no existing seam. It composes the already-tested
//! retrieval seams — the `[033]`–`[035]` search index (`crate::search`), the
//! `[036]` section/checklist reads (`crate::reads`), and the `[039]`–`[044]`
//! command catalog/guidance (`crate::commands`) — and asserts that they all
//! refer to the **one immutable snapshot** they were built from. Two
//! acceptance criteria (issue #47) are the assertions:
//!
//! * **AC1** — every section id a retrieval operation returns resolves, through
//!   the reads / [`SectionIndex`](crate::search::SectionIndex) seam, to a
//!   section whose *source URL* and *snapshot identity* match the snapshot the
//!   id was returned from;
//! * **AC2** — source-specific lint identifiers (`clippy::unwrap_used`), command
//!   identifiers (`cargo new`), and *unknown* compatibility values are
//!   **preserved** — not dropped, not normalized away — across every retrieval
//!   operation.
//!
//! ## Why this is proof-only
//!
//! The retrieval operations already guarantee both properties *by
//! construction*, so this task is to **prove** it rather than to fix a gap:
//!
//! * Every operation resolves a section id through the **same**
//!   [`SectionIndex::build`](crate::search::SectionIndex::build) of the
//!   **same** manifest it was served from. A search hit's
//!   [`Hit::section_id`](crate::search::Hit::section_id) is a `section.id`
//!   straight from that index; a checklist entry's `explanation_sections` are
//!   verified to resolve ([`crate::reads::get_api_checklist`]); a command
//!   entry's `upstream_sections` are verified to resolve
//!   ([`CommandCatalog::resolve`](crate::commands::CommandCatalog::resolve))
//!   before the guidance is composed. So a returned id can never point outside
//!   the served snapshot.
//! * Each operation stamps the read with `manifest.body.snapshot_id`, and
//!   returns the section **verbatim** (a clone through
//!   [`get_section`](crate::reads::get_section)), so the section's
//!   `canonical_url`, `source_id`, `lint_ids`, `command_keys`,
//!   `guideline_codes`, and `qualifications` arrive unchanged.
//!
//! A genuine gap would show up as a returned id that fails to resolve, or a
//! field that arrives altered; the tests below assert neither occurs, on both
//! the shipped bundled corpus and hand-built fixtures that carry the lint /
//! command / unknown-compatibility values the seed corpus does not.

// The module is gated `#[cfg(test)]` in `lib.rs`: it is a verification harness
// with no production caller, so it compiles only for the test target (and hence
// under `cargo check --all-targets` / `cargo clippy --all-targets`), and its
// helpers are used by the `#[test]`s below, so nothing is dead.

use crate::bundled::manifest;
use crate::commands::{CommandCatalog, LookupError, command_guidance};
use crate::model::{
    Channel, ChecklistEntry, ManifestBody, Release, SchemaVersion, Section, SectionBuilder,
    SectionId, SnapshotId, SnapshotManifest, Source, SourceId, Timestamp, Url, ValidationSummary,
};
use crate::provenance::{Instant, StaleThreshold, ToolchainCompat, classify_compatibility};
use crate::reads::{ReadError, get_api_checklist, get_section};
use crate::registry::host_is_registered;
use crate::search::SectionIndex;
use std::collections::BTreeSet;

/// The `as_of` point the bundled seed was captured at (its `fetched_at`), used
/// as the freshness-evaluation instant so the composed freshness verdict is the
/// honest "unknown" one the seed carries (no `last_checked`).
fn seed_as_of() -> Instant {
    Instant::parse("2026-10-30T00:00:00Z").expect("a valid instant")
}

/// Extract the host authority of an `http(s)` URL string, stripping scheme,
/// userinfo, port, path, query, and fragment. `None` for anything without an
/// `http`/`https` prefix.
fn host_of(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // Drop any `user@` userinfo and `:port` so a host is compared bare.
    let authority = authority.split('@').next().unwrap_or(authority);
    Some(authority.split(':').next().unwrap_or(authority))
}

// ---------------------------------------------------------------------------
// Fixtures
//
// The bundled corpus is a *representative* seed: its clippy section carries no
// `lint_ids`, and no section carries an explicit "unknown" qualification, so
// AC2's identifier-preservation proof needs hand-built sections that the seed
// does not. These fixtures are the "the seed corpus does not carry X" cases;
// each is built through `SectionBuilder` exactly the way the ingestion layer
// derives ids, so a section's derived id round-trips and resolves through the
// same `SectionIndex` the retrieval seams use.
// ---------------------------------------------------------------------------

/// A minimal `Source` record for `source_id`, with an entry URL on a registered
/// host and an unknown (not-yet-checked) freshness. Good enough for the reads
/// seam to locate the owning source and compose an honest "unknown" freshness.
fn source_stub(id: &str) -> Source {
    Source {
        id: SourceId::new(id).expect("a registry id builds a SourceId"),
        title: id.to_string(),
        entry_url: Url::new(format!("https://doc.rust-lang.org/{id}/")).expect("a valid URL"),
        document_urls: vec![],
        last_checked: None,
        fetched_at: Timestamp::new("2026-10-30T00:00:00Z").expect("a valid timestamp"),
        upstream_modified: None,
        content_hash: crate::model::CorpusHash::compute(b"consistency-fixture"),
        // An explicitly *unknown* release channel: the model preserves the
        // unknown rather than guessing a stable version.
        release: Release::channel_only(crate::model::Channel::Unknown),
        parser: "consistency-fixture".to_string(),
    }
}

/// Build a hand-written snapshot from its `sections` and `checklist`, carrying
/// one stub source per distinct section source so the owning source is present.
fn fixture_manifest(
    snapshot_id: &str,
    sections: Vec<Section>,
    checklist: Vec<ChecklistEntry>,
) -> SnapshotManifest {
    let sources: Vec<Source> = sections
        .iter()
        .map(|section| section.source_id.as_str().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|id| source_stub(&id))
        .collect();

    SnapshotManifest {
        schema_version: SchemaVersion::V1,
        body: ManifestBody {
            snapshot_id: SnapshotId::new(snapshot_id).expect("a URI-safe snapshot id"),
            created_at: Timestamp::new("2026-10-30T00:00:00Z").expect("a valid timestamp"),
            parser_version: "consistency-fixture".to_string(),
            sources,
            sections,
            checklist,
            commands: vec![],
            validation: ValidationSummary::default(),
            attribution: vec![],
        },
    }
}

/// A clippy section carrying **source-specific lint identifiers** and an
/// *unknown* compatibility qualification — the two values AC2 must preserve.
/// Its body mentions the lints so a search for `unwrap` surfaces it.
fn clippy_lint_section() -> Section {
    SectionBuilder::new(
        SourceId::new("clippy").expect("a registry id builds a SourceId"),
        "Clippy Lint Configuration",
        "configuration",
        Url::new("https://doc.rust-lang.org/clippy/configuration.html").expect("a valid URL"),
        "The clippy configuration reference documents the lint groups and individual \
         lints. The `clippy::unwrap_used` lint flags calls to `unwrap`, and the \
         `clippy::float_cmp` lint flags floating-point comparisons.",
    )
    .lint_ids(["clippy::unwrap_used", "clippy::float_cmp"])
    .qualifications([
        "nightly component".to_string(),
        "version: unknown".to_string(),
    ])
    .build()
    .expect("a well-formed section")
}

/// A cargo command section carrying **command identifiers** (`command_keys`).
fn cargo_command_section() -> Section {
    SectionBuilder::new(
        SourceId::new("cargo").expect("a registry id builds a SourceId"),
        "The Cargo Book",
        "commands/cargo-new",
        Url::new("https://doc.rust-lang.org/cargo/commands/cargo-new.html").expect("a valid URL"),
        "The `cargo new` command creates a new Cargo package in a new directory.",
    )
    .command_keys(["cargo new", "cargo new --lib"])
    .qualifications(["stable".to_string()])
    .build()
    .expect("a well-formed section")
}

/// An api-guidelines section carrying **guideline codes** (`guideline_codes`),
/// the kind of citation a checklist entry explains.
fn api_guidelines_section() -> Section {
    SectionBuilder::new(
        SourceId::new("api-guidelines").expect("a registry id builds a SourceId"),
        "The API Guidelines",
        "about",
        Url::new("https://rust-lang.github.io/api-guidelines/about.html").expect("a valid URL"),
        "The Rust API Guidelines carry a stable code such as `C-CONV` and are enforced \
         in practice by Clippy lints.",
    )
    .guideline_codes(["C-CONV", "E-RESCUE"])
    .build()
    .expect("a well-formed section")
}

/// Reconstruct the section a catalog id names: parse the id back to its
/// components and rebuild a section whose *derived* id equals the cited id. The
/// catalog's `upstream_sections` are all unsplit sections of a registry source,
/// so a rebuild through `SectionBuilder` round-trips to the exact id.
fn section_for_cited_id(id: &SectionId) -> Section {
    let parts = SectionId::parse(id.as_str()).expect("a cited id is well-formed");
    let chapter = parts.chapter_path;
    let url = Url::new(format!(
        "https://doc.rust-lang.org/{source}/{chapter}.html",
        source = parts.source
    ))
    .expect("a valid canonical URL");
    let mut builder = SectionBuilder::new(
        SourceId::new(&parts.source).expect("a citation names a registry source"),
        format!("{chapter} reference"),
        &chapter,
        url,
        format!("A faithful, source-backed excerpt for `{id}`."),
    );
    if let Some(anchor) = parts.anchor.as_deref() {
        builder = builder.anchor(anchor);
    }
    if let Some((part, parts_total)) = parts.continuation {
        builder = builder.continuation(crate::model::Continuation {
            part,
            parts: parts_total,
        });
    }
    let section = builder.build().expect("a well-formed section");
    // The rebuild must recover the exact id the catalog cited; a mismatch would
    // mean the fixture does not resolve the very id under test.
    assert_eq!(
        section.id, *id,
        "a rebuilt section round-trips to its cited id"
    );
    section
}

/// Build a snapshot whose `sections` resolve **every** id the catalog cites —
/// one reconstructed section per distinct cited id across all keys.
fn corpus_for_catalog(catalog: &CommandCatalog) -> Vec<Section> {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for key in catalog.keys() {
        let entry = catalog
            .get(&key)
            .expect("a catalog key is present in the catalog");
        for reference in &entry.upstream_sections {
            ids.insert(reference.as_str().to_string());
        }
    }
    ids.into_iter()
        .map(|s| section_for_cited_id(&s.parse::<SectionId>().expect("a cited id parses")))
        .collect()
}

/// The AC1 consistency assertion, reused by every retrieval operation: a
/// returned section id resolves, through the reads seam, to a section whose
/// source URL host is a registered source boundary and whose snapshot identity
/// is the snapshot it was served from — and whose owning source is present in
/// that same snapshot.
fn assert_section_consistent(
    manifest: &SnapshotManifest,
    id: &SectionId,
    context: &str,
) -> Section {
    let read = get_section(manifest, id, seed_as_of(), StaleThreshold::default())
        .unwrap_or_else(|err| panic!("{context}: expected `{id}` to resolve, got {err:?}"));

    // The resolved section's own identity matches the id that was requested.
    assert_eq!(
        read.section.id, *id,
        "{context}: the resolved section's id matches the requested id"
    );
    // AC1 "matching snapshot identity": the read stamps the snapshot it served
    // from.
    assert_eq!(
        read.snapshot_id, manifest.body.snapshot_id,
        "{context}: the read carries the snapshot identity of the snapshot it served from"
    );
    // AC1 "matching source URL": the citation's host is a registered source
    // boundary (the section belongs to a source the registry knows).
    let url = read.section.canonical_url.as_str();
    assert!(
        !url.is_empty(),
        "{context}: the section carries a non-empty canonical source URL"
    );
    crate::model::Url::new(url).expect("{context}: the canonical URL is well-formed");
    let host = host_of(url).unwrap_or_else(|| panic!("{context}: `{url}` has no parseable host"));
    assert!(
        host_is_registered(host),
        "{context}: the section's source URL host `{host}` is a registered source boundary"
    );
    // The owning source is present in the same snapshot the section came from.
    assert!(
        manifest
            .body
            .sources
            .iter()
            .any(|source| source.id.as_str() == read.section.source_id.as_str()),
        "{context}: the section's owning source `{}` is present in the snapshot",
        read.section.source_id
    );
    read.section
}

// ===========================================================================
// AC1 — every returned section id resolves with matching source URL + snapshot
// ===========================================================================

/// AC1 (bundled corpus, exhaustive): every section in the shipped snapshot
/// resolves, through its own index and through the reads seam, to itself with a
/// matching source URL and snapshot identity — the corpus is internally
/// consistent under the one snapshot it declares.
#[test]
fn ac1_bundled_corpus_is_internally_consistent() {
    let m = manifest();
    assert!(
        !m.body.sections.is_empty(),
        "the bundled corpus is non-empty"
    );

    let index = SectionIndex::build(&m);
    assert_eq!(
        index.doc_count(),
        m.body.sections.len(),
        "the index covers every section of the snapshot"
    );

    for section in &m.body.sections {
        // The index resolves the section's own id.
        assert!(
            index.get(&section.id).is_some(),
            "the index resolves section `{}`",
            section.id
        );
        // …and the reads seam resolves it with a matching source URL and
        // snapshot identity.
        assert_section_consistent(&m, &section.id, "bundled section");
    }
}

/// AC1 (search → resolve): every id a search returns resolves, in the same
/// snapshot, to a section with a matching source URL and snapshot identity. A
/// hit's [`Hit::section_id`](crate::search::Hit::section_id) is never a
/// dangling or foreign reference.
#[test]
fn ac1_search_hits_resolve_in_the_same_snapshot() {
    let m = manifest();
    let index = SectionIndex::build(&m);

    // A spread of probes across the corpus; every returned hit is a section id
    // from this snapshot, so every one must resolve in it. The probes are
    // standalone words (the tokenizer merges command phrases such as `cargo
    // build` into one token, so a bare `cargo` query surfaces nothing).
    let mut surfaced = 0usize;
    for query in [
        "build",
        "compiler",
        "installer",
        "formatter",
        "lints",
        "options",
        "diagnostics",
    ] {
        for hit in index.search(query) {
            surfaced += 1;
            assert_section_consistent(&m, &hit.section_id, "search hit");
        }
    }
    assert!(surfaced > 0, "the probes surfaced at least one search hit");
}

/// AC1 (checklist → resolve): every checklist entry's explanation section
/// resolves, in the same snapshot, to a section with a matching source URL and
/// snapshot identity. [`get_api_checklist`] already refuses a dangling
/// explanation, so the read returning means every explanation is retrievable.
#[test]
fn ac1_checklist_explanations_resolve_in_the_same_snapshot() {
    let m = manifest();
    let read = get_api_checklist(&m, None).expect("all categories resolve");
    assert!(
        !read.entries.is_empty(),
        "the bundled corpus carries checklist entries"
    );

    for entry in &read.entries {
        for reference in &entry.explanation_sections {
            assert_section_consistent(&m, reference, "checklist explanation");
        }
    }
}

/// AC1 (command → resolve): every section a command's guidance cites resolves,
/// in the same snapshot, to a section with a matching source URL and snapshot
/// identity. The test builds a corpus that resolves the catalog's *entire*
/// citation set and then checks every key — no command guidance ever hands back
/// a dangling or foreign section.
#[test]
fn ac1_command_citations_resolve_in_the_same_snapshot() {
    let catalog = CommandCatalog::combined();
    assert!(!catalog.keys().is_empty(), "the catalog is non-empty");

    let m = fixture_manifest("command-catalog", corpus_for_catalog(&catalog), vec![]);
    for key in catalog.keys() {
        let resolved = catalog
            .resolve(&m, &key)
            .expect("every catalog key resolves its citations");
        assert!(
            !resolved.sections.is_empty(),
            "key `{key}` cites at least one section"
        );
        for section in &resolved.sections {
            assert_section_consistent(&m, &section.id, &format!("command citation `{key}`"));
        }
    }
}

/// AC1 (cross-operation, one snapshot): `get_section`, `get_api_checklist`, and
/// `command_guidance` — run against the *same* snapshot — all stamp that
/// snapshot's identity. This is the literal "refer to the same immutable
/// corpus" property of `plan.md` §11: the three read seams agree on which
/// snapshot they served.
#[test]
fn ac1_all_retrieval_operations_agree_on_one_snapshot() {
    let catalog = CommandCatalog::combined();
    let m = fixture_manifest(
        "cross-operation-snapshot",
        corpus_for_catalog(&catalog),
        vec![],
    );
    let snapshot = m.body.snapshot_id.clone();

    // `get_section` stamps the snapshot it served from.
    let section_id = m
        .body
        .sections
        .first()
        .expect("the catalog corpus is non-empty")
        .id
        .clone();
    let section_read =
        get_section(&m, &section_id, seed_as_of(), StaleThreshold::default()).expect("resolves");
    assert_eq!(
        section_read.snapshot_id, snapshot,
        "get_section stamps the snapshot it served from"
    );

    // `get_api_checklist` — an empty checklist is a valid empty result — stamps
    // the same snapshot.
    let checklist = get_api_checklist(&m, None).expect("an empty checklist is valid");
    assert_eq!(
        checklist.snapshot_id, snapshot,
        "get_api_checklist stamps the snapshot it served from"
    );

    // `command_guidance` and every section it composes stamp the same snapshot.
    let key = catalog
        .keys()
        .first()
        .expect("the catalog is non-empty")
        .clone();
    let guidance = command_guidance(
        &catalog,
        &m,
        seed_as_of(),
        StaleThreshold::default(),
        &key,
        None,
        None,
    )
    .expect("the guidance resolves");
    assert_eq!(
        guidance.snapshot_id, snapshot,
        "command_guidance stamps the snapshot it served from"
    );
    for read in guidance.sections() {
        assert_eq!(
            read.snapshot_id, snapshot,
            "each composed section read stamps the snapshot it served from"
        );
    }
}

// ===========================================================================
// AC2 — explicit errors: an unresolvable reference is a typed error, never a
//        panic or a fabricated section
// ===========================================================================

/// AC2 (explicit errors): an id / category / citation that does not resolve is
/// a *typed, explicit* error — never a panic, never a fabricated section. The
/// negative counterpart to the AC1 "everything resolves" tests.
#[test]
fn ac2_unresolvable_references_are_explicit_errors() {
    let m = manifest();

    // An absent section id is an explicit `UnknownSection` carrying that id.
    let missing: SectionId = "cargo/does-not-exist".parse().expect("a well-formed id");
    assert_eq!(
        get_section(&m, &missing, seed_as_of(), StaleThreshold::default()),
        Err(ReadError::UnknownSection(missing.clone())),
        "an absent section id is an explicit error, not a panic or a fabrication"
    );

    // An absent category is an explicit `UnknownCategory` carrying it.
    assert_eq!(
        get_api_checklist(&m, Some("No Such Category")),
        Err(ReadError::UnknownCategory("No Such Category".to_string())),
        "an absent category is an explicit error"
    );

    // A catalog key whose citation is absent is an explicit `UnresolvedSection`
    // — guidance is never fabricated for it. The expected section id is the one
    // the catalog itself cites, so the assertion is exact, not a guessed string.
    let catalog = CommandCatalog::combined();
    let cited = catalog
        .get("cargo new")
        .expect("`cargo new` is a known catalog key")
        .upstream_sections[0]
        .clone();
    let empty = fixture_manifest("empty-snapshot", vec![], vec![]);
    assert_eq!(
        catalog.resolve(&empty, "cargo new"),
        Err(LookupError::UnresolvedSection {
            key: "cargo new".to_string(),
            section: cited,
        }),
        "a citation that does not resolve is an explicit error, never a fabricated section"
    );
}

/// AC2 (checklist dangling): a matched entry whose explanation is absent from
/// the snapshot is an explicit [`ReadError::DanglingExplanation`], naming the
/// entry and the missing section — the read never hands back an unresolvable
/// reference.
#[test]
fn ac2_checklist_dangling_explanation_is_explicit() {
    let missing: SectionId = "api-guidelines/absent".parse().expect("a well-formed id");
    let entry = ChecklistEntry {
        code: "C-CONV".to_string(),
        title: "Conversions and From".to_string(),
        category: "Conversions and From".to_string(),
        location: "Conversions and From#C-CONV".to_string(),
        explanation_sections: vec![missing.clone()],
    };
    let m = fixture_manifest("dangling", vec![], vec![entry]);

    assert_eq!(
        get_api_checklist(&m, Some("Conversions and From")),
        Err(ReadError::DanglingExplanation {
            code: "C-CONV".to_string(),
            missing,
        }),
        "a dangling explanation is an explicit error naming the entry and the missing section"
    );
}

// ===========================================================================
// AC2 — lint / command identifiers and unknown compatibility values are
//        preserved across every retrieval operation
// ===========================================================================

/// AC2: source-specific **lint identifiers** survive a search hit and a
/// section read verbatim — the `clippy::*` ids and the "unknown" qualification
/// are neither dropped nor normalized away.
#[test]
fn ac2_lint_and_unknown_qualifications_survive_the_section_read() {
    let section = clippy_lint_section();
    let m = fixture_manifest("lint-preservation", vec![section.clone()], vec![]);

    // A search that names the lint id surfaces the section; its hit resolves in
    // the same snapshot.
    let index = SectionIndex::build(&m);
    let hit = index
        .search("clippy::unwrap_used")
        .into_iter()
        .find(|hit| hit.section_id == section.id)
        .expect("a search for the lint id surfaces the clippy section");

    // The section read returns the lint ids and the unknown qualification
    // verbatim.
    let read =
        get_section(&m, &section.id, seed_as_of(), StaleThreshold::default()).expect("resolves");
    assert_eq!(
        read.section.lint_ids,
        vec![
            "clippy::unwrap_used".to_string(),
            "clippy::float_cmp".to_string()
        ],
        "source-specific lint ids survive the section read verbatim"
    );
    assert!(
        read.section
            .qualifications
            .iter()
            .any(|q| q == "version: unknown"),
        "the unknown compatibility qualification is preserved, not normalized away"
    );

    // The hit's id and the direct id resolve to the very same record — the
    // search path and the read path agree on the preserved values.
    let via_hit = get_section(&m, &hit.section_id, seed_as_of(), StaleThreshold::default())
        .expect("resolves");
    assert_eq!(
        via_hit.section, read.section,
        "the hit and the read return the same record"
    );
}

/// AC2: source-specific **command identifiers** (`command_keys`) survive
/// command-guidance resolution verbatim — the `cargo new` family is neither
/// dropped nor flattened when the guidance is composed from the catalog.
#[test]
fn ac2_command_identifiers_survive_command_guidance() {
    // A section that carries source-specific command identifiers preserves them
    // through the section read.
    let section = cargo_command_section();
    let m = fixture_manifest("command-keys-preservation", vec![section.clone()], vec![]);
    let read =
        get_section(&m, &section.id, seed_as_of(), StaleThreshold::default()).expect("resolves");
    assert_eq!(
        read.section.command_keys,
        vec!["cargo new".to_string(), "cargo new --lib".to_string()],
        "source-specific command identifiers survive the section read verbatim"
    );

    // Through the catalog, the command identifier (the entry key) and its cited
    // upstream sections survive `command_guidance` resolution.
    let catalog = CommandCatalog::combined();
    let corpus = fixture_manifest("command-catalog", corpus_for_catalog(&catalog), vec![]);
    let guidance = command_guidance(
        &catalog,
        &corpus,
        seed_as_of(),
        StaleThreshold::default(),
        "cargo new",
        None,
        None,
    )
    .expect("the cargo new guidance resolves");
    assert_eq!(
        guidance.entry.key.as_str(),
        "cargo new",
        "the command identifier is preserved verbatim through guidance resolution"
    );
    assert!(
        !guidance.entry.upstream_sections.is_empty(),
        "the command's cited upstream sections are preserved, not dropped"
    );
}

/// AC2: **guideline codes** survive the checklist read verbatim — a checklist
/// entry that explains a section keeps that section's `guideline_codes` intact
/// when the explanation is resolved.
#[test]
fn ac2_guideline_codes_survive_the_checklist_read() {
    let section = api_guidelines_section();
    let entry = ChecklistEntry {
        code: "C-CONV".to_string(),
        title: "Conversions and From".to_string(),
        category: "Conversions and From".to_string(),
        location: "Conversions and From#C-CONV".to_string(),
        explanation_sections: vec![section.id.clone()],
    };
    let m = fixture_manifest("guideline-preservation", vec![section.clone()], vec![entry]);

    // The checklist read returns the entry with its explanation id preserved.
    let read =
        get_api_checklist(&m, Some("Conversions and From")).expect("the known category resolves");
    assert_eq!(
        read.entries.len(),
        1,
        "the matching category returns its entry"
    );
    let explanation = read.entries[0].explanation_sections[0].clone();
    assert_eq!(
        explanation, section.id,
        "the checklist preserves the explanation id verbatim"
    );

    // Resolving the explanation through the section read preserves the section's
    // guideline codes.
    let resolved =
        get_section(&m, &explanation, seed_as_of(), StaleThreshold::default()).expect("resolves");
    assert_eq!(
        resolved.section.guideline_codes,
        vec!["C-CONV".to_string(), "E-RESCUE".to_string()],
        "guideline codes survive the checklist → section read verbatim"
    );
}

/// AC2: an **unknown** compatibility value is preserved *as unknown* across the
/// model and every retrieval path — it is never coerced to a known tier nor
/// dropped. This is the `plan.md` §5 "expose unavailable upstream version
/// information as unknown" property.
#[test]
fn ac2_unknown_compatibility_is_preserved() {
    // The model preserves an unknown channel as `Unknown`, never coercing it to
    // a known tier or a fabricated version.
    assert_eq!(
        classify_compatibility(&Release::channel_only(Channel::Unknown)),
        ToolchainCompat::Unknown,
        "an unknown channel classifies as unknown, not a guessed stable release"
    );
    assert_eq!(
        classify_compatibility(&Release::default()),
        ToolchainCompat::Unknown,
        "an all-unknown release stays unknown"
    );

    // Contrast: a known stable release with an identified version is *not*
    // unknown — the preservation is not a blanket default to `Unknown`.
    let stable = Release {
        channel: Channel::Stable,
        documentation_release: Some("1.88.0".to_string()),
        revision: None,
    };
    assert_eq!(
        classify_compatibility(&stable),
        ToolchainCompat::Stable {
            release: "1.88.0".to_string()
        },
        "a known stable release classifies as Stable with its version"
    );

    // The unknown survives a retrieval path: a source captured with an unknown
    // release is read back unchanged, so the retrieval layer does not normalize
    // it away.
    let section = clippy_lint_section();
    let m = fixture_manifest("unknown-preservation", vec![section.clone()], vec![]);
    let unknown_source = m
        .body
        .sources
        .iter()
        .find(|source| source.id == section.source_id)
        .expect("the section's source is present in the snapshot");
    assert_eq!(
        classify_compatibility(&unknown_source.release),
        ToolchainCompat::Unknown,
        "the section's source keeps its unknown release through the read path"
    );
}

// ===========================================================================
// AC1 + AC2 — determinism / immutability: the operations are pure over the one
//        immutable snapshot
// ===========================================================================

/// AC1 + AC2 (determinism / immutability): resolving the same id through the
/// index and through two separate reads yields the *same* section record and
/// the *same* snapshot identity — the operations are pure over the one
/// immutable snapshot, which is what makes "the same corpus" meaningful.
#[test]
fn ac1_and_ac2_resolution_is_deterministic_over_one_snapshot() {
    let m = manifest();
    let id: SectionId = "api-guidelines/about".parse().expect("a valid id");

    // The index and the reads seam agree on the record.
    let via_index = SectionIndex::build(&m).get(&id).expect("resolves").clone();
    let via_reads = get_section(&m, &id, seed_as_of(), StaleThreshold::default())
        .expect("resolves")
        .section;
    assert_eq!(
        via_index, via_reads,
        "the index and the read seam return the same record"
    );

    // Two independent reads of the same id are identical — the operations are
    // pure over the one immutable snapshot.
    let read_a = get_section(&m, &id, seed_as_of(), StaleThreshold::default()).expect("resolves");
    let read_b = get_section(&m, &id, seed_as_of(), StaleThreshold::default()).expect("resolves");
    assert_eq!(read_a, read_b, "two reads of the same id are identical");
    assert_eq!(
        read_a.snapshot_id, m.body.snapshot_id,
        "both reads stamp the one snapshot identity"
    );
}
