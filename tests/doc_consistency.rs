//! Offline consistency check for the documented client / operations /
//! regeneration examples — issue #176, task `[P2][QA]`.
//!
//! # What this guards
//!
//! The client-setup, operations, and regeneration guides
//! (`docs/CLIENTS.md`, `docs/OPERATIONS.md`, `docs/REGENERATION.md`, and
//! `docs/DISTRIBUTION.md`) carry **concrete, copy-pastable** section ids and
//! **manifest claims** — the source / section / checklist / command counts, the
//! parser marker, the per-source content hashes, and the `corpus_hash` that
//! `list_sources` advertises. When the bundled corpus evolved (the seed grew
//! into a hierarchical **877-section** snapshot on parser **`seed-2`**, with
//! **source-specific** content hashes), those literals went stale: the docs had
//! still quoted 8 sections / parser `seed-1` / a single shared hash / a bare
//! `api-guidelines/about` id the corpus no longer carries. That is exactly the
//! class of bug issue #176 was filed for. This test pins the values so a future
//! corpus change *fails the build* until the guides are re-reconciled, instead
//! of silently serving a section id the corpus no longer resolves.
//!
//! # Hermetic
//!
//! No network, no binary spawn, no async. The corpus is the **committed**
//! `data/bundled/snapshot.json` — the same bytes the `serve` binary embeds via
//! `include_str!` in `src/bundled.rs` — read at compile time with `include_str!`
//! and parsed as JSON. The in-memory corpus the binary *actually serves* is
//! cross-checked through the public `section_count` / `has_section` guards, so
//! the assertions hold against the corpus a real client would see, not only
//! against the file on disk.
//!
//! # What it asserts (all against the committed corpus, no live upstream)
//!
//! * the **manifest counts** the guides quote — 8 sources, 877 sections, 54
//!   checklist entries, 29 command keys, parser `rust-docs-mcp/seed-2`, schema
//!   version `1` — match the corpus;
//! * every **concrete section id** the guides show as copy-pastable resolves in
//!   the corpus — the C-CONV naming rule the `search_docs` → `get_section` chain
//!   lands on, the fixed `api-guidelines/about#rust-api-guidelines`, and the
//!   eight representative per-source ids — while the **obsolete** bare id
//!   `api-guidelines/about` the guides flag as an *expected-error* is *absent*;
//! * the **per-source content hashes** the guides quote are the source-specific
//!   values the corpus carries (no longer one shared hash);
//! * the **C-CONV checklist entry** links to the C-CONV naming-rule section the
//!   guides chain to;
//! * the **`corpus_hash`** `list_sources` advertises is the `c128-` hash of the
//!   committed manifest bytes.
//!
//! This complements `tests/first_client.rs`, which proves the *same* chain over
//! a real stdio client: that test is the behavioral proof, this one is the
//! docs-vs-corpus consistency guard.

use serde_json::Value;

/// The committed, embedded snapshot — the exact bytes `serve` serves offline
/// (the `JSON` constant in `src/bundled.rs` embeds this same file).
const SNAPSHOT_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/bundled/snapshot.json"
));

/// The manifest claims the guides quote. Kept as literals so a drift in the
/// corpus (or a stale guide) trips the build rather than shipping a section id
/// the corpus no longer resolves.
const EXPECTED_SCHEMA_VERSION: u64 = 1;
const EXPECTED_PARSER_VERSION: &str = "rust-docs-mcp/seed-2";
const EXPECTED_SNAPSHOT_ID: &str = "rust-docs-bundled-seed";
const EXPECTED_SOURCES: usize = 8;
const EXPECTED_SECTIONS: usize = 877;
const EXPECTED_CHECKLIST: usize = 54;
const EXPECTED_COMMANDS: usize = 29;

/// The `corpus_hash` `list_sources` / the catalog resource advertise — the
/// `c128-` hash over the compact serialization of the committed manifest.
const EXPECTED_CORPUS_HASH: &str = "c128-ffaf654d6d447eb40b9e9629be845a92";

