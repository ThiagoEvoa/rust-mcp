//! The bundled documentation snapshot — task `[031]` M2 (issue #31, `plan.md` §10).
//!
//! The binary ships a **validated, attributed** eight-source snapshot so a fresh
//! install serves the Rust toolchain documentation **without any network access**
//! (issue #31 acceptance criterion 1). The bundle is embedded at compile time
//! from [`JSON`] (committed at `data/bundled/snapshot.json`); the canonical
//! builder for that file is [`manifest`], and the drift test
//! `the_committed_bundle_matches_the_builder` keeps the two in lockstep.
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
//! The builder assembles a **representative** corpus: one section per collection,
//! the API-guidelines `C-CONV` checklist entry, and a few cited command entries.
//! A full, current corpus is produced by [`crate::ingest::sync`](crate::ingest)
//! and published to the data directory by the `sync` command; the bundle is the
//! offline seed that makes the *first* launch usable.

use crate::model::{
    AttributionRef, Channel, ChecklistEntry, CommandEntry, EffectKind, EffectNote, ManifestBody,
    Release, SchemaVersion, Section, SectionBuilder, SectionId, SnapshotId, SnapshotManifest,
    Source, SourceId, Timestamp, Url, ValidationSummary,
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

/// The parser that produced the seed (a seed, not the live ingestion pipeline).
const PARSER_VERSION: &str = "rust-docs-mcp/seed-1";

/// The fixed point in time the seed was assembled. Fixed so the bundle is
/// deterministic across builds; the live `sync` pipeline stamps a real time.
const CREATED_AT: &str = "2026-10-30T00:00:00Z";
const FETCHED_AT: &str = "2026-10-30T00:00:00Z";

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
/// This is the bundle [`JSON`] embeds. It is a **representative** corpus — one
/// section per collection, the `C-CONV` checklist entry, and a few cited command
/// entries — sufficient to make a fresh install serve *something* offline; the
/// full corpus arrives from `sync`. It always validates
/// ([`crate::corpus::validate_snapshot`]).
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
    use std::collections::{HashMap, HashSet};

    let excluded_set: HashSet<&str> = excluded.iter().copied().collect();

    // --- One representative section per collection, built through
    // `SectionBuilder` so each section's id and content hash are the consistent
    // values the validator requires. ---
    let sections = vec![
        api_guidelines_section(),
        style_guide_section(),
        cargo_section(),
        rustc_section(),
        rustup_section(),
        rustfmt_section(),
        clippy_section(),
        rustdoc_section(),
    ];

    // The survivor set: every collection except the excluded ones.
    let surviving: HashSet<&str> = SourceCollection::ALL
        .iter()
        .map(|c| c.id())
        .filter(|id| !excluded_set.contains(*id))
        .collect();

    // Keep only sections whose source survives.
    let sections: Vec<Section> = sections
        .into_iter()
        .filter(|s| surviving.contains(s.source_id.as_str()))
        .collect();

    // A source id -> that source's section id, for cross-references.
    let section_id_of: HashMap<&str, SectionId> = sections
        .iter()
        .map(|s| (s.source_id.as_str(), s.id.clone()))
        .collect();

    // --- Sources: one per surviving collection. ---
    let sources: Vec<Source> = SourceCollection::ALL
        .iter()
        .copied()
        .filter(|c| surviving.contains(c.id()))
        .map(source_for)
        .collect();

    // --- Checklist: the API-guidelines C-CONV entry, kept only if its target
    // section survives. It cites the *derived* section id, so it always resolves. ---
    let checklist = if let Some(target) = section_id_of.get("api-guidelines") {
        vec![ChecklistEntry {
            code: "C-CONV".into(),
            title: "Prefer `From`/`Into` for conversions".into(),
            category: "Conversions and From".into(),
            location: "Conversions and From#C-CONV".into(),
            explanation_sections: vec![target.clone()],
        }]
    } else {
        vec![]
    };

    // --- Commands: each cites the *derived* section id of its own source, so it
    // resolves only when that source survives. ---
    let mut commands: Vec<CommandEntry> = Vec::new();
    if let Some(id) = section_id_of.get("cargo") {
        commands.push(cargo_command(
            id,
            "cargo build",
            "Compile the local packages",
        ));
        commands.push(cargo_command(
            id,
            "cargo test",
            "Run the project's unit and integration tests",
        ));
    }
    if let Some(id) = section_id_of.get("rustup") {
        commands.push(rustup_command(id));
    }

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
            parser_version: PARSER_VERSION.to_string(),
            sources,
            sections,
            checklist,
            commands,
            attribution,
            validation: summary,
        },
    }
}

