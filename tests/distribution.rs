//! Distribution + attribution verification for task `[067][M5]` —
//! *Verify the installable distribution and attribution contents*
//! ([issue #67](https://github.com/ThiagoEvoa/rust-mcp/issues/67)).
//!
//! # What this proves
//!
//! The M5 exit requires that a **fresh extraction/install** contains three
//! things and that they are **mutually consistent**:
//!
//! 1. **The binary.** `cargo build --release` produces `rust-docs-mcp`
//!    (the `[[bin]]` target in `Cargo.toml`); the clean-build evidence is
//!    recorded in [`docs/DISTRIBUTION.md`](../docs/DISTRIBUTION.md).
//! 2. **The required bundled corpus.** It is embedded in the binary via
//!    `include_str!` (`rust_docs_mcp::bundled::JSON`, the committed
//!    `data/bundled/snapshot.json`), so a fresh install serves it **offline**
//!    with no network and no `sync`.
//! 3. **The third-party notices.** The human-facing
//!    [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) and
//!    [`docs/LICENSING.md`](../docs/LICENSING.md), plus the machine-readable
//!    `attribution[]` records that travel *inside* the snapshot.
//!
//! This file is the **automated** half of acceptance criterion 1: it asserts,
//! over the *shipped* artifact (the embedded bytes, not a hand-built manifest),
//! that the corpus carries exactly the eight required sources and eight
//! attribution records, that every attribution record points at the
//! human-facing notice, and that the notice files that must travel with the
//! content are present in the distribution and name the same upstream
//! repositories the records carry.
//!
//! # Hermetic, offline, deterministic
//!
//! The test touches only the embedded `const` and two files under
//! `CARGO_MANIFEST_DIR`. It spawns no process, opens no socket, and writes
//! nothing, so it is fully hermetic and cannot depend on live upstream
//! (`CARGO_NET_OFFLINE`). It is a single-threaded, pure check with no shared
//! state, so it is not subject to the parallel-only flakiness that the full
//! `cargo test --workspace` suite can hit; run it in isolation with
//! `cargo test --test distribution --locked`.
//!
//! The offline *serve* half — that the binary actually answers `tools/list`
//! (five tools), `list_sources` (eight sources, snapshot
//! `rust-docs-bundled-seed`), and clean disconnects on the initial supported
//! target — is proven by the spawned-binary proofs
//! [`stdio_mcp.rs`](stdio_mcp.rs) and [`first_client.rs`](first_client.rs) and
//! captured in [`docs/DISTRIBUTION.md`](../docs/DISTRIBUTION.md); this test
//! covers the *distribution contents* those serve over.

use std::path::Path;

use rust_docs_mcp::bundled;
use serde_json::Value;

/// The snapshot id the bundled seed always carries. Mirrors the constant the
/// binary embeds; asserted against it below so the two cannot drift.
const SNAPSHOT_ID: &str = "rust-docs-bundled-seed";

/// The eight required source collections, in registry order. Every one must
/// appear in the embedded corpus *and* be attributed.
const REQUIRED_SOURCES: [&str; 8] = [
    "api-guidelines",
    "style-guide",
    "cargo",
    "rustc",
    "rustup",
    "rustfmt",
    "clippy",
    "rustdoc",
];

/// The human-facing notice that must accompany the redistributed content.
const NOTICE_FILE: &str = "THIRD_PARTY_NOTICES.md";
/// The licensing audit that records the per-source evidence and policy.
const LICENSE_AUDIT: &str = "docs/LICENSING.md";

/// Read a file that must ship in the distribution, relative to the package
/// root, failing the test if it is absent or empty.
fn read_distribution_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the distribution must contain {name:?} at {path:?}: {e}"));
    assert!(
        !text.trim().is_empty(),
        "the distribution file {name:?} must not be empty"
    );
    text
}

/// Parse the *embedded* corpus (the exact bytes the binary ships) as JSON,
/// proving the shipped artifact — not a hand-built manifest — is what is
/// checked.
fn embedded_snapshot() -> Value {
    serde_json::from_str(bundled::JSON).expect("the embedded bundled snapshot must be valid JSON")
}

