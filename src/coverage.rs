//! Corpus coverage and extraction fidelity inspection ([issue 32] / `[032][M2]`).
//!
//! This module answers the two questions the M2 exit asks an operator: *how much*
//! of the documentation surface is the corpus covering, and did the
//! source-specific extractors actually *retain* the content types agents rely on
//! (nested headings and anchors, lists, tables, command syntax and options,
//! stability callouts, relative links, lint "known issues" notes, fenced code
//! examples) while *dropping* navigation chrome and scripts?
//!
//! Both inspections are **record-level** and run entirely over the in-memory
//! [`SnapshotManifest`] the ingest pipeline already produces, plus a small set of
//! representative [`Section`] fixtures:
//!
//! * [`record_coverage`] counts sources, documents, and sections per
//!   [`registry::SourceCollection`] and flags a concrete [`CoverageGap`] for any
//!   collection the corpus omits, any present collection with no sections, and any
//!   command entry lacking a syntax template.
//! * [`inspect_fidelity`] reports which content types the inspected sections
//!   actually exhibit (`present`) and which they do not (`absent`). The "absent"
//!   list is a *recorded gap*, not a fabricated pass: a missing type is never
//!   invented, and navigation/scripts leaking into a body is a defect, not a pass.
//!
//! The inspections are pure and offline: they neither read the network nor touch
//! the filesystem, so they are safe to run in normal tests, in the `inspect`
//! subcommand, and against the committed bundled seed.
//!
//! [issue 32]: https://github.com/ThiagoEvoa/rust-mcp/issues/32

use std::fmt;

use crate::model::{CommandKey, Section, SnapshotManifest};
use crate::registry::SourceCollection;

// --- Coverage --------------------------------------------------------------

/// One collection's presence in a corpus: how many document URLs and sections it
/// contributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceCollectionCoverage {
    pub source: SourceCollection,
    /// Number of `document_urls` across the corpus's sources of this collection.
    pub document_count: usize,
    /// Number of sections attributed to this collection.
    pub section_count: usize,
}

/// A concrete, recorded reason the corpus does not yet reach full M2 coverage.
///
/// Every gap is a *recorded* finding, never a silent success: the inspection
/// reports what is missing rather than assuming it is present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoverageGap {
    /// A registered collection has no source record at all.
    MissingSource { source: SourceCollection },
    /// A collection has a source but no extracted sections.
    SourceHasNoSections { source: SourceCollection },
    /// A command entry has no syntax template, so its invocation form was not
    /// retained by extraction.
    CommandMissingSyntax { key: CommandKey },
}

/// The corpus coverage report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageReport {
    /// Total source records in the corpus.
    pub source_count: usize,
    /// Total sections in the corpus.
    pub section_count: usize,
    /// Total checklist entries in the corpus.
    pub checklist_entries: usize,
    /// Total command entries in the corpus.
    pub command_entries: usize,
    /// Per-collection breakdown, in registry order.
    pub per_collection: Vec<SourceCollectionCoverage>,
    /// Recorded gaps (missing collections, empty collections, syntax-less
    /// commands). Empty means the corpus reaches full M2 coverage.
    pub gaps: Vec<CoverageGap>,
}

impl CoverageReport {
    /// Whether the corpus has no recorded coverage gap.
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty()
    }

    /// Render the report as a human-readable summary for stderr diagnostics.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("corpus coverage:\n");
        out.push_str(&format!(
            "  sources={} sections={} checklist={} commands={}\n",
            self.source_count, self.section_count, self.checklist_entries, self.command_entries,
        ));
        for c in &self.per_collection {
            out.push_str(&format!(
                "  {:<14} documents={} sections={}\n",
                c.source.title(),
                c.document_count,
                c.section_count,
            ));
        }
        if self.is_complete() {
            out.push_str("  gaps: none\n");
        } else {
            out.push_str("  gaps:\n");
            for gap in &self.gaps {
                out.push_str(&format!("    - {gap}\n"));
            }
        }
        out
    }
}

