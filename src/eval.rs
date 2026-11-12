//! Independent retrieval evaluation corpus — task [045][M3] (issue [#45]).
//!
//! [issue 45]: https://github.com/ThiagoEvoa/rust-mcp/issues/45
//!
//! This module is the **hand-labeled retrieval evaluation set** `plan.md` §11
//! requires *before* ranking is tuned: a fixed, curated set of realistic agent
//! queries, each paired with the genuine section label(s) it should retrieve.
//! A later M3 ranking consumer (`[035]+`) and the M5 acceptance harness
//! (`[058]+`) run the corpus through the assembled index and measure recall —
//! the corpus itself is the thing that is measured.
//!
//! ## Acceptance criteria (issue [#45], `plan.md` §11)
//!
//! * **AC1 — coverage.** All eight sources are covered, with at least three
//!   queries each, and at least twelve command/workflow queries overall,
//!   reaching at least forty cases total. See [`corpus_covers_at_least_forty_cases`],
//!   [`every_source_has_at_least_three_queries`],
//!   [`all_eight_sources_are_covered`], and [`at_least_twelve_command_workflow_queries`].
//! * **AC2 — canonical cases with genuine section labels, independent of ranking
//!   aliases.** The corpus carries canonical exact-match cases (a guideline code,
//!   a command name, and a lint id) alongside ordinary agent phrasing, and each
//!   case's expected label is a *genuine section label* — the hierarchical
//!   document path the section lives at — never the ranking's normalized/aliased
//!   query token. The corpus is **independent of the `[033]`-`[035]` ranking
//!   internals**: it evaluates the *section identity* a query retrieves, not
//!   whether a query re-matches its own normalized form. See
//!   [`canonical_cases_are_present`],
//!   [`expected_labels_are_genuine_section_labels`], and
//!   [`corpus_is_independent_of_ranking_aliases`].
//!
//! ## Why "independent of ranking aliases" is the load-bearing property
//!
//! The `[033]` tokenizer normalizes a query into atomic ranking tokens
//! (`cargo test` → one `Command` token, `C-CONV` → a `GuidelineCode` token,
//! `clippy::unwrap_used` → a `LintId` token, `--no-default-features` → a
//! `Flag`). A ranking could "cheat" recall by matching a query's *own normalized
//! token* back to a label that is just that token in disguise. This corpus
//! prevents that: every expected label is a **section path** containing a `/`
//! separator, whereas every normalized ranking token is a *flat* atomic form
//! that the tokenizer never emits with a `/` in it. The two are therefore
//! provably disjoint, and [`corpus_is_independent_of_ranking_aliases`] verifies
//! it directly by cross-checking each label against the tokenizer's output.
//!
//! ## Shape of a case
//!
//! An [`EvalCase`] is a fixed `&'static` record: a stable id, a category
//! ([`EvalKind`]), the source it primarily targets (`SourceCollection`, so it
//! can never name an unregistered source), the realistic query string, the
//! genuine expected section label(s), and the declared ranking target
//! ([`RankTarget`]) — *first* for a canonical exact-match case, *top five*
//! otherwise (`plan.md` §11: "first place for every exact … case"; the labeled
//! section "in the top five" for the rest).
//!
//! ## Transitional status
//!
//! Until a consumer wires the corpus in, it is `#[allow(dead_code)]`-covered,
//! mirroring the milestone pattern of [`crate::commands`] and
//! [`crate::reads`]; the `#[cfg(test)]` module below exercises it fully so the
//! coverage and independence invariants are proven today.

#![allow(dead_code)] // transitional: unwired until the [035]+ ranking / M5 harness

use crate::registry::SourceCollection;

// ---------------------------------------------------------------------------
// Case shape
// ---------------------------------------------------------------------------

/// The category of an evaluation case.
///
/// The categories separate the *canonical exact-match* cases
/// ([`GuidelineCode`], [`Command`], [`Lint`]) — which must rank **first** —
/// from the *command/workflow* queries ([`Workflow`]) and the *ordinary*
/// agent phrasing ([`Ordinary`]), which are measured **within the top five**.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EvalKind {
    /// A canonical exact guideline-code case (e.g. the query `C-CONV`).
    GuidelineCode,
    /// A canonical exact command-name case (e.g. the query `cargo build`).
    Command,
    /// A canonical exact lint-id case (e.g. the query `clippy::unwrap_used`).
    Lint,
    /// A natural-language query about running a command or a multi-step
    /// workflow. Counts toward the "command/workflow queries" total (AC1).
    Workflow,
    /// An ordinary natural-language agent query (not a canonical exact match).
    Ordinary,
}

