//! Faithful seed corpus generation (issue #150).
//!
//! The offline seed is no longer hand-authored. This module runs the *validated
//! ingestion pipeline* over **recorded upstream fixtures** — the actual upstream
//! HTML/Markdown captured into `tests/fixtures/upstream/` and embedded here via
//! `include_str!` — so the shipped corpus carries the real upstream wording,
//! headings, anchors, guideline codes, examples, and qualifications rather than
//! summary paragraphs. The fixtures are pinned by provenance
//! ([`provenance`]); a live upstream re-fetch is an opt-in, env-gated dev/test
//! seam that rewrites the fixtures, so the default build is fully hermetic.
//!
//! ## What is generated
//!
//! - **Every one of the eight sources** is ingested from a recorded page through
//!   the same adapter the live sync pipeline uses (`api_guidelines`,
//!   `style_guide`, `cargo`, `rustc`, `rustup`, `rustdoc`, `rustfmt`, and the
//!   Clippy per-lint catalog). A section's content hash is the FNV-1a hash of the
//!   bytes actually captured for its source, so the hash is a truthful identity
//!   of the captured content, not a decorative placeholder.
//! - **C-CONV** resolves to its real explanation — the `as_`/`to_`/`into_`
//!   naming rule on `api-guidelines/naming` — and **C-CONV-TRAITS** resolves to
//!   the standard-conversion-traits rule on `api-guidelines/interoperability`.
//!   The two are *distinct* upstream guidelines, so the seed now distinguishes
//!   them.
//! - **The checklist** is parsed from the recorded `checklist.html` through the
//!   same `parse_checklist` + `link_explanations` stages the live pipeline uses,
//!   then kept to the codes whose explanatory sections are actually shipped, so
//!   every shipped entry resolves to a real section.
//! - **Clippy lint detail** (e.g. `clippy::unwrap_used`, `clippy::clone_on_copy`)
//!   comes from the per-lint catalog extractor, so a fresh offline user can
//!   retrieve lint details.
//! - **rustfmt check-mode** and **style-guide trailing-comma** content ship as
//!   ingested sections, so the product's required offline queries resolve.
//!
//! ## Faithfulness guarantee
//!
//! The body of each shipped section is the *output of the real extractor* over
//! the recorded fixture — never a hand-copied summary. The drift test
//! `the_committed_bundle_matches_the_builder` in `crate::bundled` re-parses the
//! *same* recorded fixtures through the builder (`manifest`) and asserts the
//! committed `data/bundled/snapshot.json` is equal to that output, so any drift
//! between the shipped excerpt and the upstream content it was generated from is
//! a test failure, not a silent divergence.
//!
//! Live upstream re-fetch (rewriting `tests/fixtures/upstream/` and the committed
//! `data/bundled/snapshot.json`) is opt-in and env-gated; it never runs on the
//! default, hermetic build or test.

#![allow(dead_code)] // transitional: the seed builder is wired by `crate::bundled`

use std::collections::{BTreeMap, BTreeSet};

use super::api_guidelines::{
    ingest_chapter as ingest_api_chapter, ingest_checklist, link_explanations,
};
use super::cargo::ingest_chapter as ingest_cargo;
use super::clippy::ingest_lint_catalog;
use super::fetch::Fetched;
use super::rustc::ingest_chapter as ingest_rustc;
use super::rustdoc::ingest_chapter as ingest_rustdoc;
use super::rustfmt::ingest_markdown;
use super::rustup::ingest_chapter as ingest_rustup;
use super::style_guide::ingest_chapter as ingest_style;
use crate::model::{
    ChecklistEntry, CommandKey, CorpusHash, Release, Section, Source, SourceId, Timestamp, Url,
};
use reqwest::Url as RqUrl;

/// The parser version the seed records. It names the seed build distinctly from a
/// live-sync corpus (`rust-docs-mcp/seed-2`), so the two artifacts are
/// distinguishable in a `snapshot_id` or a `parser` field.
pub const SEED_PARSER_VERSION: &str = "rust-docs-mcp/seed-2";

/// The fixed capture timestamp the seed records for every source. The seed is a
/// *representative* corpus pinned to recorded fixtures, so a single fixed
/// `fetched_at` is truthful ("captured on this date from these fixtures") and
/// keeps the emitted JSON deterministic.
pub const CAPTURED_AT: &str = "2026-10-30T00:00:00Z";