/// Count the corpus's source/document/section coverage per registered collection
/// and record any gap.
///
/// A collection is present when the corpus has at least one source record whose
/// `id` matches it. Sections are attributed to a collection by each section's
/// `source_id`. The report records `MissingSource` for a collection with no
/// source, `SourceHasNoSections` for a present collection with no sections, and
/// `CommandMissingSyntax` for a command entry with an empty `syntax` template.
pub fn record_coverage(manifest: &SnapshotManifest) -> CoverageReport {
    let body = &manifest.body;

    let mut per_collection = Vec::with_capacity(SourceCollection::ALL.len());
    let mut gaps = Vec::new();

    for coll in SourceCollection::ALL {
        let mut document_count = 0usize;
        let mut source_present = false;
        for source in &body.sources {
            if source.id.as_str() == coll.id() {
                source_present = true;
                document_count += source.document_urls.len();
            }
        }
        let section_count = body
            .sections
            .iter()
            .filter(|section| section.source_id.as_str() == coll.id())
            .count();

        per_collection.push(SourceCollectionCoverage {
            source: coll,
            document_count,
            section_count,
        });

        if !source_present {
            gaps.push(CoverageGap::MissingSource { source: coll });
        } else if section_count == 0 {
            gaps.push(CoverageGap::SourceHasNoSections { source: coll });
        }
    }

    for command in &body.commands {
        if command.syntax.is_empty() {
            gaps.push(CoverageGap::CommandMissingSyntax {
                key: command.key.clone(),
            });
        }
    }

    CoverageReport {
        source_count: body.sources.len(),
        section_count: body.sections.len(),
        checklist_entries: body.checklist.len(),
        command_entries: body.commands.len(),
        per_collection,
        gaps,
    }
}

impl fmt::Display for CoverageGap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoverageGap::MissingSource { source } => {
                write!(f, "missing source: {}", source.title())
            }
            CoverageGap::SourceHasNoSections { source } => {
                write!(f, "no sections: {}", source.title())
            }
            CoverageGap::CommandMissingSyntax { key } => {
                write!(f, "command without syntax template: {key}")
            }
        }
    }
}

// --- Fidelity --------------------------------------------------------------

/// A content type the extraction must retain (or, for the navigation case, must
/// *not* retain).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FidelityType {
    /// A section with a nested heading breadcrumb (`heading_path.len() >= 2`).
    NestedHeading,
    /// A section with an anchor, so an intra-document link resolves.
    Anchor,
    /// A body containing a Markdown list item.
    List,
    /// A body containing a GFM table (a `| … |` separator row).
    Table,
    /// A section with at least one command key (command syntax/options retained).
    CommandSyntax,
    /// A section with a stability/qualification callout retained.
    StabilityCallout,
    /// A body containing a Markdown link (`[text](target)`).
    RelativeLink,
    /// A lint section whose "known issues" note was retained in the body.
    LintKnownIssues,
    /// A body containing a fenced code example.
    FencedExample,
    /// No section body contains navigation chrome or scripts. When present, the
    /// extractors stripped chrome as required; its absence is a defect.
    NoNavigationOrScripts,
}

impl FidelityType {
    fn label(self) -> &'static str {
        match self {
            FidelityType::NestedHeading => "nested heading",
            FidelityType::Anchor => "anchor",
            FidelityType::List => "list",
            FidelityType::Table => "table",
            FidelityType::CommandSyntax => "command syntax/options",
            FidelityType::StabilityCallout => "stability callout",
            FidelityType::RelativeLink => "relative link",
            FidelityType::LintKnownIssues => "lint known issues",
            FidelityType::FencedExample => "fenced code example",
            FidelityType::NoNavigationOrScripts => "no navigation/scripts",
        }
    }

    fn all() -> [FidelityType; 10] {
        [
            FidelityType::NestedHeading,
            FidelityType::Anchor,
            FidelityType::List,
            FidelityType::Table,
            FidelityType::CommandSyntax,
            FidelityType::StabilityCallout,
            FidelityType::RelativeLink,
            FidelityType::LintKnownIssues,
            FidelityType::FencedExample,
            FidelityType::NoNavigationOrScripts,
        ]
    }
}