impl EvalKind {
    /// Whether this category is a *canonical exact-match* case — a guideline
    /// code, a command name, or a lint id — that `plan.md` §11 requires in
    /// **first place**.
    pub fn is_canonical(self) -> bool {
        matches!(
            self,
            EvalKind::GuidelineCode | EvalKind::Command | EvalKind::Lint
        )
    }

    /// Whether this category is a *command/workflow* query — a canonical command
    /// name or a natural-language workflow question. These are the cases that
    /// count toward the "at least twelve command/workflow queries" total (AC1).
    pub fn is_command_workflow(self) -> bool {
        matches!(self, EvalKind::Command | EvalKind::Workflow)
    }
}

/// The ranking target a case declares its expected section should meet.
///
/// `plan.md` §11 sets two bars: the labeled section in **first place** for
/// every canonical exact-match case, and the labeled section **within the top
/// five** for the remaining queries. A canonical case declares [`First`];
/// every other case declares [`Top5`]. `First` is the stronger bar (a subset of
/// the top-five bar), so any case that meets `First` also meets `Top5`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RankTarget {
    /// The labeled section must rank **first** (canonical exact-match cases).
    First,
    /// The labeled section must rank **within the top five** (the default bar
    /// for ordinary and workflow queries).
    Top5,
}

impl RankTarget {
    /// The bar this target corresponds to, as a maximum allowed rank position
    /// (`1` = first place; `5` = within the top five).
    pub fn max_rank(&self) -> usize {
        match self {
            RankTarget::First => 1,
            RankTarget::Top5 => 5,
        }
    }
}

/// One hand-labeled evaluation case.
///
/// A case is a fixed `&'static` record. Its fields:
///
/// * [`id`](Self::id) — a stable, unique identifier (the corpus's own handle,
///   unrelated to any corpus snapshot identity);
/// * [`kind`](Self::kind) — the case category ([`EvalKind`]);
/// * [`source`](Self::source) — the source the query *primarily* targets, as a
///   [`SourceCollection`] so it can never name an unregistered source;
/// * [`query`](Self::query) — the realistic agent query string;
/// * [`expected_labels`](Self::expected_labels) — the **genuine section
///   label(s)** the query should retrieve (hierarchical document paths), never
///   the ranking's normalized token;
/// * [`rank_target`](Self::rank_target) — the declared ranking bar
///   ([`RankTarget`]).
#[derive(Clone, Debug)]
pub struct EvalCase {
    /// The stable, unique identifier for the case (e.g. `"E-001"`).
    pub id: &'static str,
    /// The case category.
    pub kind: EvalKind,
    /// The source the query primarily targets.
    pub source: SourceCollection,
    /// The realistic agent query string.
    pub query: &'static str,
    /// The genuine section label(s) the query should retrieve.
    pub expected_labels: &'static [&'static str],
    /// The ranking bar the query's labeled section must meet.
    pub rank_target: RankTarget,
}

// ---------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------