// --- Section builders ------------------------------------------------------
//
// One representative section per collection. Each body is a short, faithful
// distillation of the upstream document's purpose (not verbatim prose); the live
// `sync` pipeline ingests the full content. Each is attributed through the
// source's `attribution` record.

fn api_guidelines_section() -> Section {
    SectionBuilder::new(
        SourceId::new("api-guidelines").expect("a registry id builds a SourceId"),
        "The API Guidelines",
        "about",
        Url::new("https://rust-lang.github.io/api-guidelines/about.html").expect("a valid URL"),
        "The Rust API Guidelines are recommendations for the design, documentation, and testing of \
         Rust APIs. They help an API author write an API that is easy to use and hard to misuse. \
         Each guideline carries a stable code (for example C-CONV) and is enforced in practice by \
         Clippy lints.",
    )
    .guideline_codes(["C-CONV"])
    .build()
    .expect("a well-formed section")
}

fn style_guide_section() -> Section {
    SectionBuilder::new(
        SourceId::new("style-guide").expect("a registry id builds a SourceId"),
        "The Rust Style Guide",
        "intro",
        Url::new("https://doc.rust-lang.org/nightly/style-guide/intro.html").expect("a valid URL"),
        "The Rust Style Guide captures the common style conventions of the Rust community: \
         naming, whitespace, comments, error handling, and more. It is a style reference, not a \
         compiler or linter, and is updated on the nightly documentation.",
    )
    .build()
    .expect("a well-formed section")
}

fn cargo_section() -> Section {
    SectionBuilder::new(
        SourceId::new("cargo").expect("a registry id builds a SourceId"),
        "The Cargo Book",
        "commands/cargo-build",
        Url::new("https://doc.rust-lang.org/cargo/commands/cargo-build.html").expect("a valid URL"),
        "The Cargo Book documents the Rust build system and package manager. `cargo build` compiles \
         the current package (and its dependencies) for the target platform, writing artifacts under \
         `target/`. Run `cargo <command> --help` for the full option list; the book explains each \
         command, its flags, and their effects.",
    )
    .command_keys(["cargo build", "cargo test"])
    .qualifications(["Run `cargo build --help` for the authoritative, version-specific option list."])
    .build()
    .expect("a well-formed section")
}

fn rustc_section() -> Section {
    SectionBuilder::new(
        SourceId::new("rustc").expect("a registry id builds a SourceId"),
        "The Rustc Book",
        "introduction",
        Url::new("https://doc.rust-lang.org/rustc/introduction.html").expect("a valid URL"),
        "The Rustc Book is a reference for the `rustc` compiler: how to invoke it, its command-line \
         options, and how to read its diagnostics. Because the reference tracks the nightly toolchain, \
         a stable `rustc` may differ from the documented flags.",
    )
    .qualifications(["The rustc reference tracks the nightly toolchain; flag behaviour can differ on stable."])
    .build()
    .expect("a well-formed section")
}

fn rustup_section() -> Section {
    SectionBuilder::new(
        SourceId::new("rustup").expect("a registry id builds a SourceId"),
        "The Rustup Book",
        "common-install-problems",
        Url::new("https://doc.rust-lang.org/rustup/common-install-problems.html").expect("a valid URL"),
        "The Rustup Book documents the Rust toolchain installer: installing, updating, and managing \
         toolchains and components. `rustup show` reports the installed and active toolchains; \
         `rustup update` brings a toolchain current.",
    )
    .command_keys(["rustup show"])
    .build()
    .expect("a well-formed section")
}

fn rustfmt_section() -> Section {
    SectionBuilder::new(
        SourceId::new("rustfmt").expect("a registry id builds a SourceId"),
        "The rustfmt Book",
        "options",
        Url::new("https://doc.rust-lang.org/nightly/rustfmt/options.html").expect("a valid URL"),
        "The rustfmt Book documents `rustfmt`, the code formatter that ships as a component of the \
         Rust distribution. It lists the options `rustfmt` accepts and how to configure it via a \
         `rustfmt.toml` file. Because rustfmt is a nightly component, its option set can change.",
    )
    .qualifications(["rustfmt is a nightly component; its option set can change between releases."])
    .build()
    .expect("a well-formed section")
}