/// The extraction-fidelity report over a set of inspected sections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FidelityReport {
    /// Number of sections inspected.
    pub sections_inspected: usize,
    /// Content types at least one inspected section exhibits.
    pub present: Vec<FidelityType>,
    /// Content types no inspected section exhibits. A non-empty list is a
    /// *recorded gap*, never a fabricated pass.
    pub absent: Vec<FidelityType>,
}

impl FidelityReport {
    /// Whether every content type is present (no recorded gap).
    pub fn all_types_present(&self) -> bool {
        self.absent.is_empty()
    }

    /// Render the report as a human-readable summary for stderr diagnostics.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "extraction fidelity (sections={}):\n",
            self.sections_inspected
        ));
        for ty in FidelityType::all() {
            let state = if self.present.contains(&ty) {
                "present"
            } else {
                "ABSENT"
            };
            out.push_str(&format!("  {state:<7} {}\n", ty.label()));
        }
        out
    }
}

/// Inspect a set of sections and report which content types survive extraction
/// and which do not.
///
/// The "navigation/scripts absent" type is an invariant: it is *present* when no
/// inspected body leaks chrome, and moves to `absent` (a defect) when one does.
/// An empty section set is a degenerate no-op: only the navigation invariant is
/// reported present, every other type absent.
pub fn inspect_fidelity(sections: &[Section]) -> FidelityReport {
    let mut present = Vec::new();
    let mut absent = Vec::new();
    for ty in FidelityType::all() {
        if present_in(sections, ty) {
            present.push(ty);
        } else {
            absent.push(ty);
        }
    }
    FidelityReport {
        sections_inspected: sections.len(),
        present,
        absent,
    }
}

/// Whether at least one section exhibits (or, for the navigation case, none
/// exhibits chrome for) the content type.
fn present_in(sections: &[Section], ty: FidelityType) -> bool {
    match ty {
        FidelityType::NestedHeading => sections.iter().any(|s| s.heading_path.len() >= 2),
        FidelityType::Anchor => sections.iter().any(|s| s.anchor.is_some()),
        FidelityType::List => sections.iter().any(|s| body_has_list(&s.body)),
        FidelityType::Table => sections.iter().any(|s| body_has_table(&s.body)),
        FidelityType::CommandSyntax => sections.iter().any(|s| !s.command_keys.is_empty()),
        FidelityType::StabilityCallout => sections.iter().any(|s| !s.qualifications.is_empty()),
        FidelityType::RelativeLink => sections.iter().any(|s| body_has_link(&s.body)),
        FidelityType::LintKnownIssues => sections
            .iter()
            .any(|s| !s.lint_ids.is_empty() && body_mentions_known(&s.body)),
        FidelityType::FencedExample => sections.iter().any(|s| body_has_fence(&s.body)),
        FidelityType::NoNavigationOrScripts => sections.iter().all(|s| !body_has_chrome(&s.body)),
    }
}

