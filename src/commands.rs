//! Source-backed command catalog — task `[039][M3]` (issue [#39]).
//!
//! This module is the *catalog* side of `plan.md` §6 ("Build and validate the
//! source-backed command catalog, including citations, compatibility
//! qualifications, and effect notes"): the fixed, human-reviewable set of
//! [`CommandEntry`]s that the milestone (`[039]`-`[044]`) grows into the full
//! command families and that the `get_command_guidance` / catalog resource
//! (`[048]+`) serves. [039] seeded the **project setup and inspection** family —
//! `cargo new`, `cargo init`, `cargo metadata`, `cargo locate-project`, and
//! `cargo tree` — the family `plan.md` §6 names first. [040] adds the
//! **dependency and reproducibility** family — `cargo add`, `cargo remove`,
//! `cargo update`, `cargo generate-lockfile`, and `cargo fetch`, each citing its
//! command section *and* the relevant dependency/feature/lockfile reference
//! sections. [041] adds the **implementation and verification** family —
//! `cargo check`, `cargo build`, `cargo run`, `cargo test`, `cargo bench`,
//! `cargo fmt`, `cargo clippy`, `cargo doc`, and `cargo fix`, each citing its
//! Cargo command section *and* the relevant tooling chapters (the rustfmt
//! usage/configuration reference for `cargo fmt`, the Clippy usage/configuration
//! chapters for `cargo clippy`, the rustdoc book for `cargo doc`, and the rustdoc
//! documentation-tests chapter for `cargo test`'s doctests). A
//! [`CommandCatalog::combined`] concatenates all three families.
//!
//! ## Two acceptance criteria (`plan.md` §5/§6/§350, issue [#39])
//!
//! * **AC1 — resolvable citations.** Each entry carries an *exact* catalog key
//!   ([`CommandKey`]) and the upstream section ids it cites
//!   ([`CommandEntry::upstream_sections`]); a lookup by that exact key returns
//!   the entry, and [`CommandCatalog::resolve`] proves that **every** cited
//!   section id resolves against the active snapshot's [`SectionIndex`]. A
//!   section id that does not resolve is a typed [`LookupError::UnresolvedSection`]
//!   — no fabricated section is ever returned.
//! * **AC2 — upstream vs. maintained notes stay separate.** The upstream
//!   excerpts ([`CommandEntry::syntax`], [`CommandEntry::examples`]) are kept in
//!   their own fields, distinct from the maintained metadata: prerequisites,
//!   version/stability qualifications, and the **separately labeled**
//!   [`EffectNote`]s ([`CommandEntry::effect_notes`], `plan.md` §6 / §350). The
//!   `EffectNote`s describe *possible* effects and are never permission controls.
//!
//! ## Discoverable, never fabricated
//!
//! A key that is not in this catalog is rejected with the catalog's supported
//! keys as discoverable alternatives ([`LookupError::UnknownKey`]); a key that
//! carries a shell operator is refused at the key boundary
//! ([`LookupError::InvalidKey`]) — the catalog selects documentation, it never
//! interpolates a key into a process. No entry here is *executed*; a test at the
//! lookup boundary confirms command lookup never launches a subprocess.
//!
//! ## Total, pure, and offline
//!
//! Every entry point is **total** (it returns a typed error, never panics) and
//! **pure** (a fixed catalog and snapshot yield a fixed result): the only inputs
//! are the catalog and, for [`resolve`](CommandCatalog::resolve), a
//! [`SnapshotManifest`]. The module is therefore testable offline against
//! hand-built fixtures — including a fixture snapshot that makes each cited
//! section resolvable — with no upstream ingestion, network call, or
//! real-client/agent trial.
//!
//! **Out of scope for [042]** (later work, deliberately *not* built here): the
//! remaining `[043]`-`[044]` command families (the toolchain/diagnostics family —
//! `rustup show`, toolchain/override/component/target commands, and `rustc`/
//! `rustfmt`/`rustdoc` usage), the `[048]+` MCP tool/resource wiring, and any
//! change to the ingestion pipeline (`crate::ingest`) — this module *defines* the
//! catalog and its resolution seam and consumes the tested [`SectionIndex`].

// The module is the transitional catalog seam: its items are exercised by the
// tests below and by `crate::coverage`'s record-level inspections, but are not
// yet reachable from `main`/`lib`'s public API, which the `get_command_guidance`
// / catalog-resource handlers land (`[048]+`). Until then the module is
// `#[allow(dead_code)]`-covered, mirroring `crate::reads` and `crate::search`.
#![allow(dead_code)]

use std::fmt;

use crate::model::{
    CommandEntry, CommandKey, EffectKind, EffectNote, Section, SectionId, SnapshotId,
    SnapshotManifest, SourceId,
};
use crate::pagination::{
    Cursor, CursorError, FilterKey, Item, MAX_RESPONSE_BYTES, Operation, ResponsePage,
    clamp_query_limit, paginate_items,
};
use crate::provenance::{Instant, StaleThreshold};
use crate::reads::{ReadError, SectionRead, get_section};
use crate::search::SectionIndex;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Catalog error
// ---------------------------------------------------------------------------

/// A failure to look up or resolve a command entry.
///
/// Each variant carries enough context to be *actionable* and to surface the
/// "reject an unsupported key with discoverable alternatives, never fabricate"
/// rule of `plan.md` §6 / §350: [`UnknownKey`] names the request and the
/// supported alternatives; [`UnresolvedSection`] names the exact section id that
/// did not resolve so the caller can refresh the corpus rather than guess;
/// [`InvalidKey`] names the request that the key boundary refused (an empty /
/// padded / shell-operator string) so a caller never mistakes a refusal for an
/// execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupError {
    /// The requested string is not a well-formed [`CommandKey`]: it is empty,
    /// padded, or carries a shell operator. Such a string is a documentation
    /// *selector*, not a command line, and is refused rather than executed.
    InvalidKey {
        /// The request that was refused.
        requested: String,
        /// Why the key boundary refused it.
        reason: String,
    },
    /// The request is a well-formed [`CommandKey`] but belongs to no entry in this
    /// catalog. The `supported` list is the discoverable-alternatives payload.
    UnknownKey {
        /// The request that matched no entry.
        requested: String,
        /// The catalog's supported keys, the discoverable alternatives.
        supported: Vec<String>,
    },
    /// The entry exists, but one of its cited upstream section ids resolves to no
    /// section in the active snapshot. No section is fabricated in its place.
    UnresolvedSection {
        /// The catalog key whose citation could not be resolved.
        key: String,
        /// The section id that did not resolve.
        section: SectionId,
    },
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey { requested, reason } => write!(
                f,
                "invalid command key `{requested}`: {reason} \
                 (command keys are documentation selectors and are never executed)"
            ),
            Self::UnknownKey {
                requested,
                supported,
            } => write!(
                f,
                "unknown command key `{requested}`; supported keys: [{}]",
                supported.join(", ")
            ),
            Self::UnresolvedSection { key, section } => write!(
                f,
                "command `{key}` cites upstream section `{section}` that does not \
                 resolve in the active snapshot; guidance is not fabricated"
            ),
        }
    }
}

impl std::error::Error for LookupError {}

/// The reason a [`CommandKey`] validation failed, mapped from the schema layer
/// into a [`LookupError::InvalidKey`] message a caller can act on.
fn key_error_reason(err: &crate::model::SchemaError) -> String {
    match err {
        crate::model::SchemaError::ShellOperator(msg) => {
            format!("rejected shell operator ({msg})")
        }
        crate::model::SchemaError::InvalidField { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// The catalog and its setup/inspection family
// ---------------------------------------------------------------------------

/// A fixed, ordered catalog of [`CommandEntry`]s.
///
/// [039] shipped one family — the project setup and inspection commands of
/// `plan.md` §6 — via [`CommandCatalog::setup_inspection`]. [040] adds the
/// dependency and reproducibility family via [`CommandCatalog::dependencies`];
/// [041] adds the implementation and verification family via
/// [`CommandCatalog::implementation_verification`]; [042] adds the packaging and
/// release-preparation family via [`CommandCatalog::packaging_release`]; and
/// [`CommandCatalog::combined`] concatenates all four. A catalog is a plain,
/// ordered, cloneable value (no interior mutability, no global): building it twice
/// yields byte-identical entries, so lookups are pure and deterministic. The
/// remaining families (`[043]`-`[044]`, the toolchain/diagnostics family) are
/// added as further catalogs or as a concatenation this module's lookup already
/// handles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandCatalog {
    entries: Vec<CommandEntry>,
}

impl CommandCatalog {
    /// The complete project setup and inspection family (`plan.md` §6):
    /// `cargo new`, `cargo init`, `cargo metadata`, `cargo locate-project`, and
    /// `cargo tree`, in that fixed order.
    ///
    /// Each entry cites the Cargo Book section that documents the command (its
    /// `upstream_sections`), carries a documented syntax line and a documented
    /// example excerpt (upstream), and keeps its prerequisites, version/stability
    /// qualifications, and effect notes in **separate** fields (AC2).
    pub fn setup_inspection() -> CommandCatalog {
        CommandCatalog {
            entries: vec![
                cargo_new(),
                cargo_init(),
                cargo_metadata(),
                cargo_locate_project(),
                cargo_tree(),
            ],
        }
    }

    /// The complete dependency and reproducibility family (`plan.md` §6):
    /// `cargo add`, `cargo remove`, `cargo update`, `cargo generate-lockfile`, and
    /// `cargo fetch`, in that fixed order.
    ///
    /// Each entry cites the Cargo Book command section that documents it
    /// **and** the relevant dependency/feature/lockfile reference sections
    /// (`features`, `manifest`, `registry`) — AC1's "command-specific options and
    /// relevant configuration sections". Each entry keeps its prerequisites,
    /// version/stability qualifications, and the **separately labeled** effect
    /// notes in distinct fields (AC2): the lockfile/source effect is
    /// [`EffectKind::LockfileChange`], the source-tree edit
    /// [`EffectKind::SourceChange`], and the possible registry download
    /// [`EffectKind::DependencyDownload`].
    ///
    /// The `--offline`/`--locked` scoping (AC2) is a per-command *editorial*
    /// choice, not a claim about which flags `cargo` accepts: `--offline` is
    /// documented for all five because it is the cache-only flag central to these
    /// registry-touching commands, while `--locked` — the "the lockfile will not
    /// change" assertion — is highlighted only for the four commands whose purpose
    /// is to write or resolve the lockfile (`cargo add`, `cargo remove`,
    /// `cargo update`, `cargo generate-lockfile`). `cargo fetch` *does* accept
    /// `--locked` (Cargo parses it and raises a semantic lockfile error when the
    /// lockfile would change), but its purpose is to refresh and download, so its
    /// entry *chooses to highlight* `--offline` — the flag central to fetch's
    /// primary purpose — and defers the full, version-specific flag list (which
    /// includes `--locked`) to `cargo fetch --help` rather than documenting it
    /// inline. Omitting `--locked` from fetch's entry is therefore a design
    /// choice, not a claim that the flag is unsupported or inapplicable.
    pub fn dependencies() -> CommandCatalog {
        CommandCatalog {
            entries: vec![
                cargo_add(),
                cargo_remove(),
                cargo_update(),
                cargo_generate_lockfile(),
                cargo_fetch(),
            ],
        }
    }

    /// The complete implementation and verification family (`plan.md` §6):
    /// `cargo check`, `cargo build`, `cargo run`, `cargo test`, `cargo bench`,
    /// `cargo fmt`, `cargo clippy`, `cargo doc`, and `cargo fix`, in that fixed
    /// order.
    ///
    /// Each entry cites its Cargo command section **and** the relevant tooling
    /// chapters the command defers to — the rustfmt usage/configuration reference
    /// for `cargo fmt`, the Clippy usage/configuration chapters for `cargo
    /// clippy`, the rustdoc book for `cargo doc`, and the rustdoc
    /// documentation-tests chapter for `cargo test`'s doctests (AC1's
    /// "command-specific options and relevant configuration sections"). The
    /// selection/forwarding notes and the check-vs-apply / doctest-vs-test /
    /// documented-nightly distinctions are maintained metadata kept in **separate**
    /// fields (AC2): applying source changes is [`EffectKind::SourceChange`] (the
    /// `cargo fmt` and `cargo fix` apply commands), running project code is
    /// [`EffectKind::ProjectCodeExecution`] (the `cargo run`/`cargo test`/
    /// `cargo bench` execute commands), and producing build output is
    /// [`EffectKind::ArtifactWrite`]. The check commands (`cargo check`,
    /// `cargo clippy`, `cargo doc`, and `cargo test`'s non-doctest runs) carry no
    /// source change; `cargo fix` and `cargo fmt` are the apply counterparts.
    pub fn implementation_verification() -> CommandCatalog {
        CommandCatalog {
            entries: vec![
                cargo_check(),
                cargo_build(),
                cargo_run(),
                cargo_test(),
                cargo_bench(),
                cargo_fmt(),
                cargo_clippy(),
                cargo_doc(),
                cargo_fix(),
            ],
        }
    }

    /// The complete packaging and release-preparation family (`plan.md` §6):
    /// `cargo package` and `cargo publish`, in that fixed order.
    ///
    /// Each entry cites its Cargo command chapter **and** the relevant
    /// release-preparation reference chapter(s) — `packaging` for `cargo package`,
    /// and `publishing` with `registry` for `cargo publish` (AC1's "release-
    /// preparation references"). Each entry keeps its prerequisites, version/
    /// stability qualifications, and the **separately labeled** effect notes in
    /// distinct fields (AC2): the possible publication is
    /// [`EffectKind::Publication`] (`cargo publish` uploads the package to the
    /// registry), the produced `.crate` archive is
    /// [`EffectKind::ArtifactWrite`], and the possible verification download is
    /// [`EffectKind::DependencyDownload`]. A maintained note makes the effect
    /// explicit while stating that this is *documentation lookup* that does not
    /// authorize, request, or perform a publication — the calling agent decides
    /// whether to run `cargo publish` through its own terminal tools.
    pub fn packaging_release() -> CommandCatalog {
        CommandCatalog {
            entries: vec![cargo_package(), cargo_publish()],
        }
    }

    /// The complete toolchain and direct-tool family (`plan.md` §6): `rustup
    /// show`, `rustup toolchain`, `rustup override`, `rustup component`, `rustup
    /// target`, `rustc`, `rustfmt`, and `rustdoc`, in that fixed order.
    ///
    /// The rustup subcommand entries cite the rustup Book chapter that documents
    /// the command (`toolchains`, `overrides` + `toolchain-file`, `components`,
    /// `targets`, and the `index` introduction for `rustup show`) and the
    /// toolchain/component/target *mutations* are made explicit via a
    /// [`EffectKind::ToolchainChange`] effect note (the override, component,
    /// and target changes write into the user's rustup state — a possible
    /// toolchain change that is *not* a permission to make it). `rustup show` is
    /// an inspection command and carries no effect note. The direct-tool entries
    /// (`rustc`, `rustfmt`, `rustdoc`) cite the compiler/rustfmt/rustdoc
    /// reference chapters they defer to (the rustc `command-line-arguments`
    /// chapter, the rustfmt `README.md`/`Configurations.md` reference, and the
    /// rustdoc `commands`/`documentation-tests` chapters). Each entry keeps its
    /// prerequisites, version/stability qualifications, and effect notes in
    /// **separate** fields (AC2): the help/version mechanism
    /// (`--help`/`--version`/`rustup show --active-toolchain`), the
    /// stable/nightly/unknown compatibility qualifications, and the effect notes
    /// are maintained metadata that survives separately from the upstream
    /// excerpts — a documented nightly-only option keeps its "nightly"/"unstable"
    /// qualification rather than being flattened to an unconditional claim, and
    /// an unknown/undocumented-version note keeps its qualification too.
    pub fn toolchain_direct_tools() -> CommandCatalog {
        CommandCatalog {
            entries: vec![
                rustup_show(),
                rustup_toolchain(),
                rustup_override(),
                rustup_component(),
                rustup_target(),
                rustc_usage(),
                rustfmt_usage(),
                rustdoc_usage(),
            ],
        }
    }

    /// The catalog as of `[043]`: the [setup and inspection](Self::setup_inspection),
    /// [dependency and reproducibility](Self::dependencies), [implementation and
    /// verification](Self::implementation_verification), [packaging and release-
    /// preparation](Self::packaging_release), and [toolchain and direct-tool](Self::toolchain_direct_tools)
    /// families, in that order — the five families `plan.md` §6 names.
    ///
    /// This is the "a concatenation this module's lookup already handles" case:
    /// the lookup/resolve paths are unchanged by the concatenation, so every key
    /// from any family still resolves exactly as it did in its own catalog.
    pub fn combined() -> CommandCatalog {
        let mut entries = CommandCatalog::setup_inspection().entries;
        entries.extend(CommandCatalog::dependencies().entries);
        entries.extend(CommandCatalog::implementation_verification().entries);
        entries.extend(CommandCatalog::packaging_release().entries);
        entries.extend(CommandCatalog::toolchain_direct_tools().entries);
        CommandCatalog { entries }
    }

    /// Whether the catalog has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of entries in the catalog.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The catalog's supported keys, in catalog order — the discoverable
    /// alternatives a caller can list when a key is unsupported.
    pub fn keys(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.key.to_string())
            .collect()
    }

    /// Look up an entry by its exact, validated key.
    ///
    /// The request is first validated as a [`CommandKey`]; a shell-operator or
    /// padded request is [`LookupError::InvalidKey`] (never executed). A
    /// well-formed request that matches no entry is [`LookupError::UnknownKey`]
    /// carrying the catalog's supported keys as discoverable alternatives.
    /// Total: it never panics.
    pub fn get(&self, key: &str) -> Result<&CommandEntry, LookupError> {
        let parsed = CommandKey::new(key).map_err(|err| LookupError::InvalidKey {
            requested: key.to_string(),
            reason: key_error_reason(&err),
        })?;
        self.entries
            .iter()
            .find(|entry| entry.key == parsed)
            .ok_or(LookupError::UnknownKey {
                requested: key.to_string(),
                supported: self.keys(),
            })
    }

    /// Resolve an entry by key **and** prove that every upstream section it cites
    /// resolves against `manifest`'s [`SectionIndex`].
    ///
    /// This is the AC1 guard at the catalog boundary: a well-formed key whose
    /// citations do not all resolve in the active snapshot is
    /// [`LookupError::UnresolvedSection`] naming the first unresolvable id — the
    /// catalog never hands a caller a guidance entry whose citations dangle. The
    /// resolution reuses the tested in-memory [`SectionIndex`] (the same seam
    /// `crate::reads::get_section` resolves through), so "resolves" means "the
    /// snapshot carries a section with that exact id".
    pub fn resolve(
        &self,
        manifest: &SnapshotManifest,
        key: &str,
    ) -> Result<ResolvedCommand, LookupError> {
        let entry = self.get(key)?;
        let index = SectionIndex::build(manifest);
        // Resolve every cited section; the first that is absent is the error.
        let mut sections = Vec::with_capacity(entry.upstream_sections.len());
        for reference in &entry.upstream_sections {
            match index.get(reference) {
                Some(section) => sections.push(section.clone()),
                None => {
                    return Err(LookupError::UnresolvedSection {
                        key: entry.key.to_string(),
                        section: reference.clone(),
                    });
                }
            }
        }
        Ok(ResolvedCommand {
            entry: entry.clone(),
            sections,
            snapshot_id: manifest.body.snapshot_id.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Resolved entry
// ---------------------------------------------------------------------------

/// A catalog entry together with the proof that every upstream section it cites
/// resolved in the snapshot it was resolved against.
///
/// [`resolved`](ResolvedCommand::resolved) is the "cited syntax / examples are
/// retrievable" property of `plan.md` §6: each `sections` element is the exact
/// [`Section`] the entry's corresponding `upstream_sections` id resolves to, in
/// order, so a caller can fetch the section content and citations directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCommand {
    /// The catalog entry that was resolved.
    pub entry: CommandEntry,
    /// The resolved upstream sections, in the same order as
    /// [`entry.upstream_sections`](CommandEntry::upstream_sections).
    pub sections: Vec<Section>,
    /// The snapshot the entry resolved against (its identity, for provenance).
    pub snapshot_id: SnapshotId,
}

impl ResolvedCommand {
    /// The first upstream section the entry cites — its canonical documentation
    /// section. A resolved setup/inspection entry always cites at least one
    /// section (the one documenting the command), so this is total.
    pub fn primary_section(&self) -> &Section {
        &self.sections[0]
    }
}

// ---------------------------------------------------------------------------
// The five setup/inspection entries
//
// Each entry is a faithful, compact distillation of the Cargo Book section that
// documents the command (`doc.rust-lang.org/cargo/commands/…`). The syntax line
// and example excerpt are upstream; the prerequisites, version/stability
// qualifications, and effect notes are maintained and kept in separate fields
// (AC2 / `plan.md` §350). The cited section is derived from the command's Cargo
// Book chapter path so it round-trips through the [`SectionId`] grammar; whether
// that section is present is the snapshot's business (a fixture makes it
// resolvable in the tests).
// ---------------------------------------------------------------------------

/// The Cargo Book source id every setup/inspection command cites.
fn cargo_source() -> SourceId {
    SourceId::new("cargo").expect("`cargo` is a registry source id")
}

/// Derive the section id a command cites from its Cargo Book chapter path.
///
/// The chapter path is the canonical `commands/cargo-<name>` path of the Cargo
/// Book command reference; `derive` validates the continuation-free grammar, so a
/// malformed path surfaces as a typed error rather than a dangling id.
fn cargo_section(chapter: &str) -> SectionId {
    SectionId::derive(&cargo_source(), chapter, None, None)
        .expect("a well-formed cargo chapter path derives a section id")
}

// ---------------------------------------------------------------------------
// The clippy / rustfmt / rustdoc source helpers
//
// The implementation and verification family (`[041]`) cites not only the Cargo
// command chapters but the tooling chapters the commands defer to: rustfmt's
// approved static Markdown (ingested by `crate::ingest::rustfmt` at a pinned
// commit), the Clippy book's usage/configuration chapters (ingested by
// `crate::ingest::clippy_book`), and the rustdoc book's chapters (ingested by
// `crate::ingest::rustdoc`). These helpers derive the same section ids the
// ingestion stages do, so a cited section resolves against a real snapshot rather
// than a fabricated placeholder.
// ---------------------------------------------------------------------------

/// The Clippy source id — the Clippy book (`doc.rust-lang.org/clippy/`).
fn clippy_source() -> SourceId {
    SourceId::new("clippy").expect("`clippy` is a registry source id")
}

/// The rustfmt source id — the approved static Markdown ingested at a pinned
/// commit under `raw.githubusercontent.com/rust-lang/rustfmt/`.
fn rustfmt_source() -> SourceId {
    SourceId::new("rustfmt").expect("`rustfmt` is a registry source id")
}

/// The rustdoc source id — the rustdoc book (`doc.rust-lang.org/rustdoc/`).
fn rustdoc_source() -> SourceId {
    SourceId::new("rustdoc").expect("`rustdoc` is a registry source id")
}

/// Derive the section id for a Clippy Book chapter (`usage`, `configuration`,
/// …), the way `crate::ingest::clippy_book::chapter_path` derives it, so the
/// citation resolves against a real Clippy section.
fn clippy_section(chapter: &str) -> SectionId {
    SectionId::derive(&clippy_source(), chapter, None, None)
        .expect("a well-formed clippy chapter path derives a section id")
}

/// Derive the section id for an approved rustfmt Markdown file (`README.md` /
/// `Configurations.md`), the way `crate::ingest::rustfmt::chapter_path` derives
/// it (the last raw-URL path segment is the approved file name).
fn rustfmt_section(file: &str) -> SectionId {
    SectionId::derive(&rustfmt_source(), file, None, None)
        .expect("a well-formed rustfmt file derives a section id")
}

/// Derive the section id for a rustdoc Book chapter (`commands`,
/// `documentation-tests`, `cargo-integration`, …), the way
/// `crate::ingest::rustdoc::chapter_path` derives it.
fn rustdoc_section(chapter: &str) -> SectionId {
    SectionId::derive(&rustdoc_source(), chapter, None, None)
        .expect("a well-formed rustdoc chapter path derives a section id")
}

// ---------------------------------------------------------------------------
// The rustup / rustc source helpers
//
// The toolchain and direct-tool family (`[043]`) cites the *toolchain* chapters
// of the rustup Book (ingested by `crate::ingest::rustup`: `installation`,
// `toolchains`, `overrides`, `components`, `targets`, `toolchain-file`, and the
// `index` introduction) and the *compiler* chapters of the rustc Book (ingested
// by `crate::ingest::rustc`: `command-line-arguments`, `index`). These helpers
// derive the same section ids the ingestion stages do (the chapter file stem
// under each book's boundary, per `crate::ingest::<book>::chapter_path`), so a
// cited section resolves against a real snapshot rather than a fabricated
// placeholder.
// ---------------------------------------------------------------------------

/// The rustup source id — The rustup Book (`rust-lang.github.io/rustup/`).
fn rustup_source() -> SourceId {
    SourceId::new("rustup").expect("`rustup` is a registry source id")
}

/// The rustc source id — The rustc Book (`doc.rust-lang.org/rustc/`).
fn rustc_source() -> SourceId {
    SourceId::new("rustc").expect("`rustc` is a registry source id")
}

/// Derive the section id for a rustup Book chapter (`installation`, `toolchains`,
/// `overrides`, `components`, `targets`, `toolchain-file`, `index`), the way
/// `crate::ingest::rustup::chapter_path` derives it (the chapter file stem under
/// the `rustup` boundary), so the citation resolves against a real rustup
/// section.
fn rustup_chapter(chapter: &str) -> SectionId {
    SectionId::derive(&rustup_source(), chapter, None, None)
        .expect("a well-formed rustup chapter path derives a section id")
}

/// Derive the section id for a rustc Book chapter (`command-line-arguments`,
/// `index`, …), the way `crate::ingest::rustc::chapter_path` derives it (the
/// chapter file stem under the `rustc` boundary), so the citation resolves
/// against a real rustc section.
fn rustc_chapter(chapter: &str) -> SectionId {
    SectionId::derive(&rustc_source(), chapter, None, None)
        .expect("a well-formed rustc chapter path derives a section id")
}

fn cargo_new() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo new").expect("`cargo new` is a well-formed key"),
        purpose: "Create a new Cargo package in a new directory.".to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-new")],
        syntax: vec!["cargo new [OPTIONS] <NAME>".to_string()],
        examples: vec![
            "`cargo new my-project` creates a `my-project/` directory containing a \
             `Cargo.toml` and a `src/main.rs` (or `src/lib.rs` with `--lib`)."
                .to_string(),
        ],
        prerequisites: vec![
            "A Rust toolchain that includes `cargo` is installed (for example via \
             `rustup`)."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo new --help` for the version-specific option list."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::SourceChange,
            detail: "creates a new project directory (a `Cargo.toml` and a `src/` \
                     file); it does not modify an existing project."
                .to_string(),
        }],
    }
}

fn cargo_init() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo init").expect("`cargo init` is a well-formed key"),
        purpose: "Initialize a Cargo package in an existing directory.".to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-init")],
        syntax: vec!["cargo init [OPTIONS] [PATH]".to_string()],
        examples: vec![
            "Running `cargo init` inside an existing directory writes a `Cargo.toml` \
             (and a `src/` file) without overwriting existing files."
                .to_string(),
        ],
        prerequisites: vec![
            "A Rust toolchain that includes `cargo` is installed (for example via \
             `rustup`)."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo init --help` for the version-specific option list."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::SourceChange,
            detail: "writes a `Cargo.toml` (and a `src/` file) into the target \
                     directory; existing files are not overwritten."
                .to_string(),
        }],
    }
}

fn cargo_metadata() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo metadata").expect("`cargo metadata` is a well-formed key"),
        purpose: "Emit metadata about the current package, workspace, or dependency \
                  graph as a machine-readable structure."
            .to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-metadata")],
        syntax: vec!["cargo metadata [OPTIONS]".to_string()],
        examples: vec![
            "`cargo metadata --format-version 1 --manifest-path Cargo.toml` prints the \
             workspace metadata as JSON on stdout."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo project or workspace: a manifest (`Cargo.toml`) is present in the \
             working directory or an ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; the \
             `--format-version` must be supported by the installed `cargo` (run \
             `cargo metadata --help` to list it)."
                .to_string(),
        ],
        // Inspection command: it reports the manifest and dependency graph without
        // building, running, or downloading. No maintained effect note.
        effect_notes: vec![],
    }
}

