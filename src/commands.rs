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
use crate::search::SectionIndex;

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

    /// The catalog as of `[042]`: the [setup and inspection](Self::setup_inspection),
    /// [dependency and reproducibility](Self::dependencies), [implementation and
    /// verification](Self::implementation_verification), and [packaging and release-
    /// preparation](Self::packaging_release) families, in that order — the four
    /// families `plan.md` §6 names first.
    ///
    /// This is the "a concatenation this module's lookup already handles" case:
    /// the lookup/resolve paths are unchanged by the concatenation, so every key
    /// from any family still resolves exactly as it did in its own catalog.
    pub fn combined() -> CommandCatalog {
        let mut entries = CommandCatalog::setup_inspection().entries;
        entries.extend(CommandCatalog::dependencies().entries);
        entries.extend(CommandCatalog::implementation_verification().entries);
        entries.extend(CommandCatalog::packaging_release().entries);
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
        purpose: "Package the current package into a distributable `.crate` archive."
            .to_string(),
        upstream_sections: vec![
            cargo_section("commands/cargo-package"),
            packaging_section(),
        ],
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
                         upload — uploading is `cargo publish`.".to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "the default verification build may download dependencies that \
                         are not yet cached; `--offline` (or `--frozen`) prevents any \
                         download. `--no-verify` skips the verification build.".to_string(),
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
        purpose: "Package and upload the current package to the configured registry."
            .to_string(),
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
                         authorizes nor performs a publication.".to_string(),
            },
            EffectNote {
                kind: EffectKind::ArtifactWrite,
                detail: "packages the crate into a distributable `.crate` archive under \
                         `target/` (as `cargo package` does) before publishing; \
                         `--no-verify` skips the default verification build.".to_string(),
            },
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "the verification build may download dependencies that are not yet \
                         cached; `--offline` (or `--frozen`) prevents any download. A \
                         `--dry-run` run still downloads for the verification but uploads \
                         nothing.".to_string(),
            },
        ],
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
        // `combined()` is the three families concatenated; the lookup/resolve paths
        // are unchanged, so every key from any family still resolves exactly as it
        // did in its own catalog.
        let catalog = CommandCatalog::combined();
        // 5 setup/inspection + 5 dependency + 9 implementation/verification = 19.
        assert_eq!(
            catalog.len(),
            19,
            "combined is the three families concatenated"
        );
        let expected: Vec<String> = FAMILY_KEYS
            .iter()
            .chain(DEP_FAMILY_KEYS.iter())
            .chain(VERIFY_FAMILY_KEYS.iter())
            .map(|key| key.to_string())
            .collect();
        assert_eq!(
            catalog.keys(),
            expected,
            "combined keys are the three families, in order"
        );
        let manifest = combined_fixture_snapshot();
        for key in FAMILY_KEYS
            .iter()
            .copied()
            .chain(DEP_FAMILY_KEYS.iter().copied())
            .chain(VERIFY_FAMILY_KEYS.iter().copied())
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
}
