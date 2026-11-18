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
    ingest_chapter as ingest_api_chapter, ingest_checklist, link_explanations, primary_explanations,
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
    // api-guidelines — every topical chapter the checklist's 54 codes resolve
    // against (issue #170 AC2: the full upstream checklist is shipped, no code
    // filtered out). The three original pages plus the eight topical chapters
    // that carry the remaining codes.
    pub const ABOUT: &str = include_str!("../../tests/fixtures/upstream/about.html");
    pub const NAMING: &str = include_str!("../../tests/fixtures/upstream/naming.html");
    pub const INTEROPERABILITY: &str =
        include_str!("../../tests/fixtures/upstream/interoperability.html");
    pub const DEBUGGABILITY: &str =
        include_str!("../../tests/fixtures/upstream/debuggability.html");
    pub const DEPENDABILITY: &str =
        include_str!("../../tests/fixtures/upstream/dependability.html");
    pub const DOCUMENTATION: &str =
        include_str!("../../tests/fixtures/upstream/documentation.html");
    pub const FLEXIBILITY: &str = include_str!("../../tests/fixtures/upstream/flexibility.html");
    pub const FUTURE_PROOFING: &str =
        include_str!("../../tests/fixtures/upstream/future-proofing.html");
    pub const MACROS: &str = include_str!("../../tests/fixtures/upstream/macros.html");
    pub const NECESSITIES: &str = include_str!("../../tests/fixtures/upstream/necessities.html");
    pub const PREDICTABILITY: &str =
        include_str!("../../tests/fixtures/upstream/predictability.html");
    pub const TYPE_SAFETY: &str = include_str!("../../tests/fixtures/upstream/type-safety.html");
    pub const CHECKLIST: &str = include_str!("../../tests/fixtures/upstream/checklist.html");
    // style-guide — the design/style chapters the seed ships so required design
    // and style topics are covered (issue #170 AC3), not a single page.
    pub const EXPRESSIONS: &str = include_str!("../../tests/fixtures/upstream/expressions.html");
    pub const STYLE_ADVICE: &str = include_str!("../../tests/fixtures/upstream/style-advice.html");
    pub const STYLE_ITEMS: &str = include_str!("../../tests/fixtures/upstream/style-items.html");
    pub const STYLE_PRINCIPLES: &str =
        include_str!("../../tests/fixtures/upstream/style-principles.html");
    pub const STYLE_STATEMENTS: &str =
        include_str!("../../tests/fixtures/upstream/style-statements.html");
    pub const STYLE_TYPES: &str = include_str!("../../tests/fixtures/upstream/style-types.html");
    pub const STYLE_CARGO: &str = include_str!("../../tests/fixtures/upstream/style-cargo.html");
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
    // The remaining `plan.md` §6 cargo families (issue #170 AC1): the
    // setup/inspection, dependency/lockfile, verification, and
    // packaging/release families, each the real `commands/cargo-<sub>.html`
    // page.
    pub const CARGO_INIT: &str = include_str!("../../tests/fixtures/upstream/cargo-init.html");
    pub const CARGO_METADATA: &str =
        include_str!("../../tests/fixtures/upstream/cargo-metadata.html");
    pub const CARGO_LOCATE_PROJECT: &str =
        include_str!("../../tests/fixtures/upstream/cargo-locate-project.html");
    pub const CARGO_TREE: &str = include_str!("../../tests/fixtures/upstream/cargo-tree.html");
    pub const CARGO_REMOVE: &str = include_str!("../../tests/fixtures/upstream/cargo-remove.html");
    pub const CARGO_UPDATE: &str = include_str!("../../tests/fixtures/upstream/cargo-update.html");
    pub const CARGO_GENERATE_LOCKFILE: &str =
        include_str!("../../tests/fixtures/upstream/cargo-generate-lockfile.html");
    pub const CARGO_FETCH: &str = include_str!("../../tests/fixtures/upstream/cargo-fetch.html");
    pub const CARGO_RUN: &str = include_str!("../../tests/fixtures/upstream/cargo-run.html");
    pub const CARGO_BENCH: &str = include_str!("../../tests/fixtures/upstream/cargo-bench.html");
    pub const CARGO_FIX: &str = include_str!("../../tests/fixtures/upstream/cargo-fix.html");
    pub const CARGO_PACKAGE: &str =
        include_str!("../../tests/fixtures/upstream/cargo-package.html");
    pub const CARGO_PUBLISH: &str =
        include_str!("../../tests/fixtures/upstream/cargo-publish.html");
    pub const RUSTC_CLI: &str =
        include_str!("../../tests/fixtures/upstream/rustc-command-line-arguments.html");
    pub const RUSTUP_INSTALL: &str =
        include_str!("../../tests/fixtures/upstream/rustup-installation.html");
    pub const RUSTUP_CONCEPTS: &str =
        include_str!("../../tests/fixtures/upstream/rustup-concepts.html");
    // The rustup toolchain-management chapters the `rustup toolchain / override /
    // component / target` families cite (issue #170 AC1).
    pub const RUSTUP_TOOLCHAINS: &str =
        include_str!("../../tests/fixtures/upstream/rustup-toolchains.html");
    pub const RUSTUP_COMPONENTS: &str =
        include_str!("../../tests/fixtures/upstream/rustup-components.html");
    pub const RUSTUP_OVERRIDES: &str =
        include_str!("../../tests/fixtures/upstream/rustup-overrides.html");
    pub const RUSTUP_CROSS_COMPILATION: &str =
        include_str!("../../tests/fixtures/upstream/rustup-cross-compilation.html");
    pub const RUSTUP_PROFILES: &str =
        include_str!("../../tests/fixtures/upstream/rustup-profiles.html");
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
    // setup + inspection families
    ("cargo-new.html", fixtures::CARGO_NEW),
    ("cargo-init.html", fixtures::CARGO_INIT),
    ("cargo-metadata.html", fixtures::CARGO_METADATA),
    ("cargo-locate-project.html", fixtures::CARGO_LOCATE_PROJECT),
    ("cargo-tree.html", fixtures::CARGO_TREE),
    // dependency + lockfile families
    ("cargo-add.html", fixtures::CARGO_ADD),
    ("cargo-remove.html", fixtures::CARGO_REMOVE),
    ("cargo-update.html", fixtures::CARGO_UPDATE),
    (
        "cargo-generate-lockfile.html",
        fixtures::CARGO_GENERATE_LOCKFILE,
    ),
    ("cargo-fetch.html", fixtures::CARGO_FETCH),
    // build + verification families
    ("cargo-check.html", fixtures::CARGO_CHECK),
    ("cargo-build.html", fixtures::CARGO_BUILD),
    ("cargo-run.html", fixtures::CARGO_RUN),
    ("cargo-test.html", fixtures::CARGO_TEST),
    ("cargo-bench.html", fixtures::CARGO_BENCH),
    ("cargo-fix.html", fixtures::CARGO_FIX),
    // packaging + release families
    ("cargo-package.html", fixtures::CARGO_PACKAGE),
    ("cargo-publish.html", fixtures::CARGO_PUBLISH),
    // the remaining tool families (kept for the faithful corpus)
    ("cargo-fmt.html", fixtures::CARGO_FMT),
    ("cargo-clippy.html", fixtures::CARGO_CLIPPY),
    ("cargo-doc.html", fixtures::CARGO_DOC),
];