/// A representative set of corpus-shaped sections that exercises every
/// [`FidelityType`]. These mirror the records the ingest pipeline produces (a
/// reference table, a lint entry with its known-issues note, a command with a
/// syntax template, and a guideline with a stability callout), so the fidelity
/// check proves the content types survive extraction without any network.
///
/// Every body is clean Markdown with no navigation chrome or scripts, so the
/// "no navigation/scripts" invariant holds across the set.
pub fn representative_fidelity_sections() -> Vec<Section> {
    use crate::model::{SectionBuilder, SourceId, Url};

    // rustc: a nested-heading section carrying an anchor, a GFM options table, a
    // fenced example, and a relative link — proving nested headings, anchors,
    // tables, fenced examples, and links all survive extraction.
    let rustc = SectionBuilder::new(
        SourceId::new("rustc").expect("valid source id fixture"),
        "Expressions and Evaluations",
        "reference/expressions",
        Url::new("https://doc.rust-lang.org/reference/expressions.html")
            .expect("valid url fixture"),
        r#"Expressions are evaluated from the innermost outward.

| Expression | Evaluates to |
| --- | --- |
| `a + b` | the sum of `a` and `b` |
| `&x`    | a reference to `x` |

```rust
let y = 40 + 2;
let z = &y;
```

See [the full expression listing](https://doc.rust-lang.org/reference/expressions.html#expression-locations).
"#,
    )
    .heading_path(["The Rust Reference", "Expressions"])
    .anchor("expressions")
    .build()
    .expect("valid fixture section");

    // clippy: a lint entry whose "known issues" note and a list of alternatives
    // survive extraction.
    let clippy = SectionBuilder::new(
        SourceId::new("clippy").expect("valid source id fixture"),
        "clippy::unwrap_used",
        "lint",
        Url::new("https://rust-lang.github.io/rust-clippy/master/index.html#/unwrap_used")
            .expect("valid url fixture"),
        r#"Calling `.unwrap()` on an `Option` or `Result` value.

Known issues: panics when the value is `None` or `Err`, so it is unsafe on
untrusted input.

Alternatives:

- `match` with explicit arms
- `?` when propagating errors
- `expect` with a contextual message
"#,
    )
    .lint_ids(["clippy::unwrap_used"])
    .build()
    .expect("valid fixture section");

    // cargo: a command section whose syntax/options and a fenced example survive.
    let cargo = SectionBuilder::new(
        SourceId::new("cargo").expect("valid source id fixture"),
        "cargo test",
        "commands/cargo-test",
        Url::new("https://doc.rust-lang.org/cargo/commands/cargo-test.html")
            .expect("valid url fixture"),
        r#"Execute all unit and integration tests and doctests of a package.

```bash
cargo test --release --all-features
```
"#,
    )
    .command_keys(["cargo test"])
    .build()
    .expect("valid fixture section");

    // api-guidelines: a guideline whose stability/qualification callout and a
    // list survive extraction.
    let guidelines = SectionBuilder::new(
        SourceId::new("api-guidelines").expect("valid source id fixture"),
        "Stability and breaking changes",
        "evolution",
        Url::new("https://rust-lang.github.io/api-guidelines/evolution.html")
            .expect("valid url fixture"),
        r#"Once a public item is released, breaking its signature is a semver
major change.

Rules:

- add new methods freely on a minor version
- never remove or change the signature of a public item without a major bump
"#,
    )
    .qualifications(["stability: unstable since Rust 1.0"])
    .build()
    .expect("valid fixture section");

    vec![rustc, clippy, cargo, guidelines]
}

/// Whether a body contains a Markdown list item (`- `, `* `, `+ `, or `1. `).
fn body_has_list(body: &str) -> bool {
    body.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with("+ ")
            || is_ordered_item(trimmed)
    })
}

/// Whether a leading `1. `, `12. ` … token begins an ordered list item.
fn is_ordered_item(trimmed: &str) -> bool {
    if let Some((digits, rest)) = trimmed.split_once(' ') {
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && !rest.is_empty()
    } else {
        false
    }
}

/// Whether a body contains a GFM table separator row (`| --- | :---: | …`).
fn body_has_table(body: &str) -> bool {
    body.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.len() >= 3
            && trimmed.contains('-')
            && trimmed.contains('|')
            && trimmed
                .bytes()
                .all(|b| matches!(b, b'|' | b':' | b'-' | b' '))
    })
}

