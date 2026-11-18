# First MCP client configuration

**Task:** [064][M5] — *Document and verify the first MCP client configuration*
(issue [#64](https://github.com/ThiagoEvoa/rust-mcp/issues/64)).
**Status:** **Documented and verified.** A generic stdio configuration and **one
tested configuration** for the chosen first client are recorded here, together
with the real request/response transcript that proves the three operations
(list sources, search, retrieve a section with a citation).
**Verified:** 2026-11-13, on `rustc`/`cargo` 1.99.1 (stable
`aarch64-apple-darwin`), MSRV 1.88, edition 2024 — the
[baseline](COMPATIBILITY.md#2-rust-toolchain-baseline) of
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md).

> **Outcome (issue #64):** provide a generic stdio example and one tested
> configuration for the chosen first client.
>
> **Sources of truth:** [`plan.md`](../plan.md) §7 (*agent workflow and client
> integration*) and §10/M5, and the M4 client + protocol matrix recorded in
> [`docs/COMPATIBILITY.md`](COMPATIBILITY.md) §10.
>
> **Acceptance criteria (both met):**
> - **AC1** — the configuration uses the **correct absolute executable path** and
>   the `serve` arguments (§2, §3.1).
> - **AC2** — the documented client **lists sources, searches, and retrieves a
>   section with a citation** (§3, with the real transcript in §3.3).

---

## 1. The chosen first client

Per [`plan.md`](../plan.md) §7 — *"No client-specific integration is required to
begin development; the SDK test client establishes the initial contract"* — and
the M4 record in [`docs/COMPATIBILITY.md`](COMPATIBILITY.md) §10.3, the **first
client is the raw JSON-RPC 2.0 client over stdio**: a hand-written client that
speaks the newline-delimited JSON-RPC 2.0 protocol the `serve` command is wired
to, with **no `rmcp` client** (the `rmcp` `client` feature stays off) and **no
new dependency**. It is the as-built M4 first client that
[`tests/stdio_mcp.rs`](../tests/stdio_mcp.rs) also exercises.

Why this client, and why it is a sound "first client":

- **No new dependency.** Enabling `rmcp`'s `client` feature would add the client
  half of the SDK to the server's build. The raw client keeps the closure at
  `server` + `macros` + `transport-io` only — the no-new-dependency posture that
  governs the project.
- **Hermetic and authoritative.** It drives the *production* stdio path — the
  same bytes and the same process boundary a real external agent hits — rather
  than an in-memory stream. As `src/mcp.rs` records, the spawned-binary check is
  "the more authoritative check anyway."
- **Both protocol families.** It handles the `initialize`-capable versions
  (`2024-11-05 … 2025-11-25`, negotiated to `2025-11-25`) directly and the
  `2026-07-28` discovery/error-upgrade path by down-negotiation (the client asks
  for `2025-11-25`, the highest `initialize`-capable version).

Real-world / external clients (a coding agent, the MCP Inspector, a non-`rmcp`
implementation) remain **STILL TO TEST** — see §6, carrying forward
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md) §10.5.

---

## 2. AC1 — the generic stdio client configuration

Any MCP client launches the server by its **absolute executable path** with the
`serve` argument, in the standard `mcpServers` JSON shape:

```json
{
  "mcpServers": {
    "rust-docs": {
      "command": "<ABSOLUTE PATH to the rust-docs-mcp binary>",
      "args": ["serve"]
    }
  }
}
```

**`command` — the correct absolute executable path.** `command` must be an
**absolute** path to the `rust-docs-mcp` binary. Most MCP clients resolve
`command` against *their own* working directory (or not at all), so a relative
path is unreliable; an absolute path is the documented contract. Obtain it from
the [install walkthrough](../README.md#install):

- After `cargo build --release`, the binary is
  `target/release/rust-docs-mcp` — resolve it to an absolute path, e.g.
  `$(pwd)/target/release/rust-docs-mcp`.
- The README uses `cargo install --git`, which normally installs to
  `~/.cargo/bin/rust-docs-mcp` (`%USERPROFILE%\.cargo\bin\rust-docs-mcp.exe`
  on Windows). Run `command -v rust-docs-mcp` on macOS/Linux or
  `(Get-Command rust-docs-mcp).Source` in Windows PowerShell to obtain the
  actual absolute path.

**`args` — the `serve` arguments.** The required argument is `serve`, which binds
the process's standard input/output as the MCP transport and runs until the
client disconnects. Two **optional** arguments may be appended:

| Argument | Effect |
| --- | --- |
| `--data-dir PATH` | Use `PATH` as the application data directory instead of the OS default (`$XDG_DATA_HOME/rust-docs-mcp`, falling back to `$HOME/.local/share/rust-docs-mcp` on Unix; `%APPDATA%\rust-docs-mcp` on Windows). Useful for portable installs and tests. |
| `--verbose` / `-v` | Raise diagnostics to `debug` level (or set `RUST_LOG`). Diagnostics always go to **stderr**; this flag never writes query text to stdout. |

For example, with both optional arguments:

```json
{
  "mcpServers": {
    "rust-docs": {
      "command": "/home/you/.local/bin/rust-docs-mcp",
      "args": ["serve", "--data-dir", "/var/lib/rust-docs-mcp", "--verbose"]
    }
  }
}
```

**Stdio behaviour (why the config is enough).** During `serve`, **stdout carries
only MCP protocol traffic** and **diagnostics go to stderr**, so a client can
parse stdout unambiguously. `serve` makes **no network calls** — it serves the
local published snapshot, or the bundled snapshot embedded in the binary as the
first-launch fallback, so a fresh install works offline. **No client-specific
integration is required** to use the server; this generic absolute-path
configuration is the documented contract, and the `initialize` handshake is the
runtime check that it works.

---

## 3. AC2 — one tested configuration for the first client

### 3.1 The tested configuration

The one configuration tested here is the generic block of §2 with the **absolute
path filled in** and a **hermetic `--data-dir`** added so the run never touches
the host's real application cache:

```json
{
  "mcpServers": {
    "rust-docs": {
      "command": "/Users/thiagoevoa/Projects/rust-mcp/target/release/rust-docs-mcp",
      "args": ["serve", "--data-dir", "/tmp/rust-docs-mcp-client-demo"]
    }
  }
}
```

The raw-JSON-RPC "first client" performs exactly this launch — the `command`
binary with `args` `["serve", "--data-dir", <temp>]` — and then speaks the
JSON-RPC 2.0 sequence over its stdin/stdout. The `--data-dir` is a hermetic
addition to the generic `["serve"]`; without it the server falls back to the OS
default data directory (or the bundled snapshot), which is the same offline
behaviour. (The automated test in §4 drives the cargo-built binary for the test
target with the identical `serve` arguments plus a temp `--data-dir`; the
absolute `command` in the block above is the release binary on the verification
machine.)

### 3.2 The session — the three operations

After the standard `initialize` → `notifications/initialized` handshake, the
client issues the three tool calls that make up AC2. The request frames the
client actually writes are:

```jsonc
// (0) handshake — the server echoes the negotiated version + its identity
{"jsonrpc":"2.0","id":0,"method":"initialize",
 "params":{"protocolVersion":"2025-11-25","capabilities":{},
           "clientInfo":{"name":"first-client","version":"0.0.1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}

// (1) AC2(1) — list the sources
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}
// (2) AC2(2) — search (a query that returns hits; the top hit is the C-CONV rule)
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"C-CONV"}}}
// (3) AC2(3) — retrieve the section the search returns (with its citation/provenance).
//     The id is the *top "C-CONV" hit* the search returns — the ad-hoc-conversions
//     naming rule — not the bare `api-guidelines/about` (that bare id no longer
//     exists in the hierarchical snapshot, so `get_section` returns the not-found
//     error — `-32002` at the negotiated `2025-11-25` — proven by
//     `tests/stdio_mcp.rs`; see also §3.3 (3)).
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_section","arguments":{"section_id":"api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv"}}}
```

The client matches each response to its request by `id` (the server may answer
out of order — a real JSON-RPC client must tolerate that, as
[`tests/stdio_mcp.rs`](../tests/stdio_mcp.rs) and the driver in §4 both do).

### 3.3 The real captured transcript

The three blocks below are the **`structuredContent` view** of each response —
the machine-readable payload under `result.structuredContent` (re-captured for the
current seed, 2026-11-18, reproduced by `tests/first_client.rs`, §4). The `result`
envelope's `content` (human-readable text) and `isError` fields are omitted, and the
`structuredContent` object is shown directly. For `get_section` that object nests
the section under a `section` key, with the `schema_version` and `truncated`
fields as its siblings. **Distinguishing the two kinds of material in this section:**
the request *frames* in §3.2 and the one-liner in §4 are **stable machine
operations** (paste them and they resolve against the current seed); the blocks
here are a **captured example transcript** of one real session, re-captured when the
seed corpus changed, and are not themselves a copy-paste contract.

**(1) `list_sources`** — returns the **eight canonical sources** and the active
**snapshot id** (`rust-docs-bundled-seed`), the 29-key **command catalog**, and a
**`corpus_hash`** computed over the serialized manifest (there is no such field in
the manifest itself — `list_sources`/`catalog` compute it; see
[`docs/REGENERATION.md`](REGENERATION.md) §2.2). The `sources` array carries the
per-source `content_hash` (now **source-specific**), `document_count`,
`section_count`, `channel`, `parser`, and `entry_url`:

```json
{
  "command_catalog_uri": "rust-docs://snapshots/rust-docs-bundled-seed/commands",
  "corpus_hash": "c128-ffaf654d6d447eb40b9e9629be845a92",
  "schema_version": "1",
  "snapshot_id": "rust-docs-bundled-seed",
  "commands": [
    "cargo new", "cargo init", "cargo metadata", "cargo locate-project",
    "cargo tree", "cargo add", "cargo remove", "cargo update",
    "… 29 command-catalog keys in total …",
    "cargo build", "cargo test", "rustup show", "rustup toolchain",
    "rustc", "rustfmt", "rustdoc"
  ],
  "sources": [
    { "id": "api-guidelines", "title": "Rust API Guidelines", "channel": "rolling", "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "document_count": 13, "section_count": 96, "content_hash": "c128-ffa6caa702a86473fe801b0bcb02d958", "parser": "rust-docs-mcp/seed-2", "entry_url": "https://rust-lang.github.io/api-guidelines/about.html" },
    { "id": "style-guide", "title": "Rust Style Guide", "channel": "stable", "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "document_count": 7, "section_count": 71, "content_hash": "c128-ffa596f39b95d538268ea3a933acba9d", "parser": "rust-docs-mcp/seed-2", "entry_url": "https://doc.rust-lang.org/style-guide/index.html" },
    { "id": "cargo", "title": "The Cargo Book", "channel": "stable", "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "document_count": 21, "section_count": 315, "content_hash": "c128-ffa6d93dcb18fde78b2ef7926b4861b6", "parser": "rust-docs-mcp/seed-2", "entry_url": "https://doc.rust-lang.org/cargo/" },
    { "id": "rustc", "title": "The rustc Book", "channel": "stable", "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "document_count": 1, "section_count": 46, "content_hash": "c128-ffa55d2e9ca927751cfbcc7794137903", "parser": "rust-docs-mcp/seed-2", "entry_url": "https://doc.rust-lang.org/rustc/" },
    { "id": "rustup", "title": "The rustup Book", "channel": "rolling", "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "document_count": 7, "section_count": 26, "content_hash": "c128-ffa5ff983cff5cbc92f95a7ce7f3c250", "parser": "rust-docs-mcp/seed-2", "entry_url": "https://rust-lang.github.io/rustup/" },
    { "id": "rustfmt", "title": "rustfmt reference", "channel": "stable", "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "document_count": 2, "section_count": 277, "content_hash": "c128-ffa5483e8818922ce73e4a6f383a9380", "parser": "rust-docs-mcp/seed-2", "entry_url": "https://github.com/rust-lang/rustfmt#readme" },
    { "id": "clippy", "title": "Clippy", "channel": "stable", "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "document_count": 1, "section_count": 9, "content_hash": "c128-ffa5067277597fa1db7e098805bd3ead", "parser": "rust-docs-mcp/seed-2", "entry_url": "https://doc.rust-lang.org/clippy/" },
    { "id": "rustdoc", "title": "The Rustdoc Book", "channel": "stable", "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "document_count": 1, "section_count": 37, "content_hash": "c128-ffa64cec68c7765563660336e199955c", "parser": "rust-docs-mcp/seed-2", "entry_url": "https://doc.rust-lang.org/rustdoc/" }
  ]
}
```

(The `commands` array is elided in the middle for readability — it carries all
**29** command-catalog keys; the `entry_url` for `rustfmt` points at its approved
raw-Markdown source at the pinned revision, per the rustfmt adapter. Per-source
object fields are also shown in a readable order; the real `structuredContent`
keys them alphabetically.)

**(2) `search_docs` for `"C-CONV"`** — returns a **ranked** set of hits; the
**top hit** (shown below) is the **C-CONV naming rule** itself, and it carries a
**`citations`** array (each citation names its `source`). The query returns **3**
hits total (`limit` 5, `truncated` false); the client reads the top hit:

```json
{
  "snapshot_id": "rust-docs-bundled-seed",
  "limit": 5,
  "truncated": false,
  "hits": [
    {
      "section_id": "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv",
      "source": "api-guidelines",
      "title": "Rust API Guidelines",
      "score": 4.887109854836241,
      "excerpt": "## Ad-hoc conversions follow as_, to_, into_ conventions (C-CONV) Conversions should be provided as methods, with names prefixed as follows: | Prefix | Cost | Ownership | |---|---|---| | as_ | Free | borrowed -> borrowed | | to_ | Expensi…",
      "citations": [
        { "source": "api-guidelines", "section_id": "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv", "title": "Rust API Guidelines", "url": "https://rust-lang.github.io/api-guidelines/naming.html#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv", "note": "canonical upstream document" },
        { "source": "api-guidelines", "section_id": "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv", "note": "guideline code: C-CONV" }
      ]
    },
    "… 2 more hits elided for readability (see the full transcript via RUST_DOCS_MCP_TRANSCRIPT, §4) …"
  ]
}
```

**(3) `get_section` for the top hit, `"api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv"`** —
returns that **exact** section **with its provenance**: the body, its
`canonical_url`, its `heading_path`, and a **`citations`** array. This is the
section the search returned, so the search-to-section step is a **true chain**
(search returns the id → the client passes that same id to `get_section`):

```json
{
  "schema_version": "1",
  "section": {
    "id": "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv",
    "source": "api-guidelines",
    "doc_title": "Rust API Guidelines",
    "snapshot_id": "rust-docs-bundled-seed",
    "content_hash": "c128-ffa51d275f6a20c3c6d4a02b1a369f52",
    "freshness": "unknown",
    "canonical_url": "https://rust-lang.github.io/api-guidelines/naming.html#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv",
    "chapter_path": "naming",
    "heading_path": ["Naming", "Ad-hoc conversions follow as_, to_, into_ conventions (C-CONV)"],
    "body": "## Ad-hoc conversions follow as_, to_, into_ conventions (C-CONV)\n\nConversions should be provided as methods, with names prefixed as follows:\n\n| Prefix | Cost | Ownership |\n|---|---|---|\n| as_ | Free | borrowed -> borrowed |\n… (full body; the as_/to_/into_ prefix table and its standard-library examples) …",
    "citations": [
      { "source": "api-guidelines", "section_id": "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv", "title": "Rust API Guidelines", "url": "https://rust-lang.github.io/api-guidelines/naming.html#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv", "note": "canonical upstream document" },
      { "source": "api-guidelines", "section_id": "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv", "note": "guideline code: C-CONV" }
    ]
  },
  "truncated": false
}
```

(The `body` is elided in the middle for readability; the full body — the
`as_`/`to_`/`into_` prefix table and its standard-library examples — is served
in full. The two citations are the canonical upstream document and the
`guideline code: C-CONV` marker that ties the section to the `C-CONV` checklist
entry.)

Together these satisfy AC2 end to end: the client **lists the sources**
(`list_sources`, eight sources + the snapshot id + the 29 command-catalog keys),
**searches** (`search_docs`, a query that returns cited hits — the top hit is the
C-CONV naming rule), and **retrieves a section with a citation** (`get_section`,
the section body plus its `canonical_url` and `citations`). The `search_docs` top
hit and the `get_section` result are the *same* section — the hit's id is exactly
what the client passes to `get_section` — so the search-to-section workflow the
M5 exit requires works over the tested configuration.

---

## 4. Reproduce the evidence

The automated, hermetic proof of exactly these three operations is
**[`tests/first_client.rs`](../tests/first_client.rs)**
(`first_client_configuration_lists_searches_and_retrieves_with_citations`). It
spawns the real `rust-docs-mcp serve` binary over OS stdio, completes the
`initialize` handshake, and asserts all three AC2 operations (eight sources +
the snapshot id + the 29 command-catalog keys; a `C-CONV` search whose **top hit**
carries a non-empty citation array; and that top hit —
`api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv` —
retrieved with a non-empty citation array and a `canonical_url`), then a clean
self-exit. It uses only the existing `tokio` and
`serde_json` dependencies — **no new dependency, no network** — and a per-test
temp `--data-dir`, so it is offline.

Run it, and capture the request/response transcript that §3.3 reproduces:

```sh
# build + run the focused first-client proof
cargo test --test first_client -- --exact \
  first_client_configuration_lists_searches_and_retrieves_with_citations

# regenerate the §3.3 transcript to a file (the test writes it when the
# RUST_DOCS_MCP_TRANSCRIPT variable names a path), then pretty-print it:
RUST_DOCS_MCP_TRANSCRIPT=/tmp/first_client_transcript.json \
  cargo test --test first_client -- --exact \
    first_client_configuration_lists_searches_and_retrieves_with_citations
jq . /tmp/first_client_transcript.json
```

The full-surface automated proof — **all five tools and both resources**, plus
stdout protocol integrity and a clean disconnect — remains
**[`tests/stdio_mcp.rs`](../tests/stdio_mcp.rs)**
(`serve_exposes_the_five_tools_and_two_resources_over_stdio`);
`tests/first_client.rs` is the focused, transcript-capturing proof of exactly
the three AC2 operations.

**Minimal raw-client one-liner** (the same session, using the absolute path of
§3.1; responses must be matched by `id`, as a real JSON-RPC client does):

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"demo","version":"0.0.1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"C-CONV"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_section","arguments":{"section_id":"api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv"}}}' \
  | /Users/thiagoevoa/Projects/rust-mcp/target/release/rust-docs-mcp serve --data-dir /tmp/rust-docs-mcp-client-demo
```

---

## 5. Using the generic configuration in a real client

The generic block of §2 is what a real MCP client (a coding agent, Claude
Desktop, the [MCP Inspector](https://github.com/modelcontextprotocol/inspector),
etc.) is given: paste it, substituting the **absolute path** to *your* installed
`rust-docs-mcp` binary, and the client performs the same
`initialize` → tool-call sequence over stdio. No client-specific integration is
required; the server needs no network, LLM, API key, or telemetry to start.

---

## 6. Real-world clients — the tested external SDK and the remaining gap

The **verified, test-backed** compatibility claim stays scoped to **the SDK's own
`serve` server + a raw JSON-RPC 2.0 client over stdio, across all five advertised
protocol versions, exercising all five tools and both resources** (the as-built
first client of §1/§3, asserted by `tests/first_client.rs` and
`tests/stdio_mcp.rs`). That is the only client whose behavior a test in this
repository asserts.

**One external-SDK audit is recorded.** On **2026-11-16**, against the then-current
`main` (`6a7880b`; macOS arm64; `rustc`/`cargo` 1.100.0), the **official Python
MCP SDK 2.3.0** independently exercised the running binary: **all five tools
(`list_sources`, `search_docs`, `get_section`, `get_api_checklist`,
`get_command_guidance`) succeeded**, while the **advertised section resource failed
on resource discovery** (`resources/list` / `resources/read` of a concrete section
URI). That failure is the one known limitation of this audit, recorded here
*instead of* as a claim of general client support. (The audit's combination —
Python MCP SDK 2.3.0, the installed binary at `6a7880b`, macOS arm64, Cargo
1.100.0 — is the exact configuration tested; the numbers and pass/fail above are
that audit's results, not a fresh run.)

**That resource-discovery limitation is now resolved on current `main`.** Issue
[#172](https://github.com/ThiagoEvoa/rust-mcp/issues/172) / PR #180 made the
advertised section resources real: over stdio, `resources/list` now returns
`rust-docs://catalog` plus paginated concrete section URIs, and `resources/read` of
a concrete section URI (e.g.
`rust-docs://snapshots/rust-docs-bundled-seed/sections/<section-id>`) returns the
section JSON. Verified directly against the current seed (2026-11-18): a
`resources/read` of `rust-docs://catalog` and of a concrete section URI both
succeed. The audit's failure was against the older `6a7880b` binary, *before* that
fix — it is not a current-`main` failure, and this document does not claim the
reverse.

**Environment limitation for a re-run.** The exact SDK 2.3.0 audit **cannot be
reproduced in this environment**: it is Python 3.14.10 with **no `mcp` package
installed**, so the SDK 2.3.0 client cannot be launched here. This is an
environment limitation, not a claim that the audit was re-run: **no SDK 2.3.0
re-run is asserted**, and **general client support is not claimed** — the verified client remains the raw JSON-RPC 2.0 stdio client of §1/§3. A separate
non-`rmcp` implementation (a coding agent, the MCP Inspector, a TypeScript
client) remains **STILL TO TEST**, carrying forward
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md) §10.5.

---

## 7. Files touched by this task

| File | Change |
| --- | --- |
| `docs/CLIENTS.md` | **Created** — this record of the chosen first client, the generic stdio configuration (AC1), and the one tested configuration with the real three-operation transcript (AC2). |
| `tests/first_client.rs` | **Created** — the focused, hermetic, offline proof of the three AC2 operations, with `RUST_DOCS_MCP_TRANSCRIPT` transcript capture. No new dependency; no network. |
| `README.md` | The *generic* client-configuration example now points here for the tested configuration and the transcript. |
| `docs/COMPATIBILITY.md` §10.3 / §10.5 | Referenced as the source of the "first client" identity; unchanged. |