/// The concrete section ids the guides present as copy-pastable. Each must
/// resolve in the corpus. The first two are the two ids `docs/CLIENTS.md` shows
/// in its `search_docs` → `get_section` example (the top C-CONV hit the search
/// returns, then the section `get_section` fetches); the rest are the
/// representative one-section-per-source ids `docs/REGENERATION.md` §2.2 quotes.
const DOCUMENTED_SECTION_IDS: [&str; 9] = [
    // CLIENTS.md §3.3 search-to-section chain: the top "C-CONV" hit.
    "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv",
    // The fixed About id (the bare `api-guidelines/about` below is the
    // expected-error demonstration, so the anchored form is the copy-pastable one).
    "api-guidelines/about#rust-api-guidelines",
    // REGENERATION.md §2.2 — one representative section per source.
    "style-guide/expressions#expressions",
    "cargo/commands%2Fcargo-new#cargo-new1",
    "rustc/command-line-arguments#command-line-arguments",
    "rustup/installation%2Findex#installation",
    "rustfmt/README.md#quick-start",
    "clippy/clippy-catalog#unwrap_used",
    "rustdoc/command-line-arguments#command-line-arguments",
];

/// The per-source content hashes `docs/REGENERATION.md` §2.2 quotes. Since the
/// corpus grew to real content they are **source-specific** (no longer the one
/// shared `c128-ffa5092e1…` the guides used to print), so each is pinned here.
const DOCUMENTED_SOURCE_HASHES: [(&str, &str); 8] = [
    ("api-guidelines", "c128-ffa6caa702a86473fe801b0bcb02d958"),
    ("style-guide", "c128-ffa596f39b95d538268ea3a933acba9d"),
    ("cargo", "c128-ffa6d93dcb18fde78b2ef7926b4861b6"),
    ("rustc", "c128-ffa55d2e9ca927751cfbcc7794137903"),
    ("rustup", "c128-ffa5ff983cff5cbc92f95a7ce7f3c250"),
    ("rustfmt", "c128-ffa5483e8818922ce73e4a6f383a9380"),
    ("clippy", "c128-ffa5067277597fa1db7e098805bd3ead"),
    ("rustdoc", "c128-ffa64cec68c7765563660336e199955c"),
];

/// The C-CONV checklist entry the guides reference: its code, category, location,
/// and the section its explanation links to (the one the search chain lands on).
const CCODE: &str = "C-CONV";
const CCATEGORY: &str = "Naming";
const CLOCATION: &str = "Naming#C-CONV";

/// The `search_docs` → `get_section` section the guides chain through, and its
/// content hash (`docs/CLIENTS.md` §3.3 (3)).
const CCODE_SECTION_ID: &str =
    "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv";
const CCODE_SECTION_HASH: &str = "c128-ffa51d275f6a20c3c6d4a02b1a369f52";