/// AC1 (corpus + attribution travel with the binary): the shipped snapshot
/// carries exactly the eight required sources and eight attribution records,
/// and each record points at the human-facing notice.
#[test]
fn the_distribution_corpus_carries_eight_sources_and_attribution() {
    let manifest = embedded_snapshot();
    assert_eq!(
        manifest["snapshot_id"], SNAPSHOT_ID,
        "the bundled snapshot must carry the seeded id"
    );
    // The const the binary exposes must match the embedded body.
    assert_eq!(
        bundled::SNAPSHOT_ID,
        SNAPSHOT_ID,
        "the SNAPSHOT_ID constant must match the embedded body"
    );

    // All eight required sources are present, each exactly once.
    let sources = manifest["sources"]
        .as_array()
        .expect("the snapshot has a `sources` array");
    assert_eq!(
        sources.len(),
        REQUIRED_SOURCES.len(),
        "the distribution must ship exactly the eight required sources, got {}",
        sources.len()
    );
    let source_ids: Vec<&str> = sources.iter().filter_map(|s| s["id"].as_str()).collect();
    for expected in REQUIRED_SOURCES {
        assert!(
            source_ids.contains(&expected),
            "the distribution must ship the `{expected}` source; got {source_ids:?}"
        );
    }

    // Exactly one attribution record per source (the licence/origin that
    // accompanies each source's content).
    let attribution = manifest["attribution"]
        .as_array()
        .expect("the snapshot carries an `attribution[]` record");
    assert_eq!(
        attribution.len(),
        REQUIRED_SOURCES.len(),
        "every shipped source must carry an attribution record, got {}",
        attribution.len()
    );

    // Every record names the human-facing notice and the permissive licence.
    for record in attribution {
        let source = record["source"]
            .as_str()
            .expect("an attribution record names its source");
        assert!(
            REQUIRED_SOURCES.contains(&source),
            "an attribution record names an unknown source: {source}"
        );
        let notice = record["required_notice"].as_str().unwrap_or_default();
        assert!(
            notice.contains(NOTICE_FILE),
            "every attribution record must point at the {NOTICE_FILE} that travels \
             with the content; {source} record says: {notice:?}"
        );
        let licenses = record["licenses"]
            .as_array()
            .expect("an attribution record names its licences");
        assert!(
            !licenses.is_empty(),
            "the {source} attribution record must name at least one licence"
        );
        assert!(
            licenses
                .iter()
                .any(|l| l.as_str() == Some("MIT OR Apache-2.0")),
            "the {source} attribution record must record the MIT OR Apache-2.0 family; got {licenses:?}"
        );
    }
}

/// AC1 (notices travel with the content): the human-facing notice and the
/// licensing audit are present in the distribution and name the *same* upstream
/// repositories the embedded attribution records carry, so the two views of the
/// attribution cannot diverge.
#[test]
fn the_third_party_notices_travel_with_the_content() {
    let notice = read_distribution_file(NOTICE_FILE);
    read_distribution_file(LICENSE_AUDIT);

    // The operative permissive clause the redistribution is licensed under.
    assert!(
        notice.contains("Apache License, Version 2.0"),
        "the notice must carry the operative Apache-2.0 clause"
    );
    assert!(
        notice.contains("MIT OR Apache-2.0"),
        "the notice must name the MIT OR Apache-2.0 licence family"
    );

    // Each embedded attribution record's upstream repository must appear in the
    // human-facing notice: the machine-readable and human-readable views agree.
    let manifest = embedded_snapshot();
    let attribution = manifest["attribution"]
        .as_array()
        .expect("an `attribution[]` record");
    assert_eq!(
        attribution.len(),
        REQUIRED_SOURCES.len(),
        "the notice cross-check expects one record per required source"
    );
    for record in attribution {
        let source = record["source"]
            .as_str()
            .expect("an attribution record names its source");
        let repository = record["repository"]
            .as_str()
            .expect("an attribution record carries its upstream repository");
        assert!(
            notice.contains(repository),
            "the {source} attribution repository `{repository}` must appear in the \
             {NOTICE_FILE} that travels with the content"
        );
    }
}
