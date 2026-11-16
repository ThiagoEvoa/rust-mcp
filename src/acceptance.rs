//! Acceptance measurement over the *shipped* corpus (issue #156, plan §11).
//!
//! The eval harness ([`crate::eval`]) proves the ranking *mechanism* — that the
//! index ranks, ties deterministically, and the corpus is independent of the
//! ranking's aliases — over a small, deterministic corpus. This module measures
//! retrieval *quality* over the real bundled corpus a fresh install actually
//! serves: the evidence the plan's "≥90% top-5 recall + exact-match first" bar
//! requires (plan §11), and the gap the audit found — a small curated fixture
//! standing in for the shipped corpus.
//!
//! ## What it measures
//!
//! It builds the assembled [`SectionIndex`] from the bundled snapshot's manifest
//! — the same assembled corpus a `serve` process indexes, and the same
//! `search_docs` pipeline a client calls — and runs the *genuine*
//! [`crate::eval::CORPUS`] (all 45 hand-labeled cases) over it:
//!
//! * **Top-5 recall** — for every case, does some top-5 result belong to the
//!   source the query primarily targets ([`EvalCase::source`])?
//! * **Exact-match / canonical first** — for every *exact-match* case (a
//!   guideline code, a command name, or a lint id, whose query *is* the literal
//!   structured token), does the top-1 hit carry that token in the section's
//!   structured field — a guideline code, command key, or lint id — not merely a
//!   BM25 co-occurrence?
//!
//! The canonical-identity matching is the crux of the audit's "canonical
//! first-place": the eval dataset's `expected_labels` are *abstract* section
//! paths (`guidelines/C-CONV`, `commands/cargo-build`, …) while the shipped
//! sections carry those same tokens in their structured fields
//! (`Section::guideline_codes`, `::command_keys`, `::lint_ids`). Matching on the
//! structured fields — not on a section-path string — is what makes "canonical"
//! mean "the hit carries the genuine token", not "some section's prose happens to
//! mention it".
//!
//! ## Honest, not gated to a pass
//!
//! The measurement records the *real* numbers — including the misses — rather
//! than asserting a green bar the corpus may not yet clear. The accompanying test
//! pins the *measured* baseline (so a regression is caught) and reports the gap
//! against the plan's 90% target, naming the misses. A number that cannot be
//! produced — a harness timeout, an unmeasured case — is recorded as such, never
//! counted as a complete result (criterion 5).
//!
//! ## Measuring at the index level
//!
//! The audit's "7 zero hits" came from the *spawned server's* `search_docs`
//! path, where pagination, the output byte cap, and source filtering can empty a
//! result even when the index holds a match. The index is where recall is
//! honestly measurable; the accompanying integration test additionally exercises
//! the server path through the assembled binary so the two are reconciled, and a
//! missing or failed measurement never counts as a pass.

use crate::eval::{CORPUS, EvalCase, EvalKind};
use crate::model::{Section, SnapshotManifest};
use crate::search::SectionIndex;

// ---------------------------------------------------------------------------
// Corpus identity
// ---------------------------------------------------------------------------

/// The corpus identity an acceptance report is tied to, so the numbers can be
/// attributed to a specific, reproducible corpus (plan §11: "corpus identity").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorpusIdentity {
    /// The snapshot's own id (the corpus's `snapshot_id` field).
    pub snapshot_id: String,
    /// The parser version that produced the corpus.
    pub parser_version: String,
    /// The number of sections the assembled index was built from — the corpus
    /// "size".
    pub section_count: usize,
    /// The source ids present, in registry order.
    pub sources: Vec<String>,
    /// A deterministic, dependency-free content fingerprint of the corpus — its
    /// `snapshot_id`, `parser_version`, and the sorted section ids hashed with
    /// FNV-1a. It changes when the corpus changes, so a report can be tied to an
    /// exact corpus without a hash crate. (FNV-1a is a content *fingerprint*,
    /// not a cryptographic digest; the corpus is deterministic, so the
    /// `snapshot_id` + `parser_version` + `section_count` already identify it, and
    /// this makes that identity a single, diffable value.)
    pub content_fingerprint: String,
}

