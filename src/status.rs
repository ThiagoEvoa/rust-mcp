//! Corpus status and freshness reporting — task [030] M2 (issue #30).
//!
//! This is the *reporting* side of `plan.md` §5 "Runtime behavior" / §8 "status":
//! given the one [`SnapshotManifest`](crate::model::SnapshotManifest) a process
//! serves for its lifetime ([`crate::runtime::load_snapshot`]) and an explicit
//! "now", it reports the snapshot identity, the source/section counts, and each
//! source's freshness. It is the human-readable companion to
//! [`crate::ingest::pipeline::SyncReport`]: where `sync` publishes a corpus,
//! `status` describes the one already selected.
//!
//! ## What it owns — and what it deliberately does not
//!
//! - **It makes no network request.** The report is a pure function of the
//!   in-memory snapshot plus an injected `as_of` instant and a
//!   [`StaleThreshold`]; its signature carries no HTTP client and it performs no
//!   I/O (AC2 "status makes no requests"). It never fetches, downloads, or
//!   refreshes — that is `sync`'s job.
//! - **Staleness is metadata only.** A stale source is still reported in full
//!   (its identity, times, and content hash); the verdict never gates reading
//!   (AC2 "stale content remains readable").
//! - **The three times stay distinct** (AC2). `last_checked` drives staleness
//!   (via [`evaluate_freshness`]); `fetched_at` is "when this content was
//!   downloaded" and `upstream_modified` is "when the source last changed
//!   upstream" — all three are surfaced separately, never conflated. This is the
//!   exact split [`crate::provenance`] owns: a conditional (304) check advances
//!   `last_checked` *without* moving `fetched_at` or the content hash, and that
//!   source is still fresh.
//! - **Unknowns are preserved, never guessed.** A source with no known
//!   `last_checked` yields an *unknown* verdict (`stale == None`), matching
//!   [`SourceFreshness`].
//!
//! The threshold is **configurable** (AC1): [`StaleThreshold::default`] is the
//! plan's seven days, and the CLI can reconfigure it via `--stale-days`.
//!
//! ## Pure and offline by construction
//!
//! [`build_report`] takes only `(&SnapshotManifest, Instant, StaleThreshold)` —
//! no client, no filesystem, no clock. "Now" is injected, so tests are
//! deterministic (a fixed `as_of`), and the report is a pure function of its
//! inputs: two calls with equal inputs produce equal reports.

use crate::model::{CorpusHash, SnapshotId, SnapshotManifest, Timestamp};
use crate::provenance::{Instant, SourceFreshness, StaleThreshold, evaluate_freshness};

/// The per-source slice of a [`StatusReport`]: the source's identity, its three
/// distinct times, its content identity, and the computed freshness verdict.
///
/// `last_checked`, `fetched_at`, and `upstream_modified` are kept as separate
/// fields (AC2) so a reader can see *when the source was last checked* (the
/// staleness input) alongside *when it was downloaded* and *when it last changed
/// upstream*. `content_hash` is the captured-content identity, decoupled from
/// every timestamp. `freshness` is the verdict computed by
/// [`evaluate_freshness`] over `last_checked` (and only `last_checked`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStatus {
    /// The source identity.
    pub id: crate::model::SourceId,
    /// The human title of the source collection.
    pub title: String,
    /// The last successful upstream check time — the freshness input; `None`
    /// means unknown (the verdict is then unknown, not guessed).
    pub last_checked: Option<Timestamp>,
    /// When this content was downloaded. Distinct from `last_checked`: a 304
    /// check advances `last_checked` without moving this.
    pub fetched_at: Timestamp,
    /// The known upstream modification time, when available; `None` means
    /// unknown. Distinct from both `last_checked` and `fetched_at`.
    pub upstream_modified: Option<Timestamp>,
    /// The captured-content identity. Held by a conditional (304) check, so it
    /// is unchanged even when `last_checked` advances.
    pub content_hash: CorpusHash,
    /// The freshness verdict (`stale` / `as_of` / `threshold` / parsed
    /// `last_checked`) computed by [`evaluate_freshness`].
    pub freshness: SourceFreshness,
}

impl SourceStatus {
    /// The stale verdict for this source, or `None` when `last_checked` is
    /// unknown (preserved, never guessed).
    pub fn is_stale(&self) -> Option<bool> {
        self.freshness.is_stale()
    }

    /// The human rendering of the verdict: `stale`, `fresh`, or `unknown`.
    fn verdict_label(&self) -> &'static str {
        match self.is_stale() {
            Some(true) => "stale",
            Some(false) => "fresh",
            None => "unknown",
        }
    }
}

