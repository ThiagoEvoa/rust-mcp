# Distribution, install, and attribution

**Task:** [067][M5] — *Verify the installable distribution and attribution
contents*
(issue [#67](https://github.com/ThiagoEvoa/rust-mcp/issues/67)).
**Status:** **Verified.** A fresh, clean build produces the `rust-docs-mcp`
binary, the bundled corpus it ships loads **offline** (five tools, eight
sources, snapshot `rust-docs-bundled-seed`), and the third-party notices travel
with the content; the documented install/serve path works on the initial
supported target, and the release limitations are recorded.
**Verified:** 2026-11-13, on `rust-docs-mcp 0.1.0` (rustc/cargo `1.99.1`,
stable `aarch64-apple-darwin`), edition `2024`, MSRV `1.88`, `CARGO_NET_OFFLINE`
— the [baseline](COMPATIBILITY.md#2-rust-toolchain-baseline) of
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md).

> **Outcome (issue #67):** *Validate the chosen release artifact/package without
> publishing it remotely.* The "release artifact" the MVP ships is the **Cargo
> source distribution** — the binary plus the embedded, validated, attributed
> bundled corpus and the third-party notices. This record verifies that artifact
> locally (no remote release, no published binary, no container) and records the
> release limitations that follow from that choice.
>
> **Sources of truth:** [`plan.md`](../plan.md) §10/M5 *Exit* (*"a fresh
> installation can answer design/style and tooling queries offline, and an
> operator can refresh and inspect its corpus"*) and §12 (*"Initial distribution
> method and supported build targets"*); the install/first-launch walkthrough in
> the [README](../README.md#install-clean-install-walkthrough) (from
> [063](https://github.com/ThiagoEvoa/rust-mcp/issues/63)); the first-client
> contract in [`docs/CLIENTS.md`](CLIENTS.md) (from
> [064](https://github.com/ThiagoEvoa/rust-mcp/issues/64)); the operator/refresh
> guide in [`docs/OPERATIONS.md`](OPERATIONS.md) (from
> [065](https://github.com/ThiagoEvoa/rust-mcp/issues/65)); and the regeneration
> and attribution mechanism in [`docs/REGENERATION.md`](REGENERATION.md) (from
> [066](https://github.com/ThiagoEvoa/rust-mcp/issues/66)).
>
> **Acceptance criteria (both met):**
> - **AC1** — a fresh extraction/install contains the **binary**, the
>   **required bundled corpus**, and the **third-party notices** (§1).
> - **AC2** — the **documented install/serve path works on the initial supported
>   target**, and the **release limitations** are recorded (§2).

This is a **verification + documentation** task. It changes no behavior, no
dependency, no corpus, no notice, and no product boundary. The only new code is a
hermetic, offline verification test ([`tests/distribution.rs`](../tests/distribution.rs),
§1.4); the only new artifact is this record.

---

## 1. AC1 — the binary, the bundled corpus, and the notices

AC1 asks that a **fresh extraction/install** contain three things, and that they
are mutually consistent. The three are:

| Install content | What it is | Where it lives |
| --- | --- | --- |
| **The binary** | The `rust-docs-mcp` executable (`[[bin]]` in `Cargo.toml`), which runs the stdio server and every command. | Built by `cargo build --release` into `target/release/rust-docs-mcp`. |
| **The required bundled corpus** | The eight-source, validated, attributed snapshot the binary **embeds at compile time** via `include_str!` (`rust_docs_mcp::bundled::JSON` ← the committed `data/bundled/snapshot.json`). | Inside the binary; source of truth `data/bundled/snapshot.json`. |
| **The third-party notices** | The human-facing notice that must accompany redistributed content, plus the licensing audit and the machine-readable per-source attribution that travels *inside* the corpus. | [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md), [`docs/LICENSING.md`](LICENSING.md), and `attribution[]` in `data/bundled/snapshot.json`. |

### 1.1 The distribution method — Cargo source distribution, no pre-built binaries

The MVP ships **source distribution via Cargo**: a consumer builds the binary from
the repository (`cargo build --release`, the documented path in
[§2.2](#22-the-documented-install--serve-path)) rather than downloading a
pre-built release artifact. Concretely, the "release artifact/package" is the
**Cargo package** — the `[[bin]]` target plus the embedded corpus and the notice
files — **not** a published binary, container image, or platform-specific
release. This is the distribution method §12 names; it is a deliberate choice,
recorded here as a release limitation in
[§2.3](#23-the-recorded-release-limitations).

Because the corpus is **embedded in the binary** (not fetched at install time),
a fresh install is self-contained: it carries the offline seed corpus and the
notices, and needs **no network, no LLM, no API key, and no telemetry** to start
serving. The full, current corpus is a separate, explicit refresh (`sync`,
[`docs/OPERATIONS.md`](OPERATIONS.md) §2), not part of the install.

### 1.2 The install contents, in full

A fresh checkout of the repository **plus** a release build contains, and the
binary carries, exactly:

- **The `rust-docs-mcp` binary** — `cargo build --release` produces
  `target/release/rust-docs-mcp` (§1.3).
- **The bundled corpus, embedded** — `data/bundled/snapshot.json` is compiled in
  via `include_str!`, so the binary serves it offline with no `sync`
  ([`src/bundled.rs`](../src/bundled.rs), `JSON`/`manifest`).
- **The third-party notices, in three forms:**
  - [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) — the **human-facing**
    notice naming each of the eight sources, its copyright holder, and the
    operative license clause; it must accompany any redistribution.
  - [`docs/LICENSING.md`](LICENSING.md) — the audit: per-source license evidence,
    the bundled-content policy, and the first-sync fallback.
  - `attribution[]` in `data/bundled/snapshot.json` — the **machine-readable**
    per-source `licenses` / `repository` / `required_notice`, carried *inside*
    every published snapshot and in the embedded bundle, so a notice is not an
    optional comment but a field the content carries (`docs/REGENERATION.md` §4).

### 1.3 Verify AC1 — a clean build and an offline serve

**Step 1 — a clean `cargo build --release` produces the binary.** Starting from a
clean release tree (`cargo clean --release`), a release build compiles the
package and links the `[[bin]]` target:

```sh
$ cargo clean --release
# Removed 0 files   (release tree already empty after a fresh checkout/clean)
$ cargo build --release
   Compiling ... rust-docs-mcp v0.1.0 (/Users/thiagoevoa/Projects/rust-mcp)
    Finished `release` profile [optimized] in 51.33s
$ ./target/release/rust-docs-mcp --version
rust-docs-mcp 0.1.0
```

Real capture, 2026-11-13: the build finished in `51.33s` and produced
`target/release/rust-docs-mcp`, a `Mach-O 64-bit executable arm64`
(`aarch64-apple-darwin`, the baseline host in `docs/COMPATIBILITY.md` §2),
`12192944` bytes. `--version` reports `rust-docs-mcp 0.1.0`.

**Step 2 — the bundled corpus loads offline and serves the eight sources.** With
**no local snapshot** (an empty data directory), `serve` falls back to the
**embedded** bundle. A minimal raw JSON-RPC 2.0 session over stdio —
`initialize` → `notifications/initialized` → `tools/list` → `list_sources` —
produces:

```sh
$ printf '%s\n' \
    '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"067-dist-check","version":"0.0.1"}}}' \
    '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
    '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}' \
  | ./target/release/rust-docs-mcp serve --data-dir /tmp/rust-docs-mcp-067-empty
```

The data directory is **empty**, so the **embedded** seed is what is served.
Every stdout line is a JSON-RPC 2.0 message; diagnostics go to stderr.

- **`initialize`** (id `0`) — the server negotiates `protocolVersion = 2025-11-25`
  (the highest `initialize`-capable version), advertises exactly the
  `tools` + `resources` capabilities, and identifies itself
  (`serverInfo = {"name": "rust-docs-mcp", "version": "0.1.0"}`).
- **`tools/list`** (id `1`) — advertises **exactly the five tools**, in order:
  `list_sources`, `search_docs`, `get_section`, `get_api_checklist`,
  `get_command_guidance`.
- **`list_sources`** (id `2`) — returns `snapshot_id = rust-docs-bundled-seed`,
  `schema_version = 1`, and **the eight sources** (`api-guidelines`,
  `style-guide`, `cargo`, `rustc`, `rustup`, `rustfmt`, `clippy`, `rustdoc`)
  plus the command-catalog keys `cargo build`, `cargo test`, `rustup show`.
- Closing stdin (EOF) ends the session with **exit `0`**; stderr carried the
  runtime diagnostics only (no fetch/network/sync lines — the run served from the
  embedded corpus and made **no network call**).

This is the M5 exit in one session: a **fresh installation answers a design/style
and tooling query offline** — `list_sources` lists the corpus, and the same
session in [`docs/CLIENTS.md`](CLIENTS.md) §3.3 runs `search_docs`
(`"C-CONV"`, a cited hit) and `get_section` (`api-guidelines/about`, the body
with its citation). The automated, hermetic proof of that session is
[`tests/first_client.rs`](../tests/first_client.rs); the full five-tool +
two-resource proof is [`tests/stdio_mcp.rs`](../tests/stdio_mcp.rs).

**Step 3 — the notices travel with the content.** The three notice forms exist in
the distribution and are mutually consistent:

- `THIRD_PARTY_NOTICES.md` and `docs/LICENSING.md` are present in the repository
  (the human-facing notice and the audit).
- `data/bundled/snapshot.json` carries **eight `attribution[]` records** — one
  per required source — each `MIT OR Apache-2.0` and each pointing at
  `THIRD_PARTY_NOTICES.md`:

  | Source | `licenses` | `repository` | `required_notice` (abbreviated) |
  | --- | --- | --- | --- |
  | `api-guidelines` | `MIT OR Apache-2.0` | `https://github.com/rust-lang/api-guidelines` | *Rust upstream content (MIT OR Apache-2.0), attributed in `THIRD_PARTY_NOTICES.md`* |
  | `style-guide` | `MIT OR Apache-2.0` | `https://github.com/rust-lang/rust` | *Rust upstream content …* |
  | `cargo` | `MIT OR Apache-2.0` | `https://github.com/rust-lang/cargo` | *The upstream content …* |
  | `rustc` | `MIT OR Apache-2.0` | `https://github.com/rust-lang/rust` | *The upstream content …* |
  | `rustup` | `MIT OR Apache-2.0` | `https://github.com/rust-lang/rustup` | *The upstream content …* |
  | `rustfmt` | `MIT OR Apache-2.0` | `https://github.com/rust-lang/rustfmt` | *rustfmt upstream content …* |
  | `clippy` | `MIT OR Apache-2.0` | `https://github.com/rust-lang/rust-clippy` | *Clippy upstream content …* |
  | `rustdoc` | `MIT OR Apache-2.0` | `https://github.com/rust-lang/rust` | *The upstream content …* |

  The human-facing `THIRD_PARTY_NOTICES.md` names the **same** upstream
  repositories (cross-checked in §1.4), so the machine-readable and
  human-readable views of the attribution cannot diverge.

### 1.4 The automated check — `tests/distribution.rs`

AC1 is also enforced as a **hermetic, offline test**:
[`tests/distribution.rs`](../tests/distribution.rs). It runs over the **shipped
embedded bytes** (`rust_docs_mcp::bundled::JSON`), not a hand-built manifest, and
asserts:

- the embedded snapshot carries `snapshot_id = rust-docs-bundled-seed` and
  **exactly the eight required sources and eight `attribution[]` records**;
- **every** attribution record points at `THIRD_PARTY_NOTICES.md` and names the
  `MIT OR Apache-2.0` family;
- `THIRD_PARTY_NOTICES.md` and `docs/LICENSING.md` are **present** in the
  distribution, non-empty, and the notice names the **same upstream
  repositories** the `attribution[]` records carry (the notice and the corpus
  agree).

```sh
$ cargo test --test distribution --locked
running 2 tests
test the_distribution_corpus_carries_eight_sources_and_attribution ... ok
test the_third_party_notices_travel_with_the_content            ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The test spawns no process, opens no socket, and writes nothing, so it is fully
hermetic and independent of live upstream (`CARGO_NET_OFFLINE`). It is the
*contents* half of AC1; the *offline-serve* half is the spawned-binary proof in
`tests/stdio_mcp.rs` / `tests/first_client.rs`.

---

## 2. AC2 — the documented install/serve path works; release limitations recorded

### 2.1 The initial supported target

The initial supported target is the one
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md) §2 records: a **stable Rust
toolchain**, **edition `2024`**, **MSRV `1.88`** (the binding floor of the
dependency set — `rmcp`/`rmcp-macros` `rust-version = 1.88`), built with
**`CARGO_NET_OFFLINE`** over the committed `Cargo.lock` (the MSRV/stable checks
the [061] CI runs, `.github/workflows/validation.yml`). The build target is
**whatever `rustup` selects for the host** — the exact patch version is
deliberately not hardcoded (`rust-toolchain.toml` pins only the *channel*).

The verification above ran on the baseline host
(`aarch64-apple-darwin`, `rustc`/`cargo 1.99.1`); the CI validates the same
package on `ubuntu-24.04` (the `Rust stable` and `Rust MSRV` jobs). The
**release artifact is a single, host-triple native binary** — there is no
cross-compiled or multi-platform release set (see §2.3).

### 2.2 The documented install/serve path

The documented path (the
[README](../README.md#install-clean-install-walkthrough)) is:

```sh
# 1. build the release binary (needs a stable toolchain + a C compiler for rustls)
cargo build --release

# 2. confirm it runs
./target/release/rust-docs-mcp --version        # rust-docs-mcp 0.1.0

# 3. (optional) put it on a stable, absolute path a client can launch
mkdir -p ~/.local/bin
cp target/release/rust-docs-mcp ~/.local/bin/

# 4. launch the server on stdio; with no local snapshot it serves the bundled seed offline
./target/release/rust-docs-mcp serve            # or: ... serve --data-dir /path --verbose
```

An MCP client then launches the server by its **absolute executable path** with
the `serve` argument in the standard `mcpServers` shape
([`docs/CLIENTS.md` §2](CLIENTS.md#2-ac1--the-generic-stdio-client-configuration));
a clean first launch serves the **bundled snapshot offline** — no `sync` is
required. This task ran steps 1, 2, and 4 against the empty data directory
([§1.3](#13-verify-ac1--a-clean-build-and-an-offline-serve)) and observed the
documented behavior (five tools, eight sources, `rust-docs-bundled-seed`, clean
disconnect, exit `0`).

### 2.3 The recorded release limitations

The following are **intentional** consequences of the source-distribution choice
([§1.1](#11-the-distribution-method--cargo-source-distribution-no-pre-built-binaries)),
recorded here so a consumer knows what the artifact is *not*:

- **No pre-built release binaries.** There is no published binary, no
  `cargo publish` crate, no container image, and no release download. The
  artifact is the **Cargo source distribution**; a consumer builds it with
  `cargo build --release`. (No release was published as part of this task.)
- **A single build target.** The release is the **host-triple native binary**
  `rustup` produces for the building machine; no cross-compiled or
  multi-platform artifact set is produced. The build needs a **C compiler**
  (`cc`) for the `rustls` TLS stack the `sync` fetcher uses, plus the pinned
  stable toolchain (edition `2024`, MSRV `1.88`).
- **The bundled seed is the offline path; it is a representative subset, not the
  full corpus.** A fresh install serves the **seed** (one representative section
  per source, the `C-CONV` checklist entry, a few command entries) so a first
  launch is usable offline. The **full, current, revision-pinned** corpus is
  obtained by an explicit `sync` ([`docs/OPERATIONS.md`](OPERATIONS.md) §2;
  the regeneration seam in [`docs/REGENERATION.md`](REGENERATION.md) §1). On the
  seed, `last checked` and `upstream` are `unknown` for every source — it has not
  checked upstream yet; `sync` stamps those times.
- **The first `sync` requires network.** `sync` is the **only** command that
  reaches the network ([`docs/OPERATIONS.md`](OPERATIONS.md) §2); the server's
  install and serve paths make **no** network call. A first `sync` also requires
  a live connection to the official upstreams (the opt-in
  [Upstream ingestion](../.github/workflows/upstream-ingestion.yml) job runs it
  on the agent's network; it is **not** part of the mandatory offline CI gate).
- **No telemetry, no API key, no external runtime service.** The server needs
  none to start, and makes none. A fetched example or a command key such as
  `cargo test` is **documentation about** a command, never a sign the server ran
  it; the calling agent executes Rust/Cargo commands through its own terminal
  tools. No external LLM, API key, project upload, or telemetry is required.
- **Attribution is authored, not propagated.** No upstream source ships a
  `NOTICE` file, so the required notice is **authored** by this project
  (`THIRD_PARTY_NOTICES.md` + `docs/LICENSING.md`, §3) rather than copied from an
  upstream file; any *future* upstream `NOTICE` or non-permissive content is
  excluded per [`docs/LICENSING.md` §5](LICENSING.md#5-explicit-first-sync-fallback)
  and routed through the first-sync fallback, never silently bundled.
- **The seed omits nothing.** All eight required collections are MIT OR
  Apache-2.0 and bundle cleanly, so `FIRST_SYNC_REQUIRED` is empty and the seed
  ships the full source set; a documented bundling exception (a source that
  cannot be bundled) would instead be **omitted and flagged
  `first-sync-required`** while the last valid snapshot is preserved.

---

## 3. Attribution — how the notices travel with the content

The three notice forms are the same attribution seen three ways, and they are
kept in lockstep:

| Form | Role | Travels with |
| --- | --- | --- |
| `attribution[]` in `data/bundled/snapshot.json` | Machine-readable per-source `licenses` / `repository` / `required_notice`. | **Every** published snapshot *and* the embedded bundle (a populated `AttributionRef` is always serialized — the fields are `skip_serializing_if` empty only). |
| [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) | The human-facing notice a redistribution must carry. | The distribution (a file the install contains). |
| [`docs/LICENSING.md`](LICENSING.md) | The audit: per-source evidence, policy, fallback. | The distribution (a file the install contains). |

**Redistribution policy** (`docs/LICENSING.md` §3/§4). The bundle is
**MIT OR Apache-2.0, at the recipient's option**; the project adopts
**Apache-2.0** as the operative license (its notice/provenance semantics are the
explicit ones the plan expects, and Apache-2.0 is a superset of the MIT
obligations here), **retaining the MIT text alongside it**. The operative clause
the content is licensed under is, verbatim:

> Each file is licensed under either of Apache License, Version 2.0
> (LICENSE-APACHE) or MIT license (LICENSE-MIT), at your option.

So any redistribution of the content must ship the two license files
(`LICENSE-APACHE`, `LICENSE-MIT`) **and** `THIRD_PARTY_NOTICES.md`. Because the
notice is a field the content carries (§1.2, `attribution[]`) rather than an
optional comment, a published snapshot and the embedded bundle both retain it
automatically. `THIRD_PARTY_NOTICES.md` names each source's copyright holder
(`The Rust Project Developers` / `The Rust Project Contributors`), repository,
and operative license; see §1.3 step 3 and [`docs/REGENERATION.md`](REGENERATION.md) §4
for the per-source detail.

---

## 4. Reproduce the evidence

| Goal | Command |
| --- | --- |
| Produce the binary from a clean tree | `cargo clean --release && cargo build --release` |
| Confirm the binary runs | `./target/release/rust-docs-mcp --version` → `rust-docs-mcp 0.1.0` |
| Serve the bundled corpus offline (empty data dir) | `./target/release/rust-docs-mcp serve --data-dir /tmp/empty` (pipe the §1.3 session) |
| Prove the five tools + eight sources + snapshot id | `tools/list` + `list_sources` (§1.3) — automated by `tests/stdio_mcp.rs` / `tests/first_client.rs` |
| Prove the corpus + attribution + notices are consistent | `cargo test --test distribution --locked` (two hermetic tests, §1.4) |
| Inspect coverage + extraction fidelity | `./target/release/rust-docs-mcp inspect` |
| Read identity, content hashes, freshness | `./target/release/rust-docs-mcp status` |
| Obtain the full, current corpus | `./target/release/rust-docs-mcp sync` (network; then restart `serve`) |
| See the notices that travel with the content | `THIRD_PARTY_NOTICES.md`, `docs/LICENSING.md`, and `attribution[]` in `data/bundled/snapshot.json` |

---

## 5. Files touched by this task

| File | Change |
| --- | --- |
| `docs/DISTRIBUTION.md` | **Created** — this record: the distribution method, the install contents, the AC1 clean-build + offline-serve verification, the AC2 supported target and recorded release limitations, and the attribution/redistribution policy. |
| `tests/distribution.rs` | **Created** — the hermetic, offline AC1 check (embedded corpus = eight sources + eight `attribution[]` records pointing at the notice; notice files present and consistent). No new dependency; no network. |
| `README.md` | Extended — the *Related docs* / distribution pointer points here. |

No `src`, `Cargo.toml`, `Cargo.lock`, `data/bundled/snapshot.json`,
`THIRD_PARTY_NOTICES.md`, `docs/LICENSING.md`, or product-boundary file was
modified. No dependency was added, no `unsafe` introduced, and no live/network
call was added to any test.

---

## 6. Verification evidence (commands actually run)

Run 2026-11-13 on `rustc`/`cargo 1.99.1` (stable `aarch64-apple-darwin`),
edition `2024`, MSRV `1.88`, `CARGO_NET_OFFLINE`:

```sh
# AC1 — a clean build produces the binary
$ cargo clean --release && cargo build --release
    Finished `release` profile [optimized] in 51.33s
$ ./target/release/rust-docs-mcp --version
rust-docs-mcp 0.1.0
$ file target/release/rust-docs-mcp
# Mach-O 64-bit executable arm64  (12192944 bytes)

# AC1 — the bundled corpus loads offline and serves the eight sources
# (empty data dir -> the embedded seed; tools/list = 5, list_sources = 8 + rust-docs-bundled-seed)
$ printf '%s\n' \
    '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"067-dist-check","version":"0.0.1"}}}' \
    '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
    '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_sources","arguments":{}}}' \
  | ./target/release/rust-docs-mcp serve --data-dir /tmp/rust-docs-mcp-067-empty
# initialize -> protocolVersion 2025-11-25, capabilities {resources,tools}, serverInfo rust-docs-mcp 0.1.0
# tools/list -> [list_sources, search_docs, get_section, get_api_checklist, get_command_guidance]
# list_sources -> snapshot_id rust-docs-bundled-seed, 8 sources, commands [cargo build, cargo test, rustup show]
# stdout: 3 JSON-RPC 2.0 lines; stderr: runtime diagnostics only (no fetch/network/sync); exit 0 on EOF

# AC1 — the notices travel with the content
$ cargo test --test distribution --locked
# test result: ok. 2 passed; 0 failed   (embedded corpus = 8 sources + 8 attribution[]
#  records pointing at THIRD_PARTY_NOTICES.md; THIRD_PARTY_NOTICES.md + docs/LICENSING.md present)

# AC2 — the documented install/serve path works on the initial supported target
#  (stable toolchain, edition 2024, MSRV 1.88, CARGO_NET_OFFLINE; §2)
$ cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings
# both green (the new test compiles and is warning-free)
```

Checks **not** run here: the full `cargo test --workspace --locked` /
`cargo test --doc --locked` suite (carried by the [061] CI
`.github/workflows/validation.yml` and by the existing per-behavior tests; the
workspace test suite has known parallel-only flakes unrelated to this task, so
the targeted, hermetic check above is run instead). The live upstream `sync`
(`.github/workflows/upstream-ingestion.yml`) is the opt-in job that would
populate the full corpus; it was **not** run, and no network was used for the
evidence above.