/// The recorded provenance for each upstream fixture the seed ingests: the
/// upstream `ETag` (a cheap cache identity / freshness hint) and, where the
/// fixture is commit-pinned, the upstream revision. This is the "record
/// truthful provenance" part of the fix: the seed's `Source` records carry the
/// captured `fetched_at` and the content hash of the captured bytes, and these
/// fixtures pin *what* was captured.
pub mod provenance {
    /// `api-guidelines` fixtures, captured from `rust-lang.github.io/api-guidelines/`.
    pub mod api_guidelines {
        /// The upstream `ETag` for `about.html`.
        pub const ABOUT_ETAG: &str = "\"686ca47e-7ef0\"";
        /// The upstream `ETag` for `naming.html` (the C-CONV explanation).
        pub const NAMING_ETAG: &str = "\"686ca47e-7ef0\"";
        /// The upstream `ETag` for `interoperability.html` (the C-CONV-TRAITS explanation).
        pub const INTEROPERABILITY_ETAG: &str = "\"686ca47e-7672\"";
        /// The upstream `ETag` for `checklist.html`.
        pub const CHECKLIST_ETAG: &str = "\"686ca47e-547c\"";
    }
    /// `rustfmt` fixtures, commit-pinned to the approved revision (issue #147).
    pub mod rustfmt {
        /// The commit the `README.md` and `Configurations.md` fixtures were captured from.
        pub const REVISION: &str = "702df60b87e5047276b0d4c36c989e8095555e00";
    }
    /// The upstream `ETag` for the Clippy lint catalog (`clippy.rust-lang.org/stable/`).
    pub const CLIPPY_INDEX_ETAG: &str = "\"6af8ef1c-1971f8\"";
}

// ---------------------------------------------------------------------------
// Recorded upstream fixtures (embedded at compile time)
// ---------------------------------------------------------------------------

/// The recorded upstream fixtures. Each constant is the *actual* captured page
/// from `tests/fixtures/upstream/`; the seed ingests these through the real
/// adapters so a shipped section is the extractor's output, not a hand-authored
/// summary. The fixtures are pinned by [`provenance`]; a live re-fetch rewrites
/// them through the opt-in seam.
mod fixtures {
    // api-guidelines
    pub const ABOUT: &str = include_str!("../../tests/fixtures/upstream/about.html");
    pub const NAMING: &str = include_str!("../../tests/fixtures/upstream/naming.html");
    pub const INTEROPERABILITY: &str =
        include_str!("../../tests/fixtures/upstream/interoperability.html");
    pub const CHECKLIST: &str = include_str!("../../tests/fixtures/upstream/checklist.html");
    // style-guide
    pub const EXPRESSIONS: &str = include_str!("../../tests/fixtures/upstream/expressions.html");
    // rustfmt (commit-pinned Markdown)
    pub const RUSTFMT_README: &str = include_str!("../../tests/fixtures/upstream/README.md");
    pub const RUSTFMT_CONFIGURATIONS: &str =
        include_str!("../../tests/fixtures/upstream/Configurations.md");
    // clippy per-lint catalog (a curated, faithful subset of the full catalog)
    pub const CLIPPY_LINTS: &str = include_str!("../../tests/fixtures/upstream/clippy_lints.html");
    // the four books — the routine command families, each the *actual* captured
    // command page from upstream, ingested through `ingest_command_page` so the
    // section is the extractor's output and the command key is stamped from the
    // URL path (issue #149 AC3 / `plan.md` §6: a command resolves from a faithful
    // upstream page, not a hand-authored summary)
    pub const CARGO_BUILD: &str = include_str!("../../tests/fixtures/upstream/cargo-build.html");
    pub const CARGO_TEST: &str = include_str!("../../tests/fixtures/upstream/cargo-test.html");
    pub const CARGO_CHECK: &str = include_str!("../../tests/fixtures/upstream/cargo-check.html");
    pub const CARGO_FMT: &str = include_str!("../../tests/fixtures/upstream/cargo-fmt.html");
    pub const CARGO_CLIPPY: &str = include_str!("../../tests/fixtures/upstream/cargo-clippy.html");
    pub const CARGO_DOC: &str = include_str!("../../tests/fixtures/upstream/cargo-doc.html");
    pub const CARGO_ADD: &str = include_str!("../../tests/fixtures/upstream/cargo-add.html");
    pub const CARGO_NEW: &str = include_str!("../../tests/fixtures/upstream/cargo-new.html");
    pub const RUSTC_CLI: &str =
        include_str!("../../tests/fixtures/upstream/rustc-command-line-arguments.html");
    pub const RUSTUP_INSTALL: &str =
        include_str!("../../tests/fixtures/upstream/rustup-installation.html");
    pub const RUSTUP_CONCEPTS: &str =
        include_str!("../../tests/fixtures/upstream/rustup-concepts.html");
    pub const RUSTDOC_CLI: &str =
        include_str!("../../tests/fixtures/upstream/rustdoc-command-line-arguments.html");
}

// The canonical fetch URL each recorded fixture stands in for. The seed's
// `Source::document_urls` and each section's `canonical_url` are built from these
// so the seed cites the real upstream location, not a fixture path.
const AG_BASE: &str = "https://rust-lang.github.io/api-guidelines/";
const STYLE_BASE: &str = "https://doc.rust-lang.org/style-guide/";
const CARGO_BASE: &str = "https://doc.rust-lang.org/cargo/";
const RUSTC_BASE: &str = "https://doc.rust-lang.org/rustc/";
const RUSTUP_BASE: &str = "https://rust-lang.github.io/rustup/";

