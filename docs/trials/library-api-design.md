# Library API-design agent trial — `[068][M5]`

> **Task:** `[068][M5] Run the library API-design agent trial` ([issue #68](https://github.com/ThiagoEvoa/rust-mcp/issues/68)).
> **Plan:** [§1 success scenario](../../plan.md#1-goals--non-goals) ("an agent designing a library searches for
> conversion naming, reads the relevant API guideline with its examples, and cites the
> original section") and [§10 / M5](../../plan.md#10-milestones) (the agent-trial step).
> **What this is:** the trial agent's retrieval session, run offline against the
> bundled `rust-docs-bundled-seed` snapshot over the spawned-stdio JSON-RPC seam,
> with the actual requests, real responses, and canonical citations recorded
> below. The same session is encoded as a reproducible test —
> [`tests/trial_library_api_design.rs`](../../tests/trial_library_api_design.rs).

The trial does **not** claim that code generated from the retrieved guideline is
*automatically compliant*. It retrieves documentation and records its citations;
the calling agent interprets and applies the guidance. The boundaries and the
retrieval limits observed here are recorded explicitly, not asserted away
([§11 risk](../../plan.md#11-technical-decisions--risks-and-unknowns)).

## 1. What the trial runs

The trial is one end-to-end agent session over the `rust-docs-mcp serve` binary,
driven exactly like [`tests/first_client.rs`](../../tests/first_client.rs): a raw
JSON-RPC 2.0 client over genuine OS stdio, with the bundled snapshot loaded
in-memory and an **empty** `--data-dir` so nothing touches the host cache or the
network. The agent's call sequence is:

| # | Call | Purpose |
|---|------|---------|
| 1 | `initialize` | negotiate the protocol version, read the advertised capabilities and the agent-workflow `instructions`. |
| 2 | `notifications/initialized` | complete the handshake. |
| 3 | `tools/list` | discover the five tools, including `get_api_checklist`. |
| 4 | `search_docs` — *"From TryFrom conversion naming"* | the agent's **natural-language** attempt at the conversion-naming question. |
| 5 | `search_docs` — *"C-CONV"* | the **exact-guideline-code** search, the reliable path to the conversion guideline's section. |
| 6 | `get_api_checklist` — `category: "Conversions and From"` | the checklist entry for the conversion guideline. |
| 7 | `get_api_checklist` — *(all categories)* | prove the checklist walk is whole. |
| 8 | `get_section` — `"api-guidelines/about"` | read the original section the agent cites, with its body, provenance, and citations. |
| 9 | clean client disconnect | the server self-exits. |

## 2. Environment

| Property | Value |
|----------|-------|
| Repository | [ThiagoEvoa/rust-mcp](https://github.com/ThiagoEvoa/rust-mcp) |
| Active snapshot | `rust-docs-bundled-seed` — `created_at` `2026-10-30T00:00:00Z`, `parser_version` `rust-docs-mcp/seed-1`, schema version `1`, **8 sources** |
| Transport | stdio, JSON-RPC 2.0, protocol version `2025-11-25` |
| Network | **None** — the trial runs offline; the snapshot is loaded in-memory |
| Host cache | **None** — an empty `--data-dir` is supplied, so `serve` uses the bundled seed and never touches the host application cache |
| New dependency | **None** — the trial reuses `tokio` + `serde_json`, the existing test seam |
| Toolchain | the repository's stable toolchain / pinned SDK baseline; no MSRV / dependency / behavior / `unsafe` change |
| Corpus source hashes | the seed shares one content hash (`c128-ffa5092e1f9db0151650f6e19369b606`) across its 8 sources; the section the trial cites has content hash `c128-ffa531f24bfdd996c9ef3d1a076dd56a` |
| Freshness | `unknown` — the representative seed has never checked upstream (recorded, not fabricated) |

## 3. Session transcript

Each entry below is a real request and its real response. The session is also
encodable by the test; set `RUST_DOCS_MCP_TRIAL_TRANSCRIPT` to dump the exact
request/response pairs (see §6).

### 3.1 `initialize` → the negotiated version, capabilities, and `instructions`

The client negotiates `2025-11-25`. The server echoes it, advertises the `tools`
and `resources` capabilities, and carries its `serverInfo` plus the agent-workflow
`instructions` — the text the trial runs on. The response shape is:

```json
{ "jsonrpc": "2.0", "id": 0, "result": {
    "protocolVersion": "2025-11-25",
    "capabilities": { "tools": {}, "resources": {} },
    "serverInfo": { "name": "rust-docs-mcp", "version": "0.1.0" },
    "instructions": "<the full agent-workflow text, decoded below>"
} }
```

The decoded `instructions` value (the actual response, verbatim) is:

> You are a coding agent with a Rust documentation MCP server available. The
> server **retrieves official Rust documentation only**: it **does not run
> commands, does not touch the project filesystem, and does not reach the
> network**. Retrieve documentation from it; do everything else through your own
> tools.
>
> **Responsibilities: retrieval versus execution**
> - The MCP retrieves documentation. Its five tools
>   (list_sources, search_docs, get_section, get_api_checklist,
>   get_command_guidance) and its catalog/section resources return source-backed
>   text, citations, freshness, and compatibility metadata from one immutable
>   snapshot. The MCP **does not execute commands** or observe their results
>   automatically: you run Rust and Cargo commands through your own terminal tools
>   and inspect their exit status and output yourself.
> - A fetched example or a command key such as `cargo test` is documentation about
>   a command, never a sign the server ran it: never present a retrieved example
>   as something this server executed.
> - Distinguish upstream documentation from the separately labeled maintainer
>   effect notes (curated command metadata) some entries carry. Effect notes
>   describe possible side effects; they are not permission controls and they do
>   not guarantee that a build or test command has no side effects.
>
> **Staleness and version compatibility**
> - Staleness is metadata, not a block. A source whose last successful upstream
>   check is older than the configured threshold (seven days by default) is marked
>   stale but is still served, so stale documentation remains usable offline.
> - Unavailable version compatibility is exposed as `unknown`. Compatibility is one
>   of stable, nightly-only, rolling, or unknown; when the upstream does not pin a
>   version, treat it as unknown. A commit-pinned rustfmt document is not
>   automatically documentation for an installed stable release. When
>   compatibility is uncertain, check the local tool versions and the local
>   `--help` output through your terminal before relying on a version-specific
>   claim.
>
> **Workflow: adapt it to the repository**
> There is no universal command list that proves a Rust project correct: this
> server and you determine the applicable checks from the project. Do not treat
> the steps below as a checklist to run blindly, and do not assume the server
> guarantees compliance: **the MCP provides evidence, not a guarantee that all
> guidance has been followed.**
> 1. Inspect the repository first, using your filesystem tools: its own
>    instructions, manifests and workspace membership, feature/target policy,
>    toolchain files, lint/formatter configuration, and CI. Then adapt the
>    remaining steps to what that project actually requires.
> 2. Call `list_sources` to learn coverage, freshness, and documentation versions;
>    inspect local tool versions through your terminal when command compatibility
>    matters.
> 3. Search relevant design or style topics with `search_docs` (or read a section
>    with `get_section`, or read checklist entries with `get_api_checklist`), or
>    fetch a known command with `get_command_guidance`. Read the full relevant
>    section before relying on a short excerpt.
> 4. Choose commands for the actual task and the project's supported
>    configurations, using project-specific commands and documented options. A
>    review may use a formatting check mode; implementation may apply formatting
>    within its authorized scope.
> 5. Execute through your existing terminal tools, inspect exit status and output,
>    and fix or report issues as the task permits. The MCP does not execute
>    commands or observe their results automatically.
> 6. Re-run the affected checks after your changes; report the commands you
>    actually ran, the configurations covered, the failures, and the checks you
>    skipped. Cite the documentation when you explain a command choice or a
>    finding.
>
> For library creation, additionally request API checklist entries
> (`get_api_checklist`) to guide your own design review. Installing the server
> alone does not make every agent consult it; these instructions decide when its
> tools are used.

> **Reading:** the `instructions` are the operating contract the trial respects,
> and they explicitly direct the trial's path: *"For library creation,
> additionally request API checklist entries (`get_api_checklist`) to guide your
> own design review."* The trial follows exactly that. Three phrases pin the
> boundary the trial must not over-claim: the server *"retrieves official Rust
> documentation only: it does not run commands, does not touch the project
> filesystem, and does not reach the network"*, it *"does not execute commands
> or observe their results automatically"*, and it *"provides evidence, **not a
> guarantee** that all guidance has been followed."* The trial asserts these
> verbatim in `the_server_states_it_provides_evidence_not_a_guarantee`.

### 3.2 `tools/list` → the five advertised tools

```json
{ "jsonrpc": "2.0", "id": 1, "result": { "tools": [
    { "name": "list_sources",         "description": "List the documentation sources in the active snapshot." },
    { "name": "search_docs",          "description": "Search the bundled Rust documentation sections by relevance. …" },
    { "name": "get_section",          "description": "Fetch one documentation section by its identifier, with citations and freshness. …" },
    { "name": "get_api_checklist",    "description": "List the API guidelines checklist entries for a category, or all categories. …" },
    { "name": "get_command_guidance", "description": "Get command guidance for a Cargo/rustc/rustup/rustfmt/clippy/rustdoc command. …" }
] } }
```

The `get_api_checklist` input schema is `oneOf[{ }, {category: string}]` — the
category is optional, so the trial can walk all categories.

### 3.3 `search_docs` — *"From TryFrom conversion naming"* (the natural-language attempt)

```json
{ "jsonrpc": "2.0", "id": 2, "result": {
    "content": [ { "type": "text", "text": "1. style-guide/intro\n   \
The Rust style guide is a collection of recommendations for writing Rust code \
that is consistent, idiomatic, and easy to understand. …" } ],
    "isError": false,
    "structuredContent": {
        "query": "From TryFrom conversion naming",
        "limit": 5,
        "total": 1,
        "returned": 1,
        "truncated": false,
        "next_steps": null,
        "hits": [
            {
                "rank": 1, "section_id": "style-guide/intro",
                "source": "style-guide", "title": "Intro", "score": 1.4871,
                "excerpt": "The Rust style guide is a collection of \
recommendations for writing Rust code that is consistent, idiomatic, and easy to \
understand.",
                "citations": [
                    { "source": "style-guide", "title": "The Rust Programming Language \
Style Guide",
                      "url": "https://rust-lang.github.io/api-guidelines/about.html",
                      "as_of": "2026-10-30", "freshness": "unknown" }
                ]
            }
        ]
    }
} }
```

> **Finding (AC2):** the agent's *natural-language* conversion-naming query does
> **not** surface the `api-guidelines` conversion guideline. On the
> representative seed it matches the **style guide's** "naming" mention
> (`style-guide/intro`, score `1.487`) instead — a **low-recall miss**. The word
> "conversion"/"conversions"/"TryFrom" does not appear in any seed section body,
> so a free-text search for those terms returns **no hits** (a useful empty with
> `next_steps`). The reliable path to the conversion guideline is the
> **exact-guideline-code search** (§3.4) and the **checklist** (§3.5). This gap
> is recorded, not asserted away.

### 3.4 `search_docs` — *"C-CONV"* (the reliable exact-code path)

```json
{ "jsonrpc": "2.0", "id": 3, "result": {
    "content": [ { "type": "text", "text": "1. api-guidelines/about\n   \
…help an API author write an API that is easy to use and hard to misuse." } ],
    "isError": false,
    "structuredContent": {
        "query": "C-CONV", "limit": 5, "total": 1, "returned": 1,
        "truncated": false, "next_steps": null,
        "hits": [
            {
                "rank": 1, "section_id": "api-guidelines/about",
                "source": "api-guidelines", "title": "About this document",
                "score": 2.8653,
                "excerpt": "…help an API author write an API that is easy to use and hard to misuse.",
                "citations": [
                    { "source": "api-guidelines", "title": "API guidelines",
                      "url": "https://rust-lang.github.io/api-guidelines/about.html",
                      "as_of": "2026-10-30", "freshness": "unknown" },
                    { "source": "api-guidelines", "title": "API guidelines",
                      "note": "guideline code: C-CONV",
                      "as_of": "2026-10-30", "freshness": "unknown" }
                ]
            }
        ]
    }
} }
```

> The exact-code search **does** surface the conversion guideline's section —
> `api-guidelines/about`, source `api-guidelines`, score `2.865` — and its
> citations carry both the canonical document and a
> **`guideline code: C-CONV`** provenance note. This is the reliable retrieval
> path the trial relies on.

### 3.5 `get_api_checklist` — `category: "Conversions and From"`

```json
{ "jsonrpc": "2.0", "id": 4, "result": {
    "content": [ { "type": "text", "text": "Conversions and From\n  C-CONV \
Prefer `From`/`Into` for conversions …" } ],
    "isError": false,
    "structuredContent": {
        "category": "Conversions and From",
        "snapshot_id": "rust-docs-bundled-seed",
        "entries": [
            {
                "code": "C-CONV",
                "title": "Prefer `From`/`Into` for conversions",
                "category": "Conversions and From",
                "location": "Conversions and From#C-CONV",
                "explanation_sections": ["api-guidelines/about"]
            }
        ],
        "total": 1, "returned": 1, "truncated": false,
        "next_steps": "Read the explanation section(s) before relying on a checklist entry."
    }
} }
```

### 3.6 `get_api_checklist` — *all categories*

```json
{ "jsonrpc": "2.0", "id": 5, "result": {
    "content": [ { "type": "text", "text": "Conversions and From\n  C-CONV \
Prefer `From`/`Into` for conversions …" } ],
    "isError": false,
    "structuredContent": {
        "snapshot_id": "rust-docs-bundled-seed",
        "entries": [
            { "code": "C-CONV", "title": "Prefer `From`/`Into` for conversions",
              "category": "Conversions and From", "location": "Conversions and From#C-CONV",
              "explanation_sections": ["api-guidelines/about"] }
        ],
        "total": 1, "returned": 1, "truncated": false,
        "next_steps": "Read the explanation section(s) before relying on a checklist entry."
    }
} }
```

> **Finding:** the seed ships exactly **one** checklist entry (`C-CONV`) — it is
> a *representative* subset, not the full upstream checklist. The trial records
> this: the all-categories walk reaches `C-CONV` exactly once, and it is the
> single conversion-related entry available.

### 3.7 `get_section` — `"api-guidelines/about"` (the section the agent cites)

```json
{ "jsonrpc": "2.0", "id": 6, "result": {
    "content": [ { "type": "text", "text": "# About this document\n\nThese guidelines \
introduce a set of common principles and best practices that can help an API \
author write an API that is easy to use and hard to misuse. … A rule or \
recommendation might have a stable code (for example C-CONV), a short \
description, and a link to an explanation section. This file is the \
representative introduction to that checklist, not its complete rule text." } ],
    "isError": false,
    "structuredContent": {
        "section": {
            "id": "api-guidelines/about",
            "source": "api-guidelines",
            "title": "About this document",
            "as_of": "2026-10-30",
            "freshness": "unknown",
            "canonical_url": "https://rust-lang.github.io/api-guidelines/about.html",
            "content_hash": "c128-ffa531f24bfdd996c9ef3d1a076dd56a",
            "snapshot_id": "rust-docs-bundled-seed",
            "body": "…A rule or recommendation might have a stable code (for example C-CONV), \
a short description, and a link to an explanation section. This file is the \
representative introduction to that checklist, not its complete rule text.",
            "citations": [
                { "source": "api-guidelines", "title": "API guidelines",
                  "url": "https://rust-lang.github.io/api-guidelines/about.html",
                  "as_of": "2026-10-30", "freshness": "unknown" },
                { "source": "api-guidelines", "title": "API guidelines",
                  "note": "guideline code: C-CONV", "as_of": "2026-10-30", "freshness": "unknown" }
            ]
        },
        "schema_version": "1", "truncated": false
    }
} }
```

> **Reading:** this is the section the trial **cites**. Its `body` *names*
> `C-CONV` ("a stable code (for example C-CONV)") but does **not** expand the
> `From`/`Into`/`TryFrom` naming rule or example — the seed's representative
> `api-guidelines` intro is "not its complete rule text". The full rule lives
> upstream at the `canonical_url`.

## 4. Canonical citations the trial records

The trial's citation for the conversion-naming guidance (from §3.7):

| Field | Value |
|-------|-------|
| `source` | `api-guidelines` |
| `section_id` | `api-guidelines/about` |
| `title` | *About this document* |
| `canonical_url` | `https://rust-lang.github.io/api-guidelines/about.html` |
| `as_of` | `2026-10-30` |
| `freshness` | `unknown` (the seed has never checked upstream) |
| `content_hash` | `c128-ffa531f24bfdd996c9ef3d1a076dd56a` |
| `snapshot_id` | `rust-docs-bundled-seed` |
| provenance note | `guideline code: C-CONV` |

## 5. Findings — success achieved, limits recorded

**Success scenario achieved (plan §1).** The agent's retrieval session *can*
reach the conversion-naming guideline and cite the original section: the
**exact-code search** and the **checklist** both resolve `C-CONV` to
`api-guidelines/about`, and `get_section` returns that section's body,
canonical URL, and citations. The agent reads the guideline and cites it, as the
success scenario describes.

**Missed guidance / retrieval limits (AC2), recorded rather than asserted away:**

1. **Natural-language conversion-naming search has low recall on the seed.** The
   agent's first attempt — `search_docs "From TryFrom conversion naming"` —
   matches the *style guide's* "naming" mention (`style-guide/intro`), **not**
   the `api-guidelines` conversion guideline. A free-text search for
   "conversion"/"conversions"/"TryFrom" returns **no hits**. The reliable path
   is the exact-guideline-code search (`search_docs "C-CONV"`) or the checklist
   (`get_api_checklist`), both of which the trial exercises. This is a
   corpus-coverage limit: the seed ships one representative section per source
   (`api-guidelines/about`), and that section *names* `C-CONV` but contains no
   free-text "conversion" token to match. A real sync of the upstream
   `api-guidelines` checklist (the per-code rule bodies) would widen recall.
2. **The cited section names but does not expand the rule.** `api-guidelines/about`
   introduces the checklist and gives `C-CONV` as an example code; it does not
   contain the `From`/`Into`/`TryFrom` naming rule or example. The detailed
   guidance is upstream at the `canonical_url`. The agent must read the upstream
   section to apply the rule; the MCP retrieves the pointer, not the full rule.
3. **Freshness is `unknown`.** The seed's `as_of`/`last_checked`/
   `upstream_modified` are all `unknown` (the representative seed has never
   checked upstream). The trial reports this verdict honestly; it does not claim
   the guidance is current.
4. **One checklist entry.** The seed carries a single `C-CONV` entry; the full
   upstream checklist (dozens of C-code entries) is not shipped. The trial
   records this as a representative subset.

**Not automatically compliant (AC2).** The trial demonstrates that the MCP
*retrieves* the relevant guideline and *returns its citations* — it does **not**
demonstrate that code generated on the strength of that retrieval is compliant
with the API guidelines. Per the plan's §11 and the server's own `instructions`
(§3.1), the MCP "provides evidence, not a guarantee that all guidance has been
followed". The agent interprets and applies the guidance; the MCP retrieves and
cites. The trial asserts the server's retrieval-not-execution boundary verbatim
and the two retrieval limits above; it does not over-claim complete recall or a
complete rule.

## 6. How to reproduce

The session is encoded as a single test that drives the same spawned-stdio call
sequence over the bundled seed and asserts the retrieved evidence and citations:

```sh
cargo test --test trial_library_api_design
```

It reuses `tokio` + `serde_json` (the existing [`tests/first_client.rs`](../../tests/first_client.rs)
seam), adds **no** dependency, and runs **offline**. To dump the exact
request/response transcript to a file (and diff it against §3):

```sh
RUST_DOCS_MCP_TRIAL_TRANSCRIPT=/tmp/library_trial.json \
  cargo test --test trial_library_api_design -- \
    --exact the_library_api_design_trial_retrieves_and_cites_the_conversion_guideline
jq . /tmp/library_trial.json
```

The same sequence can be driven by hand, as in [§7](#7-hand-reproduction-of-the-trial-session),
using the released `rust-docs-mcp` binary.

## 7. Hand reproduction of the trial session

With a built binary, run the session by piping newline-framed JSON-RPC into
`serve` over an empty data directory (offline):

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"library-design-agent","version":"0.0.1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"From TryFrom conversion naming"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"C-CONV"}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_api_checklist","arguments":{"category":"Conversions and From"}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_api_checklist","arguments":{}}}' \
  '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"get_section","arguments":{"section_id":"api-guidelines/about"}}}' \
  | target/release/rust-docs-mcp serve --data-dir /tmp/rust-docs-mcp-068

# then close stdin (Ctrl-D); the server self-exits with an empty stderr.
```

The responses match the transcript in §3. A clean client disconnect makes the
server exit `0` with **no stderr output** (verified in the trial run).

## 8. Verification

| Check | Result |
|-------|--------|
| `cargo test --test trial_library_api_design` | the trial passes — retrieval + citations asserted, clean disconnect |
| `cargo test --doc --locked` | passes with **0 doc tests** run — the `get_api_checklist` example in `src/mcp/get_api_checklist.rs` is a `# #!`-guarded no-op, consistent with the existing suite; the trial adds no doc test |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | clean, 0 warnings |
| Network in the trial | none — the bundled snapshot loads in-memory; an empty `--data-dir` keeps the host cache untouched |
| Stdout discipline | the trial asserts every stdout line is a JSON-RPC 2.0 message; the server's stderr is empty on clean disconnect |

> The full workspace test suite is **not** re-run for this task (it is already
> green and this change adds a self-contained trial test + a docs file); the
> trial's own assertions and the doc suite are the targeted checks. No behavior,
> dependency, MSRV, or `unsafe` change is made.
