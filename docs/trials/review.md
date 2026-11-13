# Rust review agent trial — `[070][M5]`

> **Task:** `[070][M5] Run the Rust review agent trial`
> ([issue #70](https://github.com/ThiagoEvoa/rust-mcp/issues/70)).
> **Plan:** [§1 success scenario](../../plan.md#1-goals--non-goals) ("when
> reviewing Rust code, the agent retrieves relevant documentation via the
> server's tools and produces review findings that identify concrete code
> locations and relevant source citations") and [§10 / M5](../../plan.md#10-milestones)
> ("an agent trial demonstrates retrieving … and reporting actual outcomes").
> **What this is:** the trial agent's retrieval session, run offline against the
> bundled `rust-docs-bundled-seed` snapshot over the spawned-stdio JSON-RPC seam,
> with the actual requests, real responses, and canonical citations recorded
> below. The same session is encoded as a reproducible test —
> [`tests/trial_review.rs`](../../tests/trial_review.rs).
> **Verified:** 2026-11-13, on `rustc`/`cargo` 1.99.1 (stable), MSRV 1.88,
> edition 2024 — the
> [baseline](../COMPATIBILITY.md#2-rust-toolchain-baseline) of
> [`docs/COMPATIBILITY.md`](../COMPATIBILITY.md).

The trial does **not** claim that a code snippet the agent flags is *automatically
non-compliant* or that any check (Clippy, a build) ran. It retrieves
documentation and records its citations; the calling agent interprets the
guidance and, outside this trial, runs the applicable checks through *its own*
terminal tools. The observed-vs-unverified boundary and the retrieval limits
observed here are recorded explicitly, not asserted away
([§11 risk](../../plan.md#11-technical-decisions--risks-and-unknowns)).

---

## 1. What the trial runs

The trial is one end-to-end agent session over the `rust-docs-mcp serve` binary,
driven exactly like [`tests/first_client.rs`](../../tests/first_client.rs),
[`tests/stdio_mcp.rs`](../../tests/stdio_mcp.rs), and the earlier agent trials
[`tests/trial_library_api_design.rs`](../../tests/trial_library_api_design.rs)
and [`tests/trial_project_implementation.rs`](../../tests/trial_project_implementation.rs):
a raw JSON-RPC 2.0 client over genuine OS stdio, with the bundled snapshot loaded
in-memory and an **empty** `--data-dir` so nothing touches the host cache or the
network. The agent's call sequence is:

| # | Call | Purpose |
|---|------|---------|
| 1 | `initialize` | negotiate the protocol version, read the advertised capabilities and the agent-workflow `instructions`. |
| 2 | `notifications/initialized` | complete the handshake. |
| 3 | `tools/list` | discover the five tools, including `search_docs` and `get_api_checklist`. |
| 4 | `search_docs` — `"C-CONV"` | the **exact-guideline-code** search, the reliable path to the conversion guideline's section: it surfaces `api-guidelines/about`, carrying the `guideline code: C-CONV` citation. |
| 5 | `get_api_checklist` — `category: "Conversions and From"` | the checklist entry for the conversion guideline: `C-CONV` "Prefer `From`/`Into` for conversions", explaining `api-guidelines/about`. |
| 6 | `get_section` — `"api-guidelines/about"` | read the original section the **conversion finding** cites, with its body, provenance, and citations. |
| 7 | `search_docs` — `"lint"` | the reliable path to the **Clippy lint** the reviewer would cite: it surfaces `clippy/configuration` (the seed's one Clippy section), carrying the nightly-component qualification. |
| 8 | `get_section` — `"clippy/configuration"` | read the section the **lint finding** cites, with its body, `canonical_url`, `content_hash`, and the qualification citation. |
| 9 | `get_section` — `"style-guide/intro"` | read the section the **style finding** cites, with its body, `canonical_url`, `content_hash`, and the canonical-document citation. |
| 10 | `search_docs` — `"needless_return"` | a **coverage-limitation probe** (AC2): a *specific* Clippy lint whose section the representative seed does **not** ship — a useful `0 hit` with `next_steps`, the recorded gap rather than a fabricated lint section. |
| 11 | clean client disconnect | the server self-exits. |

The trial's "findings" are the review conclusions the agent draws — each pairing a
**concrete code location** (a representative review-target snippet + a `file:line`)
with a **server-returned citation**. The snippets are the agent's own artifacts:
the server **never sees or executes** them. The "check outcome" of a finding (that
the snippet *actually* violates the guideline / triggers the lint / breaks the
convention) is **not observed** — the server does not run Clippy or a build. What
the trial *observes* is the retrieval itself (§3): the sections, their
`content_hash` / `canonical_url` / `citations`, and the `0 hit` for the absent
lint.

---

## 2. Environment

| Property | Value |
|----------|-------|
| Repository | [ThiagoEvoa/rust-mcp](https://github.com/ThiagoEvoa/rust-mcp) |
| Active snapshot | `rust-docs-bundled-seed` — `created_at` `2026-10-30T00:00:00Z`, `parser_version` `rust-docs-mcp/seed-1`, schema version `1`, **8 sources**, one section per collection |
| Transport | stdio, JSON-RPC 2.0, protocol version `2025-11-25` |
| Network | **None** — the trial runs offline; the snapshot is loaded in-memory |
| Host cache | **None** — an empty `--data-dir` is supplied, so `serve` uses the bundled seed and never touches the host application cache |
| New dependency | **None** — the trial reuses `tokio` + `serde_json`, the existing test seam |
| Toolchain | the repository's stable toolchain / pinned SDK baseline: `rustc`/`cargo` 1.99.1, MSRV 1.88, edition 2024; no MSRV / dependency / behavior / `unsafe` change |
| Corpus source hashes | the seed shares one content hash (`c128-ffa5092e1f9db0151650f6e19369b606`) across its 8 sources; the three sections the trial cites have content hashes `c128-ffa531f24bfdd996c9ef3d1a076dd56a` (`api-guidelines/about`), `c128-ffa5381340fae3e97c387c8bd729fcbc` (`clippy/configuration`), and `c128-ffa549d71811a6f0c1df98d4204e09dc` (`style-guide/intro`) |
| Freshness | `unknown` — the representative seed has never checked upstream (recorded, not fabricated) |

---

## 3. Session transcript

Each entry below is a real request and its real response, regenerated from the
`RUST_DOCS_MCP_TRIAL_TRANSCRIPT` dump the test writes (see §6). The request ids
follow the session order; the server may answer out of order, and the client
matches each response to its request by `id`. The `result.structuredContent`
view of each response is shown (the `content` text block and `isError` are
omitted; for `get_section` the `structuredContent` nests the section under a
`section` key, with `schema_version` and `truncated` as siblings).

### 3.1 `initialize` → the negotiated version, capabilities, and `instructions`

The client negotiates `2025-11-25`. The server echoes it, advertises the `tools`
and `resources` capabilities, and carries its `serverInfo` plus the agent-workflow
`instructions` — the text the trial runs on:

```json
{ "jsonrpc": "2.0", "id": 0, "result": {
    "protocolVersion": "2025-11-25",
    "capabilities": { "tools": {}, "resources": {} },
    "serverInfo": { "name": "rust-docs-mcp", "version": "0.1.0" },
    "instructions": "<the full agent-workflow text; see the note below>"
} }
```

> **Reading:** three phrases in the `instructions` pin the retrieval-versus-
> execution boundary the trial must not over-claim, and the trial asserts them
> verbatim in `the_server_states_it_provides_evidence_not_a_guarantee`: the
> server *"retrieves official Rust documentation only: it does not run commands,
> does not touch the project filesystem, and does not reach the network"*, it
> *"does not execute commands or observe their results automatically"*, and it
> *"provides evidence, **not a guarantee** that all guidance has been followed."*
> The full `instructions` text is identical to the library-design and
> project-implementation trials'
> ([`docs/trials/library-api-design.md` §3.1](library-api-design.md#31-initialize--the-negotiated-version-capabilities-and-instructions));
> it is the same agent-workflow contract, and the trial reuses it.

### 3.2 `tools/list` → the five advertised tools

The server advertises the five tools. Each carries a full JSON-Schema
`inputSchema` and `outputSchema` (omitted here for brevity; the schema module
generates them from the very types that serialise the responses). The names and
descriptions below are the real ones:

```json
{ "jsonrpc": "2.0", "id": 1, "result": { "tools": [
    { "name": "list_sources",
      "description": "List the source collections this snapshot serves, with per-source freshness and the available command-catalog keys. Takes no arguments." },
    { "name": "search_docs",
      "description": "Search the Rust documentation corpus by query, optionally scoped to a source and paginated. Returns ranked hits with excerpts and citations." },
    { "name": "get_section",
      "description": "Read one documentation section by its id, with its heading breadcrumb, body, and provenance. Resumable via a continuation cursor." },
    { "name": "get_api_checklist",
      "description": "List Rust API-guideline checklist entries, optionally filtered by category and paginated." },
    { "name": "get_command_guidance",
      "description": "Read the guidance for one command (e.g. `cargo test`): its purpose, syntax, examples, prerequisites, compatibility, effect notes, and the source-backed sections it cites." }
] } }
```

### 3.3 `search_docs "C-CONV"` → the conversion guideline's section, with its `C-CONV` citation

The exact-guideline-code search surfaces `api-guidelines/about` — the section that
carries the `C-CONV` guideline code — with two citations: the canonical document
and the `guideline code: C-CONV` note. This is the reliable retrieval path for the
conversion finding:

```json
{
  "snapshot_id": "rust-docs-bundled-seed",
  "limit": 5,
  "truncated": false,
  "hits": [
    {
      "section_id": "api-guidelines/about",
      "source": "api-guidelines",
      "title": "The API Guidelines",
      "score": 2.864784293464422,
      "excerpt": "…lp an API author write an API that is easy to use and hard to misuse. Each guideline carries a stable code (for example C-CONV) and is enforced in practice by Clippy lints.",
      "citations": [
        { "note": "canonical upstream document", "section_id": "api-guidelines/about", "source": "api-guidelines", "title": "The API Guidelines", "url": "https://rust-lang.github.io/api-guidelines/about.html" },
        { "note": "guideline code: C-CONV", "section_id": "api-guidelines/about", "source": "api-guidelines" }
      ]
    }
  ]
}
```

### 3.4 `get_api_checklist { category: "Conversions and From" }` → the `C-CONV` entry

```json
{
  "category": "Conversions and From",
  "entries": [
    {
      "category": "Conversions and From",
      "code": "C-CONV",
      "title": "Prefer `From`/`Into` for conversions",
      "location": "Conversions and From#C-CONV",
      "explanation_sections": ["api-guidelines/about"]
    }
  ],
  "limit": 5,
  "snapshot_id": "rust-docs-bundled-seed",
  "truncated": false
}
```

### 3.5 `get_section "api-guidelines/about"` → the section the conversion finding cites

```json
{
  "schema_version": "1",
  "section": {
    "id": "api-guidelines/about",
    "source": "api-guidelines",
    "doc_title": "The API Guidelines",
    "snapshot_id": "rust-docs-bundled-seed",
    "content_hash": "c128-ffa531f24bfdd996c9ef3d1a076dd56a",
    "freshness": "unknown",
    "canonical_url": "https://rust-lang.github.io/api-guidelines/about.html",
    "chapter_path": "about",
    "body": "The Rust API Guidelines are recommendations for the design, documentation, and testing of Rust APIs. They help an API author write an API that is easy to use and hard to misuse. Each guideline carries a stable code (for example C-CONV) and is enforced in practice by Clippy lints.",
    "citations": [
      { "note": "canonical upstream document", "section_id": "api-guidelines/about", "source": "api-guidelines", "title": "The API Guidelines", "url": "https://rust-lang.github.io/api-guidelines/about.html" },
      { "note": "guideline code: C-CONV", "section_id": "api-guidelines/about", "source": "api-guidelines" }
    ]
  },
  "truncated": false
}
```

### 3.6 `search_docs "lint"` → the Clippy section the lint finding cites

The reliable path to the **Clippy lint** the reviewer would cite: the query
`"lint"` surfaces `clippy/configuration` — the seed's one Clippy section — with two
citations: the canonical document and the **nightly-component qualification**. The
section carries *no per-lint rule* (the seed ships the Clippy *configuration
reference*, not the per-lint catalog); the qualification is what the lint finding
records.

```json
{
  "snapshot_id": "rust-docs-bundled-seed",
  "limit": 5,
  "truncated": false,
  "hits": [
    {
      "section_id": "clippy/configuration",
      "source": "clippy",
      "title": "Clippy Lint Configuration",
      "score": 2.417584623470607,
      "excerpt": "The Clippy configuration reference documents the lint groups, individual lint levels, and per-lint options. Clippy is a nightly component, so a specific lint may be added, renamed, or removed between releases; treat an unfamiliar lint as po…",
      "citations": [
        { "note": "canonical upstream document", "section_id": "clippy/configuration", "source": "clippy", "title": "Clippy Lint Configuration", "url": "https://doc.rust-lang.org/clippy/configuration.html" },
        { "note": "qualification: Clippy is a nightly component; a lint may be added or removed between releases.", "section_id": "clippy/configuration", "source": "clippy" }
      ]
    }
  ]
}
```

### 3.7 `get_section "clippy/configuration"` → the section the lint finding cites

```json
{
  "schema_version": "1",
  "section": {
    "id": "clippy/configuration",
    "source": "clippy",
    "doc_title": "Clippy Lint Configuration",
    "snapshot_id": "rust-docs-bundled-seed",
    "content_hash": "c128-ffa5381340fae3e97c387c8bd729fcbc",
    "freshness": "unknown",
    "canonical_url": "https://doc.rust-lang.org/clippy/configuration.html",
    "chapter_path": "configuration",
    "body": "The Clippy configuration reference documents the lint groups, individual lint levels, and per-lint options. Clippy is a nightly component, so a specific lint may be added, renamed, or removed between releases; treat an unfamiliar lint as possibly version-dependent.",
    "citations": [
      { "note": "canonical upstream document", "section_id": "clippy/configuration", "source": "clippy", "title": "Clippy Lint Configuration", "url": "https://doc.rust-lang.org/clippy/configuration.html" },
      { "note": "qualification: Clippy is a nightly component; a lint may be added or removed between releases.", "section_id": "clippy/configuration", "source": "clippy" }
    ]
  },
  "truncated": false
}
```

### 3.8 `get_section "style-guide/intro"` → the section the style finding cites

```json
{
  "schema_version": "1",
  "section": {
    "id": "style-guide/intro",
    "source": "style-guide",
    "doc_title": "The Rust Style Guide",
    "snapshot_id": "rust-docs-bundled-seed",
    "content_hash": "c128-ffa549d71811a6f0c1df98d4204e09dc",
    "freshness": "unknown",
    "canonical_url": "https://doc.rust-lang.org/nightly/style-guide/intro.html",
    "chapter_path": "intro",
    "body": "The Rust Style Guide captures the common style conventions of the Rust community: naming, whitespace, comments, error handling, and more. It is a style reference, not a compiler or linter, and is updated on the nightly documentation.",
    "citations": [
      { "note": "canonical upstream document", "section_id": "style-guide/intro", "source": "style-guide", "title": "The Rust Style Guide", "url": "https://doc.rust-lang.org/nightly/style-guide/intro.html" }
    ]
  },
  "truncated": false
}
```

### 3.9 `search_docs "needless_return"` → a useful empty (the coverage-limitation probe)

`needless_return` is a *specific* Clippy lint. The representative seed ships only
the `clippy/configuration` section — **not** the per-lint catalog — so a search for
a specific lint id returns **`0 hits`** with `next_steps`. This is the trial's
coverage-limitation probe (AC2): a useful empty the trial records, **not** a
fabricated lint section.

```json
{
  "snapshot_id": "rust-docs-bundled-seed",
  "limit": 5,
  "truncated": false,
  "hits": [],
  "next_steps": [
    "No documentation matched this query in the current snapshot.",
    "Try broader or fewer terms, or use the exact guideline code, command name, or lint id.",
    "Confirm the source is registered: list_sources lists the eight canonical sources.",
    "A section you expected may sit under a different heading; verify it with get_section using its exact id."
  ]
}
```

---

## 4. Findings — concrete locations + server-returned citations (AC1)

Each finding pairs a **concrete code location** — a representative review-target
snippet + a `file:line`, the agent's *own* artifact that the server **never sees
or executes** — with the **server-returned citation** that justifies it. The
citation fields below are the values the trial asserts against the retrieved
responses of §3 (see `assert_findings_cite_retrieved_sections` in the test).

| Finding | Concrete code location (review target, *not executed*) | `source` | `section_id` | `canonical_url` | `content_hash` | Guideline / lint / qualification note |
|---|---|---|---|---|---|---|
| **F-1** — conversion (C-CONV) | `src/units/meter.rs:14` — `fn to_meter(value: f32) -> Meter { Meter { value } } // C-CONV: use From/Into` | `api-guidelines` | `api-guidelines/about` | `https://rust-lang.github.io/api-guidelines/about.html` | `c128-ffa531f24bfdd996c9ef3d1a076dd56a` | `guideline code: C-CONV` (+ canonical document) |
| **F-2** — Clippy lint | `src/parser/parse.rs:37` — `fn parse(t: &[Token]) -> Result<Expr> { let e = build(t); return Ok(e); }` (a trailing `return` a Clippy lint would flag) | `clippy` | `clippy/configuration` | `https://doc.rust-lang.org/clippy/configuration.html` | `c128-ffa5381340fae3e97c387c8bd729fcbc` | `qualification: Clippy is a nightly component; a lint may be added or removed between releases.` (+ canonical document) |
| **F-3** — style | `src/config/load.rs:8` — `pub fn do_stuff() { /* load config */ } // style-guide: naming` (a public fn whose name is not a noun phrase) | `style-guide` | `style-guide/intro` | `https://doc.rust-lang.org/nightly/style-guide/intro.html` | `c128-ffa549d71811a6f0c1df98d4204e09dc` | `canonical upstream document` |

**The citation each finding records (verbatim from §3):**

| Field (per finding) | F-1 (conversion) | F-2 (Clippy lint) | F-3 (style) |
|---|---|---|---|
| `source` | `api-guidelines` | `clippy` | `style-guide` |
| `section_id` | `api-guidelines/about` | `clippy/configuration` | `style-guide/intro` |
| `canonical_url` | `https://rust-lang.github.io/api-guidelines/about.html` | `https://doc.rust-lang.org/clippy/configuration.html` | `https://doc.rust-lang.org/nightly/style-guide/intro.html` |
| `content_hash` | `c128-ffa531f24bfdd996c9ef3d1a076dd56a` | `c128-ffa5381340fae3e97c387c8bd729fcbc` | `c128-ffa549d71811a6f0c1df98d4204e09dc` |
| Guideline / lint / qualification note | `guideline code: C-CONV` | `qualification: Clippy is a nightly component; a lint may be added or removed between releases.` | `canonical upstream document` |

> **Reading (AC1 satisfied):** each finding's location is a concrete `file:line`
> + snippet, and each finding's citation is the exact `source` / `section_id` /
> `canonical_url` / `content_hash` / note the server returned for that section.
> The test asserts the finding's recorded citation *equals* the retrieved
> section's fields, so the "concrete code location **and** relevant source
> citation" pair is machine-checked, not hand-claimed.

---

## 5. Findings — observed vs unverified, and coverage limitations (AC2)

The trial makes the boundary explicit and asserts it
(`observed_vs_unverified_are_separated`, `coverage_limitation_is_recorded`):

**Observed (what the server actually returned / what a real check *would* observe,
*described* not executed):**

- **The retrieval itself.** Every finding's citation is the section the server
  returned (§3.3–§3.8): the body, `content_hash`, `canonical_url`, `freshness`
  (`unknown`), and the `citations` (the canonical document, and for F-1/F-2 the
  guideline-code / qualification note). These are asserted verbatim.
- **A real check *would* observe — described, not run.** For F-1, a review would
  observe that `to_meter` does not implement `From`/`Into` (the C-CONV
  guideline); for F-2, that a trailing `return Ok(e)` is the shape a `needless_
  return`-family lint flags; for F-3, that a public `do_stuff` is not a noun-phrase
  name. The trial *describes* these as the outcome a real `cargo clippy` / manual
  review would surface — it does **not** run them.

**Unverified assumptions (what the agent infers without running the check):**

- That a given snippet *actually* violates `C-CONV` / triggers a Clippy lint /
  breaks a style convention is an **assumption** the review agent draws from the
  retrieved guideline plus the snippet. Because the server "does not execute
  commands or observe their results automatically", **no check outcome is
  observed** in this trial; every finding is recorded with status
  *retrieved, not checked* (`CheckStatus::RetrievedNotChecked`). The trial asserts
  that no finding is recorded as an observed check result.
- The lint finding (F-2) is doubly unverified: the seed's `clippy/configuration`
  section carries the **nightly-component qualification** ("a lint may be added or
  removed between releases"), so whether a *specific* lint even exists on the
  reviewer's toolchain is a version-dependent question the trial cannot answer
  without running the local Clippy — it records the qualification, not a firing.

**Coverage limitations recorded (AC2), not asserted away:**

1. **A specific Clippy lint is not in the representative seed.** The `0 hit` for
   `needless_return` (§3.9) shows the seed ships the Clippy *configuration
   reference* (`clippy/configuration`), not the per-lint catalog. The lint finding
   therefore cites the configuration section's **qualification**, not the
   individual lint's rule body; the detailed rule lives upstream at the
   `canonical_url`, and a real `sync` of the upstream Clippy catalog would widen
   recall. The trial records the gap as a *useful empty* (with `next_steps`), not
   a fabricated section.
2. **The cited sections name but do not expand the rule.** `api-guidelines/about`
   *names* `C-CONV` ("a stable code (for example C-CONV)") but does not expand the
   `From`/`Into`/`TryFrom` rule; `clippy/configuration` documents the *lint
   groups and levels* but not any individual lint's message. The detailed rules
   are upstream at each `canonical_url`; the MCP retrieves the pointer, not the
   full rule.
3. **Freshness is `unknown`.** Every cited section's `freshness` is `unknown`
   (the representative seed has never checked upstream). The trial reports this
   verdict honestly; it does not claim the guidance is current.
4. **Version compatibility is qualified, not absolute.** The Clippy qualification
   ("a lint may be added or removed between releases") means a specific-lint
   claim is version-dependent; the agent must confirm the local Clippy version
   through its own terminal before relying on a version-specific finding.

**Not auto-verified (AC2).** The trial demonstrates that the MCP *retrieves* the
relevant evidence and *returns its citations* — it does **not** demonstrate that
any flagged snippet is actually non-compliant, nor that any check ran. Per
[§11](../../plan.md#11-technical-decisions--risks-and-unknowns) and the
server's own `instructions` (§3.1), the MCP *"provides evidence, not a
guarantee that all guidance has been followed."* The agent interprets and applies
the guidance and runs the checks; the MCP retrieves and cites. The trial asserts
the "evidence, not a guarantee" boundary verbatim and the observed/unverified
split and coverage gaps above; it does not over-claim a check result or complete
recall.

---

## 6. How to reproduce

The session is encoded as a single test that drives the same spawned-stdio call
sequence over the bundled seed and asserts the retrieved citations, the
findings-with-citations pairing (AC1), the observed-vs-unverified split and
coverage gap (AC2), and a clean disconnect — so the trial is
**reproducible, not hand-claimed**:

```sh
cargo test --test trial_review
```

It reuses `tokio` + `serde_json` (the existing
[`tests/first_client.rs`](../../tests/first_client.rs) seam), adds **no**
dependency, and runs **offline**. To dump the exact request/response transcript to
a file (and diff it against §3):

```sh
RUST_DOCS_MCP_TRIAL_TRANSCRIPT=/tmp/review_trial.json \
  cargo test --test trial_review -- \
    --exact the_review_agent_trial_retrieves_and_cites_review_evidence
jq . /tmp/review_trial.json
```

The same sequence can be driven by hand, as in
[§7](#7-hand-reproduction-of-the-trial-session), using the released
`rust-docs-mcp` binary.

---

## 7. Hand reproduction of the trial session

With a built binary, run the session by piping newline-framed JSON-RPC into
`serve` over an empty data directory (offline):

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"rust-review-agent","version":"0.0.1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"C-CONV"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_api_checklist","arguments":{"category":"Conversions and From"}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_section","arguments":{"section_id":"api-guidelines/about"}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"lint"}}}' \
  '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"get_section","arguments":{"section_id":"clippy/configuration"}}}' \
  '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_section","arguments":{"section_id":"style-guide/intro"}}}' \
  '{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"needless_return"}}}' \
  | target/release/rust-docs-mcp serve --data-dir /tmp/rust-docs-mcp-070

# then close stdin (Ctrl-D); the server self-exits with no stdout protocol traffic.
```

The responses match the transcript in §3: the `C-CONV` search and `lint` search
surface their sections with citations; the three `get_section` reads return the
cited sections with `content_hash` / `canonical_url`; the `needless_return` probe
returns `0 hits` with `next_steps`. A clean client disconnect makes the server
exit `0` with **no stdout protocol traffic** (the diagnostics go to stderr).

---

## 8. Verification

| Check | Result |
|-------|--------|
| `cargo test --test trial_review` | the trial passes — the three findings' citations asserted against the retrieved sections (AC1); the observed-vs-unverified split and the coverage-limitation `0 hit` asserted (AC2); clean disconnect |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | clean, 0 warnings |
| Network in the trial | none — the bundled snapshot loads in-memory; an empty `--data-dir` keeps the host cache untouched |
| Stdout discipline | the trial asserts every stdout line is a JSON-RPC 2.0 message; the server's stderr carries the diagnostics only |
| Command / lint execution | **none** — the trial is offline and hermetic; no Clippy or build ran. The findings' check outcomes are *described*, not observed |

> The full workspace test suite is **not** re-run for this task (it is already
> green and this change adds a self-contained trial test + a docs file; the known
> parallel-only flakiness in `ingest`/`writer_lock`/`corpus` is unrelated and
> passes in isolation). The trial's own assertions and `fmt`/`clippy` are the
> targeted checks. No behavior, dependency, MSRV, or `unsafe` change is made.

---

## 9. Files touched by this task

| File | Change |
| --- | --- |
| `docs/trials/review.md` | **Created** — this record of the Rust review agent trial: the session, the real transcript, the three findings with their concrete locations and server-returned citations, and the observed-vs-unverified separation and coverage limitations. |
| `tests/trial_review.rs` | **Created** — the focused, hermetic, offline proof of the trial: the `C-CONV` / Clippy-lint / style-guide retrieval + citations (AC1), the observed-vs-unverified split and the coverage-limitation probe (AC2), with `RUST_DOCS_MCP_TRIAL_TRANSCRIPT` transcript capture and a single `initialize`. No new dependency; no network. |