fn cargo_locate_project() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo locate-project")
            .expect("`cargo locate-project` is a well-formed key"),
        purpose: "Print the path to the Cargo manifest that `cargo` resolves for the \
                  given (or current) location."
            .to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-locate-project")],
        syntax: vec!["cargo locate-project [OPTIONS] [PATH]".to_string()],
        examples: vec![
            "Running `cargo locate-project` prints the path to the `Cargo.toml` that \
             `cargo` would use from the current directory."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo project or workspace: `cargo` is run within (or below) a directory \
             tree that contains a manifest."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo locate-project --help` for the version-specific option list."
                .to_string(),
        ],
        // Inspection command: it only reports a manifest path. No maintained effect
        // note.
        effect_notes: vec![],
    }
}

fn cargo_tree() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo tree").expect("`cargo tree` is a well-formed key"),
        purpose: "Display a tree of the dependencies of the current package or workspace."
            .to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-tree")],
        syntax: vec!["cargo tree [OPTIONS] [DEPS...]".to_string()],
        examples: vec![
            "Running `cargo tree` prints the dependency tree; `cargo tree -p my-crate` \
             restricts it to a single package."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo project with a manifest and a resolved dependency graph; run \
             `cargo update` or `cargo generate-lockfile` to refresh `Cargo.lock` \
             before inspecting it."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo tree --help` for the version-specific option list."
                .to_string(),
        ],
        // Inspection command: it reads the resolved dependency graph without
        // building, running, or downloading. No maintained effect note.
        effect_notes: vec![],
    }
}

// ---------------------------------------------------------------------------
// The five dependency/reproducibility entries
//
// [040] adds the second family of `plan.md` §6: `cargo add`, `cargo remove`,
// `cargo update`, `cargo generate-lockfile`, and `cargo fetch`. As with the
// setup/inspection family, each entry is a faithful, compact distillation of the
// Cargo Book section that documents the command, citing that command section
// *and* the relevant dependency/feature/lockfile reference sections (`features`,
// `manifest`, `registry`) so it carries both the command-specific options and the
// relevant configuration sections (AC1). The lockfile/source effect, the possible
// registry download, and the `--offline`/`--locked` scoping are maintainer notes
// in separate fields (AC2 / `plan.md` §6/§350): `--offline` is documented for all
// five; `--locked` is highlighted for the four that write or resolve the lockfile.
// `cargo fetch` accepts `--locked` too, but its entry highlights `--offline` and
// defers the full flag list to `cargo fetch --help` — the omission is a design
// choice, not a claim that `--locked` is unsupported.
// ---------------------------------------------------------------------------

/// The Cargo Book reference chapters the dependency family cites as its
/// "relevant configuration sections" (AC1): the feature-resolution reference,
/// the `Cargo.toml` manifest reference, and the package-sources/lockfile
/// reference. These are real top-level Cargo Book chapters, distinct from the
/// per-command `commands/cargo-<name>` chapters.
fn features_section() -> SectionId {
    cargo_section("features")
}

fn manifest_section() -> SectionId {
    cargo_section("manifest")
}

fn registry_section() -> SectionId {
    cargo_section("registry")
}

/// The Cargo Book `packaging` reference chapter — the release-preparation
/// reference a `cargo package` entry cites alongside its command chapter.
fn packaging_section() -> SectionId {
    cargo_section("packaging")
}

/// The Cargo Book `publishing` reference chapter — the release-preparation
/// reference a `cargo publish` entry cites alongside its command chapter and the
/// `registry` (package sources) reference.
fn publishing_section() -> SectionId {
    cargo_section("publishing")
}

fn cargo_add() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo add").expect("`cargo add` is a well-formed key"),
        purpose: "Add a dependency to one of this package's manifest groups.".to_string(),
        upstream_sections: vec![
            cargo_section("commands/cargo-add"),
            features_section(),
            manifest_section(),
        ],
        syntax: vec!["cargo add <SPEC> [OPTIONS]".to_string()],
        examples: vec![
            "`cargo add serde` adds `serde` to the `[dependencies]` group of the \
             current package's `Cargo.toml`, resolving it against the configured \
             sources and recording the chosen version in `Cargo.lock`."
                .to_string(),
            "`cargo add serde --features derive` records the `derive` feature in the \
             manifest's feature selection for the dependency."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package or workspace: a manifest (`Cargo.toml`) is present in the \
             working directory or an ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo add --help` for the version-specific option list. The flags \
             `--offline` and `--locked` are documented for this command: `--offline` \
             resolves from the local cache only, and `--locked` asserts the \
             lockfile will not be changed."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::SourceChange,
                detail: "writes the new dependency into `Cargo.toml` (a source \
                         change); it does not touch a package's own source files."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::LockfileChange,
                detail: "updates `Cargo.lock` with the resolved version; `--locked` \
                         asserts the lockfile will not be changed and errors \
                         otherwise, and `--offline` resolves from the local cache only."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download the new dependency from a configured source; \
                         `--offline` (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

fn cargo_remove() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo remove").expect("`cargo remove` is a well-formed key"),
        purpose: "Remove dependencies from the manifest.".to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-remove"), manifest_section()],
        syntax: vec!["cargo remove <SPEC>... [OPTIONS]".to_string()],
        examples: vec![
            "`cargo remove serde` removes `serde` from the `[dependencies]` group of \
             the current package's `Cargo.toml`."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package or workspace: a manifest (`Cargo.toml`) is present in the \
             working directory or an ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo remove --help` for the version-specific option list. The flags \
             `--offline` and `--locked` are documented for this command: `--offline` \
             resolves from the local cache only, and `--locked` asserts the \
             lockfile will not be changed."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::SourceChange,
                detail: "removes the dependency entry from `Cargo.toml` (a source \
                         change); it does not touch a package's own source files."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::LockfileChange,
                detail: "may rewrite `Cargo.lock` to drop the removed package; \
                         `--locked` asserts the lockfile will not be changed and \
                         errors otherwise, and `--offline` resolves from the local \
                         cache only."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download the remaining or replacement dependencies while \
                         re-resolving; `--offline` (or `--frozen`) prevents any \
                         download."
                    .to_string(),
            },
        ],
    }
}

fn cargo_update() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo update").expect("`cargo update` is a well-formed key"),
        purpose: "Update dependencies as specified by the `Cargo.lock` file.".to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-update"), registry_section()],
        syntax: vec!["cargo update [SPEC]... [OPTIONS]".to_string()],
        examples: vec![
            "`cargo update` refreshes `Cargo.lock` to the newest versions the \
             manifest's requirements allow; `cargo update --precise 1.0.0` pins a \
             specific dependency to `1.0.0` in the lockfile."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package or workspace with a manifest; a `Cargo.lock` is read or \
             created when re-resolving."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo update --help` for the version-specific option list. The flags \
             `--offline` and `--locked` are documented for this command: `--offline` \
             resolves from the local cache only, and `--locked` asserts the \
             lockfile will not be changed."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::LockfileChange,
                detail: "rewrites `Cargo.lock` with the newly resolved versions (a \
                         lockfile change; it does not edit `Cargo.toml`); `--locked` \
                         asserts the lockfile will not be changed and errors \
                         otherwise, and `--offline` resolves from the local cache only."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download the newly resolved sources; `--offline` \
                         (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

fn cargo_generate_lockfile() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo generate-lockfile")
            .expect("`cargo generate-lockfile` is a well-formed key"),
        purpose: "Generate a `Cargo.lock` lockfile without building.".to_string(),
        upstream_sections: vec![
            cargo_section("commands/cargo-generate-lockfile"),
            registry_section(),
        ],
        syntax: vec!["cargo generate-lockfile [OPTIONS]".to_string()],
        examples: vec![
            "`cargo generate-lockfile` resolves the package's dependencies against the \
             configured sources and writes `Cargo.lock` without building or running \
             any code."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package or workspace: a manifest (`Cargo.toml`) is present in the \
             working directory or an ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo generate-lockfile --help` for the version-specific option list. \
             The flags `--offline` and `--locked` are documented for this command: \
             `--offline` resolves from the local cache only, and `--locked` asserts \
             the lockfile will not be changed."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::LockfileChange,
                detail: "creates or refreshes `Cargo.lock` by resolving the manifest \
                         against the sources (a lockfile change; it does not edit \
                         `Cargo.toml`); `--locked` asserts the lockfile will not be \
                         changed and errors otherwise, and `--offline` resolves from \
                         the local cache only."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download sources during resolution; `--offline` \
                         (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

fn cargo_fetch() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo fetch").expect("`cargo fetch` is a well-formed key"),
        purpose: "Fetch all dependencies into the local cache.".to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-fetch"), registry_section()],
        syntax: vec!["cargo fetch [OPTIONS]".to_string()],
        examples: vec![
            "`cargo fetch` downloads every dependency the manifest resolves into the \
             local cache, so later `cargo build` / `cargo check` / `cargo test` can \
             run offline."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package or workspace: a manifest (`Cargo.toml`) is present in the \
             working directory or an ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo fetch --help` for the version-specific option list. The flag \
             `--offline` is documented for this command: it makes the fetch use the \
             local cache only."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "downloads the package's dependencies into the local cache; \
                         `--offline` (or `--frozen`) makes the fetch use the cache \
                         only (no network)."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::LockfileChange,
                detail: "may update `Cargo.lock` to match the manifest when the \
                         lockfile is missing or out of date (a lockfile change; it \
                         does not edit `Cargo.toml`)."
                    .to_string(),
            },
        ],
    }
}

// ---------------------------------------------------------------------------
// The two packaging / release-preparation entries
//
// [042] adds the fourth family of `plan.md` §6: `cargo package` and
// `cargo publish`, with the documented `--dry-run` behavior of `cargo publish`.
// As with the earlier families, each entry is a faithful, compact distillation of
// the Cargo Book section that documents the command, citing that command chapter
// *and* the relevant release-preparation reference chapter(s) — `packaging` for
// `cargo package`, `publishing` and `registry` for `cargo publish` (AC1). The
// possible publication effects are made explicit via an `EffectKind::Publication`
// note, and a separate maintained note states that this is *documentation lookup*
// that does not authorize, request, or perform a publication (AC2 / `plan.md`
// §6/§350). Upstream syntax/examples stay in their own fields. No entry here is
// executed: the catalog selects documentation, and the calling agent runs these
// commands through its own terminal tools.
// ---------------------------------------------------------------------------

/// `cargo package`: package the current package into a distributable `.crate`
/// archive.
///
/// The *package* side of release preparation: it builds a distributable
/// `target/<name>-<version>.crate` tarball and, by default, verifies the package
/// is distributable. It packages but does *not* publish — uploading is
/// `cargo publish`.
fn cargo_package() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo package").expect("`cargo package` is a well-formed key"),
        purpose: "Package the current package into a distributable `.crate` archive.".to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-package"), packaging_section()],
        syntax: vec!["cargo package [OPTIONS]".to_string()],
        examples: vec![
            "`cargo package` verifies the current package and writes a distributable \
             `.crate` tarball to `target/`; `cargo package --list` lists the files that \
             would be included without creating the archive; `--no-verify` skips the \
             verification build and `--allow-dirty` packs a working tree that has local \
             changes."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package with a complete manifest (`Cargo.toml`) whose `name`, \
             `version`, and license are set: packaging verifies these before producing \
             the archive."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo package --help` for the version-specific option list. `--list` lists \
             the files that would be packaged without creating the archive, `--no-verify` \
             skips the verification build, and `--allow-dirty` packs a working tree with \
             local or uncommitted changes."
                .to_string(),
            "`cargo package` produces the distributable `.crate` archive but does not \
             publish or upload it — uploading is `cargo publish`. Retrieving this entry \
             is documentation lookup: it surfaces packaging guidance and does not \
             authorize, request, or perform a publication; the calling agent decides \
             whether to run `cargo package` (and, later, `cargo publish`) through its own \
             terminal tools."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "produces a distributable `.crate` tarball under `target/` \
                         (the packaging artifact); it packages but does not publish or \
                         upload — uploading is `cargo publish`."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "the default verification build may download dependencies that \
                         are not yet cached; `--offline` (or `--frozen`) prevents any \
                         download. `--no-verify` skips the verification build."
                    .to_string(),
            },
        ],
    }
}

