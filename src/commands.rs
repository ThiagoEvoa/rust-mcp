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
//! sections — and a [`CommandCatalog::combined`] that concatenates the two
//! families.
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
//! **Out of scope for [040]** (later work, deliberately *not* built here): the
//! remaining `[041]`-`[044]` command families, the `[048]+` MCP tool/resource
//! wiring, and any change to the ingestion pipeline (`crate::ingest`) — this
//! module *defines* the catalog and its resolution seam and consumes the tested
//! [`SectionIndex`].

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
/// dependency and reproducibility family via [`CommandCatalog::dependencies`] and
/// a [`CommandCatalog::combined`] that concatenates the two. A catalog is a plain,
/// ordered, cloneable value (no interior mutability, no global): building it twice
/// yields byte-identical entries, so lookups are pure and deterministic. The
/// remaining families (`[041]`-`[044]`) are added as further catalogs or as a
/// concatenation this module's lookup already handles.
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
    /// The `--offline`/`--locked` scoping (AC2) is per command: `--offline` is
    /// documented for all five (a universal cache-only flag for these
    /// registry-touching commands), while `--locked` — the "the lockfile will not
    /// change" assertion — is documented only for the four commands whose purpose
    /// is to write or resolve the lockfile (`cargo add`, `cargo remove`,
    /// `cargo update`, `cargo generate-lockfile`). `cargo fetch`'s purpose is to
    /// refresh/download, the inverse of that assertion, so its entry describes
    /// `--offline` and points to `--help` for the version-specific flag list
    /// rather than claiming `--locked`.
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

    /// The catalog as of `[040]`: the [setup and inspection](Self::setup_inspection)
    /// family concatenated with the [dependency and reproducibility](Self::dependencies)
    /// family, in that order — the two families `plan.md` §6 names first.
    ///
    /// This is the "a concatenation this module's lookup already handles" case:
    /// the lookup/resolve paths are unchanged by the concatenation, so every key
    /// from either family still resolves exactly as it did in its own catalog.
    pub fn combined() -> CommandCatalog {
        let mut entries = CommandCatalog::setup_inspection().entries;
        entries.extend(CommandCatalog::dependencies().entries);
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
// five, `--locked` only for the four that write or resolve the lockfile.
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
        key: CommandKey::new("cargo remove")
            .expect("`cargo remove` is a well-formed key"),
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
        purpose: "Update dependencies as specified by the `Cargo.lock` file."
            .to_string(),
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
             local cache only. `cargo fetch` refreshes and downloads the lockfile, \
             the inverse of the `--locked` assertion that the lockfile will not \
             change, so `--locked` is not documented on this entry."
                .to_string(),
        ],
        effect_notes: vec![
            EffectNote {
                kind: EffectKind::DependencyDownload,
                detail: "downloads the package's dependencies into the local cache; \
                         `--offline` makes the fetch use the cache only (no network)."
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
}