/// The Cargo command-manual pages the seed ingests: each is the *actual* captured
/// `commands/cargo-<subcommand>.html` page, ingested through
/// `ingest_command_page`, which stamps the page's canonical command key
/// (`cargo build`, `cargo test`, …) from its URL path. This is the same
/// mechanism that makes `cargo build` resolvable, extended to the routine
/// families so each resolves on a fresh offline launch (issue #149 AC3 /
/// `plan.md` §6) rather than from a hand-authored summary. The page that
/// *is* the `cargo-<subcommand>.html` manual is the command page; the rest of a
/// fetched book is ingested through `ingest_cargo`.
const CARGO_COMMAND_PAGES: &[(&str, &str)] = &[
    ("cargo-build.html", fixtures::CARGO_BUILD),
    ("cargo-test.html", fixtures::CARGO_TEST),
    ("cargo-check.html", fixtures::CARGO_CHECK),
    ("cargo-fmt.html", fixtures::CARGO_FMT),
    ("cargo-clippy.html", fixtures::CARGO_CLIPPY),
    ("cargo-doc.html", fixtures::CARGO_DOC),
    ("cargo-add.html", fixtures::CARGO_ADD),
    ("cargo-new.html", fixtures::CARGO_NEW),
];
const RUSTDOC_BASE: &str = "https://doc.rust-lang.org/rustdoc/";
const CLIPPY_BASE: &str = "https://clippy.rust-lang.org/stable/";

/// The commit-pinned raw URLs the rustfmt fixtures stand in for.
fn rustfmt_document_url(file: &str) -> RqUrl {
    RqUrl::parse(&format!(
        "https://raw.githubusercontent.com/rust-lang/rustfmt/{revision}/{file}",
        revision = provenance::rustfmt::REVISION,
    ))
    .expect("the approved rustfmt raw URL is well-formed")
}

// ---------------------------------------------------------------------------
// The generated corpus
// ---------------------------------------------------------------------------

/// The faithful seed corpus, generated by running the validated ingestion
/// adapters over the recorded upstream fixtures. `crate::bundled`'s `manifest()`
/// consumes this to assemble the offline `SnapshotManifestBody`; the live sync
/// pipeline produces an analogous corpus from a live fetch instead.
pub struct SeedCorpus {
    /// The eight source records, one per approved collection.
    pub sources: Vec<Source>,
    /// The ingested sections, one per fixture, with faithful bodies and
    /// deterministic content hashes.
    pub sections: Vec<Section>,
    /// The parsed checklist, kept to the codes whose explanations are shipped.
    pub checklist: Vec<ChecklistEntry>,
}

/// A failure to generate the seed corpus. Every variant names the fixture or
/// adapter that failed, so a broken fixture surfaces as an actionable diagnostic
/// rather than a silent omission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedError {
    /// A recorded fixture's URL was not well-formed.
    BadUrl {
        /// The source id the fixture belongs to.
        source: String,
        /// The URL that failed to parse.
        url: String,
    },
    /// A recorded fixture ingested to no usable content (an empty article), or a
    /// section's identity could not be derived.
    Ingest {
        /// The source id the fixture belongs to.
        source: String,
        /// The extractor's diagnostic.
        detail: String,
    },
    /// The checklist parsed to zero entries — a fixture that is not the real
    /// checklist, so the seed cannot publish a checklist.
    EmptyChecklist,
    /// A checklist code resolved to more than one distinct explanatory section.
    AmbiguousExplanations {
        /// The code that resolved ambiguously.
        code: String,
        /// The distinct section ids it resolved to (sorted).
        sections: Vec<String>,
    },
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeedError::BadUrl { source, url } => {
                write!(
                    f,
                    "seed fixture URL for `{source}` is not well-formed: {url}"
                )
            }
            SeedError::Ingest { source, detail } => {
                write!(
                    f,
                    "ingesting the recorded fixture for `{source}` failed: {detail}"
                )
            }
            SeedError::EmptyChecklist => write!(
                f,
                "the recorded checklist fixture parsed to zero guideline entries"
            ),
            SeedError::AmbiguousExplanations { code, sections } => write!(
                f,
                "checklist code `{code}` resolved to more than one section: {sections:?}"
            ),
        }
    }
}
impl std::error::Error for SeedError {}

// ---------------------------------------------------------------------------
// Fixture → `Fetched` helpers
// ---------------------------------------------------------------------------

/// Build the [`Fetched`] a recorded fixture stands in for: the captured bytes,
/// the canonical upstream URL, and a `200 OK` status. The `Fetched` the live
/// pipeline would produce for the same upstream URL is structurally identical;
/// the only difference is the bytes come from disk rather than a live response.
fn fetched(url: &str, body: &str, etag: Option<&str>) -> Result<Fetched, SeedError> {
    let parsed = RqUrl::parse(url).map_err(|_| SeedError::BadUrl {
        source: url.to_string(),
        url: url.to_string(),
    })?;
    Ok(Fetched {
        requested: parsed.clone(),
        final_url: parsed,
        status: reqwest::StatusCode::OK,
        body: body.as_bytes().to_vec(),
        content_type: Some("text/html; charset=utf-8".to_string()),
        etag: etag.map(str::to_string),
        last_modified: None,
        content_length: None,
    })
}