impl CorpusIdentity {
    /// The identity of a manifest's corpus — its snapshot id, the parser that
    /// produced it, the section count, the sources present, and a deterministic
    /// content fingerprint over the corpus.
    pub fn from_manifest(manifest: &SnapshotManifest) -> CorpusIdentity {
        let body = &manifest.body;
        let mut ids: Vec<String> = body
            .sections
            .iter()
            .map(|s| s.id.as_str().to_string())
            .collect();
        ids.sort();
        let fingerprint = fnv1a_fingerprint(
            body.snapshot_id.as_str(),
            body.parser_version.as_str(),
            &ids,
        );
        CorpusIdentity {
            content_fingerprint: fingerprint,
            snapshot_id: body.snapshot_id.as_str().to_string(),
            parser_version: body.parser_version.clone(),
            section_count: body.sections.len(),
            sources: body
                .sources
                .iter()
                .map(|source| source.id.as_str().to_string())
                .collect(),
        }
    }
}

/// The identity of the *shipped* (bundled) corpus — the `rust-docs-bundled-seed`
/// snapshot every fresh install serves. A convenience so a caller (the perf
/// harness, the acceptance test) can assert the corpus identity without naming the
/// private `SnapshotManifest` type.
pub fn shipped_corpus_identity() -> CorpusIdentity {
    CorpusIdentity::from_manifest(&crate::bundled::manifest())
}

/// A deterministic FNV-1a fingerprint over `salt` + `parser` + the comma-joined
/// sorted `section_ids`, returned as a 16-hex-digit string. Dependency-free and
/// order-stable, so it is reproducible across runs and machines.
fn fnv1a_fingerprint(salt: &str, parser: &str, section_ids: &[std::string::String]) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100_000001b3;
    let mut hash = FNV_OFFSET;
    for chunk in [salt, parser, &section_ids.join(",")] {
        for byte in chunk.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    format!("{hash:016x}")
}

// ---------------------------------------------------------------------------
// Per-case outcome and report
// ---------------------------------------------------------------------------

/// The measured outcome of one eval case over the shipped corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseOutcome {
    /// The case's stable id (e.g. `"E-001"`).
    pub id: String,
    /// The case's category, as a string (e.g. `"Command"`, `"Ordinary"`).
    pub kind: String,
    /// Whether this case is an *exact-match* case — a guideline code, command
    /// name, or lint id — for which the top-1 must carry the literal token.
    pub exact_match: bool,
    /// The query string that was run.
    pub query: String,
    /// The source the query primarily targets, as a registry id.
    pub expected_source: String,
    /// Whether the case produced a measurement at all — criterion 5: a
    /// skipped/unmeasured case must never count as a complete result.
    pub measured: bool,
    /// The top-1 hit's section id, if any result ranked.
    pub top1: Option<String>,
    /// Whether a top-5 result belongs to the expected source (top-5 recall).
    pub top5_in_expected_source: bool,
    /// Whether the top-1 hit carries the canonical token the query names
    /// (exact-match / canonical first). `None` for non-exact-match cases, for
    /// which the metric does not apply.
    pub canonical_first: Option<bool>,
    /// A recorded reason a case could not be measured, if any — recorded, never
    /// counted as a pass.
    pub error: Option<String>,
}

/// The environment a measurement was taken in (plan §11: "environment" +
/// "reproducible"). Recorded so a number is never attributed to the wrong
/// machine or build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Environment {
    /// The toolchain `rustc --version` reports (best-effort; a sentinel when
    /// rustc is not on the PATH).
    pub rustc: String,
    /// The build mode the measurement ran under (`debug`, `release`, …).
    pub build_mode: String,
    /// The host the measurement ran on (os + arch), so a number is attributed to
    /// a machine, not just a corpus.
    pub host: String,
}

impl Environment {
    /// Capture the running environment. The build mode is read from the profile
    /// env var Cargo sets; the toolchain is read from `rustc --version` when
    /// available, falling back to a sentinel so a measurement that cannot record
    /// it still records *something* rather than failing.
    pub fn current() -> Environment {
        Environment {
            rustc: current_rustc_version(),
            build_mode: detect_build_mode(),
            host: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        }
    }
}

