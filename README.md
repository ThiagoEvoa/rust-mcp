# rust-docs-mcp

A local **stdio** [Model Context Protocol](https://modelcontextprotocol.io)
(MCP) server that gives coding agents searchable, attributable access to the
official Rust documentation — the API guidelines, the style guide, and the
Cargo, rustc, rustup, rustfmt, Clippy, and rustdoc manuals — from a single
immutable snapshot. It runs entirely **offline**: the server *retrieves*
documentation; it does not execute commands, touch the project filesystem, or
reach the network. The calling agent runs Rust and Cargo commands through its
own terminal tools and inspects their results. See [`plan.md`](plan.md) for the
full specification and [`docs/`](docs/) for the compatibility, licensing, CI, and
client-configuration records. Refresh, status, offline behavior, and troubleshooting
are recorded in [`docs/OPERATIONS.md`](docs/OPERATIONS.md).

## What it is

`rust-docs-mcp` is one Cargo package — a binary `rust-docs-mcp` plus a small
internal library `rust_docs_mcp` — that serves eight source collections
(`api-guidelines`, `style-guide`, `cargo`, `rustc`, `rustup`, `rustfmt`,
`clippy`, `rustdoc`) over stdio. It exposes five read-only tools
(`list_sources`, `search_docs`, `get_section`, `get_api_checklist`,
`get_command_guidance`) and two resource views (a catalog and bounded section
reads). A fetched example or a command key such as `cargo test` is *documentation
about* a command, never a sign the server ran it. Retrieval, provenance, and
freshness are covered by [`plan.md`](plan.md) §3/§6 and
[`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md); the agent workflow and
client-integration sequence are in [`plan.md`](plan.md) §7.

## Prerequisites

- **A stable Rust toolchain at or above the minimum supported version.** The
  package declares `rust-version = "1.88"` (MSRV 1.88) and
  `edition = "2024"` in [`Cargo.toml`](Cargo.toml); [`rust-toolchain.toml`](rust-toolchain.toml)
  pins the *channel* to `stable` (whatever stable `rustup` selects). The
  recorded development toolchain is `rustc 1.99.1`. Use `rustup` to install a
  stable toolchain:

  ```sh
  rustup toolchain install stable
  ```

- **A C compiler** (`cc`, for the `rustls` TLS stack used by `sync`'s HTTP
  fetcher), plus the usual build tooling — the linker that produces the final
  binary. `cargo build` compiles the release binary.

- **No external runtime services and no network to start the server.** The
  server needs no external runtime services at serving time — no network service,
  LLM, API key, or telemetry — and it makes no network calls. Network access is
  required only for the explicit `sync` refresh command
  (see [Refreshing the corpus](#refreshing-the-corpus)).

## Install (clean-install walkthrough)

Build the release binary from source:

```sh
cargo build --release
```

This produces the binary at `target/release/rust-docs-mcp`. Confirm it runs:

```sh
./target/release/rust-docs-mcp --version
# rust-docs-mcp 0.1.0
```

To run it from a stable location (so an MCP client can launch it by an absolute
path), copy it somewhere on your `PATH`, for example `~/.local/bin`:

```sh
mkdir -p ~/.local/bin
cp target/release/rust-docs-mcp ~/.local/bin/
~/.local/bin/rust-docs-mcp --version
```

### Identify the bundled snapshot

A fresh install ships a **validated, attributed** snapshot embedded in the
binary (built in [task 031](https://github.com/ThiagoEvoa/rust-mcp/issues/31)),
so the server can serve documentation **without any network access**. Identify
the snapshot the binary will serve with `status`, which prints its identity and
per-source freshness offline:

```sh
./target/release/rust-docs-mcp status
```

```text
rust-docs-mcp status:
  snapshot: rust-docs-bundled-seed
  source(s): 8, section(s): 8
  freshness threshold: 7 day(s) (default)
  as_of: 1794566677 (seconds since the Unix epoch)
  [api-guidelines] Rust API Guidelines
    last checked: unknown
    fetched:      2026-10-30T00:00:00Z
    upstream:     unknown
    content:      c128-ffa5092e1f9db0151650f6e19369b606
    stale:        unknown
```

The output continues with one identical block per source (`style-guide`,
`cargo`, `rustc`, `rustup`, `rustfmt`, `clippy`, `rustdoc`). On the seed,
`last checked` and `upstream` are `unknown` for every source because it has not
checked upstream yet — `sync` stamps them. The `as_of` value is the wall clock at
the time of the run, so it varies.

The `snapshot:` line is the **snapshot identity** (`rust-docs-bundled-seed`);
the `8` sources/`8` sections confirm the eight collections are present. The
`list_sources` tool returns that same snapshot id. The `initialize` handshake
carries a *different* value — the `serverInfo` that identifies the **server**
itself: `name = "rust-docs-mcp"`, `version = "0.1.0"` — distinct from the
snapshot id.

For a coverage and extraction-fidelity report over the same snapshot, use
`inspect` (the operator view):

```sh
./target/release/rust-docs-mcp inspect
```

```text
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

`coverage_complete=true` and `fidelity_complete=true` confirm the bundled seed
covers all eight collections and that the extractors retain the content types
agents rely on. The two reports are separate `status`/`inspect` diagnostics
written to **stderr** (hence the blank line between them); the lines above have
the per-run tracing prefix (`<timestamp> <level> ... rust-docs-mcp: `) removed
for readability, and the source titles use a fixed 14-column field, so the
`documents=` column is not aligned.

## Run / first offline launch

Start the server on stdio:

```sh
./target/release/rust-docs-mcp serve
```

`serve` binds the process's standard input/output as the MCP transport and runs
until the client disconnects. On a clean first launch it serves the bundled
snapshot **offline** — no `sync` is required. Diagnostics go to **stderr**;
**stdout carries only MCP protocol traffic**, so a client can parse it
unambiguously. A minimal `initialize` handshake over stdio succeeds and reports
the server's identity and capabilities:

```text
# stdin (one newline-framed JSON-RPC 2.0 message per line)
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"my-client","version":"0.0.1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}

# stdout (the single protocol response)
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"resources":{},"tools":{}},"serverInfo":{"name":"rust-docs-mcp","version":"0.1.0"},"instructions":"..."}}
```

The response advertises exactly the `tools` and `resources` capabilities the
server implements, and identifies itself as `rust-docs-mcp 0.1.0`.

### Client configuration (generic example)

Configure an MCP client to launch the server by its **absolute executable path**
with the `serve` argument. Replace the path with the location from
[Install](#install-clean-install-walkthrough):

```json
{
  "mcpServers": {
    "rust-docs": {
      "command": "/home/you/.local/bin/rust-docs-mcp",
      "args": ["serve"]
    }
  }
}
```

Add `--data-dir` or `--verbose` to `args` if you use them (for example
`["serve", "--data-dir", "/var/lib/rust-docs-mcp", "--verbose"]`). No
client-specific integration is required to begin using the server; the generic
absolute-path configuration above is the documented contract, and the
`initialize` handshake above is the runtime check that it works.

The **tested configuration** for the first client — the raw JSON-RPC 2.0 client
over stdio — and the real captured transcript that proves it can list sources,
search, and retrieve a section with a citation are recorded in
[`docs/CLIENTS.md`](docs/CLIENTS.md) (reproduced by
[`tests/first_client.rs`](tests/first_client.rs)).

## Refreshing the corpus

`sync` performs the bounded, explicit network ingestion that discovers the
official sources, extracts and validates them, and atomically publishes a new
immutable snapshot (preserving the last valid one). It is the only command that
needs network access:

```sh
./target/release/rust-docs-mcp sync
```

A published snapshot is written under the data directory and becomes the one the
process serves on its next start. **The server keeps one snapshot fixed for the
process lifetime, with no hot reload** — restart `serve` after a `sync` to
activate the newer snapshot. A `sync` that fails to fetch or parse an expected
document aborts and leaves the last valid corpus intact; it never publishes a
partial corpus.

## Freshness and version uncertainty

`status` reports the snapshot and each source's freshness **without network
access**. Staleness is **metadata, not a block** — a stale source is still served
— and the threshold is **configurable**: `--stale-days DAYS` overrides the
seven-day default. The same source is `stale` at `7` and `fresh` at `30`:

```sh
./target/release/rust-docs-mcp status --stale-days 14
```

```text
rust-docs-mcp status:
  snapshot: rust-docs-bundled-seed
  source(s): 8, section(s): 8
  freshness threshold: 14 day(s) (configured)
  as_of: 1794576127 (seconds since the Unix epoch)
  [api-guidelines] Rust API Guidelines
    last checked: unknown
    fetched:      2026-10-30T00:00:00Z
    upstream:     unknown
    content:      c128-ffa5092e1f9db0151650f6e19369b606
    stale:        unknown
  [style-guide] Rust Style Guide
    ...
```

Two values are **preserved as `unknown`, never guessed**. A source with no known
`last checked` reports `stale: unknown`; on the bundled seed every source is
`unknown` because it has not checked upstream yet (`sync` stamps the times). And
**version compatibility** is one of `stable`, `nightly-only`, `rolling`, or
`unknown`: when upstream does not pin a version it is reported as `unknown` rather
than assumed, so **a commit-pinned rustfmt document is not automatically
documentation for an installed stable release** — check the local tool versions and
the local `--help` when compatibility is uncertain. The full freshness model, the
three distinct times, the configurable staleness policy, and the failure-recovery
and no-valid-corpus behavior are recorded in
[`docs/OPERATIONS.md`](docs/OPERATIONS.md).

## The first-sync exception

Per [`plan.md`](plan.md) §5, when **no valid snapshot exists** the binary must
**not** start an apparently-healthy empty server; it exits with an actionable
message on **stderr** directing the operator to run `sync`. In `src/runtime.rs`
the `LoadError::NoValidCorpus` case produces the following message (a single
line, wrapped here for width; `<reason>` is the specific local or bundle cause):

```text
no valid corpus is available (local published snapshot: <reason>)
(no bundled snapshot was provided).
Run `rust-docs-mcp sync` to download and publish a local snapshot,
or restore the bundled fallback.
```

In the **current build this path does not fire in normal operation**, because a
validated snapshot is always embedded in the binary
([task 031](https://github.com/ThiagoEvoa/rust-mcp/issues/31)); a fresh install
therefore serves offline without a first `sync`. The message and the recover
step are the guarantee for a configuration that would omit the bundle: run
`rust-docs-mcp sync`, then restart `serve`.

## Data directory

The server reads a snapshot from an **OS-appropriate application data
directory**:

- **Unix:** `$XDG_DATA_HOME/rust-docs-mcp`, falling back to
  `$HOME/.local/share/rust-docs-mcp`.
- **Windows:** `%APPDATA%\rust-docs-mcp`.

Override it with the **global** `--data-dir PATH` flag (accepted before *or*
after the subcommand) for tests or portable installations:

```sh
./target/release/rust-docs-mcp --data-dir /tmp/rust-docs-mcp serve
# equivalent to:
./target/release/rust-docs-mcp serve --data-dir /tmp/rust-docs-mcp
```

With no local snapshot, a command falls back to the embedded bundled snapshot, so
an empty `--data-dir` still serves offline. `sync` writes a new snapshot under
the resolved data directory; a later `serve`/`status`/`inspect` reads it. If no
`--data-dir` is given and the platform exposes no default location, the command
fails with an actionable error telling you to set `--data-dir` explicitly,
rather than writing to an unexpected place.

## Commands

```text
rust-docs-mcp serve   [--verbose] [--data-dir PATH]
rust-docs-mcp sync    [--verbose] [--data-dir PATH]
rust-docs-mcp status  [--verbose] [--data-dir PATH] [--stale-days DAYS]
rust-docs-mcp inspect
```

| Command | What it does |
| --- | --- |
| `serve` | Launch the stdio MCP server on the selected immutable snapshot. Serves offline from the local published snapshot, or the bundled snapshot as the first-launch fallback. |
| `sync` | Refresh the corpus from the official sources: discover, fetch, extract, validate, and atomically publish a new snapshot. The only command that needs network access. |
| `status` | Report the selected snapshot's identity, source/section counts, and per-source freshness **without network access**. `--stale-days DAYS` overrides the seven-day default threshold. |
| `inspect` | Operator view: corpus coverage and extraction-fidelity report over the active snapshot. Exits non-zero only on a snapshot-load failure. |

Global flags (accepted before or after the subcommand):

- `--data-dir PATH` — use `PATH` as the application data directory instead of the OS default.
- `-v`, `--verbose` — raise diagnostics to `debug` level (or set `RUST_LOG`). Diagnostics always go to **stderr**; this flag never writes query text to stdout.

`--help` and `--version` are available on the top-level command and each
subcommand.

## Troubleshooting

In every case below the **last valid corpus is preserved**, and unless a bundle-less
build is involved the server keeps serving the bundled snapshot — so a bad refresh is a
diagnostic, not a downtime. The detailed guide — with the real diagnostics, the
failure-recovery guarantees, and the no-valid-corpus error — is in
[`docs/OPERATIONS.md`](docs/OPERATIONS.md#8-troubleshooting).

| Symptom | What it is | Check | Fix |
| --- | --- | --- | --- |
| `sync` fails to publish; a collection is missing | **Missing components** — a required source could not be **fetched** (`building the fetch client failed`, `a fetch failed`, `discovery failed`) | The `sync` stderr diagnostic; the source must resolve within its fixed host/path boundary | Re-run `sync` (the opt-in [Upstream ingestion](docs/CI.md#opt-in-upstream-ingestion-check) job); the previous/bundled corpus keeps serving |
| `status` reports `stale: stale` for a source | **Stale content** — the last upstream check is older than the threshold; the content is **still served** | The `status` per-source block | Re-run `sync` then **restart `serve`**, or widen the verdict with `status --stale-days <N>` |
| `sync` fetches a source but aborts | **Extraction failure** — a source fetched but failed to parse: `sync failed: extracting source \`<id>\` failed: <reason>` (a changed layout, not a silent empty corpus) | The `sync` per-source report names the failing collection; the other seven are unaffected | The last corpus still serves; re-run when restorable, or update the parser for a persistent layout break |
| A client can't connect, the server exits, or stdout looks polluted | **Client connection failure** (stdio/transport) — wrong absolute path/args, stdout conflated with stderr, or a non-zero startup exit | The client config and the server's **stderr** (stdout is protocol-only) | See [`docs/CLIENTS.md`](docs/CLIENTS.md): absolute `command` + `serve`; don't redirect stderr into stdout; a clean disconnect exits `0`, a startup failure exits non-zero with an actionable stderr line |

## Offline use and trust boundaries

`rust-docs-mcp` retrieves documentation only. It does not execute commands, read
or write the project filesystem, or reach the network during `serve` — the
calling agent runs Rust/Cargo commands through its own terminal tools. Upstream
content is stripped of navigation, scripts, and styling, and no fetched text is
interpreted as a server instruction or executed; the Clippy catalog, rustfmt
Markdown adapter, and source registry boundaries are fixed and versioned.
Citations, provenance, and per-source freshness travel with every result, and
unavailable version compatibility is reported as *unknown* rather than guessed.
The licensing and attribution policy is recorded in
[`docs/LICENSING.md`](docs/LICENSING.md) and
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md); the full design, scope, and
trust boundary are in [`plan.md`](plan.md) §3, §5, and §9.