/// Ingest one recorded HTML/Markdown fixture through a pipeline adapter,
/// returning its sections on success and a [`SeedError::Ingest`] naming the
/// source on failure. The adapter's error type is generic: each pipeline adapter
/// owns its own `IngestError`, so the helper is parameterized over any `Display`
/// error rather than one adapter's error type.
fn ingest_html<E, F>(
    source_id: &str,
    url: &str,
    body: &str,
    etag: Option<&str>,
    adapter: F,
) -> Result<Vec<Section>, SeedError>
where
    E: std::fmt::Display,
    F: Fn(&Fetched) -> Result<Vec<Section>, E>,
{
    let fetched = fetched(url, body, etag)?;
    adapter(&fetched).map_err(|e| SeedError::Ingest {
        source: source_id.to_string(),
        detail: e.to_string(),
    })
}

/// Stamp a faithful command key onto every section in `sections` as a *separate
/// retrieval annotation*, then recompute each section's content hash so the key
/// becomes part of the section's identity — mirroring `ingest_command_page`'s
/// stamping for the cargo command manual. The section **body is left
/// untouched**: it stays the faithful upstream content, and the key is only the
/// retrieval index `CommandCatalog::from_sections` keys on — a maintainer
/// retrieval hint kept separate from the upstream prose, per the "keep source
/// text separate from server-authored retrieval hints" boundary (`plan.md` §2).
///
/// This is the extension the task #149 fix uses for the families whose usage
/// page is *not* a `commands/cargo-<subcommand>.html` manual — `rustup show`
/// (the rustup toolchain concepts), `rustc` (its command-line-arguments man
/// page), and `rustfmt` (the README/Configurations reference) — so each resolves
/// from a faithful upstream section rather than a hand-authored summary.
///
/// A key the [`CommandKey`] boundary would refuse (a shell operator / padding) is
/// a programming error here: the keys are the fixed literals this module names,
/// so a malformed one is a [`SeedError`] — never a silent drop, and never a
/// fabricated section.
fn stamp_command_key(sections: &mut [Section], key: &str) -> Result<(), SeedError> {
    let parsed = CommandKey::new(key).map_err(|err| SeedError::Ingest {
        source: "command-key".to_string(),
        detail: format!("`{key}` is not a valid command key: {err}"),
    })?;
    for section in sections {
        if !section
            .command_keys
            .iter()
            .any(|existing| existing.as_str() == parsed.as_str())
        {
            section.command_keys.push(parsed.as_str().to_string());
            section.recompute_content_hash();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Seed corpus generation
// ---------------------------------------------------------------------------

/// Build the faithful seed corpus from the recorded upstream fixtures.
///
/// Every source is ingested through the real pipeline adapter; the checklist is
/// parsed and linked through the real `parse_checklist` + `link_explanations`
/// stages and kept to the codes whose explanations are shipped. A generation
/// failure (a broken fixture, an empty article, an ambiguous mapping) is a
/// [`SeedError`] — never a silent omission.
pub fn build_seed_corpus() -> Result<SeedCorpus, SeedError> {
    // -- api-guidelines: the three pages that carry C-CONV / C-CONV-TRAITS and
    //    the source introduction. --
    let mut ag_sections: Vec<Section> = Vec::new();
    ag_sections.extend(ingest_html(
        "api-guidelines",
        &format!("{AG_BASE}about.html"),
        fixtures::ABOUT,
        Some(provenance::api_guidelines::ABOUT_ETAG),
        ingest_api_chapter,
    )?);
    ag_sections.extend(ingest_html(
        "api-guidelines",
        &format!("{AG_BASE}naming.html"),
        fixtures::NAMING,
        Some(provenance::api_guidelines::NAMING_ETAG),
        ingest_api_chapter,
    )?);
    ag_sections.extend(ingest_html(
        "api-guidelines",
        &format!("{AG_BASE}interoperability.html"),
        fixtures::INTEROPERABILITY,
        Some(provenance::api_guidelines::INTEROPERABILITY_ETAG),
        ingest_api_chapter,
    )?);

    // -- style-guide: the expressions page carries the trailing-comma guidance. --
    let style_sections = ingest_html(
        "style-guide",
        &format!("{STYLE_BASE}expressions.html"),
        fixtures::EXPRESSIONS,
        None,
        ingest_style,
    )?;

    // -- cargo / rustc / rustup / rustdoc: the routine command families, each
    //    from its *actual* captured upstream command page. --
    //
    // cargo ingests every recorded command-manual page. Each page is the real
    // `commands/cargo-<subcommand>.html` manual, ingested through
    // `ingest_command_page`, which stamps the page's canonical command key
    // (`cargo build`, `cargo test`, `cargo check`, `cargo fmt`, `cargo clippy`,
    // `cargo doc`, `cargo add`, `cargo new`) from its URL path — the exact
    // mechanism that makes `cargo build` resolvable, so every one of these
    // resolves on a fresh offline launch (issue #149 AC3 / `plan.md` §6) with
    // the faithful upstream body intact. A guide chapter (a fetched book that is
    // not a command page) would use `ingest_chapter` instead; the recorded
    // fixtures are all command pages, so the `else` branch only guards the
    // live-pipeline contract.
    let mut cargo_sections: Vec<Section> = Vec::new();
    for (file, body) in CARGO_COMMAND_PAGES {
        cargo_sections.extend(ingest_html(
            "cargo",
            &format!("{CARGO_BASE}commands/{file}"),
            body,
            None,
            // Mirror the live pipeline (`crate::ingest::pipeline`): a
            // command-manual page is ingested via `ingest_command_page` so its
            // section carries the command key `CommandCatalog::from_sections`
            // keys on; a guide chapter uses `ingest_chapter`.
            |fetched: &Fetched| {
                if crate::ingest::cargo::is_command_page(&fetched.final_url) {
                    crate::ingest::cargo::ingest_command_page(fetched)
                } else {
                    ingest_cargo(fetched)
                }
            },
        )?);
    }
    // `rustc`: the `rustc-command-line-arguments.html` page *is* the `rustc`
    // usage page (it is the `rustc` man page, rendered by the rustc book), so
    // its ingested section is the `rustc` command's upstream body. The key is
    // stamped as a separate retrieval annotation; the faithful body is intact.
    let mut rustc_sections = ingest_html(
        "rustc",
        &format!("{RUSTC_BASE}command-line-arguments.html"),
        fixtures::RUSTC_CLI,
        None,
        ingest_rustc,
    )?;
    stamp_command_key(&mut rustc_sections, "rustc")?;

    // `rustup`: the `rustup show` guidance is the toolchain model the `rustup`
    // concepts page documents (how rustup works, terminology, active/installed
    // toolchains — exactly what `rustup show` reports). `rustup-installation`
    // covers the install flow; the concepts page covers the toolchain state
    // `rustup show` surfaces. The key is stamped on the concepts sections as a
    // separate retrieval annotation; the faithful body is intact.
    let mut rustup_sections = ingest_html(
        "rustup",
        &format!("{RUSTUP_BASE}installation/index.html"),
        fixtures::RUSTUP_INSTALL,
        None,
        ingest_rustup,
    )?;
    let mut rustup_concepts = ingest_html(
        "rustup",
        &format!("{RUSTUP_BASE}concepts/index.html"),
        fixtures::RUSTUP_CONCEPTS,
        None,
        ingest_rustup,
    )?;
    stamp_command_key(&mut rustup_concepts, "rustup show")?;
    rustup_sections.extend(rustup_concepts);

    let rustdoc_sections = ingest_html(
        "rustdoc",
        &format!("{RUSTDOC_BASE}command-line-arguments.html"),
        fixtures::RUSTDOC_CLI,
        None,
        ingest_rustdoc,
    )?;

    // -- rustfmt: the two approved, commit-pinned Markdown sources. The `rustfmt`
    //    key is stamped on both sections (README.md and Configurations.md are the
    //    `rustfmt` usage + reference `rustfmt_usage` cites) as a separate
    //    retrieval annotation; the faithful Markdown is intact. --
    let mut rustfmt_sections = rustfmt_sections()?;
    stamp_command_key(&mut rustfmt_sections, "rustfmt")?;

    // -- clippy: the per-lint catalog, a curated faithful subset. --
    let clippy_sections = clippy_sections()?;

    // Collect the api-guidelines section ids so the checklist can resolve to
    // only the codes whose explanations are actually shipped.
    let ag_section_ids: BTreeSet<String> = ag_sections
        .iter()
        .map(|s| s.id.as_str().to_string())
        .collect();
    let ag_codes: BTreeSet<String> = ag_sections
        .iter()
        .flat_map(|s| s.guideline_codes.iter().cloned())
        .collect();

    // Assemble the checklist from the recorded `checklist.html`: parse every
    // entry, keep only the codes whose explanations are shipped (so each resolves
    // to a real section), then link + validate.
    let checklist = build_checklist(&ag_sections)?;

    // Order the sections source-by-source, registry order, for a stable corpus.
    let mut sections: Vec<Section> = Vec::new();
    sections.extend(ag_sections.clone());
    sections.extend(style_sections);
    sections.extend(cargo_sections);
    sections.extend(rustc_sections);
    sections.extend(rustup_sections);
    sections.extend(rustdoc_sections);
    sections.extend(rustfmt_sections);
    sections.extend(clippy_sections);

    // The source records.
    let sources = build_sources(&sections);

    // `ag_section_ids` guards the checklist resolution; it is also a regression
    // witness that the api-guidelines pages really ingested to sections.
    assert!(
        !ag_section_ids.is_empty(),
        "the api-guidelines fixtures must ingest to at least one section"
    );
    let _ = ag_codes; // carried for the checklist; a future expansion may key on it

    Ok(SeedCorpus {
        sources,
        sections,
        checklist,
    })
}

/// Ingest the two approved rustfmt Markdown sources. The rendered
/// `rust-lang.github.io/rustfmt/` site is JS-only and is never fetched; the seed
/// uses the commit-pinned `README.md` and `Configurations.md` Markdown.
fn rustfmt_sections() -> Result<Vec<Section>, SeedError> {
    let mut out = Vec::new();
    for (file, body) in [
        ("README.md", fixtures::RUSTFMT_README),
        ("Configurations.md", fixtures::RUSTFMT_CONFIGURATIONS),
    ] {
        let url = rustfmt_document_url(file).to_string();
        let fetched = fetched(&url, body, None)?;
        out.extend(ingest_markdown(&fetched).map_err(|e| SeedError::Ingest {
            source: "rustfmt".to_string(),
            detail: e.to_string(),
        })?);
    }
    Ok(out)
}

/// Ingest the curated Clippy lint-catalog fixture into per-lint sections. The
/// fixture is a faithful subset of `clippy.rust-lang.org/stable/`'s catalog
/// (the same `<article id="lint_name">` structure), so the extractor produces the
/// same per-lint sections it would for those lints in the full catalog.
fn clippy_sections() -> Result<Vec<Section>, SeedError> {
    let url = format!("{CLIPPY_BASE}index.html");
    let fetched = fetched(
        &url,
        fixtures::CLIPPY_LINTS,
        Some(provenance::CLIPPY_INDEX_ETAG),
    )?;
    ingest_lint_catalog(&fetched).map_err(|e| SeedError::Ingest {
        source: "clippy".to_string(),
        detail: e.to_string(),
    })
}

/// Parse the recorded checklist and keep only the entries whose explanatory
/// sections are shipped, then resolve and validate the mapping.
///
/// The full `checklist.html` lists every guideline code (54+). The seed ships a
/// representative subset of api-guidelines sections, so only the codes whose
/// explanations are among those sections can resolve. Keeping to those codes
/// makes every shipped entry resolve to a real section and keeps the checklist a
/// *faithful subset* rather than a fabricated or empty one.
fn build_checklist(api_guidelines: &[Section]) -> Result<Vec<ChecklistEntry>, SeedError> {
    let checklist_fetched = fetched(
        &format!("{AG_BASE}checklist.html"),
        fixtures::CHECKLIST,
        Some(provenance::api_guidelines::CHECKLIST_ETAG),
    )
    .map_err(|e| SeedError::Ingest {
        source: "api-guidelines".to_string(),
        detail: e.to_string(),
    })?;
    let entries = ingest_checklist(&checklist_fetched);
    if entries.is_empty() {
        return Err(SeedError::EmptyChecklist);
    }

    // A code's *primary* explanation is the section whose **own** heading carries
    // the code. A sub-section that merely *inherits* the code from an ancestor
    // heading — for example an "Examples from the standard library" sub-heading
    // nested under a coded guideline, which [`with_guideline_code`] tags with the
    // ancestor's code on purpose (a scoping code is never swallowed) — is *not*
    // a distinct explanation, so it is excluded from the resolution feed. Feeding
    // only the primary sections keeps each code resolving to exactly one section;
    // otherwise the real [`link_explanations`] ambiguity check would flag the
    // inherited sub-sections as a second, distinct explanation of the same code.
    let primary: Vec<Section> = api_guidelines
        .iter()
        .filter(|s| {
            s.heading_path
                .last()
                .is_some_and(|h| super::api_guidelines::guideline_code_in(h).is_some())
        })
        .cloned()
        .collect();

    // The codes the primary sections actually explain.
    let shipped_codes: BTreeSet<String> = primary
        .iter()
        .flat_map(|s| s.guideline_codes.iter().cloned())
        .collect();
    let mut kept: Vec<ChecklistEntry> = entries
        .into_iter()
        .filter(|e| shipped_codes.contains(&e.code))
        .collect();

    // Resolve the kept entries' explanation sections against the primary sections
    // and validate the mapping is neither missing nor ambiguous (the same
    // `link_explanations` the live pipeline runs).
    link_explanations(&primary, &mut kept).map_err(|e| match e {
        super::api_guidelines::IngestError::AmbiguousExplanations { code, sections } => {
            SeedError::AmbiguousExplanations {
                code: code.clone(),
                sections,
            }
        }
        other => SeedError::Ingest {
            source: "api-guidelines".to_string(),
            detail: format!("checklist mapping failed: {other}"),
        },
    })?;

    // A faithful, nonempty checklist.
    if kept.is_empty() {
        return Err(SeedError::EmptyChecklist);
    }
    Ok(kept)
}

/// Build the eight source records, one per approved collection, in registry
/// order. Each record's `content_hash` is the FNV-1a hash of the bytes actually
/// captured for that source (a truthful captured-content identity), and its
/// `document_urls` are the canonical upstream URLs the fixtures stand in for.
fn build_sources(sections: &[Section]) -> Vec<Source> {
    // Group the captured bytes by source so each record's hash is over that
    // source's actual content.
    let mut by_source: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for s in sections {
        by_source
            .entry(s.source_id.as_str())
            .or_default()
            .insert(s.body.clone());
    }

    let captured = |id: &str| -> Vec<u8> {
        let bodies = by_source
            .get(id)
            .map(|set| set.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut bytes = Vec::new();
        for b in bodies {
            bytes.extend_from_slice(b.as_bytes());
            bytes.push(0);
        }
        bytes
    };

    let mut sources: Vec<Source> = Vec::new();
    for (id, title, entry_url, docs, release) in source_specs() {
        // Compute the captured-content hash before moving `id`.
        let content_hash = CorpusHash::compute(captured(&id));
        let source_id = SourceId::new(id).expect("a registry source id is URI-safe");
        let entry = Url::new(entry_url).expect("a registry entry URL is well-formed");
        let document_urls = docs
            .iter()
            .map(|u| Url::new(u.as_str()).expect("a registry document URL is well-formed"))
            .collect::<Vec<_>>();
        sources.push(Source {
            id: source_id,
            title,
            entry_url: entry,
            document_urls,
            last_checked: None,
            fetched_at: Timestamp::new(CAPTURED_AT).expect("the capture timestamp is valid"),
            upstream_modified: None,
            etag: None,
            content_hash,
            release,
            parser: SEED_PARSER_VERSION.to_string(),
        });
    }
    sources
}

/// The eight source specs: id, title, entry URL, the canonical document URLs the
/// fixtures stand in for, and the release record. Mirrors the registry's
/// per-collection identity so the seed's source records match what a live sync
/// would publish, with truthful captured-content hashes.
fn source_specs() -> Vec<(String, String, String, Vec<String>, Release)> {
    use Release as R;
    vec![
        (
            "api-guidelines".to_string(),
            "Rust API Guidelines".to_string(),
            format!("{AG_BASE}about.html"),
            vec![
                format!("{AG_BASE}about.html"),
                format!("{AG_BASE}naming.html"),
                format!("{AG_BASE}interoperability.html"),
                format!("{AG_BASE}checklist.html"),
            ],
            R::channel_only(crate::model::Channel::Rolling),
        ),
        (
            "style-guide".to_string(),
            "Rust Style Guide".to_string(),
            format!("{STYLE_BASE}index.html"),
            vec![format!("{STYLE_BASE}expressions.html")],
            R::channel_only(crate::model::Channel::Stable),
        ),
        (
            "cargo".to_string(),
            "The Cargo Book".to_string(),
            "https://doc.rust-lang.org/cargo/".to_string(),
            // Every recorded command-manual page — the routine families ingested
            // through `ingest_command_page` — is a documented URL for the source,
            // derived from the same table the corpus ingests so the two never
            // drift.
            CARGO_COMMAND_PAGES
                .iter()
                .map(|(file, _)| format!("{CARGO_BASE}commands/{file}"))
                .collect(),
            R::channel_only(crate::model::Channel::Stable),
        ),
        (
            "rustc".to_string(),
            "The rustc Book".to_string(),
            "https://doc.rust-lang.org/rustc/".to_string(),
            vec![format!("{RUSTC_BASE}command-line-arguments.html")],
            R::channel_only(crate::model::Channel::Stable),
        ),
        (
            "rustup".to_string(),
            "The rustup Book".to_string(),
            "https://rust-lang.github.io/rustup/".to_string(),
            vec![
                format!("{RUSTUP_BASE}installation/index.html"),
                // `rustup show`'s toolchain model — the concepts page `rustup`
                // ingests for the `rustup show` command key.
                format!("{RUSTUP_BASE}concepts/index.html"),
            ],
            R::channel_only(crate::model::Channel::Rolling),
        ),
        (
            "rustfmt".to_string(),
            "rustfmt reference".to_string(),
            "https://github.com/rust-lang/rustfmt#readme".to_string(),
            vec![
                rustfmt_document_url("README.md").to_string(),
                rustfmt_document_url("Configurations.md").to_string(),
            ],
            R {
                channel: crate::model::Channel::Stable,
                documentation_release: None,
                revision: Some(provenance::rustfmt::REVISION.to_string()),
            },
        ),
        (
            "clippy".to_string(),
            "Clippy".to_string(),
            "https://doc.rust-lang.org/clippy/".to_string(),
            vec![format!("{CLIPPY_BASE}index.html")],
            R::channel_only(crate::model::Channel::Stable),
        ),
        (
            "rustdoc".to_string(),
            "The Rustdoc Book".to_string(),
            "https://doc.rust-lang.org/rustdoc/".to_string(),
            vec![format!("{RUSTDOC_BASE}command-line-arguments.html")],
            R::channel_only(crate::model::Channel::Stable),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Offline acceptance tests — issue #150
//
// The default, hermetic tests run the seed builder over the embedded recorded
// fixtures and assert the two guidelines C-CONV / C-CONV-TRAITS resolve to their
// distinct, correct explanations, plus the required retrieval content. A live
// upstream re-fetch is opt-in (see `crate::bundled`'s `emit_bundle_json` seam).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_conv_and_c_conv_traits_resolve_to_distinct_correct_explanations() {
        let corpus =
            build_seed_corpus().expect("the seed corpus builds from the recorded fixtures");

        // C-CONV is the as_/to_/into_ naming rule, on the naming page.
        let c_conv = corpus
            .checklist
            .iter()
            .find(|e| e.code == "C-CONV")
            .expect("C-CONV is a shipped checklist entry");
        assert_eq!(
            c_conv.category, "Naming",
            "C-CONV belongs to the Naming category, not a hand-authored one"
        );
        assert!(
            c_conv
                .explanation_sections
                .iter()
                .any(|s| s.as_str().starts_with("api-guidelines/naming")),
            "C-CONV must explain on api-guidelines/naming, got {:?}",
            c_conv.explanation_sections
        );

        // C-CONV-TRAITS is the standard-conversion-traits rule, on the
        // interoperability page — a *distinct* guideline.
        let c_conv_traits = corpus
            .checklist
            .iter()
            .find(|e| e.code == "C-CONV-TRAITS")
            .expect("C-CONV-TRAITS is a shipped checklist entry");
        assert_eq!(
            c_conv_traits.category, "Interoperability",
            "C-CONV-TRAITS belongs to the Interoperability category"
        );
        assert!(
            c_conv_traits
                .explanation_sections
                .iter()
                .any(|s| s.as_str().starts_with("api-guidelines/interoperability")),
            "C-CONV-TRAITS must explain on api-guidelines/interoperability, got {:?}",
            c_conv_traits.explanation_sections
        );

        // The two guidelines resolve to *distinct* sections.
        assert_ne!(
            c_conv.explanation_sections, c_conv_traits.explanation_sections,
            "C-CONV and C-CONV-TRAITS must resolve to distinct explanations"
        );
    }

    #[test]
    fn required_retrieval_content_is_present() {
        let corpus = build_seed_corpus().expect("the seed corpus builds");
        let ids: BTreeSet<String> = corpus
            .sections
            .iter()
            .map(|s| s.id.as_str().to_string())
            .collect();
        let text: String = corpus
            .sections
            .iter()
            .map(|s| s.body.clone())
            .collect::<Vec<_>>()
            .join("\n");

        // Lint detail sections carry their lint ids.
        assert!(
            corpus
                .sections
                .iter()
                .any(|s| s.lint_ids.iter().any(|l| l == "clippy::unwrap_used")),
            "a clippy::unwrap_used lint-detail section must ship"
        );
        assert!(
            corpus
                .sections
                .iter()
                .any(|s| s.lint_ids.iter().any(|l| l == "clippy::clone_on_copy")),
            "a clippy::clone_on_copy lint-detail section must ship"
        );

        // Formatter check-mode content ships in the rustfmt sections.
        assert!(
            text.to_lowercase().contains("--check"),
            "rustfmt check-mode guidance must ship"
        );

        // Trailing-comma guidance ships in the style-guide sections.
        assert!(
            text.to_lowercase().contains("trailing comma"),
            "style-guide trailing-comma guidance must ship"
        );

        // The C-CONV explanation is the as_/to_/into_ rule (not the About page).
        assert!(
            text.contains("as_") && text.contains("into_"),
            "the C-CONV as_/to_/into_ naming rule must ship"
        );

        // A representative section per source is present.
        for source in [
            "api-guidelines",
            "style-guide",
            "cargo",
            "rustc",
            "rustup",
            "rustfmt",
            "clippy",
            "rustdoc",
        ] {
            assert!(
                corpus
                    .sections
                    .iter()
                    .any(|s| s.source_id.as_str() == source),
                "the seed must ship at least one section for `{source}` (ids: {ids:?})"
            );
        }
    }

    #[test]
    fn every_shipped_checklist_entry_resolves_to_a_shipped_section() {
        let corpus = build_seed_corpus().expect("the seed corpus builds");
        let section_ids: BTreeSet<String> = corpus
            .sections
            .iter()
            .map(|s| s.id.as_str().to_string())
            .collect();
        for entry in &corpus.checklist {
            assert!(
                !entry.explanation_sections.is_empty(),
                "checklist entry {0} resolved to no section",
                entry.code
            );
            for s in &entry.explanation_sections {
                assert!(
                    section_ids.contains(s.as_str()),
                    "checklist entry {} points at a section that is not shipped: {}",
                    entry.code,
                    s
                );
            }
        }
    }
}
