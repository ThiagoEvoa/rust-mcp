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
the [install walkthrough](../README.md#install-clean-install-walkthrough):

- After `cargo build --release`, the binary is
  `target/release/rust-docs-mcp` — resolve it to an absolute path, e.g.
  `$(pwd)/target/release/rust-docs-mcp`.
- The portable install location the README uses is `~/.local/bin/rust-docs-mcp`,
  i.e. an absolute path such as `/home/you/.local/bin/rust-docs-mcp`
  (Linux) or `/Users/you/.local/bin/rust-docs-mcp` (macOS).

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
{"jsonrpc":"2.0","id":1,"method":"initialize",
 "params":{"protocolVersion":"2025-11-25","capabilities":{},
           "clientInfo":{"name":"first-client","version":"0.0.1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}

// (1) AC2(1) — list the sources
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}
// (2) AC2(2) — search (a query that returns hits)
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"C-CONV"}}}
// (3) AC2(3) — retrieve a section (with its citation/provenance)
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_section","arguments":{"section_id":"api-guidelines/about"}}}
```

The client matches each response to its request by `id` (the server may answer
out of order — a real JSON-RPC client must tolerate that, as
[`tests/stdio_mcp.rs`](../tests/stdio_mcp.rs) and the driver in §4 both do).

### 3.3 The real captured transcript

The three responses below are the **actual output** of the session above,
captured from the built binary (verified 2026-11-13; reproduced by
`tests/first_client.rs`, §4).

**(1) `list_sources`** — returns the **eight canonical sources** and the active
**snapshot id** (`rust-docs-bundled-seed`), plus the command-catalog keys:

```json
{
  "snapshot_id": "rust-docs-bundled-seed",
  "corpus_hash": "c128-ffa5bd3f0f1f20f58aa06f48bb8e42c7",
  "schema_version": "1",
  "commands": ["cargo build", "cargo test", "rustup show"],
  "sources": [
    { "id": "api-guidelines", "title": "Rust API Guidelines", "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "content_hash": "c128-ffa5092e1f9db0151650f6e19369b606" },
    { "id": "style-guide",    "title": "Rust Style Guide",     "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "content_hash": "c128-ffa5092e1f9db0151650f6e19369b606" },
    { "id": "cargo",          "title": "The Cargo Book",       "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "content_hash": "c128-ffa5092e1f9db0151650f6e19369b606" },
    { "id": "rustc",          "title": "The rustc Book",       "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "content_hash": "c128-ffa5092e1f9db0151650f6e19369b606" },
    { "id": "rustup",         "title": "The rustup Book",      "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "content_hash": "c128-ffa5092e1f9db0151650f6e19369b606" },
    { "id": "rustfmt",        "title": "rustfmt reference",    "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "content_hash": "c128-ffa5092e1f9db0151650f6e19369b606" },
    { "id": "clippy",         "title": "Clippy",               "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "content_hash": "c128-ffa5092e1f9db0151650f6e19369b606" },
    { "id": "rustdoc",        "title": "The rustdoc Book",     "fetched_at": "2026-10-30T00:00:00Z", "freshness": "unknown", "content_hash": "c128-ffa5092e1f9db0151650f6e19369b606" }
  ]
}
```

**(2) `search_docs` for `"C-CONV"`** — returns a **ranked hit** that carries a
**`citations`** array (each citation names its `source`):

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
        { "source": "api-guidelines", "section_id": "api-guidelines/about", "title": "The API Guidelines", "url": "https://rust-lang.github.io/api-guidelines/about.html", "note": "canonical upstream document" },
        { "source": "api-guidelines", "section_id": "api-guidelines/about", "note": "guideline code: C-CONV" }
      ]
    }
  ]
}
```

**(3) `get_section` for `"api-guidelines/about"`** — returns the section **with
its provenance**: the body, its `canonical_url`, and a **`citations`** array:

```json
{
  "id": "api-guidelines/about",
  "source": "api-guidelines",
  "doc_title": "The API Guidelines",
  "snapshot_id": "rust-docs-bundled-seed",
  "schema_version": "1",
  "content_hash": "c128-ffa531f24bfdd996c9ef3d1a076dd56a",
  "freshness": "unknown",
  "canonical_url": "https://rust-lang.github.io/api-guidelines/about.html",
  "chapter_path": "about",
  "body": "The Rust API Guidelines are recommendations for the design, documentation, and testing of Rust APIs. They help an API author write an API that is easy to use and hard to misuse. Each guideline carries a stable code (for example C-CONV) and is enforced in practice by Clippy lints.",
  "citations": [
    { "source": "api-guidelines", "section_id": "api-guidelines/about", "title": "The API Guidelines", "url": "https://rust-lang.github.io/api-guidelines/about.html", "note": "canonical upstream document" },
    { "source": "api-guidelines", "section_id": "api-guidelines/about", "note": "guideline code: C-CONV" }
  ]
}
```

Together these satisfy AC2 end to end: the client **lists the sources**
(`list_sources`, eight sources + the snapshot id), **searches** (`search_docs`,
a query that returns a cited hit), and **retrieves a section with a citation**
(`get_section`, the section body plus its `canonical_url` and `citations`). The
`search_docs` hit and the `get_section` result are the *same* section — the
hit's citation points at the exact section the client then reads in full — so
the search-to-section workflow the M5 exit requires works over the tested
configuration.

---

## 4. Reproduce the evidence

The automated, hermetic proof of exactly these three operations is
**[`tests/first_client.rs`](../tests/first_client.rs)**
(`first_client_configuration_lists_searches_and_retrieves_with_citations`). It
spawns the real `rust-docs-mcp serve` binary over OS stdio, completes the
`initialize` handshake, and asserts all three AC2 operations (eight sources +
snapshot id; a `C-CONV` search with a non-empty citation array; an
`api-guidelines/about` section with a non-empty citation array and a
`canonical_url`), then a clean self-exit. It uses only the existing `tokio` and
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
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"demo","version":"0.0.1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_docs","arguments":{"query":"C-CONV"}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_section","arguments":{"section_id":"api-guidelines/about"}}}' \
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

## 6. Real-world clients — still to test

No **real-world / external** MCP client (a coding agent, the MCP Inspector, or a
non-`rmcp` implementation such as a TypeScript/Python client) has been launched
against this server yet. Each remains **STILL TO TEST** — carrying forward
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md) §10.5 — and the first such trial is
owed by a later M5 task. Until then, the verified compatibility claim is scoped
to: **the SDK's own `serve` server + a raw JSON-RPC 2.0 client over stdio,
across all five advertised protocol versions, exercising all five tools and both
resources.** A real external-client run is **not** claimed here.

---

## 7. Files touched by this task

| File | Change |
| --- | --- |
| `docs/CLIENTS.md` | **Created** — this record of the chosen first client, the generic stdio configuration (AC1), and the one tested configuration with the real three-operation transcript (AC2). |
| `tests/first_client.rs` | **Created** — the focused, hermetic, offline proof of the three AC2 operations, with `RUST_DOCS_MCP_TRANSCRIPT` transcript capture. No new dependency; no network. |
| `README.md` | The *generic* client-configuration example now points here for the tested configuration and the transcript. |
| `docs/COMPATIBILITY.md` §10.3 / §10.5 | Referenced as the source of the "first client" identity; unchanged. |