/// `cargo publish`: package and upload the current package to the configured
/// registry.
///
/// The *publish* side of release preparation: it packages the crate (as `cargo
/// package` does) and, by default, uploads the resulting `.crate` to the
/// configured registry — a publication. The documented `--dry-run` behavior is
/// *prepare-only*: it performs the same packaging and verification but does not
/// upload, reporting what would be published instead. The possible publication
/// effect is made explicit via an `EffectKind::Publication` note, and a separate
/// maintained note states that this is *documentation lookup* that does not
/// authorize, request, or perform a publication (AC2).
fn cargo_publish() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo publish").expect("`cargo publish` is a well-formed key"),
        purpose: "Package and upload the current package to the configured registry.".to_string(),
        upstream_sections: vec![
            cargo_section("commands/cargo-publish"),
            publishing_section(),
            registry_section(),
        ],
        syntax: vec!["cargo publish [OPTIONS]".to_string()],
        examples: vec![
            "`cargo publish` packages the current package (as `cargo package` does) and \
             uploads the resulting `.crate` to the configured registry; \
             `cargo publish --dry-run` performs the same packaging and verification but \
             does NOT upload — it reports what would be published."
                .to_string(),
        ],
        prerequisites: vec![
            "A publishable package — a complete manifest, a valid version, and a registry \
             name not already taken — plus registry authentication: a configured registry \
             token or credential. The default registry is crates.io."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo publish --help` for the version-specific option list. The registry is \
             selected with `--registry` and authenticated with `--token` (or a configured \
             credential); `--no-verify` skips the verification build. `--dry-run` performs \
             the packaging and verification but does NOT upload — it reports what would be \
             published. Publishing is the *default* behavior: omitting `--dry-run` is what \
             uploads, so `--dry-run` is the prepare-only form (there is no `--no-dry-run` \
             flag — the default already publishes)."
                .to_string(),
            "Release-preparation guidance is documentation lookup: retrieving this entry \
             surfaces documentation and does not authorize, request, or perform a \
             publication. No registry upload happens as a result of this lookup; the \
             calling agent decides whether to run `cargo publish` through its own terminal \
             tools. The effect note below names the possible publication effect; it is not \
             an authorization."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::Publication,
                detail: "uploads the packaged `.crate` to the configured registry (by \
                         default crates.io) — a publication that reaches the registry and \
                         its downstream users. This is the *default* behavior; \
                         `cargo publish --dry-run` performs the same packaging and \
                         verification but does NOT upload, reporting what would be \
                         published instead. This note names the possible publication effect; \
                         it is documentation, not an authorization — the lookup neither \
                         authorizes nor performs a publication."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "packages the crate into a distributable `.crate` archive under \
                         `target/` (as `cargo package` does) before publishing; \
                         `--no-verify` skips the default verification build."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "the verification build may download dependencies that are not yet \
                         cached; `--offline` (or `--frozen`) prevents any download. A \
                         `--dry-run` run still downloads for the verification but uploads \
                         nothing."
                    .to_string(),
            },
        ],
    }
}

// ---------------------------------------------------------------------------
// The eight toolchain and direct-tool entries
//
// [043] adds the fifth and final family of `plan.md` §6: the toolchain
// installer and diagnostics commands (`rustup show`, `rustup toolchain`,
// `rustup override`, `rustup component`, `rustup target`) and the direct-tool
// usage entries (`rustc`, `rustfmt`, `rustdoc`). As with the earlier families,
// each entry is a faithful, compact distillation of the upstream chapter that
// documents the command, citing that chapter (AC1) and keeping its
// prerequisites, version/stability qualifications, and effect notes in
// **separate** maintained fields (AC2 / `plan.md` §6/§350).
//
// The rustup *mutation* commands — `rustup toolchain` (install/link/use),
// `rustup override` (directory overrides), `rustup component` (add/remove
// components), and `rustup target` (add/list/remove host targets) — each carry a
// [`EffectKind::ToolchainChange`] effect note: they write into the user's rustup
// state. `rustup show` is an inspection command and carries no effect note. The
// direct-tool entries cite the reference chapters the tool defers to (the rustc
// `command-line-arguments` chapter, the rustfmt `README.md`/`Configurations.md`
// reference, and the rustdoc `commands`/`documentation-tests` chapters).
//
// The help/version mechanism and the stable/nightly/unknown compatibility
// qualification are maintained metadata that survives *separately* from the
// upstream excerpts: a documented nightly-only/unstable option keeps its
// "nightly"/"unstable" qualification rather than being flattened to an
// unconditional claim, and an unknown/undocumented-version note keeps its
// qualification too. No entry here is executed: the catalog selects
// documentation, and the calling agent runs these tools through its own terminal
// tools.
// ---------------------------------------------------------------------------

/// `rustup show`: display the active toolchain, the installed toolchains, and
/// the active override for the current directory.
///
/// The *inspection* side of the rustup family — like `cargo metadata` it reports
/// the installed state without changing it, so it carries no effect note. Its
/// help/version mechanism (`rustup show --help`, `rustup --version`, and the
/// `rustup show --active-toolchain` query) and its version qualification are
/// maintained separately from the upstream excerpt.
fn rustup_show() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("rustup show").expect("`rustup show` is a well-formed key"),
        purpose: "Display the active toolchain, the installed toolchains, and the \
                  active override for the current directory."
            .to_string(),
        upstream_sections: vec![rustup_chapter("index")],
        syntax: vec!["rustup show [OPTIONS]".to_string()],
        examples: vec![
            "`rustup show` prints the active toolchain (channel, host, release), the \
             installed toolchains, and any directory override in force; \
             `rustup show --active-toolchain` prints only the active toolchain name."
                .to_string(),
        ],
        prerequisites: vec![
            "`rustup` is installed (the Rust toolchain installer); the active toolchain \
             is the one the installer selected or pinned for the directory."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `rustup`; run \
             `rustup show --help` for the version-specific option list. The \
             help/version mechanism is `rustup --help`, `rustup --version`, and \
             `rustup show --active-toolchain` (which reports the toolchain name for \
             the current directory)."
                .to_string(),
            "When the active toolchain's version is unknown or differs from the \
             documented baseline, report the version `rustup show` actually returns \
             rather than assuming a documented version applies; that \
             version-qualification is preserved, not flattened to an unconditional \
             claim. Retrieving this entry is documentation lookup: it surfaces \
             toolchain-inspection guidance and does not change the toolchain; the \
             calling agent decides whether to run `rustup show` through its own \
             terminal tools."
                .to_string(),
        ],
        // Inspection command: it reports the installed/active state without
        // changing it, so it carries no maintained effect note.
        effect_notes: vec![],
    }
}

/// `rustup toolchain`: install, link, list, or use Rust toolchains.
///
/// The *install/link* side of the rustup family: it changes which toolchains are
/// available and which is active, so it carries a [`EffectKind::ToolchainChange`]
/// effect note (a *possible* change to the user's rustup state, not a permission
/// to make it). `rustup toolchain use` changes the active toolchain; the
/// `install`/`link` forms add toolchains.
fn rustup_toolchain() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("rustup toolchain").expect("`rustup toolchain` is a well-formed key"),
        purpose: "Install, link, list, or set the active Rust toolchain.".to_string(),
        upstream_sections: vec![rustup_chapter("toolchains")],
        syntax: vec![
            "rustup toolchain install <NAME> [OPTIONS]".to_string(),
            "rustup toolchain link <NAME> <PATH>".to_string(),
            "rustup toolchain list".to_string(),
            "rustup toolchain use <NAME>".to_string(),
        ],
        examples: vec![
            "`rustup toolchain install stable` installs the stable toolchain; \
             `rustup toolchain install nightly` installs a nightly channel; \
             `rustup toolchain link my-toolchain /path/to/rustc` registers a local \
             toolchain; `rustup toolchain use stable` makes a channel the active \
             toolchain for the current directory."
                .to_string(),
        ],
        prerequisites: vec![
            "`rustup` is installed; installing or linking a toolchain may fetch it \
             from a configured toolchain source."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `rustup`; run \
             `rustup toolchain --help` for the version-specific option list. The \
             help/version mechanism is `rustup toolchain --help` (and \
             `rustup --version` for the installer version); channel availability \
             (for example `nightly`) is documented per channel, and the \
             stable/nightly qualification is preserved, not flattened to \
             unconditional."
                .to_string(),
            "Retrieving this entry is documentation lookup: it surfaces \
             toolchain-management guidance and does not install, link, or switch a \
             toolchain. No toolchain is changed as a result of this lookup; the \
             calling agent decides whether to run `rustup toolchain` through its \
             own terminal tools."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::ToolchainChange,
            detail: "changes the user's rustup toolchain state: `install`/`link` \
                     add a toolchain (a possible download of the toolchain \
                     sources), and `use`/`override` change the active toolchain for \
                     a directory. This note names the possible toolchain change; \
                     it is documentation, not a permission to make the change."
                .to_string(),
        }],
    }
}

/// `rustup override`: set or inspect the directory toolchain override.
///
/// A *mutation* of the user's rustup state: `rustup override set <NAME>` pins the
/// toolchain for the current directory (written to rustup's override store, or
/// equivalently via a `rust-toolchain.toml` file), so it carries a
/// [`EffectKind::ToolchainChange`] effect note. It cites both the `overrides`
/// chapter and the `toolchain-file` chapter, which documents the
/// `rust-toolchain.toml` form.
fn rustup_override() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("rustup override").expect("`rustup override` is a well-formed key"),
        purpose: "Set, list, or remove the directory toolchain override.".to_string(),
        upstream_sections: vec![
            rustup_chapter("overrides"),
            rustup_chapter("toolchain-file"),
        ],
        syntax: vec![
            "rustup override set <TOOLCHAIN>".to_string(),
            "rustup override list".to_string(),
            "rustup override unset [PATH]".to_string(),
        ],
        examples: vec![
            "`rustup override set nightly` pins the current directory to the nightly \
             toolchain; `rustup override list` shows the directory overrides; \
             `rustup override unset` removes them. A `rust-toolchain.toml` file in \
             the directory pins the toolchain, overrides, and components in place."
                .to_string(),
        ],
        prerequisites: vec![
            "`rustup` is installed; `rustup override set` requires the referenced \
             toolchain to be installed (or it is fetched)."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `rustup`; run \
             `rustup override --help` for the version-specific option list. The \
             help/version mechanism is `rustup override --help`; the override \
             channel (for example `nightly`) is documented per channel and its \
             stable/nightly qualification is preserved, not flattened."
                .to_string(),
            "Retrieving this entry is documentation lookup: it surfaces \
             override-management guidance and does not set, list, or remove an \
             override. No directory override is changed as a result of this lookup; \
             the calling agent decides whether to run `rustup override` through its \
             own terminal tools."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::ToolchainChange,
            detail: "`rustup override set` writes a directory override into rustup's \
                     override store (or a `rust-toolchain.toml` file), changing which \
                     toolchain runs in that directory; `unset` removes it. This note \
                     names the possible toolchain change; it is documentation, not a \
                     permission to make the change."
                .to_string(),
        }],
    }
}

/// `rustup component`: add, remove, or list toolchain components.
///
/// A *mutation* of the user's rustup state: `rustup component add`/`remove`
/// change which components (for example `rustfmt`, `clippy`, `rust-src`,
/// `rust-docs`) are installed for a toolchain, so it carries a
/// [`EffectKind::ToolchainChange`] effect note.
fn rustup_component() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("rustup component").expect("`rustup component` is a well-formed key"),
        purpose: "Add, remove, or list the components of a toolchain.".to_string(),
        upstream_sections: vec![rustup_chapter("components")],
        syntax: vec![
            "rustup component add <NAME> [OPTIONS]".to_string(),
            "rustup component remove <NAME>".to_string(),
            "rustup component list [OPTIONS]".to_string(),
        ],
        examples: vec![
            "`rustup component add rustfmt clippy` installs the rustfmt and Clippy \
             components for the active toolchain; `rustup component list` shows the \
             installed, available, and up-to-date components; `rustup component \
             remove rustfmt` uninstalls a component."
                .to_string(),
        ],
        prerequisites: vec![
            "`rustup` is installed with an active toolchain; adding a component may \
             fetch it from the configured component source."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `rustup`; run \
             `rustup component --help` for the version-specific option list. The \
             help/version mechanism is `rustup component --help`; component \
             availability varies by channel and host, and that qualification is \
             preserved, not flattened to unconditional."
                .to_string(),
            "Retrieving this entry is documentation lookup: it surfaces \
             component-management guidance and does not add or remove a component. \
             No component is installed or removed as a result of this lookup; the \
             calling agent decides whether to run `rustup component` through its own \
             terminal tools."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::ToolchainChange,
            detail: "`rustup component add`/`remove` change which components are \
                     installed for the active toolchain (a possible download when \
                     adding). This note names the possible toolchain change; it is \
                     documentation, not a permission to make the change."
                .to_string(),
        }],
    }
}

/// `rustup target`: add, list, or remove host/target triples for a toolchain.
///
/// A *mutation* of the user's rustup state: `rustup target add`/`remove` change
/// which target triples are available for a toolchain, so it carries a
/// [`EffectKind::ToolchainChange`] effect note.
fn rustup_target() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("rustup target").expect("`rustup target` is a well-formed key"),
        purpose: "Add, list, or remove target triples for a toolchain.".to_string(),
        upstream_sections: vec![rustup_chapter("targets")],
        syntax: vec![
            "rustup target add <TARGET> [OPTIONS]".to_string(),
            "rustup target list [OPTIONS]".to_string(),
            "rustup target remove <TARGET>".to_string(),
        ],
        examples: vec![
            "`rustup target add x86_64-unknown-linux-gnu` makes a target triple \
             available for cross-compilation; `rustup target list --installed` shows \
             the installed targets; `rustup target remove x86_64-unknown-linux-gnu` \
             removes one."
                .to_string(),
        ],
        prerequisites: vec![
            "`rustup` is installed with an active toolchain; adding a target may \
             fetch it from the configured target source."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `rustup`; run \
             `rustup target --help` for the version-specific option list. The \
             help/version mechanism is `rustup target --help`; which target triples \
             are available varies by host and channel, and that qualification is \
             preserved, not flattened to unconditional."
                .to_string(),
            "Retrieving this entry is documentation lookup: it surfaces \
             target-management guidance and does not add or remove a target. No \
             target triple is changed as a result of this lookup; the calling agent \
             decides whether to run `rustup target` through its own terminal tools."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::ToolchainChange,
            detail: "`rustup target add`/`remove` change which target triples are \
                     available for the active toolchain (a possible download when \
                     adding). This note names the possible toolchain change; it is \
                     documentation, not a permission to make the change."
                .to_string(),
        }],
    }
}

/// `rustc`: the Rust compiler — documented usage, options, and flags.
///
/// A *direct-tool* entry: the catalog documents how the calling agent runs
/// `rustc` through its own terminal tools, citing the rustc Book's
/// `command-line-arguments` chapter (and the `index` introduction). The
/// documented syntax/options (for example `rustc --edition 2021`) are upstream;
/// the help/version mechanism and the stable/nightly/unknown qualification are
/// maintained separately. `rustc` produces a compilation artifact but does not
/// run the project's own code as its defining effect, so it carries an
/// [`EffectKind::ArtifactWrite`] note.
fn rustc_usage() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("rustc").expect("`rustc` is a well-formed key"),
        purpose: "The Rust compiler; document its usage, options, and flags.".to_string(),
        upstream_sections: vec![
            rustc_chapter("command-line-arguments"),
            rustc_chapter("index"),
        ],
        syntax: vec!["rustc [OPTIONS] <input>".to_string()],
        examples: vec![
            "`rustc main.rs --edition 2021` compiles `main.rs` using edition 2021; \
             `rustc --version` prints the compiler version; `rustc --help` (and \
             `rustc -Z help` on a nightly build) lists the options."
                .to_string(),
        ],
        prerequisites: vec![
            "A Rust compiler (`rustc`) is installed, typically as part of a toolchain \
             managed by `rustup`; the edition and options used must be supported by \
             the installed compiler."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `rustc`; run \
             `rustc --help` for the version-specific option list. The help/version \
             mechanism is `rustc --help` and `rustc --version`; `--edition` selects \
             the language edition the compiler supports."
                .to_string(),
            "Some `rustc` options are unstable or nightly-only; the documentation \
             marks them as such and `rustc -Z help` (on a nightly build) lists the \
             unstable options. A documented unstable/nightly-only option keeps its \
             `nightly`/`unstable` qualification — it is not flattened to an \
             unconditional claim that it is available on every build. When the \
             installed compiler's version is unknown or differs from the documented \
             baseline, consult the version that is installed rather than assuming the \
             documented options apply; that version-qualification is preserved."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::ArtifactWrite,
            detail: "`rustc` compiles its input and writes the compilation output \
                     (a binary, object file, or library — a build artifact) under \
                     the working directory or `target/`. It compiles a single \
                     crate; it does not execute the project's own code as its \
                     defining effect. This note names the possible artifact write; \
                     it is documentation, not a permission to run `rustc`."
                .to_string(),
        }],
    }
}

/// `rustfmt`: the Rust code formatter — documented usage and configuration.
///
/// A *direct-tool* entry that cites the same rustfmt reference the `cargo fmt`
/// entry cites (the `README.md` usage and `Configurations.md` configuration
/// reference). The documented syntax/options (for example `rustfmt --check`,
/// `--config`) are upstream; the help/version mechanism and the stable/unstable
/// qualification are maintained separately. `rustfmt` formats source in place
/// (a source change), except `--check` mode, which only checks.
fn rustfmt_usage() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("rustfmt").expect("`rustfmt` is a well-formed key"),
        purpose: "The Rust code formatter; document its usage and configuration.".to_string(),
        upstream_sections: vec![
            rustfmt_section("README.md"),
            rustfmt_section("Configurations.md"),
        ],
        syntax: vec!["rustfmt [OPTIONS] [FILES...]".to_string()],
        examples: vec![
            "`rustfmt src/main.rs` formats the file in place; `rustfmt --check \
             src/main.rs` reports formatting differences without writing; a \
             `rustfmt.toml` file (or `--config`) sets per-project options."
                .to_string(),
        ],
        prerequisites: vec![
            "The `rustfmt` binary is installed, typically as the `rustfmt` component \
             of a toolchain (for example via `rustup component add rustfmt`)."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `rustfmt`; run \
             `rustfmt --help` for the version-specific option list. The \
             help/version mechanism is `rustfmt --help` and `rustfmt --version`; \
             `--check` runs in check mode and `--config` selects an option."
                .to_string(),
            "Unstable rustfmt options require a nightly `rustfmt` (selected via \
             `RUSTUP_TOOLCHAIN=nightly` or `RUSTC_BOOTSTRAP=1`); a documented \
             unstable option keeps its `unstable`/`nightly` qualification — it is \
             not flattened to an unconditional claim that it works on a stable \
             `rustfmt`. When the installed `rustfmt`'s version is unknown or differs \
             from the documented baseline, consult the version that is installed; \
             that version-qualification is preserved."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::SourceChange,
            detail: "`rustfmt` reformats the target source files in place (a source \
                     change); `rustfmt --check` only checks formatting and does not \
                     write. This note names the possible source change; it is \
                     documentation, not a permission to run `rustfmt`."
                .to_string(),
        }],
    }
}

/// `rustdoc`: the Rust documentation generator — documented usage and
/// configuration.
///
/// A *direct-tool* entry that cites the rustdoc Book's `commands` chapter (the
/// documented options) and the `documentation-tests` chapter (the doctests
/// `rustdoc` and `cargo test --doc` run). The documented syntax/options (for
/// example `rustdoc --document-private-items`) are upstream; the help/version
/// mechanism and the stable/nightly/unknown qualification are maintained
/// separately. `rustdoc` produces an HTML documentation tree (an artifact).
fn rustdoc_usage() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("rustdoc").expect("`rustdoc` is a well-formed key"),
        purpose: "The Rust documentation generator; document its usage and \
                  configuration."
            .to_string(),
        upstream_sections: vec![
            rustdoc_section("commands"),
            rustdoc_section("documentation-tests"),
        ],
        syntax: vec!["rustdoc [OPTIONS] <INPUT>".to_string()],
        examples: vec![
            "`rustdoc src/lib.rs` generates HTML documentation; `rustdoc \
             --document-private-items` also documents private items; `rustdoc \
             --check` runs the documentation tests (doctests) only, and `cargo \
             test --doc` does the same for a package."
                .to_string(),
        ],
        prerequisites: vec![
            "The `rustdoc` binary is installed, typically as part of a toolchain \
             (the `rust-docs` component enables rendering of intra-doc links); a \
             library or binary crate is documented as input."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `rustdoc`; run \
             `rustdoc --help` for the version-specific option list. The \
             help/version mechanism is `rustdoc --help` and `rustdoc --version`; \
             `--document-private-items` is documented and stable, while some other \
             rustdoc options are unstable."
                .to_string(),
            "Some rustdoc options are unstable and require a nightly toolchain (for \
             example via `RUSTC_BOOTSTRAP=1` or `RUSTUP_TOOLCHAIN=nightly`); a \
             documented unstable/nightly-only option keeps its `nightly`/`unstable` \
             qualification — it is not flattened to an unconditional claim that it is \
             available on a stable build. When the installed `rustdoc`'s version is \
             unknown or differs from the documented baseline, consult the version that \
             is installed; that version-qualification is preserved."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::ArtifactWrite,
            detail: "`rustdoc` writes an HTML documentation tree under `target/doc` \
                     (a documentation artifact); with `--check` it runs the \
                     documentation tests and reports without writing the tree. This \
                     note names the possible artifact write; it is documentation, not \
                     a permission to run `rustdoc`."
                .to_string(),
        }],
    }
}

// ---------------------------------------------------------------------------
// The nine implementation/verification entries
//
// [041] adds the third family of `plan.md` §6: `cargo check`, `cargo build`,
// `cargo run`, `cargo test`, `cargo bench`, `cargo fmt`, `cargo clippy`,
// `cargo doc`, and `cargo fix`. As with the earlier families, each entry is a
// faithful, compact distillation of the upstream section that documents the
// command, citing that command section *and* the relevant tooling chapter the
// command defers to (rustfmt for `cargo fmt`, Clippy for `cargo clippy`, rustdoc
// for `cargo doc`, and the rustdoc documentation-tests chapter for `cargo test`
// doctests). The maintained metadata — selection/forwarding notes, the
// check-vs-apply distinction, the doctest-vs-test distinction, and documented
// nightly qualifications — lives in the separate `compatibility` and `effect_notes`
// fields (AC2 / `plan.md` §6/§350). Upstream syntax/examples stay in their own
// fields. No entry here is executed.
//
// [042] adds the fourth family — packaging and release preparation:
// `cargo package` and `cargo publish`, with the documented `--dry-run` behavior of
// `cargo publish` (it packages and verifies but does not upload; publishing is the
// default). Each entry cites its command chapter *and* the relevant
// release-preparation reference chapters (`packaging` for `cargo package`,
// `publishing` and `registry` for `cargo publish`). The possible publication
// effects are made explicit via an [`EffectKind::Publication`] effect note, and a
// separate maintained note states that this is *documentation lookup* that does
// not authorize, request, or perform a publication — the calling agent decides
// whether to run `cargo publish` through its own terminal tools. No entry here is
// executed.
// ---------------------------------------------------------------------------

/// `cargo check`: compile to check for errors without producing final artifacts.
///
/// The *check* side of the check-vs-apply distinction: it writes intermediate
/// build metadata to `target/` but produces no final artifact and runs no code.
/// It does not execute a program, so it has no program-argument forwarding after
/// `--`; compiler flags are supplied through `RUSTFLAGS` or the build options.
fn cargo_check() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo check").expect("`cargo check` is a well-formed key"),
        purpose: "Compile the current package to check for errors, without \
                  generating a final artifact."
            .to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-check")],
        syntax: vec!["cargo check [OPTIONS]".to_string()],
        examples: vec![
            "`cargo check` compiles the package to the point of type-checking and \
             borrow-checking and reports errors, producing no final binary or library \
             artifact; it is the fast path for iterative development."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package or workspace: a manifest (`Cargo.toml`) is present in the \
             working directory or an ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo check --help` for the version-specific option list. The \
             selection flags `-p`/`--package`, `-w`/`--workspace`, `--target`, and \
             `--features`/`--all-features` choose the package(s), target, and \
             feature set to check; `--offline` resolves from the local cache only \
             and `--locked` asserts the lockfile will not be changed."
                .to_string(),
            "`cargo check` does not execute a program, so it has no \
             program-argument forwarding after `--`; compiler flags are supplied \
             through `RUSTFLAGS` or the documented build options rather than a \
             program-argument separator."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "writes intermediate build metadata to the `target/` \
                         directory; per its documented behavior it produces no \
                         final binary or library artifact and runs no code — the \
                         check side of the check-vs-apply distinction."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download dependencies that are not yet cached; \
                         `--offline` (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

/// `cargo build`: compile the package, producing its binary or library artifacts.
///
/// The *produce-artifacts* side of the check-vs-apply distinction: it writes the
/// final build output to `target/`. Like `cargo check` it does not execute a
/// program, so it has no program-argument forwarding after `--`.
fn cargo_build() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo build").expect("`cargo build` is a well-formed key"),
        purpose: "Compile the current package, producing its binary or library \
                  artifacts."
            .to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-build")],
        syntax: vec!["cargo build [OPTIONS]".to_string()],
        examples: vec![
            "`cargo build` compiles the current package and writes its artifacts (a \
             binary for a `bin` target, a library for a `lib` target) under \
             `target/`; `cargo build --release` builds an optimized release \
             profile."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package or workspace: a manifest (`Cargo.toml`) is present in the \
             working directory or an ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo build --help` for the version-specific option list. The \
             selection flags `-p`/`--package`, `-w`/`--workspace`, `--target`, and \
             `--features`/`--all-features` choose the package(s), target, and \
             feature set to build; `--offline` resolves from the local cache only \
             and `--locked` asserts the lockfile will not be changed."
                .to_string(),
            "`cargo build` does not execute a program, so it has no \
             program-argument forwarding after `--`; compiler flags are supplied \
             through `RUSTFLAGS` or the documented build options rather than a \
             program-argument separator."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "produces the final build artifacts (a binary and/or \
                         library) under `target/` — the build-output side effect \
                         that distinguishes `cargo build` from the check \
                         commands."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download dependencies that are not yet cached; \
                         `--offline` (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

/// `cargo run`: compile and then execute the package's binary.
///
/// The *execute* side of the family: it runs the package's own code, and — unlike
/// `cargo check`/`cargo build` — it forwards the arguments after `--` to the
/// program being run.
fn cargo_run() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo run").expect("`cargo run` is a well-formed key"),
        purpose: "Compile and then execute the current package's binary.".to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-run")],
        syntax: vec!["cargo run [OPTIONS] [-- <args>]".to_string()],
        examples: vec![
            "`cargo run` compiles the package and runs its binary; `cargo run -- \
             --help` compiles the binary and forwards `--help` to the program — \
             everything after `--` is the program's argument list."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package with an executable target (`bin` or `example`): a \
             manifest (`Cargo.toml`) is present in the working directory or an \
             ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo run --help` for the version-specific option list. The \
             selection flags `-p`/`--package`, `-w`/`--workspace`, `--target`, and \
             `--features`/`--all-features` choose the package(s), target, and \
             feature set to run; `--offline` resolves from the local cache only and \
             `--locked` asserts the lockfile will not be changed."
                .to_string(),
            "Argument forwarding: everything after `--` is forwarded to the program \
             being run, and nothing before `--` is (those are cargo options). This \
             is the one of the compile commands that executes a program, so it is \
             the one that forwards program arguments."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::ProjectCodeExecution,
                detail: "compiles the package and then runs its own binary (its \
                         code executes) — the execute side of the family."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "produces build artifacts under `target/` before running.".to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download dependencies that are not yet cached; \
                         `--offline` (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

/// `cargo test`: compile and run the package's tests, including doctests.
///
/// It both *executes* code (the doctest-vs-test and execute distinctions) and is
/// the command that runs doctests: `cargo test` runs unit, integration, and
/// doctests, while `cargo test --doc` runs doctests only. It cites the rustdoc
/// book's documentation-tests chapter for the doctest behavior.
fn cargo_test() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo test").expect("`cargo test` is a well-formed key"),
        purpose: "Compile and run the current package's tests, including the \
                  documentation tests (doctests)."
            .to_string(),
        upstream_sections: vec![
            cargo_section("commands/cargo-test"),
            rustdoc_section("documentation-tests"),
        ],
        syntax: vec!["cargo test [OPTIONS] [TESTNAME] [-- <args>]".to_string()],
        examples: vec![
            "`cargo test` compiles and runs the package's unit, integration, and \
             doctests; `cargo test --lib` runs only the library's unit tests and \
             `cargo test --bins` only the binaries' tests."
                .to_string(),
            "`cargo test --doc` runs the documentation tests (doctests) only — the \
             doctest-vs-other-tests distinction; a doctest is the fenced code in a \
             `///` or `//!` doc comment that `cargo test --doc` compiles and runs."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package or workspace with tests or doctests: a manifest \
             (`Cargo.toml`) is present in the working directory or an ancestor of \
             it."
            .to_string(),
        ],
        compatibility: vec![
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo test --help` for the version-specific option list. The \
             selection flags `-p`/`--package`, `-w`/`--workspace`, `--target`, and \
             `--features`/`--all-features` choose the package(s), target, and \
             feature set to test; `--offline` resolves from the local cache only and \
             `--locked` asserts the lockfile will not be changed."
                .to_string(),
            "Doctests are the fenced code in doc comments: `cargo test` runs them \
             together with the unit and integration tests, while `cargo test --doc` \
             runs the doctests only. Arguments after `--` are forwarded to the test \
             binary; `--lib`/`--bins`/`--tests`/`--doc` select which target types \
             run."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::ProjectCodeExecution,
                detail: "compiles and runs the package's tests — unit, \
                         integration, and doctests execute — the execute side of \
                         the family."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "produces the test-binary artifacts under `target/`.".to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download dependencies that are not yet cached; \
                         `--offline` (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

/// `cargo bench`: compile and run the package's benchmarks.
///
/// It *executes* the benchmark code. The documented-nightly qualification here is
/// precise: `cargo bench` itself is stable, but the built-in `#[bench]` test-
/// harness macros were nightly-only and have since been removed, so stable
/// benchmarking today uses the `criterion` crate — no nightly is required for
/// `cargo bench` itself.
fn cargo_bench() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo bench").expect("`cargo bench` is a well-formed key"),
        purpose: "Compile and run the current package's benchmarks.".to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-bench")],
        syntax: vec!["cargo bench [OPTIONS] [BENCHNAME] [-- <args>]".to_string()],
        examples: vec![
            "`cargo bench` compiles and runs the package's `[[bench]]` targets; \
             `cargo bench -- --exact` forwards `--exact` to the benchmark harness."
                .to_string(),
        ],
        prerequisites: vec![
            "A Cargo package with a `[[bench]]` target (commonly the `criterion` \
             crate): a manifest (`Cargo.toml`) is present in the working directory \
             or an ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "`cargo bench` is a stable command. The built-in `#[bench]` test-harness \
             macros were nightly-only and have since been removed; stable \
             benchmarking today uses the `criterion` crate with a `[[bench]]` \
             target — no nightly toolchain is required for `cargo bench` itself, \
             and no nightly requirement is claimed that the documentation does not \
             state."
                .to_string(),
            "Syntax and options are documented for the installed `cargo`; run \
             `cargo bench --help` for the version-specific option list. The \
             selection flags `-p`/`--package`, `-w`/`--workspace`, `--target`, and \
             `--features`/`--all-features` choose the package(s), target, and \
             feature set to benchmark; `--offline` resolves from the local cache \
             only. Arguments after `--` are forwarded to the benchmark binary."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::ProjectCodeExecution,
                detail: "compiles and runs the package's benchmarks — their code \
                         executes."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "produces the benchmark-binary artifacts under `target/`.".to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download dependencies that are not yet cached; \
                         `--offline` (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

/// `cargo fmt`: run rustfmt on the package to format its source.
///
/// The *apply* counterpart to `cargo fmt --check`: `cargo fmt` rewrites the
/// source files (a source change), while `cargo fmt --check` (a.k.a. `-n`) runs
/// the same pass without writing. It cites the rustfmt usage/configuration
/// reference, and — a per-command scoping point — it does *not* document
/// `--target` or `--features`, which are not rustfmt options.
fn cargo_fmt() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo fmt").expect("`cargo fmt` is a well-formed key"),
        purpose: "Run rustfmt on the current package to format its source code.".to_string(),
        upstream_sections: vec![
            cargo_section("commands/cargo-fmt"),
            rustfmt_section("README.md"),
            rustfmt_section("Configurations.md"),
        ],
        syntax: vec!["cargo fmt [OPTIONS] [-- <rustfmt-args>]".to_string()],
        examples: vec![
            "`cargo fmt` runs rustfmt and rewrites the package's source files to the \
             canonical formatting; `cargo fmt --check` (equivalently `-n`) reports \
             what would change without writing — the check-vs-apply distinction for \
             formatting."
                .to_string(),
            "`cargo fmt -- --config tab_spaces=4` forwards the rustfmt option after \
             `--`; rustfmt's own options follow the separator, distinct from cargo's \
             options before it."
                .to_string(),
        ],
        prerequisites: vec![
            "A Rust toolchain that includes `rustfmt` (for example via the \
             `rustup` default components)."
                .to_string(),
        ],
        compatibility: vec![
            "`cargo fmt` is a stable command that invokes `rustfmt`; only \
             rustfmt's *unstable* options require a nightly rustfmt, not the command \
             or its stable options. Cargo options (`--manifest-path`, `--all` / \
             `--all-packages`, and `-p`/`--package`) select the package(s) to \
             format and are distinct from rustfmt options, which follow `--`; \
             `--target` and `--features` are not rustfmt options and are not \
             forwarded to it."
                .to_string(),
        ],
        effect_notes: vec![EffectNote {
            kind: EffectKind::SourceChange,
            detail: "rustfmt rewrites the package's source files to apply \
                         canonical formatting (a source change); it does not change \
                         the program's behavior. `cargo fmt --check` (a.k.a. `-n`) \
                         performs the same analysis without writing."
                .to_string(),
        }],
    }
}

/// `cargo clippy`: run Clippy's lints on the package.
///
/// By default it is a *check*: it compiles the package under Clippy's lints and
/// reports them without modifying source. `cargo clippy --fix` is the apply
/// variant. It cites the Clippy book's usage and configuration chapters, and
/// documents the lint-level nuance (`-A`/`-W`/`-D`/`-F`, groups, `-D warnings`).
fn cargo_clippy() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo clippy").expect("`cargo clippy` is a well-formed key"),
        purpose: "Run Clippy's lints on the current package to find things that \
                  can be simplified or done better."
            .to_string(),
        upstream_sections: vec![clippy_section("usage"), clippy_section("configuration")],
        syntax: vec!["cargo clippy [OPTIONS] [-- <args>]".to_string()],
        examples: vec![
            "`cargo clippy --all-targets -D warnings` compiles the package under \
             Clippy's lints and treats warnings as errors; `cargo clippy --fix` \
             instead applies the lints' machine-applicable suggestions."
                .to_string(),
        ],
        prerequisites: vec![
            "A Rust toolchain that includes the `clippy` component (for example via \
             `rustup`)."
                .to_string(),
        ],
        compatibility: vec![
            "`cargo clippy` is a stable command; Clippy runs on a stable toolchain, \
             although a small number of lints are nightly-only. Lint levels are set \
             with `-A`/`--allow`, `-W`/`--warn`, `-D`/`--deny`, and \
             `-F`/`--forbid` (or by lint group, e.g. `clippy::pedantic`); \
             `-D warnings` makes warnings fatal. The full, version-specific option \
             list is in `cargo clippy --help`."
                .to_string(),
            "Clippy/compiler options are distinct from cargo options: clippy lint \
             levels and rustc flags are forwarded after `--`, while the selection \
             flags `-p`/`--package`, `-w`/`--workspace`, `--target`, and \
             `--features`/`--all-features` and `--offline`/`--locked` are cargo \
             options before it."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "compiles the package under Clippy's lints, writing build \
                         artifacts to `target/`; by default it *reports* lints and \
                         does not modify source — a check. `cargo clippy --fix` \
                         instead applies the machine-applicable suggestions."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download dependencies that are not yet cached; \
                         `--offline` (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

/// `cargo doc`: generate the package's documentation by running rustdoc.
///
/// It produces an artifact (the generated HTML under `target/doc`) but does not
/// modify source and does *not* run doctests — doctests are executed by `cargo
/// test`, not by `cargo doc`. It cites the rustdoc book's cargo-integration
/// chapter.
fn cargo_doc() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo doc").expect("`cargo doc` is a well-formed key"),
        purpose: "Generate the current package's documentation by running rustdoc.".to_string(),
        upstream_sections: vec![
            cargo_section("commands/cargo-doc"),
            rustdoc_section("cargo-integration"),
        ],
        syntax: vec!["cargo doc [OPTIONS] [-- <args>]".to_string()],
        examples: vec![
            "`cargo doc` runs rustdoc and writes HTML documentation for the \
             package's library and binaries to `target/doc`; `cargo doc --open` \
             opens the result in a browser and `cargo doc -- --help` forwards \
             `--help` to rustdoc."
                .to_string(),
        ],
        prerequisites: vec![
            "A Rust toolchain that includes `rustdoc` (for example via `rustup`): \
             a manifest (`Cargo.toml`) is present in the working directory or an \
             ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "`cargo doc` is a stable command that invokes `rustdoc`; rustdoc options \
             follow `--` and are distinct from cargo options. The selection flags \
             `-p`/`--package`, `-w`/`--workspace`, `--target`, and \
             `--features`/`--all-features` and `--offline`/`--locked` are cargo \
             options. Doctests are *executed* by `cargo test`, not by `cargo doc`."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "runs rustdoc and writes generated HTML documentation under \
                         `target/doc` (an artifact); it does not modify the source \
                         tree and does not run the doctests."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "may download dependencies that are not yet cached; \
                         `--offline` (or `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

/// `cargo fix`: apply the fixes the compiler suggests for the package's warnings.
///
/// The *apply* counterpart to the check commands: it rewrites the source files
/// with the compiler's suggested fixes (a source change). It is stable and needs
/// no nightly toolchain.
fn cargo_fix() -> CommandEntry {
    CommandEntry {
        key: CommandKey::new("cargo fix").expect("`cargo fix` is a well-formed key"),
        purpose: "Apply the fixes the compiler suggests for the current \
                  package's warnings."
            .to_string(),
        upstream_sections: vec![cargo_section("commands/cargo-fix")],
        syntax: vec!["cargo fix [OPTIONS] [-- <args>]".to_string()],
        examples: vec![
            "`cargo fix` runs the compiler with its suggestions and applies the \
             machine-applicable fixes to the source; review the diff afterward. \
             `cargo fix --broken-code` allows applying fixes that may leave the code \
             non-compiling."
                .to_string(),
        ],
        prerequisites: vec![
            "Run `cargo check` or `cargo build` first so the compiler has \
             suggestions to apply; a manifest (`Cargo.toml`) is present in the \
             working directory or an ancestor of it."
                .to_string(),
        ],
        compatibility: vec![
            "`cargo fix` is a stable command that applies the compiler's own \
             suggestions and requires no nightly toolchain. By default it refuses to \
             leave the code in a non-compiling state; `--broken-code` relaxes that. \
             The selection flags `-p`/`--package`, `-w`/`--workspace`, `--target`, and \
             `--features`/`--all-features` are cargo options, and arguments after `--` \
             are forwarded to the compiler."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::SourceChange,
                detail: "applies the compiler's suggested fixes to the package's \
                         source files (a source change — the apply counterpart to \
                         the check commands); review the diff after running, and use \
                         `--broken-code` only where a non-compiling intermediate is \
                         acceptable."
                    .to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "compiles the package to gather suggestions and may download \
                         dependencies that are not yet cached; `--offline` (or \
                         `--frozen`) prevents any download."
                    .to_string(),
            },
        ],
    }
}

// ---------------------------------------------------------------------------
// Task [044][M3]: the bounded command-guidance lookup and command inventory
//
// This is the *serving* side of the command catalog that `[039]`-`[043]` built.
// A bounded `get_command_guidance` lookup and a discoverable command inventory
// are *composed* from the already-tested seams rather than reinvented:
//
//   * the `[039]`-`[043]` [`CommandCatalog`] + [`CommandCatalog::resolve`] gives
//     the exact-key entry and proves every cited upstream section resolves in the
//     active snapshot (AC1's "source-backed sections"); a dangling citation is a
//     typed [`LookupError::UnresolvedSection`] — no section is ever fabricated;
//   * the `[036]` `crate::reads::get_section` seam turns each resolved section
//     into a complete [`SectionRead`] (Markdown body + heading breadcrumb +
//     citations + freshness + snapshot identity);
//   * the `[037]` `crate::pagination` seam — [`paginate_items`] / [`Cursor`] /
//     [`CursorError`] under the reserved [`Operation::Catalog`] — bounds the
//     assembled response to [`MAX_RESPONSE_BYTES`] with an *explicit* continuation
//     cursor and a truncation flag, so a clipped response is never silent and the
//     pages reassemble into the whole;
//
// while the `[007]` [`CommandKey`] boundary rejects an unsupported key (with the
// catalog's supported keys as the discoverable alternatives) and a shell-operator
// key. The lookup therefore has **no subprocess-execution path**: it returns
// guidance only, and the calling agent runs a command through its own terminal
// tools. No entry here is ever executed.
//
// ## No process-execution path (AC2)
//
// This module spawns no process: it holds no standard-library process
// construction, no process-spawn call, and no command execution — the catalog
// *selects documentation*, it never interpolates a key into a process. A
// source-scan test (`command_lookup_has_no_subprocess_execution_path`) enforces
// this guarantee against this very module, a documented grep check the
// compiler-independent way.
//
// ## The two seams and their bounds
//
// * `command_guidance` — resolve the exact key, compose each cited section into a
//   [`SectionRead`] via `[036]`, and page those section reads via `[037]` under
//   `Operation::Catalog` bound to the command key. The entry's metadata
//   (`syntax` / `examples` / `prerequisites` / `compatibility`) and its
//   separately-labeled [`EffectNote`]s ride on every page (they are small and
//   always fit); the section *content* is the bulk the cap bounds.
// * `command_inventory` — the discoverable catalog: the supported keys, paged via
//   the `[037]` [`Cursor`] / [`CursorError`] / [`MAX_RESPONSE_BYTES`] primitives
//   under `Operation::Catalog`, snapshot-bound (a cursor from another snapshot is
//   a [`CursorError::Mismatch`]) so a caller enumerates every key across pages
//   rather than receiving a silent full dump.
// ---------------------------------------------------------------------------

/// A failure to serve command guidance, mapping each composed seam's error into
/// one typed boundary so a caller reacts to the right cause.
///
/// * [`Lookup`](CommandGuidanceError::Lookup) — the `[039]`-`[043]` catalog
///   boundary refused the request: a shell-operator / empty / padded key
///   ([`LookupError::InvalidKey`], AC2), an unsupported key
///   ([`LookupError::UnknownKey`] carrying the discoverable alternatives, AC2),
///   or a dangling citation ([`LookupError::UnresolvedSection`], AC1 — the
///   guidance is not fabricated).
/// * [`Cursor`](CommandGuidanceError::Cursor) — the `[037]` continuation was
///   malformed or a mismatched token (a foreign operation/filter/snapshot).
/// * [`Read`](CommandGuidanceError::Read) — the `[036]` section read failed; a
///   defensive arm, because [`resolve`](CommandCatalog::resolve) already proves
///   every cited section resolves before a read is attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandGuidanceError {
    /// The command-key / citation boundary refused the request (AC1/AC2).
    Lookup(LookupError),
    /// The continuation cursor was malformed or mismatched (AC1).
    Cursor(CursorError),
    /// A section read failed (defensive; a resolved citation cannot be unknown).
    Read(ReadError),
}

impl fmt::Display for CommandGuidanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lookup(err) => write!(f, "{err}"),
            Self::Cursor(err) => write!(f, "{err}"),
            Self::Read(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CommandGuidanceError {}

impl From<LookupError> for CommandGuidanceError {
    fn from(err: LookupError) -> Self {
        Self::Lookup(err)
    }
}

impl From<CursorError> for CommandGuidanceError {
    fn from(err: CursorError) -> Self {
        Self::Cursor(err)
    }
}

impl From<ReadError> for CommandGuidanceError {
    fn from(err: ReadError) -> Self {
        Self::Read(err)
    }
}

/// One bounded page of `get_command_guidance` output: the command's entry
/// (metadata + separately-labeled effect notes) together with a `[037]`
/// [`ResponsePage`] of its source-backed section reads.
///
/// The entry rides on every page — it is small and carries the command's
/// `syntax` / `examples` / `prerequisites` / `compatibility` and its
/// [`EffectNote`]s (kept distinct from the upstream sections, AC2) — while the
/// section *content* is what the cap bounds and paginates. A clipped page
/// carries an explicit [`next_cursor`](ResponsePage::next_cursor) and a
/// [`truncated`](ResponsePage::truncated) flag (the truncation reason); resuming
/// that cursor reproduces the remaining sections, so the pages reassemble into
/// the whole.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandGuidance {
    /// The resolved command entry: metadata plus the separately-labeled effect
    /// notes.
    pub entry: CommandEntry,
    /// The snapshot the guidance was served under (provenance for the resume).
    pub snapshot_id: SnapshotId,
    /// The `[037]` cap-bounded page of the command's section reads, with its
    /// explicit continuation.
    pub page: ResponsePage,
}

impl CommandGuidance {
    /// The section reads on this page, in the command's citation order.
    ///
    /// Each is a complete [`SectionRead`] from the `[036]` seam — the Markdown
    /// body, heading breadcrumb, citations, freshness, and snapshot identity —
    /// never a clipped excerpt without its remainder.
    pub fn sections(&self) -> impl Iterator<Item = &SectionRead> + '_ {
        self.page.items.iter().filter_map(|item| match item {
            Item::Section(read) => Some(read.as_ref()),
            _ => None,
        })
    }

    /// Whether more section reads follow that this page did not return.
    pub fn has_more(&self) -> bool {
        self.page.truncated
    }
}

/// Serve the bounded, source-backed guidance for an exact command `key`.
///
/// This is the `get_command_guidance` lookup of `plan.md` §6 / §11, composed from
/// the `[036]` + `[037]` + `[039]`-`[043]` seams:
///
/// 1. **Resolve the exact key** through [`CommandCatalog::resolve`], which
///    validates the key at the `[007]` boundary (a shell operator / empty / padded
///    request is [`LookupError::InvalidKey`, never executed — AC2], an unsupported
///    request is [`LookupError::UnknownKey`] with the discoverable alternatives —
///    AC2) and proves every cited section resolves in `manifest` (a dangling
///    citation is [`LookupError::UnresolvedSection`], AC1).
/// 2. **Compose each resolved section** into a [`SectionRead`] via the `[036]`
///    [`get_section`] seam, as an [`Item::Section`].
/// 3. **Page the section reads** with the `[037]` [`paginate_items`] seam under
///    `Operation::Catalog`, bound to the command key via
///    [`FilterKey::category`] (the command key occupies the cursor's filter slot,
///    disjoint from checklist categories, which live under `Operation::Checklist`),
///    so the assembled response stays within [`MAX_RESPONSE_BYTES`] with an
///    explicit continuation.
///
/// Total: it returns a typed error (never panics) and is pure (a fixed catalog
/// and snapshot yield a fixed page). It touches no network, no filesystem, and no
/// process — it returns guidance only. `as_of` / `threshold` feed the `[036]`
/// section-read freshness; the entry metadata and effect notes are returned whole
/// on every page, and only the section *content* is clipped across continuations.
pub fn command_guidance(
    catalog: &CommandCatalog,
    manifest: &SnapshotManifest,
    as_of: Instant,
    threshold: StaleThreshold,
    key: &str,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<CommandGuidance, CommandGuidanceError> {
    // AC1/AC2 at the catalog boundary: resolve the exact key and prove every
    // cited section resolves in this snapshot. A shell operator / empty / padded
    // key, an unsupported key, or a dangling citation is refused here — never
    // executed, never fabricated.
    let resolved = catalog.resolve(manifest, key)?;
    let snapshot_id = resolved.snapshot_id.clone();

    // Compose each resolved section into a complete [036] SectionRead. `resolve`
    // already proved every cited section resolves, so each read succeeds; the
    // `Read` arm is defensive only.
    let mut items = Vec::with_capacity(resolved.sections.len());
    for section in &resolved.sections {
        let read = get_section(manifest, &section.id, as_of, threshold)?;
        items.push(Item::Section(Box::new(read)));
    }

    // Page the section reads under Operation::Catalog, bound to the command key
    // so a continuation is specific to this command's section stream. The [037]
    // seam enforces the cap and the explicit continuation.
    let filter = FilterKey::category(key);
    let page = paginate_items(
        Operation::Catalog,
        &filter,
        &snapshot_id,
        &items,
        limit,
        cursor,
    )?;

    Ok(CommandGuidance {
        entry: resolved.entry,
        snapshot_id,
        page,
    })
}

// ---------------------------------------------------------------------------
// The discoverable command inventory
// ---------------------------------------------------------------------------

/// A cap-bounded page of the command catalog's supported keys — the discoverable
/// "Resources" catalog a caller enumerates across pages rather than receiving in
/// one silent full dump.
///
/// The page is snapshot-bound (its [`Cursor`] binds the active snapshot, so a
/// cursor minted under another snapshot is a [`CursorError::Mismatch`]) and
/// bounded on two counts a caller cannot bypass: the item `limit` (via the shared
/// [`clamp_query_limit`]) and the [`MAX_RESPONSE_BYTES`] cap measured on the
/// complete serialized page. A non-final page carries an explicit
/// [`next_cursor`](CommandInventory::next_cursor) that resumes after the last
/// returned key; a final page carries `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInventory {
    /// The snapshot the inventory was served under (the cursor's binding).
    pub snapshot_id: SnapshotId,
    /// The keys returned on this page, in catalog order.
    pub keys: Vec<String>,
    /// The number of keys returned (`== keys.len()`).
    pub returned: usize,
    /// Whether more keys follow that this page did not return.
    pub truncated: bool,
    /// The cursor to resume this exact stream, or `None` when this is the last
    /// page.
    pub next_cursor: Option<String>,
}

impl CommandInventory {
    /// The complete serialized size of this page in bytes — the measurement AC1
    /// enforces: every page this seam produces is `<=` [`MAX_RESPONSE_BYTES`].
    pub fn serialized_bytes(&self) -> usize {
        inventory_page_bytes(
            self.snapshot_id.as_str(),
            &self.keys,
            self.truncated,
            self.next_cursor.as_deref(),
        )
    }

    /// Whether more keys follow that this page did not return.
    pub fn has_more(&self) -> bool {
        self.truncated
    }
}

/// The exact serialized page envelope `command_inventory` bounds — mirroring the
/// `[037]` `serialized_page_bytes` discipline (snapshot identity, operation,
/// counts, truncation flag, next cursor, and the keys in full) so the cap is
/// measured on the complete response a server would send, not on the keys alone.
#[derive(Serialize)]
struct SerializedInventoryPage {
    snapshot_id: String,
    operation: Operation,
    returned: usize,
    truncated: bool,
    next_cursor: Option<String>,
    keys: Vec<String>,
}

/// The complete serialized byte length of an inventory page holding `keys`, the
/// way AC1 measures it — over the whole JSON a server would send. On the
/// (impossible) serialization failure the page is treated as over the cap so the
/// caller never lets an unmeasured page through.
fn inventory_page_bytes(
    snapshot_id: &str,
    keys: &[String],
    truncated: bool,
    next_cursor: Option<&str>,
) -> usize {
    let page = SerializedInventoryPage {
        snapshot_id: snapshot_id.to_string(),
        operation: Operation::Catalog,
        returned: keys.len(),
        truncated,
        next_cursor: next_cursor.map(str::to_string),
        keys: keys.to_vec(),
    };
    serde_json::to_vec(&page)
        .map(|bytes| bytes.len())
        .unwrap_or(MAX_RESPONSE_BYTES + 1)
}

/// Serve a cap-bounded, snapshot-bound page of the command catalog's supported
/// keys — the discoverable "Resources" inventory of `plan.md` §6 / §11.
///
/// The keys come from `catalog.keys()` (the `[039]`-`[043]` catalog's fixed
/// order) and are paged under the `[037]` [`Cursor`] / [`CursorError`] / [`FilterKey`]
/// / [`MAX_RESPONSE_BYTES`] primitives, bound to `Operation::Catalog` and the
/// active snapshot, so a caller walks every key across pages with an explicit
/// continuation rather than receiving a silent full dump. A cursor minted under
/// another operation/filter/snapshot is a [`CursorError::Mismatch`]; a malformed
/// token is [`CursorError::Malformed`] — never a panic, never a wrong page.
///
/// Total and pure: it returns a typed [`CursorError`] (never panics) and a fixed
/// catalog plus snapshot yields a fixed page. It touches no network, no
/// filesystem, and no process.
pub fn command_inventory(
    catalog: &CommandCatalog,
    manifest: &SnapshotManifest,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<CommandInventory, CursorError> {
    let snapshot_id = manifest.body.snapshot_id.clone();
    let op = Operation::Catalog;
    let filter = FilterKey::all();

    // AC1/AC2: resume from a cursor this layer minted, rejecting a foreign /
    // malformed one; `None` is the first page (offset 0).
    let start = match cursor {
        None => 0,
        Some(token) => {
            let cursor = Cursor::from_token(token)?;
            cursor.verify(op, &filter, &snapshot_id)?
        }
    };

    let all = catalog.keys();
    let remaining = all.get(start..).unwrap_or(&[]);
    // The item count: default and clamp via the shared [037] rule so the
    // inventory and the [038] `search_docs` request agree. `0` yields an empty
    // page; a request above the cap is clamped to the cap.
    let count_limit = clamp_query_limit(limit);

    // Greedily fill keys until the complete serialized page would exceed the cap,
    // stopping short of the key that would push it over — never silently
    // dropping it. This mirrors `[037]` `paginate_items`'s cap discipline over
    // the key stream (whose items are too small to page through `Item` directly).
    let mut chosen: Vec<String> = Vec::with_capacity(count_limit.min(remaining.len()));
    let mut k = 0;
    while k < remaining.len() && chosen.len() < count_limit {
        let would_truncate = (k + 1) < remaining.len();
        let next_cursor = if would_truncate {
            Some(Cursor::mint(op, &filter, &snapshot_id, start + k + 1).to_token())
        } else {
            None
        };
        let mut candidate = chosen.clone();
        candidate.push(remaining[k].clone());
        if inventory_page_bytes(
            snapshot_id.as_str(),
            &candidate,
            would_truncate,
            next_cursor.as_deref(),
        ) > MAX_RESPONSE_BYTES
        {
            break; // this key would exceed the cap; stop here
        }
        chosen = candidate;
        k += 1;
    }

    let truncated = k < remaining.len();
    let next_cursor = if truncated {
        Some(Cursor::mint(op, &filter, &snapshot_id, start + k).to_token())
    } else {
        None
    };

    Ok(CommandInventory {
        snapshot_id,
        returned: chosen.len(),
        keys: chosen,
        truncated,
        next_cursor,
    })
}

// ---------------------------------------------------------------------------
// Tests: total, pure, offline. No upstream ingestion, no process execution.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ManifestBody, SchemaVersion, SectionBuilder, SnapshotId, Timestamp, Url};

    /// The five setup/inspection keys, in the catalog's fixed order.
    const FAMILY_KEYS: &[&str] = &[
        "cargo new",
        "cargo init",
        "cargo metadata",
        "cargo locate-project",
        "cargo tree",
    ];

    /// Build a section for one of the five Cargo Book command chapters, the way
    /// `crate::bundled` builds its representative sections, so its derived id
    /// matches the id the catalog entry cites and resolution succeeds.
    fn command_section(chapter: &str, title: &str) -> Section {
        SectionBuilder::new(
            SourceId::new("cargo").expect("`cargo` is a registry source id"),
            title,
            chapter,
            Url::new(format!("https://doc.rust-lang.org/cargo/{chapter}.html"))
                .expect("a valid canonical URL"),
            format!("{title}: a short, faithful excerpt from the Cargo Book."),
        )
        .build()
        .expect("a well-formed section")
    }

    /// Build a section for a Clippy Book chapter (`usage`, `configuration`, …),
    /// the way `crate::ingest::clippy_book` derives it, so its derived id matches
    /// the id a `cargo clippy` entry cites.
    fn clippy_chapter(chapter: &str, title: &str) -> Section {
        SectionBuilder::new(
            SourceId::new("clippy").expect("`clippy` is a registry source id"),
            title,
            chapter,
            Url::new(format!("https://doc.rust-lang.org/clippy/{chapter}.html"))
                .expect("a valid canonical URL"),
            format!("{title}: a short, faithful excerpt from the Clippy book."),
        )
        .build()
        .expect("a well-formed section")
    }

    /// Build a section for an approved rustfmt Markdown file (`README.md` /
    /// `Configurations.md`) at a pinned commit, the way `crate::ingest::rustfmt`
    /// derives it (the last raw-URL path segment is the approved file name), so
    /// its derived id matches the id a `cargo fmt` entry cites.
    fn rustfmt_file(file: &str, title: &str) -> Section {
        SectionBuilder::new(
            SourceId::new("rustfmt").expect("`rustfmt` is a registry source id"),
            title,
            file,
            Url::new(format!(
                "https://raw.githubusercontent.com/rust-lang/rustfmt/0f4c1e2b3d5a6f8b7c9d1e2f4a6b8c0d3e5f7a9b/{file}"
            ))
            .expect("a valid canonical URL"),
            format!("{title}: a short, faithful excerpt from rustfmt."),
        )
        .build()
        .expect("a well-formed section")
    }

    /// Build a section for a rustdoc Book chapter (`commands`,
    /// `documentation-tests`, `cargo-integration`, …), the way
    /// `crate::ingest::rustdoc` derives it, so its derived id matches the id a
    /// `cargo doc` / `cargo test` entry cites.
    fn rustdoc_chapter(chapter: &str, title: &str) -> Section {
        SectionBuilder::new(
            SourceId::new("rustdoc").expect("`rustdoc` is a registry source id"),
            title,
            chapter,
            Url::new(format!("https://doc.rust-lang.org/rustdoc/{chapter}.html"))
                .expect("a valid canonical URL"),
            format!("{title}: a short, faithful excerpt from the rustdoc book."),
        )
        .build()
        .expect("a well-formed section")
    }

    /// Build a section for a rustup Book chapter (`installation`, `toolchains`,
    /// `overrides`, `components`, `targets`, `toolchain-file`, `index`), the way
    /// `crate::ingest::rustup` derives it (the chapter file stem under the
    /// `rustup` boundary), so its derived id matches the id a `rustup …` entry
    /// cites.
    fn rustup_book_section(chapter: &str, title: &str) -> Section {
        SectionBuilder::new(
            SourceId::new("rustup").expect("`rustup` is a registry source id"),
            title,
            chapter,
            Url::new(format!("https://rust-lang.github.io/rustup/{chapter}.html"))
                .expect("a valid canonical URL"),
            format!("{title}: a short, faithful excerpt from the rustup book."),
        )
        .build()
        .expect("a well-formed section")
    }

    /// Build a section for a rustc Book chapter (`command-line-arguments`,
    /// `index`, …), the way `crate::ingest::rustc` derives it (the chapter file
    /// stem under the `rustc` boundary), so its derived id matches the id a
    /// `rustc` entry cites.
    fn rustc_book_section(chapter: &str, title: &str) -> Section {
        SectionBuilder::new(
            SourceId::new("rustc").expect("`rustc` is a registry source id"),
            title,
            chapter,
            Url::new(format!("https://doc.rust-lang.org/rustc/{chapter}.html"))
                .expect("a valid canonical URL"),
            format!("{title}: a short, faithful excerpt from the rustc book."),
        )
        .build()
        .expect("a well-formed section")
    }

    /// The nine implementation/verification keys, in the catalog's fixed order.
    const VERIFY_FAMILY_KEYS: &[&str] = &[
        "cargo check",
        "cargo build",
        "cargo run",
        "cargo test",
        "cargo bench",
        "cargo fmt",
        "cargo clippy",
        "cargo doc",
        "cargo fix",
    ];

    /// The two packaging/release-preparation keys, in the catalog's fixed order.
    const PKG_FAMILY_KEYS: &[&str] = &["cargo package", "cargo publish"];

    /// The eight toolchain and direct-tool keys, in the catalog's fixed order.
    const TOOLCHAIN_FAMILY_KEYS: &[&str] = &[
        "rustup show",
        "rustup toolchain",
        "rustup override",
        "rustup component",
        "rustup target",
        "rustc",
        "rustfmt",
        "rustdoc",
    ];

    /// A fixture snapshot carrying exactly the sections the implementation/
    /// verification family cites: the eight Cargo command chapters (note there is
    /// no `commands/cargo-clippy` — `cargo clippy` cites the Clippy book instead),
    /// the rustfmt usage/configuration files, and the rustdoc chapters. This is
    /// the AC1 "sections RESOLVE in a snapshot" seam for [041].
    fn verify_fixture_snapshot() -> SnapshotManifest {
        use crate::model::SnapshotManifest;
        SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: ManifestBody {
                snapshot_id: SnapshotId::new("fixture-041").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![
                    command_section("commands/cargo-check", "cargo check"),
                    command_section("commands/cargo-build", "cargo build"),
                    command_section("commands/cargo-run", "cargo run"),
                    command_section("commands/cargo-test", "cargo test"),
                    command_section("commands/cargo-bench", "cargo bench"),
                    command_section("commands/cargo-fmt", "cargo fmt"),
                    command_section("commands/cargo-doc", "cargo doc"),
                    command_section("commands/cargo-fix", "cargo fix"),
                    rustfmt_file("README.md", "rustfmt"),
                    rustfmt_file("Configurations.md", "rustfmt configuration"),
                    clippy_chapter("usage", "Clippy usage"),
                    clippy_chapter("configuration", "Clippy configuration"),
                    rustdoc_chapter("documentation-tests", "Documentation tests"),
                    rustdoc_chapter("cargo-integration", "Cargo integration"),
                ],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        }
    }

    /// A fixture snapshot carrying exactly the five command sections the family
    /// cites. This is the "sections RESOLVE in a snapshot" seam of AC1: a
    /// hand-built snapshot makes each cited id resolvable offline, with no
    /// upstream ingestion.
    fn fixture_snapshot() -> SnapshotManifest {
        use crate::model::SnapshotManifest;
        SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: ManifestBody {
                snapshot_id: SnapshotId::new("fixture-039").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![
                    command_section("commands/cargo-new", "cargo new"),
                    command_section("commands/cargo-init", "cargo init"),
                    command_section("commands/cargo-metadata", "cargo metadata"),
                    command_section("commands/cargo-locate-project", "cargo locate-project"),
                    command_section("commands/cargo-tree", "cargo tree"),
                ],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        }
    }

    // --- AC1: each key is exact and its cited section resolves -------------

    #[test]
    fn the_family_carries_the_five_exact_keys_in_order() {
        let catalog = CommandCatalog::setup_inspection();
        assert_eq!(
            catalog.keys(),
            FAMILY_KEYS,
            "the family is the five named keys, in order"
        );
        assert_eq!(catalog.len(), 5);
    }

    #[test]
    fn every_family_key_looks_up_to_its_entry() {
        let catalog = CommandCatalog::setup_inspection();
        for key in FAMILY_KEYS.iter().copied() {
            let entry = catalog
                .get(key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve to its entry, got {err}"));
            assert_eq!(
                entry.key.as_str(),
                key,
                "the entry's key is the exact key queried"
            );
        }
    }

    #[test]
    fn every_family_entry_cites_a_resolvable_upstream_section() {
        let catalog = CommandCatalog::setup_inspection();
        let manifest = fixture_snapshot();
        for key in FAMILY_KEYS.iter().copied() {
            let resolved = catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve, got {err}"));
            assert!(
                !resolved.sections.is_empty(),
                "`{key}` resolved with at least one section"
            );
            // The primary section is the one the entry cites, in order.
            assert_eq!(
                resolved.sections[0].id, resolved.entry.upstream_sections[0],
                "the resolved section matches the entry's cited id"
            );
            // The primary section's source is the Cargo Book (a real upstream),
            // not a fabricated placeholder.
            assert_eq!(
                resolved.primary_section().source_id.as_str(),
                "cargo",
                "the cited section resolves to a cargo-source section"
            );
        }
    }

    #[test]
    fn a_documented_syntax_excerpt_is_present_and_cited() {
        let catalog = CommandCatalog::setup_inspection();
        for key in FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a family key resolves");
            assert!(
                !entry.syntax.is_empty(),
                "`{key}` carries a documented syntax excerpt"
            );
            assert!(
                !entry.examples.is_empty(),
                "`{key}` carries a documented example excerpt"
            );
            assert!(
                !entry.upstream_sections.is_empty(),
                "`{key}` cites the upstream section that documents it"
            );
        }
    }

    // --- AC2: upstream excerpts and maintained notes stay separate --------

    #[test]
    fn prerequisites_qualifications_and_effect_notes_are_separate_fields() {
        let catalog = CommandCatalog::setup_inspection();

        // The mutating setup commands carry maintained effect notes ...
        for key in ["cargo new", "cargo init"] {
            let entry = catalog.get(key).expect("a family key resolves");
            assert!(
                !entry.prerequisites.is_empty(),
                "`{key}` carries documented prerequisites"
            );
            assert!(
                !entry.compatibility.is_empty(),
                "`{key}` carries version/stability qualifications"
            );
            assert!(
                !entry.effect_notes.is_empty(),
                "`{key}` carries separately-labeled maintainer effect notes"
            );
            // The effect note is a distinct, labeled field: its kind is a
            // recognized effect family and its text describes a *possible* effect
            // (a source change), not an excerpt of upstream prose.
            for note in &entry.effect_notes {
                assert_eq!(
                    note.kind,
                    EffectKind::SourceChange,
                    "`{key}` effects are source changes"
                );
                assert!(
                    note.detail.contains("Cargo.toml"),
                    "the effect note names the concrete effect"
                );
            }
            // The effect note text is not duplicated into the upstream excerpt
            // fields: the two stay in separate slots.
            for excerpt in entry.syntax.iter().chain(entry.examples.iter()) {
                assert!(
                    !excerpt.contains("does not modify an existing project"),
                    "`{key}` keeps the effect note out of the upstream excerpt"
                );
            }
        }

        // The inspection commands have no maintained effect: reading a manifest
        // or the dependency graph has no side effect, so effect_notes is empty.
        for key in ["cargo metadata", "cargo locate-project", "cargo tree"] {
            let entry = catalog.get(key).expect("a family key resolves");
            assert!(
                entry.effect_notes.is_empty(),
                "`{key}` is read-only and carries no effect note"
            );
            assert!(
                !entry.prerequisites.is_empty(),
                "`{key}` still carries its documented prerequisites"
            );
        }
    }

    // --- Discoverability and the no-execution boundary --------------------

    #[test]
    fn an_unsupported_but_valid_key_is_rejected_with_discoverable_alternatives() {
        let catalog = CommandCatalog::setup_inspection();
        // `cargo build` is a well-formed key but is not in this family.
        let err = catalog
            .get("cargo build")
            .expect_err("`cargo build` is not in the family");
        match &err {
            LookupError::UnknownKey {
                requested,
                supported,
            } => {
                assert_eq!(requested, "cargo build");
                assert_eq!(
                    supported,
                    &FAMILY_KEYS
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>(),
                    "the alternatives are the five family keys"
                );
                // The error is actionable: it names the request and the options.
                assert!(err.to_string().contains("cargo build"));
                assert!(err.to_string().contains("cargo new"));
            }
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn a_shell_operator_key_is_refused_not_executed() {
        let catalog = CommandCatalog::setup_inspection();
        for bad in [
            "cargo new ; rm -rf /",
            "cargo tree | xargs -n1 echo",
            "cargo metadata $(whoami)",
            "cargo init && echo pwned",
            "cargo new", // valid, for contrast below
        ] {
            let result = catalog.get(bad);
            if bad == "cargo new" {
                assert!(
                    result.is_ok(),
                    "a clean family key still resolves; shell-operator refusal is \
                     specific to operator characters"
                );
            } else {
                match result {
                    Err(LookupError::InvalidKey { requested, .. }) => {
                        assert_eq!(requested, bad, "the refused request is preserved");
                    }
                    Err(other) => panic!("expected InvalidKey for `{bad}`, got {other:?}"),
                    Ok(_) => panic!("`{bad}` must not resolve: a shell-operator key is refused"),
                }
            }
        }
    }

    #[test]
    fn resolve_never_fabricates_a_missing_section() {
        let catalog = CommandCatalog::setup_inspection();
        // An empty snapshot carries no section, so every citation dangles — the
        // catalog reports the unresolvable id rather than inventing content.
        let empty = crate::model::SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: crate::model::ManifestBody {
                snapshot_id: SnapshotId::new("empty").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        };
        let err = catalog
            .resolve(&empty, "cargo new")
            .expect_err("no section resolves in an empty snapshot");
        match err {
            LookupError::UnresolvedSection { key, section } => {
                assert_eq!(key, "cargo new");
                assert_eq!(
                    section,
                    SectionId::derive(
                        &SourceId::new("cargo").expect("registry id"),
                        "commands/cargo-new",
                        None,
                        None,
                    )
                    .expect("a well-formed id")
                );
            }
            other => panic!("expected UnresolvedSection, got {other:?}"),
        }
    }

    // --- Totality, purity, and the lookups are pure ----------------------

    #[test]
    fn lookups_are_pure_and_deterministic() {
        let a = CommandCatalog::setup_inspection();
        let b = CommandCatalog::setup_inspection();
        assert_eq!(a, b, "two catalogs built from the same code are identical");
        assert_eq!(a.keys(), b.keys());
        // Repeated lookups of the same key yield the same entry, and a lookup on
        // one catalog agrees with the identical lookup on the other.
        let new_a = a.get("cargo new").expect("cargo new resolves");
        let new_b = b.get("cargo new").expect("cargo new resolves");
        assert_eq!(new_a, new_b);
        assert_eq!(new_a, a.get("cargo new").expect("repeat lookup agrees"));
    }

    #[test]
    fn an_empty_catalog_reports_no_keys() {
        // A catalog built from no entries is an empty, well-formed result rather
        // than a panic; the discovery helpers are total over it.
        let empty = CommandCatalog { entries: vec![] };
        assert!(empty.is_empty());
        assert_eq!(empty.keys(), Vec::<String>::new());
        let err = empty
            .get("cargo new")
            .expect_err("an empty catalog has no entries");
        assert!(matches!(err, LookupError::UnknownKey { .. }));
    }

    // ===========================================================
    // [040] dependency / reproducibility family
    // ===========================================================

    /// The five dependency/reproducibility keys, in the catalog's fixed order.
    const DEP_FAMILY_KEYS: &[&str] = &[
        "cargo add",
        "cargo remove",
        "cargo update",
        "cargo generate-lockfile",
        "cargo fetch",
    ];

    /// The reference chapters the dependency family cites as its "relevant
    /// configuration sections" (AC1): feature resolution, the manifest, and the
    /// package sources / lockfile reference.
    const REFERENCE_CHAPTERS: &[&str] = &["features", "manifest", "registry"];

    /// A fixture snapshot carrying exactly the sections the dependency family
    /// cites: the five command chapters plus the three reference chapters. This
    /// is the AC1 "sections RESOLVE in a snapshot" seam for [040].
    fn dep_fixture_snapshot() -> SnapshotManifest {
        use crate::model::SnapshotManifest;
        SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: ManifestBody {
                snapshot_id: SnapshotId::new("fixture-040").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![
                    command_section("commands/cargo-add", "cargo add"),
                    command_section("commands/cargo-remove", "cargo remove"),
                    command_section("commands/cargo-update", "cargo update"),
                    command_section(
                        "commands/cargo-generate-lockfile",
                        "cargo generate-lockfile",
                    ),
                    command_section("commands/cargo-fetch", "cargo fetch"),
                    command_section("features", "Features"),
                    command_section("manifest", "Manifest"),
                    command_section("registry", "Registry"),
                ],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        }
    }

    /// A fixture snapshot carrying every section the combined catalog cites: the
    /// setup/inspection family's five command chapters, the dependency family's
    /// five command chapters and three reference chapters, and the implementation
    /// / verification family's eight command chapters, rustfmt files, and rustdoc
    /// chapters.
    fn combined_fixture_snapshot() -> SnapshotManifest {
        use crate::model::SnapshotManifest;
        SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: ManifestBody {
                snapshot_id: SnapshotId::new("fixture-041-combined")
                    .expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![
                    command_section("commands/cargo-new", "cargo new"),
                    command_section("commands/cargo-init", "cargo init"),
                    command_section("commands/cargo-metadata", "cargo metadata"),
                    command_section("commands/cargo-locate-project", "cargo locate-project"),
                    command_section("commands/cargo-tree", "cargo tree"),
                    command_section("commands/cargo-add", "cargo add"),
                    command_section("commands/cargo-remove", "cargo remove"),
                    command_section("commands/cargo-update", "cargo update"),
                    command_section(
                        "commands/cargo-generate-lockfile",
                        "cargo generate-lockfile",
                    ),
                    command_section("commands/cargo-fetch", "cargo fetch"),
                    command_section("features", "Features"),
                    command_section("manifest", "Manifest"),
                    command_section("registry", "Registry"),
                    command_section("commands/cargo-check", "cargo check"),
                    command_section("commands/cargo-build", "cargo build"),
                    command_section("commands/cargo-run", "cargo run"),
                    command_section("commands/cargo-test", "cargo test"),
                    command_section("commands/cargo-bench", "cargo bench"),
                    command_section("commands/cargo-fmt", "cargo fmt"),
                    command_section("commands/cargo-doc", "cargo doc"),
                    command_section("commands/cargo-fix", "cargo fix"),
                    rustfmt_file("README.md", "rustfmt"),
                    rustfmt_file("Configurations.md", "rustfmt configuration"),
                    clippy_chapter("usage", "Clippy usage"),
                    clippy_chapter("configuration", "Clippy configuration"),
                    rustdoc_chapter("documentation-tests", "Documentation tests"),
                    rustdoc_chapter("cargo-integration", "Cargo integration"),
                    command_section("commands/cargo-package", "cargo package"),
                    command_section("commands/cargo-publish", "cargo publish"),
                    command_section("packaging", "Packaging"),
                    command_section("publishing", "Publishing"),
                    rustup_book_section("index", "Introduction"),
                    rustup_book_section("toolchains", "Toolchains"),
                    rustup_book_section("overrides", "Overrides"),
                    rustup_book_section("toolchain-file", "The toolchain file"),
                    rustup_book_section("components", "Components"),
                    rustup_book_section("targets", "Targets"),
                    rustc_book_section("command-line-arguments", "Command-line arguments"),
                    rustc_book_section("index", "Introduction"),
                    rustdoc_chapter("commands", "Commands"),
                ],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        }
    }

    /// A fixture snapshot carrying exactly the sections the packaging/release
    /// family cites: the two command chapters and the two release-preparation
    /// reference chapters (`packaging`, `publishing`), plus the `registry`
    /// reference that `cargo publish` cites. This is the AC1 "sections RESOLVE in
    /// a snapshot" seam for [042].
    fn pkg_fixture_snapshot() -> SnapshotManifest {
        use crate::model::SnapshotManifest;
        SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: ManifestBody {
                snapshot_id: SnapshotId::new("fixture-042").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![
                    command_section("commands/cargo-package", "cargo package"),
                    command_section("commands/cargo-publish", "cargo publish"),
                    command_section("packaging", "Packaging"),
                    command_section("publishing", "Publishing"),
                    command_section("registry", "Registry"),
                ],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        }
    }

    /// A fixture snapshot carrying exactly the sections the toolchain and
    /// direct-tool family cites: the rustup Book chapters (`index`, `toolchains`,
    /// `overrides`, `toolchain-file`, `components`, `targets`), the rustc Book
    /// chapters (`command-line-arguments`, `index`), the rustfmt reference
    /// (`README.md`, `Configurations.md`), and the rustdoc Book chapters
    /// (`commands`, `documentation-tests`). This is the AC1 "sections RESOLVE in
    /// a snapshot" seam for [043].
    fn toolchain_fixture_snapshot() -> SnapshotManifest {
        use crate::model::SnapshotManifest;
        SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: ManifestBody {
                snapshot_id: SnapshotId::new("fixture-043").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![
                    rustup_book_section("index", "Introduction"),
                    rustup_book_section("toolchains", "Toolchains"),
                    rustup_book_section("overrides", "Overrides"),
                    rustup_book_section("toolchain-file", "The toolchain file"),
                    rustup_book_section("components", "Components"),
                    rustup_book_section("targets", "Targets"),
                    rustc_book_section("command-line-arguments", "Command-line arguments"),
                    rustc_book_section("index", "Introduction"),
                    rustfmt_file("README.md", "rustfmt"),
                    rustfmt_file("Configurations.md", "rustfmt configuration"),
                    rustdoc_chapter("commands", "Commands"),
                    rustdoc_chapter("documentation-tests", "Documentation tests"),
                ],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        }
    }

    /// The maintained-note text of an entry (its effect-note details and
    /// compatibility qualifications joined): the fields where the `--offline` /
    /// `--locked` scoping is declared (AC2). Upstream syntax/examples are
    /// excluded so a flag mention is a maintained decision, not an upstream
    /// excerpt.
    fn maintained_text(entry: &CommandEntry) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(entry.effect_notes.iter().map(|note| note.detail.clone()));
        parts.extend(entry.compatibility.iter().cloned());
        parts.join("\n")
    }

    // --- [040] AC1: exact keys and resolvable citations -------------------

    #[test]
    fn the_dependency_family_carries_the_five_exact_keys_in_order() {
        let catalog = CommandCatalog::dependencies();
        assert_eq!(
            catalog.keys(),
            DEP_FAMILY_KEYS,
            "the family is the five named keys, in order"
        );
        assert_eq!(catalog.len(), 5);
    }

    #[test]
    fn every_dependency_key_looks_up_to_its_entry() {
        let catalog = CommandCatalog::dependencies();
        for key in DEP_FAMILY_KEYS.iter().copied() {
            let entry = catalog
                .get(key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve to its entry, got {err}"));
            assert_eq!(
                entry.key.as_str(),
                key,
                "the entry's key is the exact key queried"
            );
        }
    }

    #[test]
    fn every_dependency_entry_cites_a_resolvable_upstream_section() {
        let catalog = CommandCatalog::dependencies();
        let manifest = dep_fixture_snapshot();
        for key in DEP_FAMILY_KEYS.iter().copied() {
            let resolved = catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve, got {err}"));
            assert!(
                !resolved.sections.is_empty(),
                "`{key}` resolved with at least one section"
            );
            // The first resolved section is the entry's first cited id, in order.
            assert_eq!(
                resolved.sections[0].id, resolved.entry.upstream_sections[0],
                "the first resolved section is the entry's first cited id"
            );
            // Every resolved section is a cargo-source section (no fabricated
            // placeholder), and the count equals the citations: none dropped or
            // invented.
            assert_eq!(
                resolved.sections.len(),
                resolved.entry.upstream_sections.len(),
                "every cited section resolved, none dropped or added"
            );
            for section in &resolved.sections {
                assert_eq!(section.source_id.as_str(), "cargo");
            }
        }
    }

    #[test]
    fn every_dependency_entry_cites_a_relevant_configuration_section() {
        // AC1: each entry cites its command chapter AND at least one relevant
        // configuration/reference section (features/manifest/registry).
        let catalog = CommandCatalog::dependencies();
        let manifest = dep_fixture_snapshot();
        let references: Vec<SectionId> = REFERENCE_CHAPTERS
            .iter()
            .copied()
            .map(cargo_section)
            .collect();
        // The five command chapters, the same way the entries cite them, so the
        // check is an exact id comparison (the chapter `/` is percent-encoded in
        // the id, so a raw `contains("commands/")` would miss it, and the
        // chapter is hyphenated `commands/cargo-add`, not the spaced key).
        let command_chapters: Vec<SectionId> = [
            "commands/cargo-add",
            "commands/cargo-remove",
            "commands/cargo-update",
            "commands/cargo-generate-lockfile",
            "commands/cargo-fetch",
        ]
        .iter()
        .copied()
        .map(cargo_section)
        .collect();
        for key in DEP_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a dependency key resolves");
            assert!(
                entry
                    .upstream_sections
                    .iter()
                    .any(|id| command_chapters.contains(id)),
                "`{key}` cites its command chapter"
            );
            assert!(
                entry
                    .upstream_sections
                    .iter()
                    .any(|id| references.contains(id)),
                "`{key}` cites a relevant configuration section (features/manifest/registry)"
            );
            // The cited reference section resolves to a cargo-source section.
            let resolved = catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` resolves, got {err}"));
            assert!(
                resolved
                    .sections
                    .iter()
                    .all(|s| s.source_id.as_str() == "cargo"),
                "`{key}`'s cited sections are all cargo-source"
            );
        }
    }

    #[test]
    fn every_dependency_entry_cites_command_specific_options_and_examples() {
        // AC1: command-specific options (documented syntax) and relevant
        // configuration sections are present on every entry.
        let catalog = CommandCatalog::dependencies();
        for key in DEP_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a dependency key resolves");
            assert!(
                !entry.syntax.is_empty(),
                "`{key}` carries a documented syntax excerpt"
            );
            assert!(
                !entry.examples.is_empty(),
                "`{key}` carries a documented example excerpt"
            );
            assert!(
                !entry.prerequisites.is_empty(),
                "`{key}` carries documented prerequisites"
            );
            assert!(
                !entry.compatibility.is_empty(),
                "`{key}` carries version/stability qualifications"
            );
            assert!(
                !entry.upstream_sections.is_empty(),
                "`{key}` cites its upstream section(s)"
            );
        }
    }

    // --- [040] AC2: accurate lockfile/source/download + flag scoping -------

    #[test]
    fn every_dependency_command_documents_offline() {
        // AC2 (universal half of the scoping): `--offline` is documented for all
        // five dependency commands — the universal cache-only flag for these
        // registry-touching commands.
        let catalog = CommandCatalog::dependencies();
        for key in DEP_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a dependency key resolves");
            assert!(
                maintained_text(entry).contains("--offline"),
                "`{key}` documents --offline"
            );
        }
    }

    #[test]
    fn the_lockfile_commands_describe_locked_and_offline() {
        // AC2: the four commands that write or resolve the lockfile document
        // BOTH `--offline` and `--locked`.
        let catalog = CommandCatalog::dependencies();
        for key in [
            "cargo add",
            "cargo remove",
            "cargo update",
            "cargo generate-lockfile",
        ] {
            let entry = catalog.get(key).expect("a lockfile command resolves");
            let text = maintained_text(entry);
            assert!(text.contains("--offline"), "`{key}` documents --offline");
            assert!(text.contains("--locked"), "`{key}` documents --locked");
        }
    }

    #[test]
    fn cargo_fetch_describes_offline_but_not_locked() {
        // AC2 (the per-command scoping): `--offline` is universal, but the
        // `--locked` "the lockfile will not change" assertion is *not highlighted*
        // on `cargo fetch`. `cargo fetch` accepts `--locked` (Cargo parses it and
        // raises a semantic lockfile error if the lockfile would change), but its
        // purpose is to refresh and download, so its entry *chooses to highlight*
        // `--offline` and defers the full, version-specific flag list — which
        // includes `--locked` — to `cargo fetch --help`. This proves that a flag is
        // not *documented* for a command that does not document it, without
        // claiming the flag is unsupported or inapplicable.
        let catalog = CommandCatalog::dependencies();
        let fetch = catalog.get("cargo fetch").expect("cargo fetch resolves");
        let text = maintained_text(fetch);
        assert!(
            text.contains("--offline"),
            "cargo fetch documents --offline"
        );
        assert!(
            !text.contains("--locked"),
            "cargo fetch's entry omits --locked by design (deferred to --help), \
             not because --locked is inapplicable"
        );
        // `--locked` never leaks into fetch's upstream excerpt fields either.
        for excerpt in fetch.syntax.iter().chain(fetch.examples.iter()) {
            assert!(
                !excerpt.contains("--locked"),
                "cargo fetch's upstream excerpts carry no --locked"
            );
        }
    }

    #[test]
    fn each_dependency_command_notes_a_possible_download() {
        // AC2: "possible downloads are described accurately" — every dependency
        // command can pull from the registry, and each carries a DependencyDownload
        // note qualified by `--offline`/`--frozen`.
        let catalog = CommandCatalog::dependencies();
        for key in DEP_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a dependency key resolves");
            assert!(
                entry
                    .effect_notes
                    .iter()
                    .any(|note| note.kind == EffectKind::DependencyDownload),
                "`{key}` notes a possible dependency download"
            );
        }
    }

    #[test]
    fn add_and_remove_note_a_source_change_the_resolvers_do_not() {
        // AC2 effect-family scoping: only `cargo add` and `cargo remove` edit
        // `Cargo.toml` (a SourceChange); the resolvers/refreshers do not.
        let catalog = CommandCatalog::dependencies();
        for key in ["cargo add", "cargo remove"] {
            let entry = catalog
                .get(key)
                .expect("a source-changing command resolves");
            assert!(
                entry
                    .effect_notes
                    .iter()
                    .any(|note| note.kind == EffectKind::SourceChange),
                "`{key}` notes a source change (a Cargo.toml edit)"
            );
        }
        for key in ["cargo update", "cargo generate-lockfile", "cargo fetch"] {
            let entry = catalog.get(key).expect("a resolver/refresher resolves");
            assert!(
                !entry
                    .effect_notes
                    .iter()
                    .any(|note| note.kind == EffectKind::SourceChange),
                "`{key}` must NOT note a source change (it does not edit Cargo.toml)"
            );
        }
    }

    #[test]
    fn lockfile_mutation_commands_note_a_lockfile_change() {
        // AC2: every dependency command reads or writes `Cargo.lock`, so each
        // carries a LockfileChange note describing the lockfile/source change
        // accurately.
        let catalog = CommandCatalog::dependencies();
        for key in DEP_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a dependency key resolves");
            assert!(
                entry
                    .effect_notes
                    .iter()
                    .any(|note| note.kind == EffectKind::LockfileChange),
                "`{key}` notes its lockfile change"
            );
        }
    }

    #[test]
    fn dependency_upstream_excerpts_and_maintained_notes_are_separate() {
        // AC2 (separation): a maintained effect-note's prose is kept out of the
        // upstream syntax/example fields, mirroring the [039] pattern.
        let catalog = CommandCatalog::dependencies();
        for key in DEP_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a dependency key resolves");
            for note in &entry.effect_notes {
                assert!(
                    !note.detail.trim().is_empty(),
                    "`{key}` carries a non-trivial effect note"
                );
                for excerpt in entry.syntax.iter().chain(entry.examples.iter()) {
                    assert!(
                        !excerpt.contains(&note.detail),
                        "`{key}` keeps effect-note prose out of the upstream excerpt"
                    );
                }
            }
        }
    }

    #[test]
    fn a_missing_section_is_never_fabricated_for_a_dependency_key() {
        // AC1 (dangling): a cited section that is absent from the active snapshot
        // is an UnresolvedSection, never a fabricated section.
        let catalog = CommandCatalog::dependencies();
        let empty = crate::model::SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: crate::model::ManifestBody {
                snapshot_id: SnapshotId::new("empty-040").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        };
        // The first cited section of `cargo add` is its command chapter; with no
        // sections in the snapshot it dangles rather than being fabricated.
        let err = catalog
            .resolve(&empty, "cargo add")
            .expect_err("no section resolves in an empty snapshot");
        match err {
            LookupError::UnresolvedSection { key, section } => {
                assert_eq!(key, "cargo add");
                let expected = SectionId::derive(
                    &SourceId::new("cargo").expect("registry id"),
                    "commands/cargo-add",
                    None,
                    None,
                )
                .expect("a well-formed id");
                assert_eq!(section, expected);
            }
            other => panic!("expected UnresolvedSection, got {other:?}"),
        }
    }

    #[test]
    fn a_shell_operator_dependency_key_is_refused_not_executed() {
        // The no-execution boundary holds for the dependency family too: a
        // shell-operator key is refused at the key boundary, never interpolated
        // into a process.
        let catalog = CommandCatalog::dependencies();
        for bad in [
            "cargo add serde ; rm -rf /",
            "cargo update | xargs -n1 echo",
            "cargo fetch $(whoami)",
        ] {
            match catalog.get(bad) {
                Err(LookupError::InvalidKey { requested, .. }) => {
                    assert_eq!(requested, bad, "the refused request is preserved");
                }
                Err(other) => panic!("expected InvalidKey for `{bad}`, got {other:?}"),
                Ok(_) => panic!("`{bad}` must not resolve: a shell-operator key is refused"),
            }
        }
        // A clean dependency key still resolves: refusal is specific to operators.
        assert!(
            catalog.get("cargo add").is_ok(),
            "a clean dependency key resolves"
        );
    }

    // --- [041] concatenation, totality, and purity ------------------------

    #[test]
    fn the_combined_catalog_resolves_every_family_entry() {
        // `combined()` is the five families concatenated; the lookup/resolve paths
        // are unchanged, so every key from any family still resolves exactly as it
        // did in its own catalog.
        let catalog = CommandCatalog::combined();
        // 5 setup/inspection + 5 dependency + 9 implementation/verification + 2
        // packaging/release + 8 toolchain/direct-tool = 29.
        assert_eq!(
            catalog.len(),
            29,
            "combined is the five families concatenated"
        );
        let expected: Vec<String> = FAMILY_KEYS
            .iter()
            .chain(DEP_FAMILY_KEYS.iter())
            .chain(VERIFY_FAMILY_KEYS.iter())
            .chain(PKG_FAMILY_KEYS.iter())
            .chain(TOOLCHAIN_FAMILY_KEYS.iter())
            .map(|key| key.to_string())
            .collect();
        assert_eq!(
            catalog.keys(),
            expected,
            "combined keys are the five families, in order"
        );
        let manifest = combined_fixture_snapshot();
        for key in FAMILY_KEYS
            .iter()
            .copied()
            .chain(DEP_FAMILY_KEYS.iter().copied())
            .chain(VERIFY_FAMILY_KEYS.iter().copied())
            .chain(PKG_FAMILY_KEYS.iter().copied())
            .chain(TOOLCHAIN_FAMILY_KEYS.iter().copied())
        {
            let resolved = catalog.resolve(&manifest, key).unwrap_or_else(|err| {
                panic!("`{key}` resolves in the combined catalog, got {err}")
            });
            assert!(
                !resolved.sections.is_empty(),
                "`{key}` resolved with a section"
            );
            assert_eq!(
                resolved.snapshot_id, manifest.body.snapshot_id,
                "the entry resolved against the combined snapshot"
            );
        }
    }

    #[test]
    fn dependency_catalogs_are_pure_and_deterministic() {
        let a = CommandCatalog::dependencies();
        let b = CommandCatalog::dependencies();
        assert_eq!(
            a, b,
            "two dependency catalogs built from the same code are identical"
        );
        assert_eq!(a.keys(), b.keys());
        let add_a = a.get("cargo add").expect("cargo add resolves");
        let add_b = b.get("cargo add").expect("cargo add resolves");
        assert_eq!(add_a, add_b);
    }

    // ===========================================================
    // [041] implementation / verification family
    // ===========================================================

    // --- [041] AC1: exact keys and resolvable citations -------------------

    #[test]
    fn the_verification_family_carries_the_nine_exact_keys_in_order() {
        let catalog = CommandCatalog::implementation_verification();
        assert_eq!(
            catalog.keys(),
            VERIFY_FAMILY_KEYS,
            "the family is the nine named keys, in order"
        );
        assert_eq!(catalog.len(), 9);
    }

    #[test]
    fn every_verification_key_looks_up_to_its_entry() {
        let catalog = CommandCatalog::implementation_verification();
        for key in VERIFY_FAMILY_KEYS.iter().copied() {
            let entry = catalog
                .get(key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve to its entry, got {err}"));
            assert_eq!(
                entry.key.as_str(),
                key,
                "the entry's key is the exact key queried"
            );
        }
    }

    #[test]
    fn every_verification_entry_cites_a_resolvable_upstream_section() {
        let catalog = CommandCatalog::implementation_verification();
        let manifest = verify_fixture_snapshot();
        for key in VERIFY_FAMILY_KEYS.iter().copied() {
            let resolved = catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve, got {err}"));
            assert!(
                !resolved.sections.is_empty(),
                "`{key}` resolved with at least one section"
            );
            // Every cited section resolved, none dropped or invented.
            assert_eq!(
                resolved.sections.len(),
                resolved.entry.upstream_sections.len(),
                "every cited section resolved, none dropped or added"
            );
            // No fabricated placeholder: each resolved section belongs to one of the
            // eight registry sources, never an empty/unknown source.
            for section in &resolved.sections {
                assert!(!section.source_id.as_str().is_empty());
            }
        }
    }

    #[test]
    fn every_verification_entry_cites_command_specific_options_and_examples() {
        // AC1: command-specific options (documented syntax) and relevant
        // configuration sections are present on every entry.
        let catalog = CommandCatalog::implementation_verification();
        for key in VERIFY_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a verification key resolves");
            assert!(
                !entry.syntax.is_empty(),
                "`{key}` carries a documented syntax excerpt"
            );
            assert!(
                !entry.examples.is_empty(),
                "`{key}` carries a documented example excerpt"
            );
            assert!(
                !entry.prerequisites.is_empty(),
                "`{key}` carries documented prerequisites"
            );
            assert!(
                !entry.compatibility.is_empty(),
                "`{key}` carries version/stability qualifications"
            );
            assert!(
                !entry.upstream_sections.is_empty(),
                "`{key}` cites its upstream section(s)"
            );
        }
    }

    #[test]
    fn the_verification_entries_cite_their_tooling_chapters() {
        // AC1: each tool-deferring command cites the relevant tooling chapter it
        // defers to, resolvable against the fixture snapshot.
        let catalog = CommandCatalog::implementation_verification();
        let manifest = verify_fixture_snapshot();

        // `cargo fmt` cites the rustfmt usage (README.md) and configuration
        // (Configurations.md) reference.
        let fmt = catalog.get("cargo fmt").expect("cargo fmt resolves");
        assert!(
            fmt.upstream_sections
                .iter()
                .any(|id| *id == rustfmt_section("README.md")),
            "cargo fmt cites the rustfmt usage reference"
        );
        assert!(
            fmt.upstream_sections
                .iter()
                .any(|id| *id == rustfmt_section("Configurations.md")),
            "cargo fmt cites the rustfmt configuration reference"
        );

        // `cargo clippy` cites the Clippy book's usage and configuration chapters
        // (not a fabricated cargo command chapter).
        let clippy = catalog.get("cargo clippy").expect("cargo clippy resolves");
        assert!(
            clippy
                .upstream_sections
                .iter()
                .any(|id| *id == clippy_section("usage")),
            "cargo clippy cites the Clippy usage chapter"
        );
        assert!(
            clippy
                .upstream_sections
                .iter()
                .any(|id| *id == clippy_section("configuration")),
            "cargo clippy cites the Clippy configuration chapter"
        );

        // `cargo doc` cites the rustdoc book's cargo-integration chapter.
        let doc = catalog.get("cargo doc").expect("cargo doc resolves");
        assert!(
            doc.upstream_sections
                .iter()
                .any(|id| *id == rustdoc_section("cargo-integration")),
            "cargo doc cites the rustdoc cargo-integration chapter"
        );

        // `cargo test` cites the rustdoc book's documentation-tests chapter for its
        // doctests.
        let test = catalog.get("cargo test").expect("cargo test resolves");
        assert!(
            test.upstream_sections
                .iter()
                .any(|id| *id == rustdoc_section("documentation-tests")),
            "cargo test cites the rustdoc documentation-tests chapter"
        );

        // Every cited section of these entries resolves against the fixture.
        for key in ["cargo fmt", "cargo clippy", "cargo doc", "cargo test"] {
            catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` resolves, got {err}"));
        }
    }

    // --- [041] AC1: selection and per-command argument-forwarding ----------

    #[test]
    fn the_compiler_commands_preserve_package_target_and_feature_selection() {
        // AC1: each compile/verify command documents package/workspace/target/
        // feature selection — the selection flags it actually supports.
        let catalog = CommandCatalog::implementation_verification();
        for key in [
            "cargo check",
            "cargo build",
            "cargo run",
            "cargo test",
            "cargo bench",
            "cargo clippy",
            "cargo doc",
            "cargo fix",
        ] {
            let entry = catalog.get(key).expect("a compiler command resolves");
            let text = maintained_text(entry);
            assert!(
                text.contains("--package"),
                "`{key}` documents package selection (-p/--package)"
            );
            assert!(
                text.contains("--target"),
                "`{key}` documents target selection (--target)"
            );
            assert!(
                text.contains("--features"),
                "`{key}` documents feature selection (--features/--all-features)"
            );
        }
    }

    #[test]
    fn cargo_fmt_documents_its_own_selection_and_not_the_cargo_flags() {
        // AC1 per-command scoping: `cargo fmt` documents its *own* selection
        // (`--manifest-path`, `-p`/`--package`, `--check`) and explicitly notes that
        // `--target`/`--features` are *not* rustfmt options — it does not apply a
        // cargo selection flag that rustfmt does not support.
        let catalog = CommandCatalog::implementation_verification();
        let fmt = catalog.get("cargo fmt").expect("cargo fmt resolves");
        let text = maintained_text(fmt);
        assert!(
            text.contains("--manifest-path"),
            "cargo fmt documents --manifest-path selection"
        );
        assert!(
            text.contains("--package"),
            "cargo fmt documents -p/--package selection"
        );
        assert!(
            text.contains("--check"),
            "cargo fmt documents --check (check mode)"
        );
        // The scoping point: the note explicitly says these are not rustfmt options,
        // so a target/feature flag is not applied to a command that lacks it.
        assert!(
            text.contains("are not rustfmt options"),
            "cargo fmt notes that --target/--features are not rustfmt options"
        );
    }

    #[test]
    fn argument_forwarding_is_documented_per_command() {
        // AC1: each command that forwards arguments after `--` documents *where*
        // they go; the two commands that do not execute a program document that
        // they have no program-argument forwarding.
        let catalog = CommandCatalog::implementation_verification();
        let forwarders: &[(&str, &str)] = &[
            ("cargo run", "forwarded to the program"),
            ("cargo test", "forwarded to the test"),
            ("cargo bench", "forwarded to the benchmark"),
            ("cargo clippy", "forwarded after"),
            ("cargo doc", "rustdoc options"),
            ("cargo fmt", "rustfmt options"),
        ];
        for (key, phrase) in forwarders {
            let entry = catalog.get(key).expect("a forwarding command resolves");
            assert!(
                maintained_text(entry).contains(phrase),
                "`{key}` documents its argument-forwarding rule"
            );
        }
        for key in ["cargo check", "cargo build"] {
            let entry = catalog.get(key).expect("a check command resolves");
            assert!(
                maintained_text(entry).contains("does not execute a program"),
                "`{key}` documents that it has no program-argument forwarding"
            );
        }
    }

    // --- [041] AC2: check-vs-apply, doctest-vs-test, and nightly ----------

    #[test]
    fn check_commands_do_not_modify_source_but_fix_and_fmt_do() {
        // AC2 (check vs. apply): the check commands carry no SourceChange effect;
        // `cargo fix` and `cargo fmt` are the apply counterparts that do.
        let catalog = CommandCatalog::implementation_verification();
        for key in [
            "cargo check",
            "cargo build",
            "cargo run",
            "cargo test",
            "cargo bench",
            "cargo clippy",
            "cargo doc",
        ] {
            let entry = catalog.get(key).expect("a check command resolves");
            assert!(
                !entry
                    .effect_notes
                    .iter()
                    .any(|note| note.kind == EffectKind::SourceChange),
                "`{key}` does not apply changes to source"
            );
        }
        for key in ["cargo fix", "cargo fmt"] {
            let entry = catalog.get(key).expect("an apply command resolves");
            assert!(
                entry
                    .effect_notes
                    .iter()
                    .any(|note| note.kind == EffectKind::SourceChange),
                "`{key}` applies changes to source (SourceChange)"
            );
        }
    }

    #[test]
    fn run_test_and_bench_execute_project_code() {
        // AC2 (execute vs. check): the three commands that run project code carry
        // a ProjectCodeExecution effect; the build/check/doc/fmt/fix/… do not run
        // the project's code as their defining effect.
        let catalog = CommandCatalog::implementation_verification();
        for key in ["cargo run", "cargo test", "cargo bench"] {
            let entry = catalog.get(key).expect("an executing command resolves");
            assert!(
                entry
                    .effect_notes
                    .iter()
                    .any(|note| note.kind == EffectKind::ProjectCodeExecution),
                "`{key}` executes project code (ProjectCodeExecution)"
            );
        }
        for key in [
            "cargo check",
            "cargo build",
            "cargo clippy",
            "cargo doc",
            "cargo fmt",
        ] {
            let entry = catalog.get(key).expect("a non-executing command resolves");
            assert!(
                !entry
                    .effect_notes
                    .iter()
                    .any(|note| note.kind == EffectKind::ProjectCodeExecution),
                "`{key}` does not run project code as its defining effect"
            );
        }
    }

    #[test]
    fn cargo_test_distinguishes_doctests_from_other_tests() {
        // AC2 (doctest vs. other tests): `cargo test` runs unit, integration, AND
        // doctests, while `cargo test --doc` runs doctests only; it cites the
        // rustdoc documentation-tests chapter for that behavior.
        let catalog = CommandCatalog::implementation_verification();
        let test = catalog.get("cargo test").expect("cargo test resolves");
        let text = maintained_text(test);
        assert!(text.contains("doctest"), "cargo test documents doctests");
        assert!(
            text.contains("--doc"),
            "cargo test documents the --doc doctests-only selector"
        );
        assert!(
            test.upstream_sections
                .iter()
                .any(|id| *id == rustdoc_section("documentation-tests")),
            "cargo test cites the rustdoc documentation-tests chapter"
        );
        // `cargo doc` does not run doctests: it notes that doctests are executed
        // by `cargo test`, not by `cargo doc`.
        let doc = catalog.get("cargo doc").expect("cargo doc resolves");
        assert!(
            maintained_text(doc).contains("not by `cargo doc`"),
            "cargo doc does not claim to run doctests"
        );
    }

    #[test]
    fn nightly_requirements_are_documented_not_fabricated() {
        // AC2 (documented nightly requirements): the nightly nuance is stated where
        // the documentation actually states it, and *not* fabricated for commands
        // that have no nightly requirement.
        let catalog = CommandCatalog::implementation_verification();

        // `cargo bench` documents the real nuance: the command is stable, the old
        // `#[bench]` harness was nightly-only and removed, and `criterion` is the
        // stable path — no nightly requirement is fabricated for `cargo bench`.
        let bench = catalog.get("cargo bench").expect("cargo bench resolves");
        let bench_text = maintained_text(bench);
        assert!(
            bench_text.contains("criterion"),
            "cargo bench documents the stable criterion path"
        );
        assert!(
            bench_text.contains("nightly-only"),
            "cargo bench documents the removed nightly-only #[bench] harness"
        );
        assert!(
            bench_text.contains("stable command"),
            "cargo bench is documented as a stable command"
        );

        // `cargo fmt` documents that only rustfmt's *unstable* options need a
        // nightly rustfmt — not the command itself.
        let fmt = catalog.get("cargo fmt").expect("cargo fmt resolves");
        assert!(
            maintained_text(fmt).contains("require a nightly rustfmt"),
            "cargo fmt documents that only unstable rustfmt options need nightly"
        );

        // `cargo clippy` documents that most lints are stable though a few are
        // nightly-only.
        let clippy = catalog.get("cargo clippy").expect("cargo clippy resolves");
        assert!(
            maintained_text(clippy).contains("nightly-only"),
            "cargo clippy documents the nightly-only lint nuance"
        );

        // No nightly requirement is fabricated for the commands that have none.
        for key in ["cargo check", "cargo build", "cargo run", "cargo doc"] {
            let entry = catalog
                .get(key)
                .expect("a command with no nightly nuance resolves");
            assert!(
                !maintained_text(entry).contains("nightly"),
                "`{key}` documents no nightly requirement (none fabricated)"
            );
        }
    }

    #[test]
    fn verification_upstream_excerpts_and_maintained_notes_are_separate() {
        // AC2 (separation): a maintained effect-note's prose is kept out of the
        // upstream syntax/example fields, mirroring the [039]/[040] pattern.
        let catalog = CommandCatalog::implementation_verification();
        for key in VERIFY_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a verification key resolves");
            for note in &entry.effect_notes {
                assert!(
                    !note.detail.trim().is_empty(),
                    "`{key}` carries a non-trivial effect note"
                );
                for excerpt in entry.syntax.iter().chain(entry.examples.iter()) {
                    assert!(
                        !excerpt.contains(&note.detail),
                        "`{key}` keeps effect-note prose out of the upstream excerpt"
                    );
                }
            }
        }
    }

    #[test]
    fn a_missing_section_is_never_fabricated_for_a_verification_key() {
        // AC1 (dangling): a cited section absent from the active snapshot is an
        // UnresolvedSection, never a fabricated section.
        let catalog = CommandCatalog::implementation_verification();
        let empty = crate::model::SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: crate::model::ManifestBody {
                snapshot_id: SnapshotId::new("empty-041").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        };
        // The first cited section of `cargo check` is its command chapter; with no
        // sections in the snapshot it dangles rather than being fabricated.
        let err = catalog
            .resolve(&empty, "cargo check")
            .expect_err("no section resolves in an empty snapshot");
        match err {
            LookupError::UnresolvedSection { key, section } => {
                assert_eq!(key, "cargo check");
                let expected = SectionId::derive(
                    &SourceId::new("cargo").expect("registry id"),
                    "commands/cargo-check",
                    None,
                    None,
                )
                .expect("a well-formed id");
                assert_eq!(section, expected);
            }
            other => panic!("expected UnresolvedSection, got {other:?}"),
        }
    }

    #[test]
    fn a_shell_operator_verification_key_is_refused_not_executed() {
        // The no-execution boundary holds for the verification family too: a
        // shell-operator key is refused at the key boundary, never interpolated
        // into a process.
        let catalog = CommandCatalog::implementation_verification();
        for bad in [
            "cargo run ; rm -rf /",
            "cargo test | xargs -n1 echo",
            "cargo clippy $(whoami)",
            "cargo fmt && echo pwned",
        ] {
            match catalog.get(bad) {
                Err(LookupError::InvalidKey { requested, .. }) => {
                    assert_eq!(requested, bad, "the refused request is preserved");
                }
                Err(other) => panic!("expected InvalidKey for `{bad}`, got {other:?}"),
                Ok(_) => panic!("`{bad}` must not resolve: a shell-operator key is refused"),
            }
        }
        // A clean verification key still resolves: refusal is specific to operators.
        assert!(
            catalog.get("cargo check").is_ok(),
            "a clean verification key resolves"
        );
    }

    #[test]
    fn verification_catalogs_are_pure_and_deterministic() {
        let a = CommandCatalog::implementation_verification();
        let b = CommandCatalog::implementation_verification();
        assert_eq!(
            a, b,
            "two verification catalogs built from the same code are identical"
        );
        assert_eq!(a.keys(), b.keys());
        let check_a = a.get("cargo check").expect("cargo check resolves");
        let check_b = b.get("cargo check").expect("cargo check resolves");
        assert_eq!(check_a, check_b);
    }

    // ===========================================================
    // [042] packaging / release-preparation family
    // ===========================================================

    // --- [042] AC1: exact keys and resolvable citations -------------------

    #[test]
    fn the_packaging_family_carries_the_two_exact_keys_in_order() {
        let catalog = CommandCatalog::packaging_release();
        assert_eq!(
            catalog.keys(),
            PKG_FAMILY_KEYS,
            "the family is the two named keys, in order"
        );
        assert_eq!(catalog.len(), 2);
    }

    #[test]
    fn every_packaging_key_looks_up_to_its_entry() {
        let catalog = CommandCatalog::packaging_release();
        for key in PKG_FAMILY_KEYS.iter().copied() {
            let entry = catalog
                .get(key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve to its entry, got {err}"));
            assert_eq!(
                entry.key.as_str(),
                key,
                "the entry's key is the exact key queried"
            );
        }
    }

    #[test]
    fn every_packaging_entry_cites_a_resolvable_upstream_section() {
        // AC1: every cited section resolves against a fixture snapshot (no
        // fabricated section); the first resolved id is the entry's first cited id.
        let catalog = CommandCatalog::packaging_release();
        let manifest = pkg_fixture_snapshot();
        for key in PKG_FAMILY_KEYS.iter().copied() {
            let resolved = catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve, got {err}"));
            assert!(
                !resolved.sections.is_empty(),
                "`{key}` resolved with at least one section"
            );
            assert_eq!(
                resolved.sections[0].id, resolved.entry.upstream_sections[0],
                "the first resolved section is the entry's first cited id"
            );
            // Every cited section resolved (none dropped or invented) and every
            // resolved section is a cargo-source section (no fabricated
            // placeholder).
            assert_eq!(
                resolved.sections.len(),
                resolved.entry.upstream_sections.len(),
                "every cited section resolved, none dropped or added"
            );
            for section in &resolved.sections {
                assert_eq!(section.source_id.as_str(), "cargo");
            }
        }
    }

    #[test]
    fn every_packaging_entry_cites_syntax_examples_and_prerequisites() {
        // AC1: each entry cites syntax, prerequisites, and release-preparation
        // references — the documented syntax/option excerpt, the documented
        // prerequisites, and the cited upstream section(s) are all present.
        let catalog = CommandCatalog::packaging_release();
        for key in PKG_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a packaging key resolves");
            assert!(
                !entry.syntax.is_empty(),
                "`{key}` carries a documented syntax excerpt"
            );
            assert!(
                !entry.examples.is_empty(),
                "`{key}` carries a documented example excerpt"
            );
            assert!(
                !entry.prerequisites.is_empty(),
                "`{key}` carries documented prerequisites"
            );
            assert!(
                !entry.compatibility.is_empty(),
                "`{key}` carries version/stability qualifications"
            );
            assert!(
                !entry.upstream_sections.is_empty(),
                "`{key}` cites its upstream section(s)"
            );
        }
    }

    #[test]
    fn every_packaging_entry_cites_release_preparation_references() {
        // AC1: each entry cites its command chapter AND a release-preparation
        // reference chapter — `packaging` for `cargo package`, `publishing` with
        // `registry` for `cargo publish` — resolvable against the fixture.
        let catalog = CommandCatalog::packaging_release();
        let manifest = pkg_fixture_snapshot();

        let package = catalog
            .get("cargo package")
            .expect("cargo package resolves");
        assert!(
            package
                .upstream_sections
                .iter()
                .any(|id| *id == cargo_section("commands/cargo-package")),
            "cargo package cites its command chapter"
        );
        assert!(
            package
                .upstream_sections
                .iter()
                .any(|id| *id == packaging_section()),
            "cargo package cites the packaging release-preparation reference"
        );

        let publish = catalog
            .get("cargo publish")
            .expect("cargo publish resolves");
        assert!(
            publish
                .upstream_sections
                .iter()
                .any(|id| *id == cargo_section("commands/cargo-publish")),
            "cargo publish cites its command chapter"
        );
        assert!(
            publish
                .upstream_sections
                .iter()
                .any(|id| *id == publishing_section()),
            "cargo publish cites the publishing release-preparation reference"
        );
        assert!(
            publish
                .upstream_sections
                .iter()
                .any(|id| *id == registry_section()),
            "cargo publish cites the registry (package sources) reference"
        );

        // Every cited section of both entries resolves against the fixture.
        for key in PKG_FAMILY_KEYS.iter().copied() {
            catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` resolves, got {err}"));
        }
    }

    // --- [042] AC2: explicit publication effects, never a permission -------

    #[test]
    fn cargo_publish_notes_an_explicit_publication_effect() {
        // AC2 (effects explicit): `cargo publish` names its publication side
        // effect — it uploads the package to the registry — via the
        // `EffectKind::Publication` note, and the note names the concrete effect.
        let catalog = CommandCatalog::packaging_release();
        let publish = catalog
            .get("cargo publish")
            .expect("cargo publish resolves");
        let publication = publish
            .effect_notes
            .iter()
            .find(|note| note.kind == EffectKind::Publication)
            .expect("cargo publish carries a Publication effect note");
        let detail = &publication.detail;
        assert!(
            detail.contains("upload") && detail.contains("registry"),
            "the Publication note names the upload-to-registry effect: {detail}"
        );
        // The note states that publishing is the default behavior.
        assert!(
            detail.contains("default"),
            "the Publication note states publishing is the default: {detail}"
        );
    }

    #[test]
    fn cargo_package_notes_an_artifact_write_but_no_publication() {
        // AC2 effect scoping: `cargo package` produces the `.crate` artifact but
        // carries NO Publication effect — it packages, it does not publish.
        let catalog = CommandCatalog::packaging_release();
        let package = catalog
            .get("cargo package")
            .expect("cargo package resolves");
        assert!(
            package
                .effect_notes
                .iter()
                .any(|note| note.kind == EffectKind::ArtifactWrite),
            "cargo package notes the .crate artifact write"
        );
        assert!(
            !package
                .effect_notes
                .iter()
                .any(|note| note.kind == EffectKind::Publication),
            "cargo package must NOT carry a Publication effect (it does not publish)"
        );
    }

    #[test]
    fn cargo_publish_documents_dry_run_as_prepare_only() {
        // AC1/AC2: `cargo publish --dry-run` is documented as prepare-only — it
        // packages and verifies and reports, but does NOT upload; publishing is
        // the default.
        let catalog = CommandCatalog::packaging_release();
        let publish = catalog
            .get("cargo publish")
            .expect("cargo publish resolves");
        let text = maintained_text(publish);
        assert!(
            text.contains("--dry-run"),
            "cargo publish documents --dry-run"
        );
        assert!(
            text.contains("does NOT upload"),
            "cargo publish states --dry-run does NOT upload (prepare-only)"
        );
        assert!(
            text.contains("reports what would be published"),
            "cargo publish states --dry-run reports what would be published"
        );
        assert!(
            text.contains("prepare-only"),
            "cargo publish frames --dry-run as the prepare-only form"
        );
        assert!(
            text.contains("default"),
            "cargo publish states publishing is the default behavior"
        );
    }

    #[test]
    fn lookup_is_never_described_as_permission_to_publish() {
        // AC2 (the safety boundary of the family): the guidance makes the
        // publication effect explicit while stating plainly that this is
        // *documentation lookup* that does not authorize, request, or perform a
        // publication. The maintained text carries that explicit not-permission
        // statement and carries NO permission-granting phrasing.
        let catalog = CommandCatalog::packaging_release();
        for key in PKG_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a packaging key resolves");
            let text = maintained_text(entry);
            assert!(
                text.contains("does not authorize"),
                "`{key}` states lookup does not authorize a publication"
            );
            assert!(
                text.contains("documentation lookup"),
                "`{key}` frames the entry as documentation lookup"
            );
            // The guidance must not describe lookup as a permission/authorization
            // to publish: none of these permission-granting phrases appear.
            for forbidden in [
                "permits publishing",
                "permission to publish",
                "authorized to publish",
                "grants permission",
                "authorizes publishing",
                "allows publishing",
                "entitled to publish",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "`{key}` must not describe lookup as permission to publish (found \
                     {forbidden:?})"
                );
            }
        }
    }

    #[test]
    fn packaging_upstream_excerpts_and_maintained_notes_are_separate() {
        // AC2 (separation): a maintained effect-note's prose is kept out of the
        // upstream syntax/example fields, mirroring the [039]/[040]/[041] pattern.
        let catalog = CommandCatalog::packaging_release();
        for key in PKG_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a packaging key resolves");
            for note in &entry.effect_notes {
                assert!(
                    !note.detail.trim().is_empty(),
                    "`{key}` carries a non-trivial effect note"
                );
                for excerpt in entry.syntax.iter().chain(entry.examples.iter()) {
                    assert!(
                        !excerpt.contains(&note.detail),
                        "`{key}` keeps effect-note prose out of the upstream excerpt"
                    );
                }
            }
        }
    }

    #[test]
    fn a_missing_section_is_never_fabricated_for_a_packaging_key() {
        // AC1 (dangling): a cited section absent from the active snapshot is an
        // UnresolvedSection, never a fabricated section.
        let catalog = CommandCatalog::packaging_release();
        let empty = crate::model::SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: crate::model::ManifestBody {
                snapshot_id: SnapshotId::new("empty-042").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        };
        // The first cited section of `cargo package` is its command chapter; with
        // no sections in the snapshot it dangles rather than being fabricated.
        let err = catalog
            .resolve(&empty, "cargo package")
            .expect_err("no section resolves in an empty snapshot");
        match err {
            LookupError::UnresolvedSection { key, section } => {
                assert_eq!(key, "cargo package");
                let expected = SectionId::derive(
                    &SourceId::new("cargo").expect("registry id"),
                    "commands/cargo-package",
                    None,
                    None,
                )
                .expect("a well-formed id");
                assert_eq!(section, expected);
            }
            other => panic!("expected UnresolvedSection, got {other:?}"),
        }
    }

    #[test]
    fn a_shell_operator_packaging_key_is_refused_not_executed() {
        // The no-execution boundary holds for the packaging family too: a
        // shell-operator key is refused at the key boundary, never interpolated
        // into a process. The catalog selects documentation; it never executes.
        let catalog = CommandCatalog::packaging_release();
        for bad in [
            "cargo publish ; rm -rf /",
            "cargo package | xargs -n1 echo",
            "cargo publish $(whoami)",
        ] {
            match catalog.get(bad) {
                Err(LookupError::InvalidKey { requested, .. }) => {
                    assert_eq!(requested, bad, "the refused request is preserved");
                }
                Err(other) => panic!("expected InvalidKey for `{bad}`, got {other:?}"),
                Ok(_) => panic!("`{bad}` must not resolve: a shell-operator key is refused"),
            }
        }
        // A clean packaging key still resolves: refusal is specific to operators.
        assert!(
            catalog.get("cargo publish").is_ok(),
            "a clean packaging key resolves"
        );
    }

    #[test]
    fn packaging_catalogs_are_pure_and_deterministic() {
        let a = CommandCatalog::packaging_release();
        let b = CommandCatalog::packaging_release();
        assert_eq!(
            a, b,
            "two packaging catalogs built from the same code are identical"
        );
        assert_eq!(a.keys(), b.keys());
        let pub_a = a.get("cargo publish").expect("cargo publish resolves");
        let pub_b = b.get("cargo publish").expect("cargo publish resolves");
        assert_eq!(pub_a, pub_b);
    }

    // ===========================================================
    // [043] toolchain and direct-tool family
    // ===========================================================

    // --- [043] AC1: exact keys and resolvable citations -------------------

    #[test]
    fn the_toolchain_family_carries_the_eight_exact_keys_in_order() {
        let catalog = CommandCatalog::toolchain_direct_tools();
        assert_eq!(
            catalog.keys(),
            TOOLCHAIN_FAMILY_KEYS,
            "the family is the eight named keys, in order"
        );
        assert_eq!(catalog.len(), 8);
    }

    #[test]
    fn every_toolchain_key_looks_up_to_its_entry() {
        let catalog = CommandCatalog::toolchain_direct_tools();
        for key in TOOLCHAIN_FAMILY_KEYS.iter().copied() {
            let entry = catalog
                .get(key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve to its entry, got {err}"));
            assert_eq!(
                entry.key.as_str(),
                key,
                "the entry's key is the exact key queried"
            );
        }
    }

    #[test]
    fn every_toolchain_entry_cites_a_resolvable_upstream_section() {
        // AC1: every cited section resolves against a fixture snapshot (no
        // fabricated section); the first resolved id is the entry's first cited
        // id, and no section is dropped or invented.
        let catalog = CommandCatalog::toolchain_direct_tools();
        let manifest = toolchain_fixture_snapshot();
        for key in TOOLCHAIN_FAMILY_KEYS.iter().copied() {
            let resolved = catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` should resolve, got {err}"));
            assert!(
                !resolved.sections.is_empty(),
                "`{key}` resolved with at least one section"
            );
            assert_eq!(
                resolved.sections[0].id, resolved.entry.upstream_sections[0],
                "the first resolved section is the entry's first cited id"
            );
            assert_eq!(
                resolved.sections.len(),
                resolved.entry.upstream_sections.len(),
                "every cited section resolved, none dropped or added"
            );
            // No fabricated placeholder: each resolved section belongs to a real
            // registry source (rustup/rustc/rustfmt/rustdoc), never an empty one.
            for section in &resolved.sections {
                assert!(!section.source_id.as_str().is_empty());
            }
        }
    }

    // --- [043] AC1: toolchain/component/target changes documented w/ links --

    #[test]
    fn toolchain_mutation_entries_document_changes_with_source_links() {
        // AC1: each rustup mutation entry cites its rustup Book chapter
        // (resolvable against the fixture) and carries a `ToolchainChange` effect
        // note naming the possible change — the "toolchain/component/target
        // changes … documented with source links" of the acceptance criteria.
        let catalog = CommandCatalog::toolchain_direct_tools();
        let manifest = toolchain_fixture_snapshot();
        let mutators: &[(&str, &str)] = &[
            ("rustup toolchain", "toolchains"),
            ("rustup override", "overrides"),
            ("rustup component", "components"),
            ("rustup target", "targets"),
        ];
        for (key, chapter) in mutators {
            let entry = catalog.get(key).expect("a rustup mutation resolves");
            let cited = rustup_chapter(chapter);
            assert!(
                entry.upstream_sections.contains(&cited),
                "`{key}` cites the rustup `{chapter}` chapter"
            );
            assert!(
                entry
                    .effect_notes
                    .iter()
                    .any(|note| note.kind == EffectKind::ToolchainChange),
                "`{key}` carries a ToolchainChange effect note"
            );
            // The cited chapter resolves: the source link is real, not dangling.
            catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` resolves, got {err}"));
        }
        // `rustup override` additionally cites the `toolchain-file` chapter (the
        // `rust-toolchain.toml` form of a directory override).
        let override_entry = catalog
            .get("rustup override")
            .expect("rustup override resolves");
        assert!(
            override_entry
                .upstream_sections
                .iter()
                .any(|id| *id == rustup_chapter("toolchain-file")),
            "rustup override cites the toolchain-file chapter"
        );
    }

    #[test]
    fn direct_tool_entries_document_prerequisites_with_source_links() {
        // AC1: each direct-tool entry (`rustc`, `rustfmt`, `rustdoc`) cites its
        // reference chapter(s), carries documented prerequisites, and resolves
        // against the fixture — the "direct-tool prerequisites documented with
        // source links" of the acceptance criteria.
        let catalog = CommandCatalog::toolchain_direct_tools();
        let manifest = toolchain_fixture_snapshot();
        for (key, expected) in [
            ("rustc", rustc_chapter("command-line-arguments")),
            ("rustfmt", rustfmt_section("README.md")),
            ("rustdoc", rustdoc_section("commands")),
        ] {
            let entry = catalog.get(key).expect("a direct-tool entry resolves");
            assert!(
                !entry.prerequisites.is_empty(),
                "`{key}` carries documented prerequisites"
            );
            assert!(
                entry.upstream_sections.contains(&expected),
                "`{key}` cites its reference chapter"
            );
            // The cited chapter resolves: the source link is real, not dangling.
            catalog
                .resolve(&manifest, key)
                .unwrap_or_else(|err| panic!("`{key}` resolves, got {err}"));
        }
        // `rustup show` is the inspection entry: it documents the active/
        // installed toolchain (a prerequisite) and carries no effect note.
        let show = catalog.get("rustup show").expect("rustup show resolves");
        assert!(
            !show.prerequisites.is_empty(),
            "rustup show carries prerequisites"
        );
        assert!(
            show.effect_notes.is_empty(),
            "rustup show is read-only and carries no effect note"
        );
    }

    #[test]
    fn every_toolchain_entry_cites_syntax_examples_and_prerequisites() {
        let catalog = CommandCatalog::toolchain_direct_tools();
        for key in TOOLCHAIN_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a toolchain key resolves");
            assert!(
                !entry.syntax.is_empty(),
                "`{key}` carries a documented syntax excerpt"
            );
            assert!(
                !entry.examples.is_empty(),
                "`{key}` carries a documented example excerpt"
            );
            assert!(
                !entry.prerequisites.is_empty(),
                "`{key}` carries documented prerequisites"
            );
            assert!(
                !entry.compatibility.is_empty(),
                "`{key}` carries version/stability qualifications"
            );
            assert!(
                !entry.upstream_sections.is_empty(),
                "`{key}` cites its upstream section(s)"
            );
        }
    }

    // --- [043] AC2: help/version and stable/nightly/unknown qualifications --

    #[test]
    fn help_and_version_mechanisms_are_documented_per_entry() {
        // AC2: the help/version mechanism is documented for every entry — each
        // names its `--help` (and the family documents `--version` and
        // `rustup show --active-toolchain`), kept as maintained metadata
        // separate from the upstream excerpts.
        let catalog = CommandCatalog::toolchain_direct_tools();
        for key in TOOLCHAIN_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a toolchain key resolves");
            assert!(
                maintained_text(entry).contains("--help"),
                "`{key}` documents its --help mechanism"
            );
        }
        // The family collectively documents a version query and the active-
        // toolchain inspection.
        let show = catalog.get("rustup show").expect("rustup show resolves");
        let show_text = maintained_text(show);
        assert!(
            show_text.contains("--version"),
            "the family documents a --version query"
        );
        assert!(
            show_text.contains("active-toolchain"),
            "rustup show documents the --active-toolchain inspection"
        );
        // A direct tool documents its own version query.
        let rustc = catalog.get("rustc").expect("rustc resolves");
        assert!(
            maintained_text(rustc).contains("--version"),
            "rustc documents its --version query"
        );
    }

    #[test]
    fn nightly_only_qualifications_are_retained_not_flattened() {
        // AC2: a documented nightly-only/unstable option keeps its qualification
        // rather than being flattened to an unconditional claim — the rustc,
        // rustfmt, and rustdoc entries each preserve the "nightly"/"unstable"
        // wording and state the qualification is not flattened.
        let catalog = CommandCatalog::toolchain_direct_tools();
        for key in ["rustc", "rustfmt", "rustdoc"] {
            let entry = catalog.get(key).expect("a direct-tool entry resolves");
            let text = maintained_text(entry);
            assert!(
                text.contains("nightly") || text.contains("unstable"),
                "`{key}` preserves the nightly/unstable qualification"
            );
            assert!(
                text.contains("not flattened"),
                "`{key}` states the qualification is not flattened to unconditional"
            );
            // The qualification is not flattened: the entry does not claim every
            // option is unconditionally available on a stable build.
            assert!(
                !text.contains("available on every stable build"),
                "`{key}` does not flatten to an unconditional stable-build claim"
            );
        }
    }

    #[test]
    fn unknown_version_notes_keep_their_qualification() {
        // AC2: the "unknown/undocumented version" note keeps its qualification —
        // the guidance says to consult the installed version and preserves the
        // qualification rather than asserting the documented options apply.
        let catalog = CommandCatalog::toolchain_direct_tools();
        for key in ["rustc", "rustfmt", "rustdoc", "rustup show"] {
            let entry = catalog.get(key).expect("a toolchain key resolves");
            let text = maintained_text(entry);
            assert!(
                text.contains("unknown"),
                "`{key}` carries the unknown-version note"
            );
            assert!(
                text.contains("preserved"),
                "`{key}` preserves the version-qualification"
            );
        }
    }

    // --- [043] AC2: separation and no-execution ---------------------------

    #[test]
    fn toolchain_and_direct_tool_excerpts_and_maintained_notes_are_separate() {
        // AC2 (separation): a maintained effect-note's prose is kept out of the
        // upstream syntax/example fields, mirroring the [039]-[042] pattern.
        let catalog = CommandCatalog::toolchain_direct_tools();
        for key in TOOLCHAIN_FAMILY_KEYS.iter().copied() {
            let entry = catalog.get(key).expect("a toolchain key resolves");
            for note in &entry.effect_notes {
                assert!(
                    !note.detail.trim().is_empty(),
                    "`{key}` carries a non-trivial effect note"
                );
                for excerpt in entry.syntax.iter().chain(entry.examples.iter()) {
                    assert!(
                        !excerpt.contains(&note.detail),
                        "`{key}` keeps effect-note prose out of the upstream excerpt"
                    );
                }
            }
        }
    }

    #[test]
    fn a_missing_section_is_never_fabricated_for_a_toolchain_key() {
        // AC1 (dangling): a cited section absent from the active snapshot is an
        // UnresolvedSection, never a fabricated section.
        let catalog = CommandCatalog::toolchain_direct_tools();
        let empty = crate::model::SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: crate::model::ManifestBody {
                snapshot_id: SnapshotId::new("empty-043").expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections: vec![],
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        };
        // The first cited section of `rustup show` is the rustup `index`
        // chapter; with no sections in the snapshot it dangles rather than being
        // fabricated.
        let err = catalog
            .resolve(&empty, "rustup show")
            .expect_err("no section resolves in an empty snapshot");
        match err {
            LookupError::UnresolvedSection { key, section } => {
                assert_eq!(key, "rustup show");
                let expected = SectionId::derive(
                    &SourceId::new("rustup").expect("registry id"),
                    "index",
                    None,
                    None,
                )
                .expect("a well-formed id");
                assert_eq!(section, expected);
            }
            other => panic!("expected UnresolvedSection, got {other:?}"),
        }
    }

    #[test]
    fn a_shell_operator_toolchain_key_is_refused_not_executed() {
        // The no-execution boundary holds for the toolchain/direct-tool family
        // too: a shell-operator key is refused at the key boundary, never
        // interpolated into a process. The catalog selects documentation; it
        // never executes.
        let catalog = CommandCatalog::toolchain_direct_tools();
        for bad in [
            "rustup toolchain install nightly ; rm -rf /",
            "rustup override set stable | xargs -n1 echo",
            "rustc $(whoami)",
            "rustfmt && echo pwned",
        ] {
            match catalog.get(bad) {
                Err(LookupError::InvalidKey { requested, .. }) => {
                    assert_eq!(requested, bad, "the refused request is preserved");
                }
                Err(other) => panic!("expected InvalidKey for `{bad}`, got {other:?}"),
                Ok(_) => panic!("`{bad}` must not resolve: a shell-operator key is refused"),
            }
        }
        // A clean toolchain key still resolves: refusal is specific to operators.
        assert!(
            catalog.get("rustc").is_ok(),
            "a clean toolchain key resolves"
        );
    }

    #[test]
    fn toolchain_catalogs_are_pure_and_deterministic() {
        let a = CommandCatalog::toolchain_direct_tools();
        let b = CommandCatalog::toolchain_direct_tools();
        assert_eq!(
            a, b,
            "two toolchain catalogs built from the same code are identical"
        );
        assert_eq!(a.keys(), b.keys());
        let a_show = a.get("rustup show").expect("rustup show resolves");
        let b_show = b.get("rustup show").expect("rustup show resolves");
        assert_eq!(a_show, b_show);
    }
}

// ---------------------------------------------------------------------------
// Task [044][M3] tests: total, pure, offline. No upstream ingestion, no
// process execution. The guidance and inventory are composed from the [036]
// section-read, [037] pagination, and [039]-[043] catalog seams.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod guidance_tests {
    use super::*;
    use crate::model::{
        ManifestBody, SchemaVersion, Section, SectionBuilder, SnapshotId, SnapshotManifest,
        SourceId, Timestamp, Url,
    };
    use crate::pagination::{MAX_RESPONSE_BYTES, MismatchField};
    use crate::provenance::{Instant, StaleThreshold};

    // --- fixtures ---------------------------------------------------------

    /// Build a snapshot carrying exactly the given sections, with the other
    /// collections empty — the offline "a hand-built snapshot makes the cited
    /// ids resolvable" seam.
    fn manifest(snapshot_id: &str, sections: Vec<Section>) -> SnapshotManifest {
        SnapshotManifest {
            schema_version: SchemaVersion::V1,
            body: ManifestBody {
                snapshot_id: SnapshotId::new(snapshot_id).expect("a URI-safe snapshot id"),
                created_at: Timestamp::new("2026-01-01T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test".to_string(),
                sections,
                sources: vec![],
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        }
    }

    /// Build a section under `source`/`chapter` the way the catalog derives its
    /// cited ids, so a fixture section's derived id matches the id a catalog
    /// entry cites and resolution succeeds.
    fn section(source: &str, chapter: &str, body: &str) -> Section {
        SectionBuilder::new(
            SourceId::new(source).expect("a registry source id"),
            format!("{chapter} section"),
            chapter,
            Url::new(format!("https://doc.rust-lang.org/{source}/{chapter}.html"))
                .expect("a valid canonical URL"),
            body.to_string(),
        )
        .build()
        .expect("a well-formed section")
    }

    /// A snapshot with one small `cargo new` section — the "a single command's
    /// content fits in one page" case.
    fn small_snapshot() -> SnapshotManifest {
        manifest(
            "fixture-044-small",
            vec![section(
                "cargo",
                "commands/cargo-new",
                "A short, faithful excerpt from the Cargo Book.",
            )],
        )
    }

    /// A snapshot with two small sections the `cargo clippy` entry cites — the
    /// "a multi-section command that fits in one page" case.
    fn multi_small_snapshot() -> SnapshotManifest {
        manifest(
            "fixture-044-multi",
            vec![
                section("clippy", "usage", "Clippy usage excerpt."),
                section("clippy", "configuration", "Clippy configuration excerpt."),
            ],
        )
    }

    /// A snapshot with the two `cargo test` sections, each with a 20 KiB body so
    /// the pair overflows the response cap and a guidance lookup must page it
    /// with an explicit continuation.
    fn huge_cargo_test_snapshot() -> SnapshotManifest {
        manifest(
            "fixture-044",
            vec![
                section("cargo", "commands/cargo-test", &"x".repeat(20_000)),
                section("rustdoc", "documentation-tests", &"y".repeat(20_000)),
            ],
        )
    }

    /// A snapshot with one 40 KiB `cargo new` section — the "a single section
    /// overflows the cap" case, which sets the `next_item_oversized` signal.
    fn oversized_section_snapshot() -> SnapshotManifest {
        manifest(
            "fixture-044-oversized",
            vec![section("cargo", "commands/cargo-new", &"z".repeat(40_000))],
        )
    }

    /// A snapshot carrying a different `cargo new` section — used as the "another
    /// snapshot" in the snapshot-bound test.
    fn other_snapshot() -> SnapshotManifest {
        manifest(
            "fixture-044-other",
            vec![section(
                "cargo",
                "commands/cargo-new",
                "A different snapshot's excerpt.",
            )],
        )
    }

    fn as_of() -> Instant {
        Instant::parse("2026-01-01T00:00:00Z").expect("a valid instant")
    }

    fn threshold() -> StaleThreshold {
        StaleThreshold::default()
    }

    // --- AC1: supported keys return source-backed sections, metadata, effect
    //     notes, and an explicit continuation -----------------------------

    #[test]
    fn a_supported_key_returns_source_backed_sections_metadata_and_effect_notes() {
        let catalog = CommandCatalog::setup_inspection();
        let manifest = small_snapshot();
        let guidance = command_guidance(
            &catalog,
            &manifest,
            as_of(),
            threshold(),
            "cargo new",
            None,
            None,
        )
        .expect("`cargo new` resolves in the fixture snapshot");

        // The entry's metadata rides on the page, kept distinct from the upstream
        // sections (AC2 / `plan.md` §350).
        assert!(
            !guidance.entry.syntax.is_empty(),
            "documented syntax is present"
        );
        assert!(
            !guidance.entry.examples.is_empty(),
            "documented examples are present"
        );
        assert!(
            !guidance.entry.prerequisites.is_empty(),
            "prerequisites are present"
        );
        assert!(
            !guidance.entry.compatibility.is_empty(),
            "version/stability qualifications are present"
        );
        assert!(
            !guidance.entry.effect_notes.is_empty(),
            "separately-labeled effect notes are present"
        );

        // The source-backed section content is retrievable, non-fabricated, and
        // snapshot-bound.
        let sections: Vec<_> = guidance.sections().collect();
        assert_eq!(sections.len(), 1, "`cargo new` cites one section");
        let read = &sections[0];
        assert!(
            !read.section.body.is_empty(),
            "the section body is source-backed, not fabricated"
        );
        assert_eq!(
            read.section.source_id.as_str(),
            "cargo",
            "the section is the real upstream source, not a placeholder"
        );
        assert_eq!(
            read.snapshot_id.as_str(),
            "fixture-044-small",
            "the read is snapshot-bound"
        );
        assert_eq!(
            guidance.snapshot_id.as_str(),
            "fixture-044-small",
            "the guidance carries the snapshot identity"
        );

        // No continuation: the one section fits, so the page is not truncated.
        assert!(
            !guidance.page.truncated,
            "a single fitting section needs no continuation"
        );
        assert!(
            guidance.page.next_cursor.is_none(),
            "the final page carries no next cursor"
        );
    }

    #[test]
    fn a_multi_section_command_that_fits_has_no_continuation() {
        let catalog = CommandCatalog::implementation_verification();
        let manifest = multi_small_snapshot();
        let guidance = command_guidance(
            &catalog,
            &manifest,
            as_of(),
            threshold(),
            "cargo clippy",
            None,
            None,
        )
        .expect("`cargo clippy` resolves in the fixture snapshot");

        let sections: Vec<_> = guidance.sections().collect();
        assert_eq!(sections.len(), 2, "both cited sections fit on one page");
        assert!(
            !guidance.page.truncated,
            "a page that fits all sections is not truncated"
        );
        assert!(
            guidance.page.next_cursor.is_none(),
            "the final page carries no next cursor"
        );

        // The section order matches the entry's citation order.
        let ids: Vec<String> = sections
            .iter()
            .map(|read| read.section.id.as_str().to_string())
            .collect();
        let expected = catalog
            .get("cargo clippy")
            .expect("`cargo clippy` is in the catalog")
            .upstream_sections
            .iter()
            .map(|s| s.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids, expected, "the sections are returned in citation order");
    }

    #[test]
    fn a_clipped_guidance_pages_with_an_explicit_continuation_and_reassembles() {
        let catalog = CommandCatalog::implementation_verification();
        let manifest = huge_cargo_test_snapshot();

        // Page 1: the first section, with an explicit continuation and a
        // truncation reason (no silent truncation).
        let page1 = command_guidance(
            &catalog,
            &manifest,
            as_of(),
            threshold(),
            "cargo test",
            None,
            None,
        )
        .expect("`cargo test` resolves");
        assert!(
            page1.page.truncated,
            "a clipped response reports truncation, never silently truncating"
        );
        let next = page1
            .page
            .next_cursor
            .clone()
            .expect("a clipped page carries an explicit next cursor");
        assert!(
            !page1.page.next_item_oversized,
            "the next section fits on its own page (not oversized)"
        );
        assert!(
            page1.page.serialized_bytes() <= MAX_RESPONSE_BYTES,
            "page 1 respects the response cap"
        );

        // Page 2: resume the explicit cursor; the remainder is returned whole.
        let page2 = command_guidance(
            &catalog,
            &manifest,
            as_of(),
            threshold(),
            "cargo test",
            None,
            Some(&next),
        )
        .expect("the continuation cursor resumes the page");
        assert!(!page2.page.truncated, "the final page is not truncated");
        assert!(
            page2.page.next_cursor.is_none(),
            "the final page carries no next cursor"
        );
        assert!(
            page2.page.serialized_bytes() <= MAX_RESPONSE_BYTES,
            "page 2 respects the response cap"
        );

        // Reassembling the pages reproduces the whole command's cited sections, in
        // order, with each body intact.
        let mut ids: Vec<String> = page1
            .sections()
            .map(|r| r.section.id.as_str().to_string())
            .collect();
        let mut bodies: Vec<String> = page1.sections().map(|r| r.section.body.clone()).collect();
        for read in page2.sections() {
            ids.push(read.section.id.as_str().to_string());
            bodies.push(read.section.body.clone());
        }
        let expected = catalog
            .get("cargo test")
            .expect("`cargo test` is in the catalog")
            .upstream_sections
            .iter()
            .map(|s| s.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            ids, expected,
            "the pages reassemble into the whole, in citation order"
        );
        assert_eq!(
            bodies.len(),
            2,
            "both section bodies are carried, none dropped"
        );
        assert!(
            bodies.iter().all(|b| b.len() == 20_000),
            "no section body is silently truncated"
        );
    }

    #[test]
    fn an_oversized_section_sets_the_next_item_oversized_signal() {
        let catalog = CommandCatalog::setup_inspection();
        let manifest = oversized_section_snapshot();
        let page = command_guidance(
            &catalog,
            &manifest,
            as_of(),
            threshold(),
            "cargo new",
            None,
            None,
        )
        .expect("`cargo new` resolves");
        assert!(
            page.page.truncated,
            "the oversized section is not returned on this page"
        );
        assert!(
            page.page.next_item_oversized,
            "the oversized next item is flagged for clipping, never silently dropped"
        );
        assert!(
            page.page.items.is_empty(),
            "no oversized item is returned whole"
        );
        assert!(
            page.page.serialized_bytes() <= MAX_RESPONSE_BYTES,
            "the page itself stays within the response cap"
        );
    }

    // --- AC1/AC2: rejection at the key / citation boundary ----------------

    #[test]
    fn an_unsupported_key_is_rejected_with_discoverable_alternatives() {
        let catalog = CommandCatalog::setup_inspection();
        let manifest = small_snapshot();
        let err = command_guidance(
            &catalog,
            &manifest,
            as_of(),
            threshold(),
            "cargo build",
            None,
            None,
        )
        .expect_err("`cargo build` is not in the setup/inspection family");
        let message = err.to_string();
        match err {
            CommandGuidanceError::Lookup(LookupError::UnknownKey {
                requested,
                supported,
            }) => {
                assert_eq!(requested, "cargo build");
                assert_eq!(
                    supported,
                    catalog.keys(),
                    "the alternatives are exactly the catalog's supported keys"
                );
                assert!(
                    supported.contains(&"cargo new".to_string()),
                    "the alternatives name a supported key"
                );
                assert!(
                    message.contains("cargo build"),
                    "the error names the refused request"
                );
            }
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn a_shell_operator_key_is_refused_not_executed() {
        let catalog = CommandCatalog::setup_inspection();
        let manifest = small_snapshot();
        for bad in [
            "cargo new ; rm -rf /",
            "cargo new | xargs -n1 echo",
            "cargo metadata $(whoami)",
            "cargo init && echo pwned",
            "cargo new", // a clean key, for contrast
        ] {
            let result =
                command_guidance(&catalog, &manifest, as_of(), threshold(), bad, None, None);
            if bad == "cargo new" {
                assert!(
                    result.is_ok(),
                    "a clean key still resolves; refusal is specific to operators"
                );
            } else {
                match result {
                    Err(CommandGuidanceError::Lookup(LookupError::InvalidKey {
                        requested,
                        ..
                    })) => {
                        assert_eq!(requested, bad, "the refused request is preserved");
                    }
                    other => panic!("expected InvalidKey for `{bad}`, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn a_dangling_citation_is_not_fabricated() {
        let catalog = CommandCatalog::setup_inspection();
        let manifest = manifest("fixture-044-empty", vec![]);
        let err = command_guidance(
            &catalog,
            &manifest,
            as_of(),
            threshold(),
            "cargo new",
            None,
            None,
        )
        .expect_err("no section resolves in an empty snapshot");
        match err {
            CommandGuidanceError::Lookup(LookupError::UnresolvedSection { key, section }) => {
                assert_eq!(key, "cargo new");
                let expected = SectionId::derive(
                    &SourceId::new("cargo").expect("a registry source id"),
                    "commands/cargo-new",
                    None,
                    None,
                )
                .expect("a well-formed section id");
                assert_eq!(
                    section, expected,
                    "the unresolvable citation is named, not invented"
                );
            }
            other => panic!("expected UnresolvedSection, got {other:?}"),
        }
    }

    // --- AC2: no process-execution path ---------------------------------

    #[test]
    fn command_lookup_has_no_subprocess_execution_path() {
        // The command catalog is documentation-only: no process-execution
        // primitive may appear anywhere in this module. The forbidden tokens are
        // assembled from pieces so this test's own source never matches its own
        // scan.
        let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/commands.rs"));
        let forbidden = [
            "std::".to_string() + "process", // the standard-library process module
            "Command::".to_string() + "new", // the standard-library command constructor
            ".spaw".to_string() + "n(",      // the spawn method call
            ".exe".to_string() + "c(",       // the exec method call
        ];
        for needle in forbidden {
            assert!(
                !source.contains(needle.as_str()),
                "the command catalog must contain no process-execution primitive, but found: {needle}"
            );
        }
    }

    // --- AC1: the discoverable, snapshot-bound, cap-bounded inventory ----

    #[test]
    fn the_inventory_paginates_keys_with_an_explicit_continuation() {
        let catalog = CommandCatalog::combined();
        let manifest = small_snapshot();
        let total = catalog.keys().len();

        let page1 = command_inventory(&catalog, &manifest, Some(3), None)
            .expect("the first inventory page is served");
        assert_eq!(
            page1.returned, 3,
            "the first page returns the requested count"
        );
        assert_eq!(page1.keys.len(), 3, "returned matches the page's keys");
        assert!(page1.truncated, "more keys follow the first page");
        let next = page1
            .next_cursor
            .clone()
            .expect("a non-final page carries an explicit continuation cursor");

        let page2 = command_inventory(&catalog, &manifest, Some(3), Some(&next))
            .expect("the continuation cursor resumes the inventory");
        assert_eq!(
            page2.returned, 3,
            "the second page returns the requested count"
        );
        assert!(
            page1.serialized_bytes() <= MAX_RESPONSE_BYTES,
            "page 1 respects the cap"
        );
        assert!(
            page2.serialized_bytes() <= MAX_RESPONSE_BYTES,
            "page 2 respects the cap"
        );
        assert!(total >= 6, "the combined catalog has enough keys to page");
    }

    #[test]
    fn the_inventory_reassembles_to_all_supported_keys() {
        let catalog = CommandCatalog::combined();
        let manifest = small_snapshot();

        let mut collected: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = command_inventory(&catalog, &manifest, Some(4), cursor.as_deref())
                .expect("each inventory page is served");
            assert!(
                page.serialized_bytes() <= MAX_RESPONSE_BYTES,
                "every page respects the cap"
            );
            collected.extend(page.keys.iter().cloned());
            if let Some(next) = page.next_cursor {
                cursor = Some(next);
            } else {
                break;
            }
        }
        assert_eq!(
            collected,
            catalog.keys(),
            "walking every page reproduces all supported keys, in order, with no silent full dump"
        );
        assert!(
            collected.len() >= 20,
            "the combined catalog is large enough to page"
        );
    }

    #[test]
    fn the_inventory_is_snapshot_bound() {
        let catalog = CommandCatalog::combined();
        let manifest_a = small_snapshot();
        let manifest_b = other_snapshot();

        let page1 = command_inventory(&catalog, &manifest_a, Some(3), None)
            .expect("the first page is served under manifest_a");
        let cursor = page1
            .next_cursor
            .clone()
            .expect("a non-final page carries a cursor");

        // A cursor minted under manifest_a cannot resume a call under manifest_b.
        let err = command_inventory(&catalog, &manifest_b, Some(3), Some(&cursor))
            .expect_err("a cursor from another snapshot is rejected");
        match err {
            CursorError::Mismatch { field, .. } => {
                assert_eq!(
                    field,
                    MismatchField::Snapshot,
                    "the mismatch is the snapshot"
                )
            }
            other => panic!("expected a snapshot mismatch, got {other:?}"),
        }

        // A malformed token is also rejected, never a panic or a wrong page.
        let bad = command_inventory(&catalog, &manifest_a, Some(3), Some("not-a-cursor"))
            .expect_err("a malformed token is rejected");
        assert!(
            matches!(bad, CursorError::Malformed(_)),
            "a malformed token is a Malformed error"
        );
    }

    #[test]
    fn an_inventory_page_never_exceeds_the_response_cap() {
        let catalog = CommandCatalog::combined();
        let manifest = small_snapshot();

        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let page = command_inventory(&catalog, &manifest, None, cursor.as_deref())
                .expect("each page is served");
            assert!(
                page.serialized_bytes() <= MAX_RESPONSE_BYTES,
                "every default-limit page respects the cap"
            );
            pages += 1;
            if let Some(next) = page.next_cursor {
                cursor = Some(next);
            } else {
                break;
            }
        }
        assert!(
            pages >= 2,
            "the combined catalog pages under the default limit"
        );
    }

    #[test]
    fn guidance_lookups_are_pure_and_deterministic() {
        let catalog = CommandCatalog::combined();
        let manifest = huge_cargo_test_snapshot();
        let a = command_guidance(
            &catalog,
            &manifest,
            as_of(),
            threshold(),
            "cargo test",
            None,
            None,
        )
        .expect("resolves");
        let b = command_guidance(
            &catalog,
            &manifest,
            as_of(),
            threshold(),
            "cargo test",
            None,
            None,
        )
        .expect("resolves");
        assert_eq!(
            a, b,
            "two guidance lookups of the same inputs are identical"
        );

        let inv_a = command_inventory(&catalog, &manifest, Some(5), None).expect("resolves");
        let inv_b = command_inventory(&catalog, &manifest, Some(5), None).expect("resolves");
        assert_eq!(
            inv_a, inv_b,
            "two inventory pages of the same inputs are identical"
        );
    }
}