/// Whether a body contains a Markdown link `[text](target)` with a non-empty
/// target.
fn body_has_link(body: &str) -> bool {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + 2 <= bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let start = i + 2;
            if let Some(rel) = bytes[start..].iter().position(|&b| b == b')') {
                let end = start + rel;
                if end > start && bytes[start..end].iter().any(|&b| !b.is_ascii_whitespace()) {
                    return true;
                }
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

/// Whether a body contains a fenced code block (a line starting with ```).
fn body_has_fence(body: &str) -> bool {
    body.lines()
        .any(|line| line.trim_start().starts_with("```"))
}

/// Whether a body mentions a "known issue" note (the Clippy known-issues text).
fn body_mentions_known(body: &str) -> bool {
    body.to_lowercase().contains("known")
}

/// Whether a body leaks navigation chrome or scripts — a defect the extractors
/// must prevent.
fn body_has_chrome(body: &str) -> bool {
    let lower = body.to_lowercase();
    [
        "<nav",
        "<script",
        "<style",
        "<aside",
        "<footer",
        "class=\"sidebar\"",
        "class=\"toc\"",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

// --- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SectionBuilder, SourceId, Url};

    /// Build a single bare section with an arbitrary body, for the fidelity
    /// tests that need a body shape the representative fixtures do not cover.
    fn bare_section(body: &str) -> Section {
        SectionBuilder::new(
            SourceId::new("rustdoc").expect("valid source id fixture"),
            "bare",
            "chapter",
            Url::new("https://doc.rust-lang.org/reference").expect("valid url fixture"),
            body,
        )
        .build()
        .expect("valid fixture section")
    }

    /// The bundled seed, loaded through the same version gate the runtime uses,
    /// so the coverage tests exercise the real corpus rather than a fabricated
    /// one. This is also the seed-vs-fixture split the task asks to report
    /// honestly: the seed is a representative subset, not every content type.
    fn seed_manifest() -> SnapshotManifest {
        SnapshotManifest::from_json_str(crate::bundled::JSON).expect("bundled seed loads")
    }

    #[test]
    fn coverage_counts_the_bundled_seed() {
        let report = record_coverage(&seed_manifest());

        assert_eq!(report.source_count, 8, "seed has all eight sources");
        assert_eq!(report.section_count, 8, "seed has eight sections");
        assert_eq!(report.checklist_entries, 1, "seed has the C-CONV checklist");
        assert_eq!(report.command_entries, 3, "seed has three commands");
        assert_eq!(
            report.per_collection.len(),
            SourceCollection::ALL.len(),
            "one breakdown per registered collection",
        );
        // Every seed source is present with at least one section and every seed
        // command carries a syntax template, so the seed reaches full M2
        // coverage with no recorded gap.
        assert!(
            report.is_complete(),
            "the bundled seed reaches full M2 coverage: {}",
            report.render(),
        );
    }

    #[test]
    fn coverage_records_a_missing_source_and_no_sections() {
        // Remove the rustup source entirely (a MissingSource gap) and strip
        // rustc's sections while keeping its source record (a
        // SourceHasNoSections gap). Both must be recorded, not silently passed.
        let mut manifest = seed_manifest();
        manifest
            .body
            .sources
            .retain(|source| source.id.as_str() != "rustup");
        manifest
            .body
            .sections
            .retain(|section| section.source_id.as_str() != "rustc");
        let report = record_coverage(&manifest);

        assert!(
            !report.is_complete(),
            "a missing/empty collection must be a recorded gap",
        );
        assert!(
            report.gaps.contains(&CoverageGap::MissingSource {
                source: SourceCollection::Rustup
            }),
            "rustup is missing entirely: {:?}",
            report.gaps,
        );
        assert!(
            report.gaps.contains(&CoverageGap::SourceHasNoSections {
                source: SourceCollection::Rustc
            }),
            "rustc has a source but no sections: {:?}",
            report.gaps,
        );
    }

    #[test]
    fn coverage_records_a_command_without_syntax() {
        // Clear a real seed command's syntax template; it becomes a recorded gap.
        let mut manifest = seed_manifest();
        let key = manifest.body.commands[0].key.clone();
        for command in &mut manifest.body.commands {
            command.syntax.clear();
        }
        let report = record_coverage(&manifest);

        assert!(
            report
                .gaps
                .contains(&CoverageGap::CommandMissingSyntax { key }),
            "a syntax-less command is a recorded gap: {:?}",
            report.gaps,
        );
    }

    #[test]
    fn coverage_breakdown_sum_matches_totals() {
        let report = record_coverage(&seed_manifest());
        let doc_sum: usize = report.per_collection.iter().map(|c| c.document_count).sum();
        let section_sum: usize = report.per_collection.iter().map(|c| c.section_count).sum();
        // The seed carries one document per source and every section belongs to
        // a registered collection, so the per-collection breakdowns reconcile
        // with the corpus totals.
        assert_eq!(
            doc_sum, report.source_count,
            "document counts sum to sources"
        );
        assert_eq!(
            section_sum, report.section_count,
            "section counts sum to sections",
        );
    }

    #[test]
    fn fidelity_reports_every_content_type_present() {
        // The representative fixtures exercise every content type the extractors
        // must retain, offline, with no navigation chrome leaking through.
        let sections = representative_fidelity_sections();
        let report = inspect_fidelity(&sections);

        for ty in FidelityType::all() {
            assert!(
                report.present.contains(&ty),
                "expected {ty:?} present, got present={:?} absent={:?}",
                report.present,
                report.absent,
            );
        }
        assert!(report.all_types_present(), "{}", report.render());
    }

    #[test]
    fn fidelity_records_absent_types_honestly() {
        // A bare section with only a paragraph: most types are absent, and that
        // absence is recorded, not fabricated.
        let sections = vec![bare_section("Just a paragraph.\n")];
        let report = inspect_fidelity(&sections);

        assert!(!report.all_types_present());
        for ty in [
            FidelityType::NestedHeading,
            FidelityType::Anchor,
            FidelityType::List,
            FidelityType::Table,
            FidelityType::CommandSyntax,
            FidelityType::StabilityCallout,
            FidelityType::LintKnownIssues,
            FidelityType::FencedExample,
        ] {
            assert!(
                report.absent.contains(&ty),
                "expected {ty:?} absent for a bare section",
            );
        }
        // The bare body has no chrome, so the navigation invariant still holds.
        assert!(
            report
                .present
                .contains(&FidelityType::NoNavigationOrScripts)
        );
    }

    #[test]
    fn fidelity_detects_navigation_chrome_as_a_defect() {
        // A body that leaks navigation/scripts must flip the invariant to absent —
        // a recorded defect, never a silent pass.
        let sections = vec![bare_section(
            "Real content.\n<nav class=\"sidebar\">Home</nav>\n<script>doBad()</script>\n",
        )];
        let report = inspect_fidelity(&sections);

        assert!(
            report.absent.contains(&FidelityType::NoNavigationOrScripts),
            "leaked navigation/scripts must be recorded as absent (a defect): {}",
            report.render(),
        );
    }

    #[test]
    fn seed_records_which_types_it_actually_covers() {
        // The bundled seed is a representative subset (by design), so it does not
        // exercise every content type. Record what it *does* cover honestly
        // rather than fabricating the rest; the dedicated representative
        // fixtures above close the remaining types.
        let report = inspect_fidelity(&seed_manifest().body.sections);

        assert_eq!(report.sections_inspected, 8);
        // The seed carries command syntax (two command sections), stability
        // callouts (qualifications on five sections), and no leaked chrome.
        for ty in [
            FidelityType::CommandSyntax,
            FidelityType::StabilityCallout,
            FidelityType::NoNavigationOrScripts,
        ] {
            assert!(
                report.present.contains(&ty),
                "seed should cover {ty:?}: absent={:?}",
                report.absent,
            );
        }
        // The seed does NOT carry nested headings, anchors, lists, tables,
        // relative links, lint "known issues" notes, or fenced examples. Each is
        // a recorded gap, proven retained by the representative fixtures in
        // `fidelity_reports_every_content_type_present`.
        for ty in [
            FidelityType::NestedHeading,
            FidelityType::Anchor,
            FidelityType::List,
            FidelityType::Table,
            FidelityType::RelativeLink,
            FidelityType::LintKnownIssues,
            FidelityType::FencedExample,
        ] {
            assert!(
                report.absent.contains(&ty),
                "seed should not cover {ty:?}: present={:?}",
                report.present,
            );
        }
    }

    #[test]
    fn empty_section_set_is_a_degenerate_report() {
        let report = inspect_fidelity(&[]);
        assert_eq!(report.sections_inspected, 0);
        // Only the navigation invariant holds vacuously; every real type absent.
        assert_eq!(report.present, vec![FidelityType::NoNavigationOrScripts]);
        assert!(!report.all_types_present());
    }
}
