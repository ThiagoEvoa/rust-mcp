# Reproducing, inspecting, validating, and attributing the bundled corpus

**Task:** [066][M5] — *Document reproducible bundle regeneration and notices* (issue [#66](https://github.com/ThiagoEvoa/rust-mcp/issues/66)).
**Status:** Documented and verified.
**Verified:** 2026-11-13, on `rust-docs-mcp 0.1.0` (rustc/cargo `1.99.1`), edition `2024`, MSRV `1.88`.
**Sources of truth:** the plan (§5 *Licensing gate / Runtime behavior / Data model*; the M5 milestone in §10); the licensing audit [`docs/LICENSING.md`](LICENSING.md) and its machine-readable notice [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) (from [026]/[#6] and [048]/[#31]); the operator commands [`docs/OPERATIONS.md`](OPERATIONS.md) (from [065]); the CI design [`docs/CI.md`](CI.md) (from [062]).

This record is the canonical "how the bundled corpus is produced, checked, and attributed" reference. It makes the two acceptance criteria concrete:

- **AC1** — the instructions identify **source revisions, parser/schema versions, and deterministic content hashes** so a consumer can reproduce and verify the bundle.
- **AC2** — the **required notices** travel with the redistributed content and **corpus validation is part of regeneration**.

---

## 0. Two distinct artifacts — don't conflate them

The word "bundle" covers two things that must be kept separate:

| Artifact | What it is | Produced by | Identity |
| --- | --- | --- | --- |
| **Bundled seed** | A *representative* corpus embedded in the binary via `include_str!`, so a fresh install serves offline. | `src/bundled.rs` `manifest()` → `data/bundled/snapshot.json`, guarded by a drift test. | `snapshot_id = rust-docs-bundled-seed`; **fixed in time**. |
| **Published corpus** | The *full, current* corpus on disk. | `rust-docs-mcp sync` (network) → `<data-dir>/corpus/manifest.json`. | A fresh, **version-stamped** snapshot; the operator path. |

Issue [#66](https://github.com/ThiagoEvoa/rust-mcp/issues/66) is about the **bundle the binary ships** (the seed). The `sync` path is how a *current* corpus is obtained and how the seed's `unknown` provenance fields get populated on a live fetch. The two share the same schema, validator, and corpus model.

---

## 1. Regenerate the bundle

### 1.1 What the bundle is

The canonical builder is `manifest()` in [`src/bundled.rs`](../src/bundled.rs). It produces a single `SnapshotManifest`:

| Field | Value | Notes |
| --- | --- | --- |
| `snapshot_id` | `rust-docs-bundled-seed` | `SNAPSHOT_ID` constant; identifies the seed. |
| `schema_version` | `1` | `SchemaVersion::CURRENT` (`V1`) — the only supported version. |
| `parser_version` | `rust-docs-mcp/seed-1` | A seed marker, *not* the live pipeline version. A live `sync` stamps `rust-docs-mcp/<CARGO_PKG_VERSION>` = `rust-docs-mcp/0.1.0` (`parser_version()` in `src/ingest/pipeline.rs`). |
| `created_at` / `fetched_at` | `2026-10-30T00:00:00Z` | **Fixed** so the bundle is deterministic across builds (a real sync stamps a wall-clock `fetched_at`). |
| Sources | 8 | The eight required `SourceCollection`s. |
| Sections | 8 | One representative section per source. |
| `checklist` | 1 | Entry `C-CONV` (*"Prefer `From`/`Into` for conversions"*). |
| `commands` | 3 | `cargo build`, `cargo test`, `rustup show`. |

The JSON is embedded in the binary with `include_str!("../../data/bundled/snapshot.json")` (the `JSON` constant in `src/bundled.rs`), so the published binary carries the corpus and serves offline with no network.

### 1.2 The reproducible seam

The committed `data/bundled/snapshot.json` is **not hand-edited** — it is generated and guarded:

1. **Generate:** the ignored seam rewrites the file from the builder.

   ```sh
   cargo test --locked bundled::tests::emit_bundle_json -- --ignored
   ```

   `emit_bundle_json` writes `manifest().to_json_string()` to `data/bundled/snapshot.json`.

2. **Guard:** the drift test in the ordinary suite asserts the committed bytes equal the builder's output:

   ```sh
   cargo test --locked bundled::tests::the_committed_bundle_matches_the_builder
   ```

   So the *only* way to change the committed file is through the builder — no silent edits survive a suite run.

### 1.3 Determinism

- `created_at`/`fetched_at` are fixed, and the compact serializer (`SnapshotManifest::to_json_string`) emits **no insignificant whitespace**, so the bundle is byte-stable and therefore hash-able byte-for-byte.
- **Verified reproducible:** running the seam above produced a byte-identical file — `md5 = b4b468eeac20ea6289d910e224287a41` before and after, and `git status` clean — proving the committed `data/bundled/snapshot.json` is exactly what `manifest()` produces.
- **Exclusions seam:** `manifest_with_exclusions(&first_sync_required)` is the documented path for a source that cannot be bundled yet (it is omitted and flagged *first-sync-required* rather than dropped from the schema). It is **not** used by the current seed: `FIRST_SYNC_REQUIRED` is empty, because all eight sources are MIT OR Apache-2.0 and bundle cleanly.

### 1.4 Getting a *full, current* corpus (the live path)

The seed is a representative subset. To obtain the full corpus:

```sh
rust-docs-mcp sync     # network; publishes to <data-dir>/corpus/manifest.json
rust-docs-mcp serve    # restart serve to activate the new corpus
```

Full detail (the conditional/HTTP 304 refresh, the writer lock, atomic publish, offline fallback) is in [`docs/OPERATIONS.md`](OPERATIONS.md) §2–§6. The opt-in CI job that runs `sync` on the agent's network is [`docs/CI.md`](CI.md) → [`.github/workflows/upstream-ingestion.yml`](../.github/workflows/upstream-ingestion.yml) (from [062]); it is `workflow_dispatch` + weekly and **not** part of the mandatory gate. **`sync` is never run in the offline test suite or in `cargo test`** (the `CARGO_NET_OFFLINE` invariant; see `docs/CI.md`).

---

## 2. Inspect the corpus

### 2.1 The operator view — `inspect`

`inspect` prints coverage and extraction fidelity over the active snapshot (default: the bundled seed). **Real captured output** from this binary, `RUST_DOCS_MCP_DATA_DIR` pointing at an empty data directory (so the seed loads):

```
coverage_complete=true
fidelity_complete=true

corpus coverage:
  sources=8 sections=8 checklist=1 commands=3
  Rust API Guidelines documents=1 sections=1
  Rust Style Guide documents=1 sections=1
  The Cargo Book documents=1 sections=1
  The rustc Book documents=1 sections=1
  The rustup Book documents=1 sections=1
  rustfmt reference documents=1 sections=1
  Clippy         documents=1 sections=1
  The rustdoc Book documents=1 sections=1
  gaps: none

extraction fidelity (sections=4):
  present nested heading
  present anchor
  present list
  present table
  present command syntax/options
  present stability callout
  present relative link
  present lint known issues
  present fenced code example
  present no navigation/scripts
```

(The per-run tracing prefix on each line — e.g. `2026-11-13T16:14:00.333305Z  INFO 304: rust-docs-mcp:` — is omitted for readability; it is timestamped per run and carries no corpus fact.)

- **`coverage_complete=true`** — every *required* `SourceCollection` has ≥1 section and its `checklist`/`commands` cross-references resolve.
- **`fidelity_complete=true`** — all ten content types the extractors must retain are *present* (no recorded gap): `FidelityReport::all_types_present` (`src/coverage.rs`) is true when the report's `absent` list is empty. The run is over the representative fixtures (`inspect_fidelity` over `representative_fidelity_sections()`, hence `sections=4`); a type no fixture exhibits lands in `absent`, flips `fidelity_complete` to `false`, and is *recorded* as a gap — never a fabricated pass. This is **not** "every section body is untruncated"; that is not what the fidelity check measures.

The same run is reproducible: because the seed is fixed and the serializer is compact, the coverage/fidelity lines are identical on every run.

### 2.2 The machine-readable identity and content hashes

The snapshot manifest (`data/bundled/snapshot.json`, or a synced `<data-dir>/corpus/manifest.json`) carries the identity and hashes that AC1 requires. **Real fields, real values** from the committed seed:

| Field | Value | Meaning |
| --- | --- | --- |
| `schema_version` | `1` | Corpus schema version; the validator rejects any other version. |
| `snapshot_id` | `rust-docs-bundled-seed` | Snapshot identity. |
| `parser_version` | `rust-docs-mcp/seed-1` | Parser/ingester version marker (see §1.1). |
| per-source `content_hash` | `c128-ffa5092e1f9db0151650f6e19369b606` | `CorpusHash::compute` over the source content; identical for all eight seed sources. |
| per-section `content_hash` | one per section (below) | `CorpusHash::compute` over the section's content; the validator recomputes and matches it. |
| `attribution[].licenses` | `MIT OR Apache-2.0` (×8) | The permissive license family every source falls under. |
| `attribution[].repository` / `.required_notice` | per-source (see §4.2) | The notice that travels with the content. |

The **eight per-section content hashes** (real, from the committed `data/bundled/snapshot.json`):

| `SectionId` | `content_hash` |
| --- | --- |
| `api-guidelines/about` | `c128-ffa531f24bfdd996c9ef3d1a076dd56a` |
| `style-guide/intro` | `c128-ffa549d71811a6f0c1df98d4204e09dc` |
| `cargo/commands%2Fcargo-build` | `c128-ffa550364dc212527d904d59fe85fe84` |
| `rustc/introduction` | `c128-ffa54a0177b917cbcb3dc0413a060228` |
| `rustup/common-install-problems` | `c128-ffa519dd5f27e9d2da9c1389aa2bfc0a` |
| `rustfmt/options` | `c128-ffa5470b6839fef52f4de6d9b8dff329` |
| `clippy/configuration` | `c128-ffa5381340fae3e97c387c8bd729fcbc` |
| `rustdoc/command-line-arguments` | `c128-ffa51a23788aa244ba8f56032617f86e` |

How a content hash is derived (AC1 "deterministic content hashes"):

- **`CorpusHash`** is a `c128-`-prefixed 128-bit dual accumulator over the bytes (see `CorpusHash::compute` in `src/model.rs`; it is **not** FNV-1a — task [025] may swap it for SHA-256). The seed's per-source hash is `CorpusHash::compute(b"seed-document")`; a per-section hash is computed over that section's content (id, document title, chapter path, anchor, heading path, canonical URL, body, and the attached `guideline_codes` / `command_keys` / `lint_ids` / `qualifications`).
- **There is no single top-level "corpus hash" field.** The deterministic identities are: (a) the per-source `content_hash`, (b) the per-section `content_hash`, and (c) the **compact JSON of the whole manifest** — `to_json_string` is byte-stable (§1.3), so the whole bundle is reproducible and hash-able as a unit.
- The **validator** recomputes each section hash from its content and rejects a mismatch (`ContentHashMismatch`). So a hash is not just a label — it is checked.

### 2.3 Deterministic section identity

Each section's `SectionId` is derived from its fields — `<source_id>/<url-encoded chapter_path>`, plus an optional `#anchor` and a `$c{part}.{parts}` continuation marker — by `SectionId::derive` (in `src/model.rs`). The validator re-derives it and rejects a stored id that does not match (`SectionIdMismatch`). Note the percent-encoding: the cargo section's chapter path `commands/cargo-build` yields `cargo/commands%2Fcargo-build` (the slash is encoded). The eight seed ids are listed in §2.2.

### 2.4 Source revisions, documentation release, and channel

AC1 requires "source revisions **where identifiable; unknown where not**." For the **seed**, the real per-source `release` and freshness fields are:

| Field | Value on the seed | Meaning |
| --- | --- | --- |
| `release.channel` | `stable` for cargo, rustc, style-guide, clippy, rustdoc; `rolling` for api-guidelines, rustup, rustfmt | Representative channel per `channel_for` (§1.1). |
| `release.documentation_release` | *absent* (`unknown`) | The seed records no doc release. |
| `release.revision` | *absent* (`unknown`) | The seed records no upstream revision. |
| `last_checked` | *absent* (`unknown`) | The seed has not checked upstream. |
| `upstream_modified` | *absent* (`unknown`) | The seed has not checked upstream. |
| `fetched_at` / `created_at` | `2026-10-30T00:00:00Z` | The fixed seed time. |

`rust-docs-mcp status` renders these for each source — on the seed every `last checked` and `upstream` is `unknown`, and the `content:` line shows the per-source `content_hash` (the channel lives in the manifest's `release.channel`, not on a `status` row). **Unknown values are preserved explicitly, never guessed** (plan §5/§130/§139). The model's `Release` and `Source` fields are `Option`-bearing precisely so an unknown is a first-class "absent," not a zero.

A live `sync` publishes a **fresh, version-stamped snapshot** (`snapshot_id` = `rust-docs-mcp/<CARGO_PKG_VERSION>-<nanos>`, a wall-clock `fetched_at`, and a per-source `content_hash` over the ingested content) while **preserving the last valid corpus**. The full freshness model — conditional HTTP 304 checks, the `StaleThreshold::default()` (seven-day) window, the last-valid fallback — is in [`docs/OPERATIONS.md`](OPERATIONS.md) §4–§5.

---

## 3. Validate the corpus — validation is part of regeneration (AC2)

### 3.1 The gate

`validate_snapshot(&SnapshotManifest)` (in `src/corpus.rs`) is the **complete-corpus gate**, and it runs *before* anything is published. A snapshot is publishable iff `is_valid()` — i.e. its `ValidationReport` has no problems.

### 3.2 What it checks (all eight required collections)

1. **All eight required `SourceCollection`s are present, each exactly once** — `MissingSource` / `DuplicateSource`.
2. **Per-section integrity:** non-empty body (`EmptySectionBody`); `id` matches its fields (`SectionIdMismatch`); `content_hash` matches its content (`ContentHashMismatch`); ids are unique (`DuplicateSectionId`); and a corpus with **zero** sections is `EmptyCorpus`.
3. **Cross-references resolve:** every `checklist` `explanation_sections` and every `command` `upstream_section` names an existing section (`UnresolvedChecklistReference` / `UnresolvedCommandReference`).

### 3.3 Validation is part of `sync` (atomic publish only if every check passes)

`publish_snapshot` — the free function `crate::corpus::publish_snapshot(data_dir, manifest)` in `src/corpus.rs` — is ordered so a bad corpus can never become active:

1. **Validate in memory** (`validate_snapshot(manifest)` → `.require_valid()?`) *before* touching the filesystem.
2. Serialize to compact JSON and enforce the size bound (`MAX_SNAPSHOT_BYTES = 64 MiB`).
3. Resolve the corpus directory; acquire the **writer lock** (so two concurrent `sync`s don't interleave a write).
4. Clean stale temp files.
5. Write the candidate to a **transient file** and flush it.
6. **Atomically rename** the transient file over `manifest.json`.

Any fetch, parse, or **validation** failure aborts **before the atomic promotion** — so the **last valid snapshot is preserved** and a partial corpus is never published. This is the AC of [027] and the "atomic publication, last valid preserved" behavior documented in [`docs/OPERATIONS.md`](OPERATIONS.md) §6.

### 3.4 Verify the seed's validation yourself

- The **embedded bytes validate through the same gate:** `the_bundled_json_parses_and_validates` loads `data/bundled/snapshot.json` (the exact bytes the binary embeds) and asserts it parses and `is_valid()`.
- `the_bundled_snapshot_validates` asserts the builder output is valid with all eight sources.
- `the_real_manifest_has_no_excluded_sources` asserts the shipped JSON carries no `first-sync-required` flags.
- `rust-docs-mcp inspect` reports `coverage_complete` / `fidelity_complete` over the active snapshot.

All of these run in the ordinary offline suite.

---

## 4. Attribute the corpus — notices travel with the content (AC2)

### 4.1 Where the notices live

| Artifact | Role |
| --- | --- |
| [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) | The **human-facing** notice that must accompany redistributed content (template from `docs/LICENSING.md` §8). |
| [`docs/LICENSING.md`](LICENSING.md) | The audit: per-source evidence, the bundled-content policy (§3), and the first-sync fallback (§5). |
| Manifest `attribution[]` | **Machine-readable** per-source `licenses`, `repository`, and `required_notice`, carried in every published snapshot and in the embedded bundle. |

### 4.2 Which source carries which notice (real values from the seed's `attribution[]`)

All eight sources are **MIT OR Apache-2.0**; none ships its own `NOTICE`, so the notice is **authored** by this project and kept in `THIRD_PARTY_NOTICES.md` / `docs/LICENSING.md`, not propagated from upstream:

| Source | `repository` | `licenses` | `required_notice` (verbatim) |
| --- | --- | --- | --- |
| Rust API Guidelines | `https://github.com/rust-lang/api-guidelines` | `["MIT OR Apache-2.0"]` | `Rust upstream content (MIT OR Apache-2.0), attributed in THIRD_PARTY_NOTICES.md` |
| Rust Style Guide | `https://github.com/rust-lang/rust` | `["MIT OR Apache-2.0"]` | `Rust upstream content (MIT OR Apache-2.0), attributed in THIRD_PARTY_NOTICES.md` |
| The Cargo Book | `https://github.com/rust-lang/cargo` | `["MIT OR Apache-2.0"]` | `The upstream content (MIT OR Apache-2.0), attributed in THIRD_PARTY_NOTICES.md` |
| The rustc Book | `https://github.com/rust-lang/rust` | `["MIT OR Apache-2.0"]` | `The upstream content (MIT OR Apache-2.0), attributed in THIRD_PARTY_NOTICES.md` |
| The rustup Book | `https://github.com/rust-lang/rustup` | `["MIT OR Apache-2.0"]` | `The upstream content (MIT OR Apache-2.0), attributed in THIRD_PARTY_NOTICES.md` |
| rustfmt reference | `https://github.com/rust-lang/rustfmt` | `["MIT OR Apache-2.0"]` | `rustfmt upstream content (MIT OR Apache-2.0), attributed in THIRD_PARTY_NOTICES.md` |
| Clippy | `https://github.com/rust-lang/rust-clippy` | `["MIT OR Apache-2.0"]` | `Clippy upstream content (MIT OR Apache-2.0), attributed in THIRD_PARTY_NOTICES.md` |
| The rustdoc Book | `https://github.com/rust-lang/rust` | `["MIT OR Apache-2.0"]` | `The upstream content (MIT OR Apache-2.0), attributed in THIRD_PARTY_NOTICES.md` |

### 4.3 Notices travel with the content

- The `attribution` records are part of the manifest body (`AttributionRef` fields are `skip_serializing_if` empty, so a populated record is **always** serialized). A published snapshot and the embedded bundle both carry them; the notice is not an optional comment.
- **Redistribution:** the bundle may be distributed under **Apache-2.0** (the operative license) with the **MIT text retained**; `THIRD_PARTY_NOTICES.md` and the two license files (`LICENSE-APACHE`, `LICENSE-MIT`) **must accompany** any redistributed content. (See `docs/LICENSING.md` §3/§4.)
- **First-sync fallback:** if a source cannot be bundled, it is *omitted and flagged `first_sync_required`* (via `manifest_with_exclusions`), the **last valid snapshot is preserved**, and `status`/`serve` direct the operator to `sync`. This is the AC of [048] — the seed currently has **none** (`FIRST_SYNC_REQUIRED` empty).
- **Any future non-permissive or upstream-`NOTICE`-carrying content** is excluded per `docs/LICENSING.md` §5 and routed through the fallback, never silently bundled.

### 4.4 What this task did **not** change

This task is **documentation only**. The notice files (`THIRD_PARTY_NOTICES.md`, `docs/LICENSING.md`) and the per-source attribution records are **unchanged and preserved** — this document *records the mechanism* (how the notices are produced, carried, and enforced) rather than authoring new notices.

---

## 5. Quick reference

| Goal | Command / artifact |
| --- | --- |
| Regenerate the committed bundle | `cargo test --locked bundled::tests::emit_bundle_json -- --ignored` (then commit; the drift test guards it) |
| Confirm the bundle has not drifted | `cargo test --locked bundled::tests::the_committed_bundle_matches_the_builder` |
| Confirm the bundled bytes validate | `cargo test --locked` (the `bundled::tests` block, incl. `the_bundled_json_parses_and_validates`) |
| Inspect coverage + extraction fidelity | `rust-docs-mcp inspect` |
| Read identity, content hashes, and freshness | `rust-docs-mcp status` |
| Obtain a full, current corpus | `rust-docs-mcp sync` (network; then restart `serve`) — or the opt-in CI job (`docs/CI.md`) |
| See the notices that travel with the content | `THIRD_PARTY_NOTICES.md`, `docs/LICENSING.md`, and the manifest's `attribution[]` |

## 6. Files touched by this task

| File | Change |
| --- | --- |
| `docs/REGENERATION.md` | **Created** — this reference (regeneration, inspection, validation, attribution). |
| `README.md` | **Extended** — a "Reproducing the bundled corpus" entry in *Related docs* pointing here. |

No `src`, `tests`, `Cargo.toml`, `Cargo.lock`, or `THIRD_PARTY_NOTICES.md` was modified; no dependency was added; no live/network call was added to any test.