/// A complete status report for the one snapshot a process serves.
///
/// It carries the snapshot identity, the source/section counts, the threshold
/// and the "now" the freshness was judged against, and one [`SourceStatus`] per
/// source. It is built by [`build_report`] and rendered to **stderr** by
/// [`StatusReport::to_stderr`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusReport {
    /// The selected snapshot identity.
    pub snapshot_id: SnapshotId,
    /// The number of sources in the snapshot.
    pub source_count: usize,
    /// The number of sections in the snapshot.
    pub section_count: usize,
    /// The "now" the freshness verdicts were judged against.
    pub as_of: Instant,
    /// The staleness threshold the verdicts used (configurable; seven days by
    /// default).
    pub threshold: StaleThreshold,
    /// One [`SourceStatus`] per source, in the order the snapshot lists them.
    pub sources: Vec<SourceStatus>,
}

/// Build a [`StatusReport`] for `manifest`, judged at `as_of` under `threshold`.
///
/// This is the pure core of `status` (AC1/AC2): it reads only the in-memory
/// `manifest` and the two injected parameters. For each source it computes
/// [`evaluate_freshness`] over `last_checked` and records the three times
/// (`last_checked` / `fetched_at` / `upstream_modified`) and the content hash as
/// distinct fields, so a stale source is still fully reported and a 304-style
/// "checked but unchanged" source is still fresh. It makes **no** network request
/// and performs **no** I/O — the report is a pure function of its arguments.
pub fn build_report(
    manifest: &SnapshotManifest,
    as_of: Instant,
    threshold: StaleThreshold,
) -> StatusReport {
    let sources = manifest
        .body
        .sources
        .iter()
        .map(|source| {
            let freshness = evaluate_freshness(source, as_of, threshold);
            SourceStatus {
                id: source.id.clone(),
                title: source.title.clone(),
                last_checked: source.last_checked.clone(),
                fetched_at: source.fetched_at.clone(),
                upstream_modified: source.upstream_modified.clone(),
                content_hash: source.content_hash.clone(),
                freshness,
            }
        })
        .collect();

    StatusReport {
        snapshot_id: manifest.body.snapshot_id.clone(),
        source_count: manifest.body.sources.len(),
        section_count: manifest.body.sections.len(),
        as_of,
        threshold,
        sources,
    }
}

