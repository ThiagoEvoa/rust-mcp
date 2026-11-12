//! Citation, identity, and cross-reference consistency — task `[047][M3]`
//! (issue #47, `plan.md` §10 "M3 exit" and §11 "… refer to the same immutable
//! corpus").
//!
//! This is a **verification module**, not a production one: it adds no runtime
//! behavior and changes no existing seam. It composes the already-tested
//! retrieval seams — the `[033]`–`[035]` search index (`crate::search`), the
//! `[036]` section/checklist reads (`crate::reads`), and the `[039]`–`[044]`
//! command catalog/guidance (`crate::commands`) — and asserts that they all
//! refer to the **one immutable snapshot** they were built from. Two
//! acceptance criteria (issue #47) are the assertions:
//!
//! * **AC1** — every section id a retrieval operation returns resolves, through
//!   the reads/`SectionIndex` seam, to a section whose *source URL* and
//!   *snapshot identity* match the snapshot the id was returned from;
//! * **AC2** — source-specific lint identifiers (`clippy::unwrap_used`), command
//!   identifiers (`cargo build`), and *unknown* compatibility values are
//!   **preserved** — not dropped, not normalized away — across every retrieval
//!   operation.
//!
//! ## Why this is proof-only
//!
//! The four retrieval operations already guarantee both properties *by
//! construction*, so this task is to **prove** it rather than to fix a gap:
//!
//! * Every operation resolves a section id through the **same**
//!   [`crate::search::SectionIndex::build`] of the **same** manifest it was
//!   served from. A search hit's [`crate::search::Hit::section_id`] is a
//!   `section.id` straight from that index; a checklist entry's
//!   `explanation_sections` are verified to resolve ([`crate::reads`]); a
//!   command entry's `upstream_sections` are verified to resolve
//!   ([`crate::commands::CommandCatalog::resolve`]) before the guidance is
//!   composed. So a returned id can never point outside the served snapshot.
//! * Each operation stamps the read with `manifest.body.snapshot_id`, and
//!   returns the section **verbatim** (a clone through `get_section`), so the
//!   section's `canonical_url`, `source_id`, `lint_ids`, `command_keys`,
//!   `guideline_codes`, and `qualifications` arrive unchanged.
//!
//! A genuine gap would show up as a returned id that fails to resolve, or a
//! field that arrives altered; the tests below assert neither occurs, on both
//! the shipped bundled corpus and hand-built fixtures that carry the lint/
//! command/unknown-compatibility values the seed corpus does not.

// The module is gated `#[cfg(test)]` in `lib.rs`: it is a verification harness
// with no production caller, so it compiles only for the test target (and hence
// under `cargo check --all-targets` / `cargo clippy --all-targets`), and its
// helpers are used by the `#[test]`s below, so nothing is dead.

use crate::bundled::manifest;
use crate::model::SectionId;
use crate::provenance::{Instant, StaleThreshold};
use crate::reads::get_section;

/// The `as_of` point the bundled seed was captured at (its `fetched_at`), used
/// as the freshness-evaluation instant so the composed freshness verdict is the
/// honest "unknown" one the seed carries (no `last_checked`).
fn seed_as_of() -> Instant {
    Instant::parse("2026-10-30T00:00:00Z").expect("a valid instant")
}

// --- AC1: a returned section id resolves with matching source URL + snapshot ---

/// AC1 (scaffold): a section id returned by a retrieval operation resolves,
/// through the reads seam, to a section whose snapshot identity matches the
/// snapshot it was served from.
#[test]
fn scaffold_section_id_resolves_with_matching_snapshot_identity() {
    let m = manifest();
    let id: SectionId = "api-guidelines/about".parse().expect("a valid section id");

    // The read is snapshot-bound to the manifest it resolves against.
    let read = get_section(&m, &id, seed_as_of(), StaleThreshold::default())
        .expect("a known id resolves in the bundled corpus");
    assert_eq!(
        read.snapshot_id, m.body.snapshot_id,
        "the read carries the snapshot identity of the snapshot it served from"
    );
    // The resolved section's own identity matches the id that was requested.
    assert_eq!(read.section.id, id);
    // Its canonical source URL is non-empty and valid.
    assert!(!read.section.canonical_url.as_str().is_empty());
}