/// The rustup Book chapters the single-hop book discovery does not reach on its
/// own (issue #184's discovery-depth gap): the nav reaches `concepts/index.html`
/// but not its sub-pages, so the curated set is booked directly by the live sync.
/// This is the same set the seed builder ingests below (`installation`, the
/// `concepts/*` toolchain chapters, and the override / cross-compilation pages),
/// so the live corpus and the seed corpus carry the same rustup sub-pages.
const RUSTUP_CONCEPT_PAGES: &[&str] = &[
    "installation/index.html",
    "concepts/index.html",
    "concepts/toolchains.html",
    "concepts/components.html",
    "concepts/profiles.html",
    "overrides.html",
    "cross-compilation.html",
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

/// The curated catalog command-key map — the **single source of truth** shared by
/// the seed builder ([`build_seed_corpus`]) and the live sync pipeline
/// ([`crate::ingest::pipeline::ingest_source`]) (issue #184). Given a section's
/// source and the page it came from, the catalog command key to stamp on it, or
/// `None` when the page carries no catalog key.
///
/// `cargo` is deliberately absent: its keys are derived from the URL by
/// [`crate::ingest::cargo::command_key_from_base_url`] (the `commands/cargo-<sub>`
/// stem), so cargo routing stays URL-driven and this map is a no-op for cargo. The
/// map covers only the curated non-cargo pages whose key is editorial (not
/// derivable from a `cargo-<sub>` stem): the `rustc`/`rustdoc` man pages, the
/// rustup toolchain-management chapters, and the rustfmt reference.
///
/// Both paths route through this one function, so the seed's 29-entry catalog and
/// the live catalog cannot drift: the same page stamps the same key in both. This
/// is the "lift the map to a shared single-source-of-truth location" half of
/// issue #184 — the curated map previously lived only in the seed builder, so the
/// live pipeline stamped no key and its command catalog was empty.
pub(crate) fn curated_command_key(source: &str, final_url: &RqUrl) -> Option<&'static str> {
    use crate::ingest::rustup;
    match source {
        // The `rustc`/`rustdoc` man pages are the `command-line-arguments` page
        // (rendered by the respective book); the key is the tool name.
        "rustc" if final_url.path().contains("command-line-arguments") => Some("rustc"),
        "rustdoc" if final_url.path().contains("command-line-arguments") => Some("rustdoc"),
        // The rustfmt reference is ingested as approved Markdown; every reference
        // section documents the `rustfmt` command, so the key is uniform.
        "rustfmt" => Some("rustfmt"),
        // The rustup toolchain-management families key off the chapter path
        // (`concepts/index`, `concepts/toolchains`, …); the install flow and the
        // profiles page carry no catalog key, so they fall through to `None`.
        "rustup" => match rustup::chapter_path(final_url).as_str() {
            "concepts/index" => Some("rustup show"),
            "concepts/toolchains" => Some("rustup toolchain"),
            "concepts/components" => Some("rustup component"),
            "overrides" => Some("rustup override"),
            "cross-compilation" => Some("rustup target"),
            _ => None,
        },
        // No curated key (cargo is URL-derived; api-guidelines, style-guide, and
        // clippy carry none), so the map is a no-op for them.
        _ => None,
    }
}