/// The independent retrieval evaluation corpus.
///
/// At least forty hand-labeled cases spanning all eight sources, with at least
/// three per source and at least twelve command/workflow queries (AC1), plus the
/// canonical exact-match cases and genuine section labels that make the set
/// independent of ranking aliases (AC2). The cases are ordered by source (in
/// `plan.md` §2 registry order) and by ascending id, so the corpus is itself
/// deterministic.
pub const CORPUS: &[EvalCase] = &[
    // --- api-guidelines (5): one canonical guideline-code case, four ordinary
    EvalCase {
        id: "E-001",
        kind: EvalKind::GuidelineCode,
        source: SourceCollection::ApiGuidelines,
        query: "C-CONV",
        expected_labels: &["guidelines/C-CONV"],
        rank_target: RankTarget::First,
    },
    EvalCase {
        id: "E-002",
        kind: EvalKind::Ordinary,
        source: SourceCollection::ApiGuidelines,
        query: "which trait should I implement to convert a value to another type?",
        expected_labels: &["guidelines/conversions"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-003",
        kind: EvalKind::Ordinary,
        source: SourceCollection::ApiGuidelines,
        query: "when should I provide Into instead of From?",
        expected_labels: &["guidelines/conversions"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-004",
        kind: EvalKind::Ordinary,
        source: SourceCollection::ApiGuidelines,
        query: "how should a library expose its error types to consumers?",
        expected_labels: &["guidelines/error-types"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-005",
        kind: EvalKind::Ordinary,
        source: SourceCollection::ApiGuidelines,
        query: "which standard traits are idiomatic to implement on a type?",
        expected_labels: &["guidelines/standard-traits"],
        rank_target: RankTarget::Top5,
    },
    // --- style-guide (5): five ordinary agent queries
    EvalCase {
        id: "E-006",
        kind: EvalKind::Ordinary,
        source: SourceCollection::StyleGuide,
        query: "does Rust use tabs or spaces for indentation?",
        expected_labels: &["style/indentation"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-007",
        kind: EvalKind::Ordinary,
        source: SourceCollection::StyleGuide,
        query: "how should Rust functions and types be named?",
        expected_labels: &["style/naming"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-008",
        kind: EvalKind::Ordinary,
        source: SourceCollection::StyleGuide,
        query: "what documentation comment style is conventional in Rust?",
        expected_labels: &["style/comments"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-009",
        kind: EvalKind::Ordinary,
        source: SourceCollection::StyleGuide,
        query: "how should use imports be organized and grouped?",
        expected_labels: &["style/imports"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-010",
        kind: EvalKind::Ordinary,
        source: SourceCollection::StyleGuide,
        query: "what field and module declaration ordering does the style guide recommend?",
        expected_labels: &["style/ordering"],
        rank_target: RankTarget::Top5,
    },
    // --- cargo (9): six canonical command-name cases, two workflow queries,
    //     one ordinary query
    EvalCase {
        id: "E-011",
        kind: EvalKind::Command,
        source: SourceCollection::Cargo,
        query: "cargo build",
        expected_labels: &["commands/cargo-build"],
        rank_target: RankTarget::First,
    },
    EvalCase {
        id: "E-012",
        kind: EvalKind::Command,
        source: SourceCollection::Cargo,
        query: "cargo test",
        expected_labels: &["commands/cargo-test"],
        rank_target: RankTarget::First,
    },
    EvalCase {
        id: "E-013",
        kind: EvalKind::Command,
        source: SourceCollection::Cargo,
        query: "cargo check",
        expected_labels: &["commands/cargo-check"],
        rank_target: RankTarget::First,
    },
    EvalCase {
        id: "E-014",
        kind: EvalKind::Command,
        source: SourceCollection::Cargo,
        query: "cargo fmt",
        expected_labels: &["commands/cargo-fmt"],
        rank_target: RankTarget::First,
    },
    // `cargo clippy` must retrieve *usage guidance* rather than only the
    // individual lint examples the catalog would otherwise crowd it under
    // (`plan.md` §11).
    EvalCase {
        id: "E-015",
        kind: EvalKind::Command,
        source: SourceCollection::Cargo,
        query: "cargo clippy",
        expected_labels: &["commands/cargo-clippy"],
        rank_target: RankTarget::First,
    },
    EvalCase {
        id: "E-016",
        kind: EvalKind::Command,
        source: SourceCollection::Cargo,
        query: "cargo doc",
        expected_labels: &["commands/cargo-doc"],
        rank_target: RankTarget::First,
    },
    EvalCase {
        id: "E-017",
        kind: EvalKind::Workflow,
        source: SourceCollection::Cargo,
        query: "how do I add a new dependency to my project?",
        expected_labels: &["commands/cargo-add"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-018",
        kind: EvalKind::Workflow,
        source: SourceCollection::Cargo,
        query: "how do I run the current binary from Cargo?",
        expected_labels: &["commands/cargo-run"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-019",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Cargo,
        query: "how do Cargo features select dependencies?",
        expected_labels: &["reference/features"],
        rank_target: RankTarget::Top5,
    },
    // --- rustc (5): one canonical command-name case, one workflow, three ordinary
    EvalCase {
        id: "E-020",
        kind: EvalKind::Command,
        source: SourceCollection::Rustc,
        query: "rustc",
        expected_labels: &["rustc/options"],
        rank_target: RankTarget::First,
    },
    EvalCase {
        id: "E-021",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustc,
        query: "what compiler options does rustc accept?",
        expected_labels: &["rustc/options"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-022",
        kind: EvalKind::Workflow,
        source: SourceCollection::Rustc,
        query: "how do I enable or configure compiler lints?",
        expected_labels: &["rustc/lints"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-023",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustc,
        query: "how do I target a different platform when compiling with rustc?",
        expected_labels: &["rustc/targets"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-024",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustc,
        query: "how should I interpret the diagnostics that rustc reports?",
        expected_labels: &["rustc/diagnostics"],
        rank_target: RankTarget::Top5,
    },
    // --- rustup (5): three workflow queries, two ordinary
    EvalCase {
        id: "E-025",
        kind: EvalKind::Workflow,
        source: SourceCollection::Rustup,
        query: "how do I install Rust with rustup?",
        expected_labels: &["rustup/installation"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-026",
        kind: EvalKind::Workflow,
        source: SourceCollection::Rustup,
        query: "how do I list or switch between toolchains?",
        expected_labels: &["rustup/toolchains"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-027",
        kind: EvalKind::Workflow,
        source: SourceCollection::Rustup,
        query: "how do I set a per-directory toolchain override?",
        expected_labels: &["rustup/overrides"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-028",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustup,
        query: "how do I add or remove a component such as clippy or rustfmt?",
        expected_labels: &["rustup/components"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-029",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustup,
        query: "how do I add a new target for cross-compilation?",
        expected_labels: &["rustup/targets"],
        rank_target: RankTarget::Top5,
    },
    // --- rustfmt (5): two workflow queries, three ordinary
    EvalCase {
        id: "E-030",
        kind: EvalKind::Workflow,
        source: SourceCollection::Rustfmt,
        query: "how do I format my project with cargo fmt?",
        expected_labels: &["rustfmt/usage"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-031",
        kind: EvalKind::Workflow,
        source: SourceCollection::Rustfmt,
        query: "how do I run the formatter in check mode without writing?",
        expected_labels: &["rustfmt/check"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-032",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustfmt,
        query: "where do I set rustfmt configuration options?",
        expected_labels: &["rustfmt/configuration"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-033",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustfmt,
        query: "which edition-related formatting settings are available?",
        expected_labels: &["rustfmt/editions"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-034",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustfmt,
        query: "are rustfmt options stable across toolchain versions?",
        expected_labels: &["rustfmt/stability"],
        rank_target: RankTarget::Top5,
    },
    // --- clippy (6): two canonical lint-id cases, one workflow, three ordinary
    EvalCase {
        id: "E-035",
        kind: EvalKind::Lint,
        source: SourceCollection::Clippy,
        query: "clippy::unwrap_used",
        expected_labels: &["lints/unwrap_used"],
        rank_target: RankTarget::First,
    },
    EvalCase {
        id: "E-036",
        kind: EvalKind::Lint,
        source: SourceCollection::Clippy,
        query: "clippy::clone_on_copy",
        expected_labels: &["lints/clone_on_copy"],
        rank_target: RankTarget::First,
    },
    EvalCase {
        id: "E-037",
        kind: EvalKind::Workflow,
        source: SourceCollection::Clippy,
        query: "how do I run Clippy and see the lints it reports?",
        expected_labels: &["clippy/usage"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-038",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Clippy,
        query: "how do I configure Clippy lint levels like allow and deny?",
        expected_labels: &["clippy/configuration"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-039",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Clippy,
        query: "what are the Clippy lint groups such as correctness and style?",
        expected_labels: &["clippy/lint-groups"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-040",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Clippy,
        query: "how do I install Clippy?",
        expected_labels: &["clippy/installation"],
        rank_target: RankTarget::Top5,
    },
    // --- rustdoc (5): two workflow queries, three ordinary
    EvalCase {
        id: "E-041",
        kind: EvalKind::Workflow,
        source: SourceCollection::Rustdoc,
        query: "how do I generate documentation with cargo doc?",
        expected_labels: &["rustdoc/build"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-042",
        kind: EvalKind::Workflow,
        source: SourceCollection::Rustdoc,
        query: "how do I write documentation tests, also called doctests?",
        expected_labels: &["rustdoc/testing"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-043",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustdoc,
        query: "how do intra-documentation links work?",
        expected_labels: &["rustdoc/links"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-044",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustdoc,
        query: "what rustdoc configuration options exist?",
        expected_labels: &["rustdoc/configuration"],
        rank_target: RankTarget::Top5,
    },
    EvalCase {
        id: "E-045",
        kind: EvalKind::Ordinary,
        source: SourceCollection::Rustdoc,
        query: "which rustdoc command-line options are documented?",
        expected_labels: &["rustdoc/options"],
        rank_target: RankTarget::Top5,
    },
];

// ---------------------------------------------------------------------------
// Coverage helpers
// ---------------------------------------------------------------------------

/// The number of corpus cases that primarily target `source`.
pub fn count_for_source(source: SourceCollection) -> usize {
    CORPUS.iter().filter(|c| c.source == source).count()
}

/// The distinct sources the corpus covers, in registry order.
pub fn distinct_sources() -> Vec<SourceCollection> {
    let mut out = Vec::new();
    for source in SourceCollection::ALL {
        if count_for_source(source) > 0 {
            out.push(source);
        }
    }
    out
}

/// The number of command/workflow queries (canonical command names plus natural
/// language workflow questions) — the AC1 "at least twelve" total.
pub fn command_workflow_count() -> usize {
    CORPUS
        .iter()
        .filter(|c| c.kind.is_command_workflow())
        .count()
}

/// The number of canonical exact-match cases (guideline codes, command names,
/// lint ids).
pub fn canonical_count() -> usize {
    CORPUS.iter().filter(|c| c.kind.is_canonical()).count()
}

// ---------------------------------------------------------------------------
// Section-label shape (the genuine-label predicate)
// ---------------------------------------------------------------------------

/// Whether `label` is a **genuine section label**: a non-empty, hierarchical
/// document path made of `/`-separated, non-empty slug segments, with no
/// leading, trailing, or doubled separator.
///
/// This is the structural marker that separates a *section label* from a
/// *ranking token*. Every genuine section path contains a `/` separator
/// (`commands/cargo-build`, `lints/unwrap_used`, `guidelines/C-CONV`), whereas
/// the `[033]` tokenizer emits only **flat** atomic forms — a word, a flag
/// (`--no-default-features`), a command phrase (`cargo test`), a guideline code
/// (`C-CONV`), or a lint id (`clippy::unwrap_used`) — none of which contains a
/// `/`. The two are therefore disjoint, which is what makes the corpus
/// independent of the ranking's alias/normalized forms (see the module
/// documentation and [`super::tests::corpus_is_independent_of_ranking_aliases`]).
fn is_genuine_section_label(label: &str) -> bool {
    if label.is_empty() || !label.contains('/') {
        return false;
    }
    // No empty segment guards against a leading, trailing, or doubled separator.
    if label.split('/').any(|seg| seg.is_empty()) {
        return false;
    }
    // Only ASCII alphanumerics and the slug separators may appear.
    for c in label.chars() {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/') {
            return false;
        }
    }
    // The first character must begin a slug (an identifier start), not a
    // separator — so a path never opens with `-` or `_`.
    matches!(
        label.chars().next(),
        Some(c) if c.is_ascii_alphanumeric()
    )
}

// ---------------------------------------------------------------------------
// Tests: the coverage (AC1) and independence (AC2) invariants are proven here.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        CORPUS, EvalCase, EvalKind, RankTarget, canonical_count, command_workflow_count,
        count_for_source, distinct_sources, is_genuine_section_label,
    };
    use crate::registry::SourceCollection;
    use crate::search::{Token, tokenize};

    // --- AC1: coverage -----------------------------------------------------

    /// AC1: the corpus has at least forty cases.
    #[test]
    fn corpus_covers_at_least_forty_cases() {
        assert!(
            CORPUS.len() >= 40,
            "the corpus must hold at least 40 cases, has {}",
            CORPUS.len()
        );
    }

    /// AC1: every one of the eight registered sources is covered, with at least
    /// three queries each.
    #[test]
    fn every_source_has_at_least_three_queries() {
        for source in SourceCollection::ALL {
            let n = count_for_source(source);
            assert!(
                n >= 3,
                "source {:?} must have at least 3 queries, has {n}",
                source.id()
            );
        }
    }

    /// AC1: all eight sources are covered (the corpus reaches every collection).
    #[test]
    fn all_eight_sources_are_covered() {
        let covered = distinct_sources();
        assert_eq!(
            covered.len(),
            8,
            "the corpus must cover all 8 sources, covered {covered:?}"
        );
        for source in SourceCollection::ALL {
            assert!(
                covered.contains(&source),
                "source {:?} must be covered",
                source.id()
            );
        }
    }

    /// AC1: at least twelve command/workflow queries overall.
    #[test]
    fn at_least_twelve_command_workflow_queries() {
        assert!(
            command_workflow_count() >= 12,
            "there must be at least 12 command/workflow queries, has {}",
            command_workflow_count()
        );
    }

    /// AC1 + AC2: the corpus carries at least one canonical case of every
    /// required kind — a guideline code, a command name, and a lint id.
    #[test]
    fn canonical_cases_are_present() {
        let has_code = CORPUS.iter().any(|c| c.kind == EvalKind::GuidelineCode);
        let has_command = CORPUS.iter().any(|c| c.kind == EvalKind::Command);
        let has_lint = CORPUS.iter().any(|c| c.kind == EvalKind::Lint);
        assert!(has_code, "a canonical guideline-code case is required");
        assert!(has_command, "a canonical command-name case is required");
        assert!(has_lint, "a canonical lint-id case is required");
        // A non-zero canonical total is the sanity backstop.
        assert!(
            canonical_count() >= 3,
            "at least one case of each canonical kind"
        );
    }

    // --- AC1 / AC2: per-case structural validity --------------------------

    /// Every case has a non-empty, non-blank query and at least one expected
    /// label, and its case id is unique.
    #[test]
    fn every_case_is_well_formed() {
        let mut ids = Vec::new();
        for case in CORPUS {
            assert!(
                !case.query.trim().is_empty(),
                "case {} has a blank query",
                case.id
            );
            assert!(
                !case.expected_labels.is_empty(),
                "case {} has no expected label",
                case.id
            );
            ids.push(case.id);
        }
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "case ids must be unique");
    }

    /// No two cases share the same query string — the corpus must not test the
    /// same query twice.
    #[test]
    fn queries_are_unique() {
        let mut queries: Vec<&str> = CORPUS.iter().map(|c| c.query).collect();
        let total = queries.len();
        queries.sort_unstable();
        queries.dedup();
        assert_eq!(
            queries.len(),
            total,
            "queries must be unique; a duplicate exists"
        );
    }

    /// Every expected label is a genuine section path (see
    /// [`is_genuine_section_label`]): the corpus's labels are real section
    /// identifiers, never a bare ranking token.
    #[test]
    fn expected_labels_are_genuine_section_labels() {
        for case in CORPUS {
            for label in case.expected_labels {
                assert!(
                    is_genuine_section_label(label),
                    "case {} expected label {label:?} is not a genuine section path",
                    case.id
                );
            }
        }
    }

    /// The ranking target is consistent with the case kind: a canonical
    /// exact-match case declares `First` (first place); every other case
    /// declares `Top5` (the top-five bar) — `plan.md` §11.
    #[test]
    fn rank_target_matches_canonicality() {
        for case in CORPUS {
            let expected = if case.kind.is_canonical() {
                RankTarget::First
            } else {
                RankTarget::Top5
            };
            assert_eq!(
                case.rank_target, expected,
                "case {} (kind {:?}) declares {:?}, expected {expected:?}",
                case.id, case.kind, case.rank_target
            );
        }
    }

    /// `First` is the stronger bar: every canonical case's target ranks within
    /// the top five too, so "first place for canonical cases" implies the
    /// top-five bar the rest of the corpus carries.
    #[test]
    fn first_place_cases_also_satisfy_the_top_five_bar() {
        for case in CORPUS {
            if case.kind.is_canonical() {
                assert_eq!(
                    case.rank_target.max_rank(),
                    1,
                    "canonical case {} must target first place",
                    case.id
                );
            }
        }
    }

    // --- AC2: independence from ranking aliases ---------------------------

    /// AC2 (load-bearing): the corpus is **independent of ranking aliases**.
    ///
    /// For every case, each expected label must differ from (a) the query's raw
    /// string and (b) every atomic token the `[033]` tokenizer produces for that
    /// query. The tokenizer is used here only as an *external oracle* to verify
    /// the corpus — the corpus data itself never imports `crate::search`.
    ///
    /// Because a genuine section label always contains a `/` (see
    /// [`is_genuine_section_label`]) and the tokenizer never emits a token with
    /// a `/`, the two are structurally disjoint; this test makes that disjointness
    /// explicit so a future edit cannot quietly turn a label into the query's own
    /// normalized/aliased form.
    #[test]
    fn corpus_is_independent_of_ranking_aliases() {
        for case in CORPUS {
            // (a) A label is never the raw query string.
            for label in case.expected_labels {
                assert_ne!(
                    *label, case.query,
                    "case {} label is the raw query — the corpus would be \
                     matching on the query itself, not a section label",
                    case.id
                );
            }
            // (b) A label is never one of the query's normalized/aliased tokens.
            let tokens = tokenize(case.query);
            let token_forms: Vec<&str> = tokens.iter().map(Token::as_str).collect();
            for label in case.expected_labels {
                assert!(
                    !token_forms.contains(label),
                    "case {} label {label:?} collides with a ranking \
                     token of its query {:?} — the corpus is not independent",
                    case.id,
                    token_forms
                );
            }
        }
    }

    /// The structural basis of the independence guarantee: no ranking token the
    /// tokenizer ever produces contains a `/`, while every genuine label does.
    /// This documents *why* the disjointness in
    /// [`corpus_is_independent_of_ranking_aliases`] holds, so a future tokenizer
    /// change that started emitting path-like tokens would surface here.
    #[test]
    fn ranking_tokens_are_flat_and_section_labels_are_not() {
        // A representative spread of the corpus's query forms, including the
        // canonical exact-match queries the tokenizer normalizes most.
        let probe_queries = [
            "C-CONV",
            "cargo build",
            "clippy::unwrap_used",
            "how do I add a new dependency to my project?",
            "are rustfmt options stable across toolchain versions?",
            "cargo clippy --no-default-features",
        ];
        for query in probe_queries {
            for token in tokenize(query) {
                assert!(
                    !token.as_str().contains('/'),
                    "token of {query:?} unexpectedly contains a '/'"
                );
            }
        }
        // And every corpus label does contain a '/'.
        for case in CORPUS {
            for label in case.expected_labels {
                assert!(
                    label.contains('/'),
                    "corpus label {label:?} is not a section path"
                );
            }
        }
    }

    /// A single `EvalCase` built through the public shape is well-formed and
    /// exercises the `max_rank` target seam, pinning the two ranking bars.
    #[test]
    fn rank_target_bars_are_as_documented() {
        assert_eq!(RankTarget::First.max_rank(), 1);
        assert_eq!(RankTarget::Top5.max_rank(), 5);
        // A constructed case: canonical → First, ordinary → Top5.
        let canonical = EvalCase {
            id: "E-099",
            kind: EvalKind::Command,
            source: SourceCollection::Cargo,
            query: "cargo build",
            expected_labels: &["commands/cargo-build"],
            rank_target: RankTarget::First,
        };
        let ordinary = EvalCase {
            id: "E-100",
            kind: EvalKind::Ordinary,
            source: SourceCollection::Cargo,
            query: "how do I select features?",
            expected_labels: &["reference/features"],
            rank_target: RankTarget::Top5,
        };
        assert!(canonical.kind.is_canonical());
        assert!(canonical.kind.is_command_workflow());
        assert!(!ordinary.kind.is_canonical());
        assert!(!ordinary.kind.is_command_workflow());
        assert_eq!(canonical.rank_target.max_rank(), 1);
        assert_eq!(ordinary.rank_target.max_rank(), 5);
    }
}