impl StatusReport {
    /// Render a human-readable, **stderr-only** report: the snapshot identity,
    /// the source/section counts, the freshness threshold (noting the default),
    /// the "now", and each source's three times, content identity, and verdict.
    ///
    /// The text is *returned* (not written) so the caller decides where to emit
    /// it — the CLI writes it to stderr, keeping stdout exclusive to MCP
    /// messages (`plan.md` §8/§9). It mirrors
    /// [`crate::ingest::pipeline::SyncReport::to_stderr`].
    pub fn to_stderr(&self) -> String {
        let mut out = String::from("rust-docs-mcp status:\n");
        out.push_str(&format!("  snapshot: {}\n", self.snapshot_id.as_str()));
        out.push_str(&format!(
            "  source(s): {}, section(s): {}\n",
            self.source_count, self.section_count
        ));
        let threshold_note = if self.threshold == StaleThreshold::default() {
            "default"
        } else {
            "configured"
        };
        out.push_str(&format!(
            "  freshness threshold: {} day(s) ({threshold_note})\n",
            self.threshold.as_days()
        ));
        out.push_str(&format!(
            "  as_of: {} (seconds since the Unix epoch)\n",
            self.as_of.as_secs()
        ));
        for source in &self.sources {
            out.push_str(&format!("  [{}] {}\n", source.id, source.title));
            out.push_str(&format!(
                "    last checked: {}\n",
                source
                    .last_checked
                    .as_ref()
                    .map(|ts| ts.as_str())
                    .unwrap_or("unknown")
            ));
            out.push_str(&format!("    fetched:      {}\n", source.fetched_at));
            out.push_str(&format!(
                "    upstream:     {}\n",
                source
                    .upstream_modified
                    .as_ref()
                    .map(|ts| ts.as_str())
                    .unwrap_or("unknown")
            ));
            out.push_str(&format!(
                "    content:      {}\n",
                source.content_hash.as_str()
            ));
            out.push_str(&format!("    stale:        {}\n", source.verdict_label()));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Tests — offline and deterministic: `as_of` is always an injected fixed
// instant, never the wall clock, so no check depends on the time the test runs.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Channel, CorpusHash, ManifestBody, Release, SchemaVersion, SectionBuilder,
        SnapshotManifest, Source, SourceId, Timestamp, Url, ValidationSummary,
    };

    /// A minimal, dependency-free `Source` carrying the fields the report reads;
    /// everything else takes a valid default so a source has the full shape while
    /// the tests stay focused on the freshness-relevant fields.
    fn source(
        id: &str,
        title: &str,
        last_checked: Option<&str>,
        fetched_at: &str,
        upstream_modified: Option<&str>,
        content: &[u8],
    ) -> Source {
        Source {
            id: SourceId::new(id).unwrap(),
            title: title.into(),
            entry_url: Url::new("https://doc.rust-lang.org/cargo/").unwrap(),
            document_urls: vec![],
            last_checked: last_checked.map(|s| Timestamp::new(s).unwrap()),
            fetched_at: Timestamp::new(fetched_at).unwrap(),
            upstream_modified: upstream_modified.map(|s| Timestamp::new(s).unwrap()),
            content_hash: CorpusHash::compute(content),
            release: Release::channel_only(Channel::Stable),
            parser: "test/1".into(),
        }
    }

    /// A snapshot carrying the given sources and a given number of (well-formed,
    /// otherwise-decorative) sections, enough for `build_report`, which reads
    /// only the identity, the counts, and the sources — it does not validate the
    /// corpus (that is `sync`'s job). The sections are built through
    /// `SectionBuilder` so each has the consistent id/hash the type layer expects.
    fn manifest(snapshot_id: &str, sources: Vec<Source>, section_count: usize) -> SnapshotManifest {
        let section = SectionBuilder::new(
            SourceId::new("cargo").unwrap(),
            "The cargo book",
            "commands/index",
            Url::new("https://doc.rust-lang.org/cargo/commands/index.html#build").unwrap(),
            "# Build\n\nRun `cargo build` to compile without linking.",
        )
        .anchor("build")
        .build()
        .expect("a well-formed section builds");

        SnapshotManifest {
            schema_version: SchemaVersion::CURRENT,
            body: ManifestBody {
                snapshot_id: SnapshotId::new(snapshot_id).unwrap(),
                created_at: Timestamp::new("2026-10-30T00:00:00Z").unwrap(),
                parser_version: "test/1".into(),
                sources,
                sections: std::iter::repeat_n(section, section_count).collect(),
                checklist: vec![],
                commands: vec![],
                validation: ValidationSummary::default(),
                attribution: vec![],
            },
        }
    }

    // AC1: the default threshold is seven days.
    #[test]
    fn default_threshold_is_seven_days() {
        assert_eq!(StaleThreshold::default().as_days(), 7);
        assert_eq!(StaleThreshold::default().seconds(), 7 * 86_400);

        // A report built with no explicit threshold uses the seven-day default,
        // and records it.
        let s = source(
            "cargo",
            "The Cargo Book",
            Some("2026-10-28T00:00:00Z"),
            "2026-10-28T00:00:00Z",
            None,
            b"x",
        );
        let report = build_report(
            &manifest("rust-docs", vec![s], 0),
            as_of("2026-10-31T00:00:00Z"),
            StaleThreshold::default(),
        );
        assert_eq!(report.threshold.as_days(), 7);
        assert_eq!(report.threshold, StaleThreshold::default());
    }

    // AC1/AC2: a source checked eight days ago is stale under the default.
    #[test]
    fn source_eight_days_old_is_stale_under_default() {
        // 2026-10-26 -> 2026-11-03 is eight days, older than the seven-day window.
        let s = source(
            "cargo",
            "The Cargo Book",
            Some("2026-10-26T00:00:00Z"),
            "2026-10-26T00:00:00Z",
            None,
            b"x",
        );
        let report = build_report(
            &manifest("rust-docs", vec![s], 0),
            as_of("2026-11-03T00:00:00Z"),
            StaleThreshold::default(),
        );
        assert_eq!(report.sources[0].is_stale(), Some(true));
        // The stale verdict lives in the recorded freshness, and the source is
        // still fully present (AC2: stale content remains readable).
        assert_eq!(report.sources[0].freshness.is_stale(), Some(true));
        assert_eq!(report.sources[0].id.as_str(), "cargo");
    }

    // AC1: the same eight-day-old source is fresh under a longer threshold, so
    // the threshold is configurable.
    #[test]
    fn old_source_is_fresh_under_a_longer_threshold() {
        let s = source(
            "cargo",
            "The Cargo Book",
            Some("2026-10-26T00:00:00Z"),
            "2026-10-26T00:00:00Z",
            None,
            b"x",
        );
        let report = build_report(
            &manifest("rust-docs", vec![s], 0),
            as_of("2026-11-03T00:00:00Z"),
            StaleThreshold::days(30),
        );
        assert_eq!(report.sources[0].is_stale(), Some(false));
        assert_eq!(report.threshold.as_days(), 30);
    }

    // AC2: a source checked one day ago is fresh under the default.
    #[test]
    fn recent_source_is_fresh_under_default() {
        let s = source(
            "rustc",
            "The Rustc Book",
            Some("2026-10-31T00:00:00Z"),
            "2026-10-31T00:00:00Z",
            None,
            b"x",
        );
        let report = build_report(
            &manifest("rust-docs", vec![s], 0),
            as_of("2026-11-01T00:00:00Z"),
            StaleThreshold::default(),
        );
        assert_eq!(report.sources[0].is_stale(), Some(false));
    }

    // AC2: last_checked and fetched_at are distinct. A conditional (304) check
    // advances last_checked WITHOUT changing fetched_at or the content hash, and
    // the source is still fresh — staleness tracks last_checked, not download
    // time or content identity.
    #[test]
    fn last_checked_and_fetched_at_are_distinct_under_a_304() {
        // "Before": last checked and downloaded at the same time, content v1.
        let before = source(
            "rustup",
            "The Rustup Book",
            Some("2026-10-28T00:00:00Z"),
            "2026-10-28T00:00:00Z",
            None,
            b"v1",
        );
        // "After": a conditional (304) check advanced last_checked to 2026-11-04,
        // but the content (and its hash, and the download time) did not change.
        let after = source(
            "rustup",
            "The Rustup Book",
            Some("2026-11-04T00:00:00Z"),
            "2026-10-28T00:00:00Z",
            None,
            b"v1",
        );
        let report = build_report(
            &manifest("rust-docs", vec![after], 0),
            as_of("2026-11-05T00:00:00Z"),
            StaleThreshold::default(),
        );
        let status = &report.sources[0];

        // The two times are distinct fields and differ after the 304.
        assert_ne!(
            status.last_checked.as_ref().unwrap(),
            &status.fetched_at,
            "a 304 advances last_checked without moving fetched_at"
        );
        assert_eq!(status.fetched_at.as_str(), "2026-10-28T00:00:00Z");
        assert_eq!(
            status.last_checked.as_ref().unwrap().as_str(),
            "2026-11-04T00:00:00Z"
        );

        // Content identity and download time were NOT moved by the 304.
        assert_eq!(status.fetched_at, before.fetched_at);
        assert_eq!(status.content_hash, before.content_hash);

        // last_checked advanced within the window -> still fresh, proving the
        // verdict tracks last_checked, not fetched_at or the content hash.
        assert_eq!(status.is_stale(), Some(false));
        assert_eq!(
            status.freshness.last_checked,
            Some(as_of("2026-11-04T00:00:00Z"))
        );
    }

    // AC2: an unknown last_checked yields an unknown verdict (preserved, not
    // guessed).
    #[test]
    fn unknown_last_checked_is_an_unknown_verdict() {
        let s = source(
            "clippy",
            "Clippy documentation",
            None,
            "2026-10-30T00:00:00Z",
            None,
            b"x",
        );
        let report = build_report(
            &manifest("rust-docs", vec![s], 0),
            as_of("2026-11-30T00:00:00Z"),
            StaleThreshold::default(),
        );
        let status = &report.sources[0];
        assert!(status.last_checked.is_none());
        assert_eq!(
            status.is_stale(),
            None,
            "an unknown check must not be guessed"
        );
        assert_eq!(status.freshness.is_stale(), None);
        // Even with an unknown verdict, the source is fully reported (readable).
        assert_eq!(status.fetched_at.as_str(), "2026-10-30T00:00:00Z");
        assert_eq!(
            status.content_hash.as_str(),
            CorpusHash::compute(b"x").as_str()
        );
    }

    // AC2: a stale source is still fully reported — its identity, times, and
    // content are all present, so staleness never gates reading.
    #[test]
    fn a_stale_source_is_still_fully_reported() {
        let s = source(
            "cargo",
            "The Cargo Book",
            Some("2026-10-01T00:00:00Z"),
            "2026-10-01T00:00:00Z",
            Some("2026-09-30T00:00:00Z"),
            b"old-content",
        );
        let report = build_report(
            &manifest("rust-docs", vec![s], 1),
            as_of("2026-11-30T00:00:00Z"),
            StaleThreshold::default(),
        );
        let status = &report.sources[0];

        // Stale under the default window (almost two months old).
        assert_eq!(status.is_stale(), Some(true));

        // But every field that a reader needs is still present: the source is
        // readable regardless of its verdict.
        assert_eq!(status.id.as_str(), "cargo");
        assert_eq!(status.title, "The Cargo Book");
        assert_eq!(status.fetched_at.as_str(), "2026-10-01T00:00:00Z");
        assert_eq!(
            status.upstream_modified.as_ref().unwrap().as_str(),
            "2026-09-30T00:00:00Z"
        );
        assert_eq!(
            status.content_hash.as_str(),
            CorpusHash::compute(b"old-content").as_str()
        );

        // The rendered report still carries the stale source's identity, content,
        // and a verdict — it is not omitted or hidden.
        let rendered = report.to_stderr();
        assert!(
            rendered.contains("cargo"),
            "the stale source's id is reported"
        );
        assert!(
            rendered.contains("The Cargo Book"),
            "the stale source's title is reported"
        );
        assert!(
            rendered.contains(CorpusHash::compute(b"old-content").as_str()),
            "the stale source's content identity is reported"
        );
        assert!(rendered.contains("stale"), "the verdict is reported");
    }

    // AC2: status makes no network request. `build_report` takes only an in-memory
    // snapshot and two injected parameters (no client, no I/O), so its output is
    // a pure function of its inputs: two calls with equal inputs produce equal
    // reports, with no external effect to observe.
    #[test]
    fn build_report_is_pure_and_makes_no_network_request() {
        let s = source(
            "cargo",
            "The Cargo Book",
            Some("2026-10-28T00:00:00Z"),
            "2026-10-28T00:00:00Z",
            Some("2026-10-27T00:00:00Z"),
            b"content",
        );
        let manifest = manifest("rust-docs-2026-10-30", vec![s], 42);
        let as_of = as_of("2026-10-31T00:00:00Z");
        let threshold = StaleThreshold::default();

        // Pure: identical inputs give identical reports, with no I/O or clock.
        let first = build_report(&manifest, as_of, threshold);
        let second = build_report(&manifest, as_of, threshold);
        assert_eq!(first, second);

        // The report reflects the in-memory snapshot exactly — nothing external.
        assert_eq!(first.snapshot_id.as_str(), "rust-docs-2026-10-30");
        assert_eq!(first.source_count, 1);
        assert_eq!(first.section_count, 42);
        assert_eq!(first.as_of, as_of);
        assert_eq!(first.threshold, threshold);
    }

    // The report records the snapshot identity and the source/section counts
    // (plan.md §5 "snapshot ID, source ID, …").
    #[test]
    fn report_records_snapshot_identity_and_counts() {
        let sources = vec![
            source(
                "cargo",
                "The Cargo Book",
                None,
                "2026-10-30T00:00:00Z",
                None,
                b"a",
            ),
            source(
                "rustc",
                "The Rustc Book",
                None,
                "2026-10-30T00:00:00Z",
                None,
                b"b",
            ),
        ];
        let report = build_report(
            &manifest("rust-docs-2026-10-30", sources, 7),
            as_of("2026-10-31T00:00:00Z"),
            StaleThreshold::default(),
        );
        assert_eq!(report.snapshot_id.as_str(), "rust-docs-2026-10-30");
        assert_eq!(report.source_count, 2);
        assert_eq!(report.section_count, 7);
        assert_eq!(report.sources.len(), 2);
    }

    // A rendered report shows the threshold and notes whether it is the default.
    #[test]
    fn rendered_report_shows_threshold_and_default_note() {
        let s = source(
            "cargo",
            "The Cargo Book",
            Some("2026-10-30T00:00:00Z"),
            "2026-10-30T00:00:00Z",
            None,
            b"x",
        );
        let manifest = manifest("rust-docs", vec![s], 0);

        let with_default = build_report(
            &manifest,
            as_of("2026-10-31T00:00:00Z"),
            StaleThreshold::default(),
        )
        .to_stderr();
        assert!(
            with_default.contains("7 day(s)"),
            "the default window is shown"
        );
        assert!(with_default.contains("default"), "the default is noted");

        let with_config = build_report(
            &manifest,
            as_of("2026-10-31T00:00:00Z"),
            StaleThreshold::days(14),
        )
        .to_stderr();
        assert!(
            with_config.contains("14 day(s)"),
            "the configured window is shown"
        );
        assert!(
            !with_config.contains("default"),
            "a configured window is not the default"
        );
    }

    /// Parse an RFC 3339 literal into a fixed `Instant` for a test's `as_of`.
    fn as_of(literal: &str) -> Instant {
        Instant::parse(literal).expect("a well-formed test timestamp parses")
    }
}
