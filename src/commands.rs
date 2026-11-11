//! Source-backed command catalog — task `[039][M3]` (issue [#39]).
//!
//! This module is the *catalog* side of `plan.md` §6 ("Build and validate the
//! source-backed command catalog, including citations, compatibility
//! qualifications, and effect notes"): the fixed, human-reviewable set of
//! [`CommandEntry`]s that a later task (`[040]`-`[044]`) grows into the full
//! command families and that the `get_command_guidance` / catalog resource
//! (`[048]+`) serves. [039] seeds the **project setup and inspection** family —
//! `cargo new`, `cargo init`, `cargo metadata`, `cargo locate-project`, and
//! `cargo tree` — the family `plan.md` §6 names first.
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
//! **Out of scope for [039]** (later work, deliberately *not* built here): the
//! `[040]`-`[044]` command families, the `[048]+` MCP tool/resource wiring, and
//! any change to the ingestion pipeline (`crate::ingest`) — this module *defines*
//! the catalog and its resolution seam and consumes the tested [`SectionIndex`].

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
            Self::UnknownKey { requested, supported } => write!(
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
/// [039] ships one family — the project setup and inspection commands of
/// `plan.md` §6 — via [`CommandCatalog::setup_inspection`]. A catalog is a plain,
/// ordered, cloneable value (no interior mutability, no global): building it twice
/// yields byte-identical entries, so lookups are pure and deterministic. The
/// later families (`[040]`-`[044]`) are added as further catalogs or as a
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
                    })
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
        purpose: "Create a new Cargo package in a new directory."
            .to_string(),
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
        key: CommandKey::new("cargo metadata")
            .expect("`cargo metadata` is a well-formed key"),
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