/// The best-effort `rustc --version` string, or a sentinel when rustc is not on
/// the PATH. A measurement never fails because the toolchain cannot be probed;
/// the number it records is reproducible on a machine that has rustc.
fn current_rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "rustc (unavailable on PATH)".to_string())
}

/// The build profile this binary was compiled under, read from the profile env
/// var Cargo sets, with a neutral fallback so a measurement with no env still
/// records a build-mode field.
fn detect_build_mode() -> String {
    match std::env::var("CARGO_CFG_PROFILE") {
        Ok(mode) if !mode.is_empty() => mode,
        _ => "unspecified".to_string(),
    }
}

/// The full acceptance measurement: the corpus identity it was taken against,
/// the environment it ran in, and the per-case outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptanceReport {
    /// The corpus the numbers were measured against.
    pub corpus: CorpusIdentity,
    /// The environment the measurement ran in.
    pub environment: Environment,
    /// The per-case outcomes, in `CORPUS` order.
    pub cases: Vec<CaseOutcome>,
}

/// Measure retrieval quality over `manifest`'s corpus: build the assembled index
/// from the manifest (the same assembled corpus `serve` indexes) and run every
/// [`crate::eval::CORPUS`] case through it. `environment` is recorded with the
/// report so the numbers are reproducible.
pub fn measure(manifest: &SnapshotManifest, environment: &Environment) -> AcceptanceReport {
    let index = SectionIndex::build(manifest);
    let corpus = CorpusIdentity::from_manifest(manifest);
    let cases = CORPUS
        .iter()
        .map(|case| measure_case(&index, case))
        .collect();
    AcceptanceReport {
        corpus,
        environment: environment.clone(),
        cases,
    }
}

/// Measure one case over the assembled index: run its query, record the top-1
/// hit, whether a top-5 result is in the expected source, and — for exact-match
/// cases — whether the top-1 carries the literal token. A case always produces a
/// measurement; if it cannot, the reason is recorded in `error` and `measured`
/// is `false` (it is never counted as a pass).
fn measure_case(index: &SectionIndex, case: &EvalCase) -> CaseOutcome {
    let hits = index.search(case.query);
    // The top-1 hit's section id (a string for the report) and the section it
    // resolves to (so its structured fields can be checked).
    let top1_id = hits.first().map(|hit| hit.section_id.as_str().to_string());
    let top1_section = hits.first().and_then(|hit| index.get(&hit.section_id));
    let top5_in_expected_source = hits.iter().take(5).any(|hit| {
        index
            .get(&hit.section_id)
            .map(|s| s.source_id.as_str() == case.source.id())
            .unwrap_or(false)
    });
    let exact_match = case.kind.is_canonical();
    let canonical_first = if exact_match {
        Some(carries_canonical_token(case, top1_section))
    } else {
        None
    };
    CaseOutcome {
        id: case.id.to_string(),
        kind: format!("{:?}", case.kind),
        exact_match,
        query: case.query.to_string(),
        expected_source: case.source.id().to_string(),
        measured: true,
        top1: top1_id,
        top5_in_expected_source,
        canonical_first,
        error: None,
    }
}

/// Whether the top-1 section (if any) carries the literal token the query names,
/// in the structured field the case's category designates. This is the
/// "exact-match / canonical first" check: the top-1 *carries the genuine token*,
/// not merely a BM25 co-occurrence.
fn carries_canonical_token(case: &EvalCase, top1: Option<&Section>) -> bool {
    let section = match top1 {
        Some(s) => s,
        None => return false,
    };
    match case.kind {
        EvalKind::GuidelineCode => section.guideline_codes.iter().any(|c| c == case.query),
        EvalKind::Command => section
            .command_keys
            .iter()
            .any(|c| c.eq_ignore_ascii_case(case.query)),
        EvalKind::Lint => section.lint_ids.iter().any(|l| l == case.query),
        // A non-canonical case is not an exact-match case, so this is not
        // reached (the caller gates on `is_canonical`); the arm keeps the match
        // exhaustive if the gate ever loosens.
        EvalKind::Workflow | EvalKind::Ordinary => false,
    }
}

impl AcceptanceReport {
    /// The count of cases with a top-5 hit in the expected source, over the
    /// total cases measured — the top-5 recall numerator/denominator.
    pub fn top5_recall(&self) -> (usize, usize) {
        let pass = self
            .cases
            .iter()
            .filter(|c| c.top5_in_expected_source)
            .count();
        (pass, self.cases.len())
    }