/// Stamp the shared [`curated_command_key`] on every section of a non-cargo seed
/// page, deriving the key from the page's source and URL. Used by
/// [`build_seed_corpus`] for the curated non-cargo pages (the rustc/rustdoc man
/// pages, the rustup toolchain chapters, and the rustfmt reference), so the seed
/// and the live pipeline route through the *same* key map (issue #184). A curated
/// page the map does not name is an ingest misconfiguration (a curated page with
/// no key) and surfaces as an error rather than a silent un-keyed section.
fn stamp_curated_key(sections: &mut [Section], source: &str, url: &str) -> Result<(), SeedError> {
    let parsed = RqUrl::parse(url).map_err(|_| SeedError::BadUrl {
        source: source.to_string(),
        url: url.to_string(),
    })?;
    let key = curated_command_key(source, &parsed).ok_or_else(|| SeedError::Ingest {
        source: source.to_string(),
        detail: format!("curated page {source} {url} has no catalog command key"),
    })?;
    stamp_command_key(sections, key)
}

/// The curated command/concept sub-pages the single-hop book discovery does not
/// reach (issue #184's discovery-depth gap), one list per source. The live
/// [`crate::ingest::pipeline::sync`] books these *in addition* to the nav-
/// discovered URLs so the URL-derivation (cargo) and the curated key map (rustup)
/// actually run on the sub-pages:
///
/// * **cargo**: the `commands/cargo-<sub>.html` command reference — the nav
///   reaches `commands/index.html` but not the per-command pages, so they are
///   booked directly from [`CARGO_COMMAND_PAGES`].
/// * **rustup**: the `concepts/*` toolchain chapters (plus the install / override /
///   cross-compilation pages) — the nav reaches `concepts/index.html` but not its
///   sub-pages, so the curated set is booked directly.
///
/// Both lists are the authoritative source lists the seed builder ingests, so the
/// live corpus and the seed corpus carry the same sub-pages. An already-discovered
/// URL is deduplicated by the sync loop's `seen` set, so booking a page the nav
/// also reached is a harmless no-op.
pub(crate) fn command_subpage_urls(source: &str) -> Vec<String> {
    match source {
        "cargo" => CARGO_COMMAND_PAGES
            .iter()
            .map(|(file, _)| format!("{CARGO_BASE}commands/{file}"))
            .collect(),
        "rustup" => RUSTUP_CONCEPT_PAGES
            .iter()
            .map(|file| format!("{RUSTUP_BASE}{file}"))
            .collect(),
        _ => Vec::new(),
    }
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
    // -- api-guidelines: every topical chapter so the full 54-code checklist
    //    resolves to a real explanation (issue #170 AC2). The three original
    //    pages plus the eight topical chapters that carry the remaining codes.
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
    // The eight topical chapters. These carry no recorded etag (consistent with
    // the style-guide/rustc/rustup/rustdoc fixtures, which also pass `None`);
    // the section's content hash is over the captured bytes regardless.
    for (path, body) in [
        ("debuggability.html", fixtures::DEBUGGABILITY),
        ("dependability.html", fixtures::DEPENDABILITY),
        ("documentation.html", fixtures::DOCUMENTATION),
        ("flexibility.html", fixtures::FLEXIBILITY),
        ("future-proofing.html", fixtures::FUTURE_PROOFING),
        ("macros.html", fixtures::MACROS),
        ("necessities.html", fixtures::NECESSITIES),
        ("predictability.html", fixtures::PREDICTABILITY),
        ("type-safety.html", fixtures::TYPE_SAFETY),
    ] {
        ag_sections.extend(ingest_html(
            "api-guidelines",
            &format!("{AG_BASE}{path}"),
            body,
            None,
            ingest_api_chapter,
        )?);
    }

    // -- style-guide: the design/style chapters (issue #170 AC3: required design
    //    and style topics are covered, not a single page). The expressions page
    //    carries the trailing-comma guidance; the others cover principles,
    //    advice, items, statements, types, and Cargo.toml conventions. --
    let mut style_sections: Vec<Section> = Vec::new();
    for (path, body) in [
        ("expressions.html", fixtures::EXPRESSIONS),
        ("principles.html", fixtures::STYLE_PRINCIPLES),
        ("advice.html", fixtures::STYLE_ADVICE),
        ("items.html", fixtures::STYLE_ITEMS),
        ("statements.html", fixtures::STYLE_STATEMENTS),
        ("types.html", fixtures::STYLE_TYPES),
        ("cargo.html", fixtures::STYLE_CARGO),
    ] {
        style_sections.extend(ingest_html(
            "style-guide",
            &format!("{STYLE_BASE}{path}"),
            body,
            None,
            ingest_style,
        )?);
    }

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
    stamp_curated_key(
        &mut rustc_sections,
        "rustc",
        &format!("{RUSTC_BASE}command-line-arguments.html"),
    )?;

    // `rustup`: the `rustup show` guidance is the toolchain model the `rustup`
    // concepts page documents (how rustup works, terminology, active/installed
    // toolchains — exactly what `rustup show` reports). `rustup-installation`
    // covers the install flow; the concepts page covers the toolchain state
    // `rustup show` surfaces. The key is stamped on the concepts sections as a
    // separate retrieval annotation; the faithful body is intact.
    // `rustup`: the toolchain-management families. `rustup show` reports the
    // toolchain model the concepts page documents; the install flow and the
    // toolchains/components/overrides/cross-compilation chapters carry the
    // `rustup toolchain`, `rustup component`, `rustup override`, and
    // `rustup target` usage. Each key is stamped as a *separate* retrieval
    // annotation over the faithful upstream chapter (issue #170 AC1 / `plan.md`
    // §6); the body stays the upstream text.
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
    stamp_curated_key(
        &mut rustup_concepts,
        "rustup",
        &format!("{RUSTUP_BASE}concepts/index.html"),
    )?;
    rustup_sections.extend(rustup_concepts);

    let mut rustup_toolchains = ingest_html(
        "rustup",
        &format!("{RUSTUP_BASE}concepts/toolchains.html"),
        fixtures::RUSTUP_TOOLCHAINS,
        None,
        ingest_rustup,
    )?;
    stamp_curated_key(
        &mut rustup_toolchains,
        "rustup",
        &format!("{RUSTUP_BASE}concepts/toolchains.html"),
    )?;
    rustup_sections.extend(rustup_toolchains);

    let mut rustup_components = ingest_html(
        "rustup",
        &format!("{RUSTUP_BASE}concepts/components.html"),
        fixtures::RUSTUP_COMPONENTS,
        None,
        ingest_rustup,
    )?;
    stamp_curated_key(
        &mut rustup_components,
        "rustup",
        &format!("{RUSTUP_BASE}concepts/components.html"),
    )?;
    rustup_sections.extend(rustup_components);

    let mut rustup_overrides = ingest_html(
        "rustup",
        &format!("{RUSTUP_BASE}overrides.html"),
        fixtures::RUSTUP_OVERRIDES,
        None,
        ingest_rustup,
    )?;
    stamp_curated_key(
        &mut rustup_overrides,
        "rustup",
        &format!("{RUSTUP_BASE}overrides.html"),
    )?;
    rustup_sections.extend(rustup_overrides);

    // `rustup target` manages the target triples used for cross-compilation; the
    // `cross-compilation` chapter is where rustup documents them (there is no
    // dedicated `targets` page), so the key is stamped on that faithful chapter.
    let mut rustup_cross = ingest_html(
        "rustup",
        &format!("{RUSTUP_BASE}cross-compilation.html"),
        fixtures::RUSTUP_CROSS_COMPILATION,
        None,
        ingest_rustup,
    )?;
    stamp_curated_key(
        &mut rustup_cross,
        "rustup",
        &format!("{RUSTUP_BASE}cross-compilation.html"),
    )?;
    rustup_sections.extend(rustup_cross);

    // The profiles chapter is ingested for its faithful content (the default
    // toolchain profile model); it carries no command key of its own.
    rustup_sections.extend(ingest_html(
        "rustup",
        &format!("{RUSTUP_BASE}concepts/profiles.html"),
        fixtures::RUSTUP_PROFILES,
        None,
        ingest_rustup,
    )?);

    // `rustdoc`: the `rustdoc` command's usage is its `command-line-arguments`
    // man page; the key is stamped as a separate retrieval annotation over the
    // faithful page so `get_command_guidance("rustdoc")` resolves (issue #170
    // AC1).
    let mut rustdoc_sections = ingest_html(
        "rustdoc",
        &format!("{RUSTDOC_BASE}command-line-arguments.html"),
        fixtures::RUSTDOC_CLI,
        None,
        ingest_rustdoc,
    )?;
    stamp_curated_key(
        &mut rustdoc_sections,
        "rustdoc",
        &format!("{RUSTDOC_BASE}command-line-arguments.html"),
    )?;

    // -- rustfmt: the two approved, commit-pinned Markdown sources. The `rustfmt`
    //    key is stamped on both sections (README.md and Configurations.md are the
    //    `rustfmt` usage + reference `rustfmt_usage` cites) as a separate
    //    retrieval annotation; the faithful Markdown is intact. --
    let mut rustfmt_sections = rustfmt_sections()?;
    stamp_curated_key(
        &mut rustfmt_sections,
        "rustfmt",
        rustfmt_document_url("README.md").as_ref(),
    )?;

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

/// Parse the recorded checklist and resolve every entry to its real explanation
/// — no entry is filtered out (issue #170 AC2).
///
/// The full `checklist.html` lists every guideline code (54). The seed now ships
/// *all* of the api-guidelines topical chapters, so every code's primary
/// explanation is among the ingested sections and every entry resolves. The
/// entries are passed to `link_explanations` **unfiltered**: it validates that
/// each code resolves to exactly one section and fails with an actionable
/// [`SeedError`] on a missing or ambiguous mapping, so a code whose explanation
/// is not shipped surfaces as a hard error rather than a silently dropped entry.
/// That is the invariant AC2 names: no entry is filtered to make validation pass.
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
    //
    // This is the same primary-heading resolution rule shared with the live sync
    // path (`ingest_checklist_document`), applied here through the shared
    // [`primary_explanations`] helper, so the bundled checklist and the live sync
    // can no longer disagree about which sections are primary definitions versus
    // inherited continuations.
    let primary: Vec<Section> = primary_explanations(api_guidelines);

    // Keep *every* upstream entry. Resolving all of them against the primary
    // sections is the real `link_explanations` the live pipeline runs: it
    // fails on a missing or ambiguous mapping, so a code whose explanation is
    // absent is a hard error, never a dropped entry (AC2).
    let mut kept: Vec<ChecklistEntry> = entries;

    // Resolve every entry's explanation sections against the primary sections and
    // validate the mapping is neither missing nor ambiguous (the same
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

    // A faithful, complete checklist: every recorded entry is preserved.
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
                format!("{AG_BASE}debuggability.html"),
                format!("{AG_BASE}dependability.html"),
                format!("{AG_BASE}documentation.html"),
                format!("{AG_BASE}flexibility.html"),
                format!("{AG_BASE}future-proofing.html"),
                format!("{AG_BASE}macros.html"),
                format!("{AG_BASE}necessities.html"),
                format!("{AG_BASE}predictability.html"),
                format!("{AG_BASE}type-safety.html"),
                format!("{AG_BASE}checklist.html"),
            ],
            R::channel_only(crate::model::Channel::Rolling),
        ),
        (
            "style-guide".to_string(),
            "Rust Style Guide".to_string(),
            format!("{STYLE_BASE}index.html"),
            vec![
                format!("{STYLE_BASE}expressions.html"),
                format!("{STYLE_BASE}principles.html"),
                format!("{STYLE_BASE}advice.html"),
                format!("{STYLE_BASE}items.html"),
                format!("{STYLE_BASE}statements.html"),
                format!("{STYLE_BASE}types.html"),
                format!("{STYLE_BASE}cargo.html"),
            ],
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
                // The toolchain-management chapters the `rustup toolchain /
                // component / override / target` families cite (issue #170 AC1).
                format!("{RUSTUP_BASE}concepts/toolchains.html"),
                format!("{RUSTUP_BASE}concepts/components.html"),
                format!("{RUSTUP_BASE}overrides.html"),
                format!("{RUSTUP_BASE}cross-compilation.html"),
                format!("{RUSTUP_BASE}concepts/profiles.html"),
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
    use crate::ingest::fetch::{BoundClient, FetchPolicy};

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

    // --- AC4 (issue #155): the canonical Clippy lint-catalog fixture and its
    //     regression guards. The fixture freezes the canonical lint set; a
    //     hermetic test guards drift against the embedded catalog, and an
    //     ignored live test guards drift against the live catalog. ------

    /// Extract the canonical Clippy lint-catalog lint ids (the `clippy::…` ids
    /// the catalog sections document), sorted deterministically. This is the
    /// emitter that keeps the frozen fixture in sync with the catalog.
    fn extract_catalog_lint_ids(sections: &[Section]) -> Vec<String> {
        let mut ids: BTreeSet<String> = BTreeSet::new();
        for section in sections {
            if section.id.as_str().starts_with("clippy/clippy-catalog#") {
                for lint in &section.lint_ids {
                    ids.insert(lint.clone());
                }
            }
        }
        ids.into_iter().collect()
    }

    /// Load the canonical lint ids frozen in the committed fixture (issue #155,
    /// AC4). `include_str!` takes a string literal, so the `data/fixtures/`
    /// path is inlined here.
    fn fixture_catalog_lint_ids() -> Vec<String> {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../data/fixtures/clippy-catalog.json"))
                .expect("the clippy catalog fixture is valid JSON");
        fixture["lint_ids"]
            .as_array()
            .expect("the fixture carries a `lint_ids` array")
            .iter()
            .map(|v| v.as_str().expect("a lint id string").to_string())
            .collect()
    }

    /// AC4 (hermetic): the committed fixture stays in sync with the embedded
    /// catalog. If the catalog's lint set drifts from the frozen fixture, this
    /// fails loudly, pointing at the emitter to regenerate the fixture.
    #[test]
    fn clippy_catalog_fixture_stays_current() {
        let corpus = build_seed_corpus().expect("the seed corpus builds");
        let extracted = extract_catalog_lint_ids(&corpus.sections);
        assert_eq!(
            extracted,
            fixture_catalog_lint_ids(),
            "the frozen clippy catalog fixture is stale — re-run the emitter so \n             `data/fixtures/clippy-catalog.json` reflects the current catalog lint set"
        );
    }

    /// AC4 (live, opt-in): the live Clippy catalog still contains the canonical
    /// lints frozen in the fixture, failing loudly if they drift. Ignored by
    /// default — an opt-in network fetch, off the normal hermetic suite — and
    /// env-gated so even an accidental `--ignored` run doesn't touch the
    /// network without `RUST_DOCS_MCP_LIVE`.
    #[tokio::test]
    #[ignore = "opt-in live upstream fetch; off the normal suite"]
    async fn clippy_live_catalog_still_covers_the_canonical_fixture() {
        if std::env::var_os("RUST_DOCS_MCP_LIVE").is_none() {
            eprintln!(
                "skipping: set RUST_DOCS_MCP_LIVE=1 to run the live Clippy \
                 catalog regression (issue #155, AC4)"
            );
            return;
        }
        let client =
            BoundClient::new(FetchPolicy::default()).expect("a registry-gated client builds");
        let fetched = client
            .fetch(&crate::ingest::clippy::catalog_url())
            .await
            .expect("the live clippy catalog fetches within the registry boundary");
        let live = ingest_lint_catalog(&fetched).expect("the live clippy catalog parses");
        let live_lints: BTreeSet<String> = live
            .iter()
            .flat_map(|section| section.lint_ids.clone())
            .collect();
        for lint in fixture_catalog_lint_ids() {
            assert!(
                live_lints.contains(&lint),
                "the live clippy catalog no longer contains the canonical lint \
                 `{lint}` — the catalog drifted; update the frozen fixture"
            );
        }
    }

    // --- issue #184 (F-184-1): the shared live-catalog wiring the discovery-
    //     depth gap added — the curated sub-page URL list and the curated
    //     command-key map that the seed and the live pipeline now share. Before
    //     the fix the live pipeline stamped no catalog key (empty catalog) and
    //     carried no attribution; these two functions are the new single source
    //     of truth. A focused unit test guards them so a future regression that
    //     breaks live catalog population while the seed stays green fails here
    //     rather than only at a live sync (the offline acceptance tests exercise
    //     the downstream effect through `sync`; these exercise the functions
    //     directly).

    /// The curated sub-page URL list books exactly the cargo command reference
    /// and the rustup concept chapters the single-hop book discovery misses
    /// (issue #184's discovery-depth gap), and no sub-pages for a source that
    /// needs none.
    #[test]
    fn command_subpage_urls_books_the_cargo_and_rustup_sub_pages() {
        let cargo = command_subpage_urls("cargo");
        assert!(
            !cargo.is_empty(),
            "cargo must book its per-command reference sub-pages"
        );
        assert!(
            cargo.iter().all(|u| {
                u.starts_with("https://doc.rust-lang.org/cargo/commands/cargo-")
                    && u.ends_with(".html")
                    && !u.ends_with("cargo-index.html")
            }),
            "every cargo sub-page is a per-command reference (never the index), got {cargo:?}"
        );
        for sub in ["cargo-build", "cargo-new", "cargo-test", "cargo-doc"] {
            assert!(
                cargo
                    .iter()
                    .any(|u| u.ends_with(&format!("commands/{sub}.html"))),
                "cargo must book its `{sub}` sub-page, got {cargo:?}"
            );
        }

        let rustup = command_subpage_urls("rustup");
        assert!(!rustup.is_empty(), "rustup must book its concept sub-pages");
        assert!(
            rustup
                .iter()
                .all(|u| u.starts_with("https://rust-lang.github.io/rustup/")),
            "every rustup sub-page is under the rustup boundary, got {rustup:?}"
        );
        for sub in [
            "concepts/index.html",
            "concepts/toolchains.html",
            "concepts/components.html",
            "overrides.html",
            "cross-compilation.html",
        ] {
            assert!(
                rustup.iter().any(|u| u.ends_with(sub)),
                "rustup must book its `{sub}` sub-page, got {rustup:?}"
            );
        }

        // A source that needs no curated sub-page books none: its pages are
        // already reached by the nav, or it carries no catalog key.
        assert!(
            command_subpage_urls("rustc").is_empty(),
            "rustc books no sub-pages (its man page is already nav-linked)"
        );
        assert!(
            command_subpage_urls("clippy").is_empty(),
            "clippy books no sub-pages"
        );
    }

    /// The curated command-key map resolves the non-cargo catalog keys from a
    /// page's source and URL — the single source of truth the seed and the live
    /// pipeline share. `cargo` is URL-derived (never curated), so the map is a
    /// no-op for it, and a curated page the map does not name yields `None`.
    #[test]
    fn curated_command_key_resolves_the_non_cargo_catalog_keys() {
        let url = |p: &str| RqUrl::parse(p).expect("a well-formed fixture URL");

        assert_eq!(
            curated_command_key(
                "rustc",
                &url("https://doc.rust-lang.org/rustc/command-line-arguments.html")
            ),
            Some("rustc")
        );
        assert_eq!(
            curated_command_key(
                "rustdoc",
                &url("https://doc.rust-lang.org/rustdoc/command-line-arguments.html")
            ),
            Some("rustdoc")
        );
        assert_eq!(
            curated_command_key(
                "rustup",
                &url("https://rust-lang.github.io/rustup/concepts/index.html")
            ),
            Some("rustup show")
        );
        assert_eq!(
            curated_command_key(
                "rustup",
                &url("https://rust-lang.github.io/rustup/concepts/toolchains.html")
            ),
            Some("rustup toolchain")
        );
        assert_eq!(
            curated_command_key(
                "rustup",
                &url("https://rust-lang.github.io/rustup/concepts/components.html")
            ),
            Some("rustup component")
        );
        assert_eq!(
            curated_command_key(
                "rustup",
                &url("https://rust-lang.github.io/rustup/overrides.html")
            ),
            Some("rustup override")
        );
        assert_eq!(
            curated_command_key(
                "rustup",
                &url("https://rust-lang.github.io/rustup/cross-compilation.html")
            ),
            Some("rustup target")
        );
        assert_eq!(
            curated_command_key(
                "rustfmt",
                &url("https://raw.githubusercontent.com/rust-lang/rustfmt/main/README.md")
            ),
            Some("rustfmt")
        );

        // Negative cases: cargo is URL-derived, a curated source on a non-curated
        // page yields `None`, and an unrelated source yields `None`.
        assert_eq!(
            curated_command_key(
                "cargo",
                &url("https://doc.rust-lang.org/cargo/commands/cargo-build.html")
            ),
            None
        );
        assert_eq!(
            curated_command_key(
                "rustup",
                &url("https://rust-lang.github.io/rustup/installation/index.html")
            ),
            None
        );
        assert_eq!(
            curated_command_key(
                "rustup",
                &url("https://rust-lang.github.io/rustup/concepts/profiles.html")
            ),
            None
        );
        assert_eq!(
            curated_command_key(
                "rustc",
                &url("https://doc.rust-lang.org/rustc/getting-started.html")
            ),
            None
        );
    }
}
