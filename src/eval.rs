//! Independent retrieval evaluation corpus — task [045][M3] (issue #45).
//!
//! Placeholder scaffold; the full corpus and coverage tests land in the same
//! module in the same task. This transitional module is `#[allow(dead_code)]`
//! covered until a consumer (the [035]+ ranking / the [048]+ MCP or M5
//! acceptance harness) wires it in.

#![allow(dead_code)]

use crate::registry::SourceCollection;

/// The category of an evaluation case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalKind {
    /// A canonical exact guideline-code case (e.g. `C-CONV`).
    GuidelineCode,
    /// A canonical exact command-name case (e.g. `cargo build`).
    Command,
    /// A canonical exact lint-id case (e.g. `clippy::unwrap_used`).
    Lint,
    /// A natural-language command/workflow query.
    Workflow,
    /// An ordinary natural-language agent query.
    Ordinary,
}

/// One hand-labeled evaluation case.
#[derive(Clone, Debug)]
pub struct EvalCase {
    /// A stable, unique identifier for the case.
    pub id: &'static str,
    /// The case category.
    pub kind: EvalKind,
    /// The source the query primarily targets.
    pub source: SourceCollection,
    /// The realistic agent query string.
    pub query: &'static str,
    /// The genuine section label(s) the query should retrieve.
    pub expected_labels: &'static [&'static str],
}

/// The independent evaluation corpus.
pub const CORPUS: &[EvalCase] = &[
    EvalCase {
        id: "E-001",
        kind: EvalKind::GuidelineCode,
        source: SourceCollection::ApiGuidelines,
        query: "C-CONV",
        expected_labels: &["guidelines/C-CONV"],
    },
    EvalCase {
        id: "E-002",
        kind: EvalKind::Ordinary,
        source: SourceCollection::ApiGuidelines,
        query: "which trait should I implement for type conversion?",
        expected_labels: &["guidelines/conversions"],
    },
];

#[cfg(test)]
mod tests {
    use super::CORPUS;

    #[test]
    fn corpus_is_nonempty_placeholder() {
        assert!(!CORPUS.is_empty(), "the scaffold corpus must be non-empty");
    }
}