    /// The count of exact-match cases whose top-1 carries the canonical token,
    /// over the total exact-match cases — the "canonical first-place"
    /// numerator/denominator.
    pub fn canonical_first(&self) -> (usize, usize) {
        let exact: Vec<&CaseOutcome> = self.cases.iter().filter(|c| c.exact_match).collect();
        let pass = exact
            .iter()
            .filter(|c| c.canonical_first == Some(true))
            .count();
        (pass, exact.len())
    }

    /// The cases that did *not* get a top-5 hit in their expected source — the
    /// recall misses, returned so the gap is visible, not hidden.
    pub fn top5_misses(&self) -> Vec<&CaseOutcome> {
        self.cases
            .iter()
            .filter(|c| !c.top5_in_expected_source)
            .collect()
    }

    /// The exact-match cases whose top-1 did *not* carry the canonical token —
    /// the canonical-first misses.
    pub fn canonical_misses(&self) -> Vec<&CaseOutcome> {
        self.cases
            .iter()
            .filter(|c| c.canonical_first == Some(false))
            .collect()
    }

    /// Whether every case produced a measurement with no recorded error
    /// (criterion 5: a harness that skips or fails a case must not report a
    /// complete result).
    pub fn all_measured(&self) -> bool {
        self.cases.iter().all(|c| c.measured && c.error.is_none())
    }

    /// The top-5 recall as a percentage (0–100), rounded to one decimal.
    pub fn top5_recall_pct(&self) -> f64 {
        pct(self.top5_recall())
    }

    /// A human-readable, deterministic report: the corpus identity, the
    /// environment, the recall/canonical numbers, the target gap, and the miss
    /// lists. This is what the test emits and what `docs/ACCEPTANCE.md` quotes.
    pub fn render(&self) -> String {
        let (recall_pass, recall_total) = self.top5_recall();
        let (canonical_pass, canonical_total) = self.canonical_first();
        let mut out = String::new();
        out.push_str("retrieval acceptance — shipped corpus\n");
        out.push_str(&format!("  snapshot_id : {}\n", self.corpus.snapshot_id));
        out.push_str(&format!("  parser      : {}\n", self.corpus.parser_version));
        out.push_str(&format!(
            "  fingerprint : {}\n",
            self.corpus.content_fingerprint
        ));
        out.push_str(&format!(
            "  sections    : {} ({})\n",
            self.corpus.section_count,
            self.corpus.sources.join(", ")
        ));
        out.push_str(&format!(
            "  environment : {} ({} build, {})\n",
            self.environment.rustc, self.environment.build_mode, self.environment.host
        ));
        out.push_str(&format!(
            "  top-5 recall: {}/{} ({}%; plan target ≥90%, gap {}pp)\n",
            recall_pass,
            recall_total,
            self.top5_recall_pct(),
            (90.0 - self.top5_recall_pct()).round() * 10.0 / 10.0
        ));
        out.push_str(&format!(
            "  canonical first: {}/{} exact-match cases\n",
            canonical_pass, canonical_total
        ));
        out.push_str(&format!("  all measured: {}\n", self.all_measured()));
        append_misses(&mut out, "top-5 misses", &self.top5_misses());
        append_misses(&mut out, "canonical-first misses", &self.canonical_misses());
        out
    }
}

/// Append a labelled miss list to `out`, or a "none" line when empty.
fn append_misses(out: &mut String, label: &str, misses: &[&CaseOutcome]) {
    if misses.is_empty() {
        out.push_str(&format!("  {label}: none\n"));
    } else {
        out.push_str(&format!("  {label}:\n"));
        for miss in misses {
            out.push_str(&format!(
                "    - {} [{}] {} (expected {})\n",
                miss.id, miss.kind, miss.query, miss.expected_source
            ));
        }
    }
}

/// `pass/total` as a percentage rounded to one decimal; `0.0` when there is no
/// denominator.
fn pct((pass, total): (usize, usize)) -> f64 {
    if total == 0 {
        0.0
    } else {
        (pass as f64 * 100.0 / total as f64).round() * 10.0 / 10.0
    }
}