fn clippy_section() -> Section {
    SectionBuilder::new(
        SourceId::new("clippy").expect("a registry id builds a SourceId"),
        "Clippy Lint Configuration",
        "configuration",
        Url::new("https://doc.rust-lang.org/clippy/configuration.html").expect("a valid URL"),
        "The Clippy configuration reference documents the lint groups, individual lint levels, and \
         per-lint options. Clippy is a nightly component, so a specific lint may be added, renamed, or \
         removed between releases; treat an unfamiliar lint as possibly version-dependent.",
    )
    .qualifications(["Clippy is a nightly component; a lint may be added or removed between releases."])
    .build()
    .expect("a well-formed section")
}

fn rustdoc_section() -> Section {
    SectionBuilder::new(
        SourceId::new("rustdoc").expect("a registry id builds a SourceId"),
        "The Rustdoc Book",
        "command-line-arguments",
        Url::new("https://doc.rust-lang.org/rustdoc/command-line-arguments.html")
            .expect("a valid URL"),
        "The Rustdoc Book documents `rustdoc`, the documentation generator for Rust. It lists the \
         command-line options that control the generated documentation, from output formats to the \
         HTML template and theme. Because rustdoc tracks the compiler, option behaviour can differ \
         between toolchains.",
    )
    .qualifications([
        "rustdoc tracks the compiler; option behaviour can differ between toolchains.",
    ])
    .build()
    .expect("a well-formed section")
}

// --- Sources ---------------------------------------------------------------

/// Build a source record for a collection, attributed to its registry entry.
fn source_for(collection: SourceCollection) -> Source {
    let id = SourceId::new(collection.id()).expect("a registry id builds a SourceId");
    Source {
        id,
        title: collection.title().to_string(),
        entry_url: Url::new(collection.entry_url()).expect("a registry URL is valid"),
        document_urls: vec![Url::new(collection.entry_url()).expect("a registry URL is valid")],
        last_checked: None, // a seed has not verified the upstream; `sync` stamps this.
        fetched_at: Timestamp::new(FETCHED_AT).expect("a valid timestamp"),
        upstream_modified: None,
        content_hash: crate::model::CorpusHash::compute(b"seed-document"),
        release: Release::channel_only(channel_for(collection)),
        parser: PARSER_VERSION.to_string(),
    }
}

// --- Command entries -------------------------------------------------------

/// A representative seed channel for a collection. The live `sync` pipeline
/// stamps the channel actually captured from upstream; a seed snapshot has not
/// checked upstream, so it records a representative channel (stable-distribution
/// docs on `doc.rust-lang.org` are `Stable`; project-hosted docs are `Rolling`).
fn channel_for(collection: SourceCollection) -> Channel {
    match collection {
        SourceCollection::Cargo
        | SourceCollection::Rustc
        | SourceCollection::StyleGuide
        | SourceCollection::Clippy
        | SourceCollection::Rustdoc => Channel::Stable,
        SourceCollection::ApiGuidelines | SourceCollection::Rustup | SourceCollection::Rustfmt => {
            Channel::Rolling
        }
    }
}

fn cargo_command(target: &SectionId, key: &str, purpose: &str) -> CommandEntry {
    CommandEntry {
        key: crate::model::CommandKey::new(key).expect("a well-formed command key"),
        purpose: purpose.to_string(),
        upstream_sections: vec![target.clone()],
        syntax: vec![format!("{key} [OPTIONS]")],
        examples: vec![],
        prerequisites: vec![],
        compatibility: vec!["stable".to_string()],
        effect_notes: vec![EffectNote {
            kind: EffectKind::ProjectCodeExecution,
            detail: "compiles and/or runs the project's own code".to_string(),
        }],
    }
}

fn rustup_command(target: &SectionId) -> CommandEntry {
    CommandEntry {
        key: crate::model::CommandKey::new("rustup show").expect("a well-formed command key"),
        purpose: "Report the installed toolchains and the active one.".to_string(),
        upstream_sections: vec![target.clone()],
        syntax: vec!["rustup show [OPTIONS]".to_string()],
        examples: vec![],
        prerequisites: vec![],
        compatibility: vec!["stable".to_string()],
        effect_notes: vec![],
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
        repository: collection.entry_url().to_string(),
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