/// Mirror of `CorpusHash::compute` in `src/model.rs` (a `c128-`-prefixed 128-bit
/// dual accumulator — **not** FNV-1a). Reproduced here rather than called because
/// `CorpusHash` lives in a private module; if that algorithm ever changes this
/// pin trips, which is the point. The corpus hash `list_sources` advertises is
/// this over the compact serialization of the manifest — byte-identical to the
/// committed `snapshot.json` (the builder's drift test asserts that).
fn corpus_hash(bytes: &[u8]) -> String {
    const PREFIX: &str = "c128-";
    let mut hi: u64 = 0xcbf2_9ce4_8422_2325;
    let mut lo: u64 = 0xffa5_3c27_52a3_4b71;
    const PRIME_64: u64 = 0x0000_0100_0000_01b1;
    for &b in bytes {
        lo = (lo ^ (b as u64).wrapping_mul(PRIME_64)).wrapping_add(0x85eb_c6d3);
        hi = (hi ^ (b as u64)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{PREFIX}{lo:016x}{hi:016x}")
}

/// Parse the committed snapshot once and hand it back as a `Value`.
fn committed_corpus() -> Value {
    serde_json::from_str(SNAPSHOT_JSON)
        .expect("the committed data/bundled/snapshot.json must be valid JSON")
}

#[test]
fn documented_examples_match_the_committed_corpus() {
    let corpus = committed_corpus();

    // ---- Manifest claims the guides quote must match the corpus. ----------
    // `schema_version` serializes as the JSON number `1` (not the string), so the
    // guides' "schema version 1" claim is checked against the numeric field.
    assert_eq!(
        corpus["schema_version"].as_u64(),
        Some(EXPECTED_SCHEMA_VERSION),
        "schema_version the guides quote must match the corpus"
    );
    assert_eq!(
        corpus["parser_version"].as_str(),
        Some(EXPECTED_PARSER_VERSION),
        "the guides quote parser `seed-2`, not the obsolete `seed-1`"
    );
    assert_eq!(
        corpus["snapshot_id"].as_str(),
        Some(EXPECTED_SNAPSHOT_ID),
        "the guides quote the bundled seed snapshot id"
    );

    let sources = corpus["sources"]
        .as_array()
        .expect("the corpus carries a `sources` array");
    let sections = corpus["sections"].as_array().expect("a `sections` array");
    let checklist = corpus["checklist"].as_array().expect("a `checklist` array");
    let commands = corpus["commands"].as_array().expect("a `commands` array");

    // The four counts the guides quote — the exact numbers #176 found stale
    // (the guides had 8 / 544 / 1 / 3; the corpus is 8 / 877 / 54 / 29).
    assert_eq!(
        sources.len(),
        EXPECTED_SOURCES,
        "source count the guides quote must match the corpus"
    );
    assert_eq!(
        sections.len(),
        EXPECTED_SECTIONS,
        "section count the guides quote must match the corpus"
    );
    assert_eq!(
        checklist.len(),
        EXPECTED_CHECKLIST,
        "checklist-entry count the guides quote must match the corpus"
    );
    assert_eq!(
        commands.len(),
        EXPECTED_COMMANDS,
        "command-catalog count the guides quote must match the corpus"
    );

    // The in-memory corpus the binary serves must agree with the committed file.
    // `section_count` / `has_section` are the public guards `src/bundled.rs`
    // exposes exactly for this "do these ids name real shipped sections?" check.
    assert_eq!(
        rust_docs_mcp::bundled::section_count(),
        EXPECTED_SECTIONS,
        "the served corpus's section count must match the committed file"
    );

    // ---- Concrete copy-pastable section ids the guides show must resolve. --
    for id in DOCUMENTED_SECTION_IDS {
        assert!(
            rust_docs_mcp::bundled::has_section(id),
            "the copy-pastable section id `{id}` the guides show must resolve \
             in the served corpus"
        );
    }

    // The obsolete bare id the guides flag as an *expected-error* demonstration
    // must stay absent: a reappearing `api-guidelines/about` would make the
    // documented `-32002` example wrong.
    assert!(
        !rust_docs_mcp::bundled::has_section("api-guidelines/about"),
        "the obsolete bare id `api-guidelines/about` must stay absent — the guides \
         rely on it as the expected-error (-32002) demonstration"
    );

    // ---- Per-source content hashes are source-specific and match. --------
    let mut have_source_hashes: Vec<(&str, &str)> = sources
        .iter()
        .filter_map(|s| Some((s["id"].as_str()?, s["content_hash"].as_str()?)))
        .collect();
    have_source_hashes.sort_by(|a, b| a.0.cmp(b.0));
    let mut want_source_hashes = DOCUMENTED_SOURCE_HASHES.to_vec();
    want_source_hashes.sort_by(|a, b| a.0.cmp(b.0));
    assert_eq!(
        have_source_hashes, want_source_hashes,
        "the per-source content hashes the guides quote must be the \
         source-specific values the corpus carries"
    );

    // ---- The C-CONV checklist entry links to the naming-rule section. ----
    let cconv = checklist
        .iter()
        .find(|e| e["code"].as_str() == Some(CCODE))
        .expect("the C-CONV checklist entry is present");
    assert_eq!(
        cconv["category"].as_str(),
        Some(CCATEGORY),
        "C-CONV category"
    );
    assert_eq!(
        cconv["location"].as_str(),
        Some(CLOCATION),
        "C-CONV location"
    );
    let explanations: Vec<&str> = cconv["explanation_sections"]
        .as_array()
        .expect("the C-CONV entry names its explanation sections")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        explanations.contains(&CCODE_SECTION_ID),
        "the C-CONV entry must explain itself via the C-CONV naming-rule \
         section the guides chain to: {explanations:?}"
    );

    // The C-CONV section the guides fetch carries the documented content hash.
    let cconv_section = sections
        .iter()
        .find(|s| s["id"].as_str() == Some(CCODE_SECTION_ID))
        .expect("the C-CONV naming-rule section is present");
    assert_eq!(
        cconv_section["content_hash"].as_str(),
        Some(CCODE_SECTION_HASH),
        "the C-CONV section's content hash must match the guide"
    );

    // ---- The advertised corpus_hash is the hash of the committed bytes. --
    // `list_sources` / the catalog resource compute it over the compact
    // serialization of the manifest, which is byte-identical to the committed
    // `snapshot.json` (the builder's drift test enforces that).
    let computed = corpus_hash(SNAPSHOT_JSON.as_bytes());
    assert_eq!(
        computed, EXPECTED_CORPUS_HASH,
        "the `corpus_hash` the guides quote must be the hash of the \
         committed manifest bytes"
    );
}
