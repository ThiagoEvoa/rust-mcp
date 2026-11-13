# Project-implementation agent trial — `[069][M5]`

> **Task:** `[069][M5] Run the project-implementation agent trial`
> ([issue #69](https://github.com/ThiagoEvoa/rust-mcp/issues/69)).
> **Plan:** [§1 success scenario](../../plan.md#1-goals--non-goals) ("when
> implementing or reviewing the project, it retrieves documentation for the
> applicable build, formatting, lint, test, and documentation commands, then
> executes them through its own terminal tools") and [§10 / M5](../../plan.md#10-milestones)
> ("an agent trial demonstrates retrieving command guidance, running relevant
> checks through terminal tools, and reporting actual outcomes").
> **What this is:** the trial agent's retrieval session, run offline against the
> bundled `rust-docs-bundled-seed` snapshot over the spawned-stdio JSON-RPC seam,
> with the actual requests, real responses, and canonical citations recorded
> below. The same session is encoded as a reproducible test —
> [`tests/trial_project_implementation.rs`](../../tests/trial_project_implementation.rs).
> **Verified:** 2026-11-13, on `rustc`/`cargo` 1.99.1 (stable), MSRV 1.88,
> edition 2024 — the
> [baseline](../COMPATIBILITY.md#2-rust-toolchain-baseline) of
> [`docs/COMPATIBILITY.md`](../COMPATIBILITY.md).

The trial does **not** claim that the commands it retrieves guidance for are run
by the server, nor that any command the agent runs is *automatically correct*.
It retrieves documentation and records its citations, compatibility
qualifications, and effect notes; the calling agent executes the commands
through *its own* terminal tools, outside this trial. The boundaries and the
retrieval limits observed here are recorded explicitly, not asserted away
([§11 risk](../../plan.md#11-technical-decisions--risks-and-unknowns)).

---

## 1. What the trial runs

The trial is one end-to-end agent session over the `rust-docs-mcp serve` binary,
driven exactly like [`tests/first_client.rs`](../../tests/first_client.rs),
[`tests/stdio_mcp.rs`](../../tests/stdio_mcp.rs), and the library-design trial
[`tests/trial_library_api_design.rs`](../../tests/trial_library_api_design.rs): a
raw JSON-RPC 2.0 client over genuine OS stdio, with the bundled snapshot loaded
in-memory and an **empty** `--data-dir` so nothing touches the host cache or the
network. The agent's call sequence is:

| # | Call | Purpose |
|---|------|---------|
| 1 | `initialize` | negotiate the protocol version, read the advertised capabilities and the agent-workflow `instructions`. |
| 2 | `notifications/initialized` | complete the handshake. |
| 3 | `tools/list` | discover the five tools, including `get_command_guidance`. |
| 4 | `get_command_guidance "cargo build"` | the **one** seed command whose full-catalog citation resolves in the representative seed — the guidance the agent would run via its own terminal. |
| 5 | `get_command_guidance "cargo test"` | a seed-advertised command whose full-catalog citation is **not** in the representative seed — a typed `UnresolvedSection` error the trial records (AC2). |
| 6 | `get_command_guidance "rustup show"` | the same, for the toolchain-inspection command — a typed `UnresolvedSection` error. |
| 7 | `get_command_guidance "cargo foobar"` | a command **not in the catalog** — a typed `UnknownKey` error naming the supported keys; the catalog-boundary probe that also proves the key is a *selector*, never a shell line. |
| 8 | clean client disconnect | the server self-exits. |

The trial's "actual commands" are the commands the agent **would run through its
own terminal** after reading the guidance — `cargo build` (and, once its
guidance resolves, `cargo test`), plus `rustup show` to confirm the active
toolchain. The trial itself performs **no command execution**: the server is a
documentation-only retrieval endpoint, and the "exit status" the trial records is
the *retrieval's* JSON-RPC result/error — never the exit status of a command the
server ran.

---

## 2. Environment

| Property | Value |
|----------|-------|
| Repository | [ThiagoEvoa/rust-mcp](https://github.com/ThiagoEvoa/rust-mcp) |
| Active snapshot | `rust-docs-bundled-seed` — `created_at` `2026-10-30T00:00:00Z`, `parser_version` `rust-docs-mcp/seed-1`, schema version `1`, **8 sections**, **3 command-catalog entries** |
| Command catalog served | the full `CommandCatalog::combined()` — **29 keys** (the `list_sources` `commands` field surfaces the seed's 3; the server resolves `get_command_guidance` against the full catalog, so a key resolves only when its cited section is in the snapshot) |
| Transport | stdio, JSON-RPC 2.0, protocol version `2025-11-25` |
| Network | **None** — the trial runs offline; the snapshot is loaded in-memory |
| Host cache | **None** — an empty `--data-dir` is supplied, so `serve` uses the bundled seed and never touches the host application cache |
| New dependency | **None** — the trial reuses `tokio` + `serde_json`, the existing test seam |
| Toolchain | the repository's stable toolchain / pinned SDK baseline: `rustc`/`cargo` 1.99.1, `rustup` 1.29.3, MSRV 1.88, edition 2024; no MSRV / dependency / behavior / `unsafe` change |
| Corpus source hashes | the seed shares one content hash (`c128-ffa5092e1f9db0151650f6e19369b606`) across its 8 sources; the section the trial cites (`cargo/commands%2Fcargo-build`) has content hash `c128-ffa550364dc212527d904d59fe85fe84` |
| Freshness | `unknown` — the representative seed has never checked upstream (recorded, not fabricated) |

---

## 3. Session transcript

Each entry below is a real request and its real response, regenerated from the
`RUST_DOCS_MCP_TRIAL_TRANSCRIPT` dump the test writes (see §6). The request ids
follow the session order; the server may answer out of order, and the client
matches each response to its request by `id`.

### 3.1 `initialize` → the negotiated version, capabilities, and `instructions`

The client negotiates `2025-11-25`. The server echoes it, advertises the `tools`
and `resources` capabilities, and carries its `serverInfo` plus the agent-workflow
`instructions` — the text the trial runs on:

```json
{ "jsonrpc": "2.0", "id": 0, "result": {
    "protocolVersion": "2025-11-25",
    "capabilities": { "tools": {}, "resources": {} },
    "serverInfo": { "name": "rust-docs-mcp", "version": "0.1.0" },
    "instructions": "<the full agent-workflow text; see §3.1 note>"
} }
```

> **Reading:** three phrases in the `instructions` pin the retrieval-versus-
> execution boundary the trial must not over-claim, and the trial asserts them
> verbatim in `the_server_states_it_provides_evidence_not_a_guarantee`: the
> server *"retrieves official Rust documentation only: it does not run commands,
> does not touch the project filesystem, and does not reach the network"*, it
> *"does not execute commands or observe their results automatically"*, and it
> *"provides evidence, **not a guarantee** that all guidance has been
> followed."* The full `instructions` text is identical to the library-design
> trial's ([`docs/trials/library-api-design.md` §3.1](library-api-design.md#31-initialize--the-negotiated-version-capabilities-and-instructions));
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

### 3.3 `get_command_guidance "cargo build"` → the resolved guidance (the trial's success path)

This is the one seed command whose full-catalog citation resolves in the
representative seed. The response carries the command's catalog entry (purpose,
documented syntax, documented examples, prerequisites, the compatibility
qualifications, and the **separately-labeled** effect notes) plus its one cited
section with its body, canonical URL, content hash, and citations. The
`result.structuredContent` is:

```json
{
  "entry": {
    "compatibility": [
      "Syntax and options are documented for the installed `cargo`; run `cargo build --help` for the version-specific option list. The selection flags `-p`/`--package`, `-w`/`--workspace`, `--target`, and `--features`/`--all-features` choose the package(s), target, and feature set to build; `--offline` resolves from the local cache only and `--locked` asserts the lockfile will not be changed.",
      "`cargo build` does not execute a program, so it has no program-argument forwarding after `--`; compiler flags are supplied through `RUSTFLAGS` or the documented build options rather than a program-argument separator."
    ],
    "effect_notes": [
      {
        "detail": "produces the final build artifacts (a binary and/or library) under `target/` — the build-output side effect that distinguishes `cargo build` from the check commands.",
        "kind": "artifact_write"
      },
      {
        "detail": "may download dependencies that are not yet cached; `--offline` (or `--frozen`) prevents any download.",
        "kind": "dependency_download"
      }
    ],
    "examples": [
      "`cargo build` compiles the current package and writes its artifacts (a binary for a `bin` target, a library for a `lib` target) under `target/`; `cargo build --release` builds an optimized release profile."
    ],
    "key": "cargo build",
    "prerequisites": [
      "A Cargo package or workspace: a manifest (`Cargo.toml`) is present in the working directory or an ancestor of it."
    ],
    "purpose": "Compile the current package, producing its binary or library artifacts.",
    "syntax": [
      "cargo build [OPTIONS]"
    ],
    "upstream_sections": [
      "cargo/commands%2Fcargo-build"
    ]
  },
  "limit": 5,
  "sections": [
    {
      "body": "The Cargo Book documents the Rust build system and package manager. `cargo build` compiles the current package (and its dependencies) for the target platform, writing artifacts under `target/`. Run `cargo <command> --help` for the full option list; the book explains each command, its flags, and their effects.",
      "canonical_url": "https://doc.rust-lang.org/cargo/commands/cargo-build.html",
      "chapter_path": "commands/cargo-build",
      "citations": [
        {
          "note": "canonical upstream document",
          "section_id": "cargo/commands%2Fcargo-build",
          "source": "cargo",
          "title": "The Cargo Book",
          "url": "https://doc.rust-lang.org/cargo/commands/cargo-build.html"
        },
        {
          "note": "command key: cargo build",
          "section_id": "cargo/commands%2Fcargo-build",
          "source": "cargo"
        },
        {
          "note": "command key: cargo test",
          "section_id": "cargo/commands%2Fcargo-build",
          "source": "cargo"
        },
        {
          "note": "qualification: Run `cargo build --help` for the authoritative, version-specific option list.",
          "section_id": "cargo/commands%2Fcargo-build",
          "source": "cargo"
        }
      ],
      "content_hash": "c128-ffa550364dc212527d904d59fe85fe84",
      "doc_title": "The Cargo Book",
      "freshness": "unknown",
      "id": "cargo/commands%2Fcargo-build",
      "snapshot_id": "rust-docs-bundled-seed",
      "source": "cargo"
    }
  ],
  "snapshot_id": "rust-docs-bundled-seed",
  "truncated": false
}
```

> **Reading:** the entry's upstream excerpts (`purpose`, `syntax`, `examples`,
> `prerequisites`, `compatibility`) and the maintainer **effect notes** occupy
> distinct fields — the effect notes are labeled *"possible effects, not
> permissions"* and carry snake-case `kind`s (`artifact_write`,
> `dependency_download`). The cited section carries its `content_hash`
> (`c128-ffa550364dc212527d904d59fe85fe84`), its `canonical_url`, its `freshness`
> (`unknown`), and citations including the canonical document, the `cargo build`
> and `cargo test` **command-key** notes, and a **qualification** note. This is
> the guidance the agent would read and then run `cargo build` on for.

### 3.4 `get_command_guidance "cargo test"` → an unresolved section (a recorded skipped config)

`cargo test` is one of the seed's three advertised command keys, but the server
resolves it against the **full** `combined()` catalog, whose `cargo test` entry
cites `cargo/commands%2Fcargo-test` (+ `rustdoc/documentation-tests`) — sections
the representative seed does **not** carry (the seed's one cargo section is
`cargo/commands%2Fcargo-build`). The server returns a typed error; it does **not**
fabricate a section:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "error": {
    "code": -32603,
    "message": "command 'cargo test' cites section 'cargo/commands%2Fcargo-test', which is not in the current index; the guidance was not fabricated — refresh the snapshot to resolve it",
    "data": { "key": "cargo test", "section": "cargo/commands%2Fcargo-test" }
  }
}
```

### 3.5 `get_command_guidance "rustup show"` → an unresolved section (a recorded skipped config)

`rustup show` is likewise advertised, but its full-catalog citation is
`rustup/index`, which the representative seed does not carry (the seed's one
rustup section is `rustup/common-install-problems`):

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "error": {
    "code": -32603,
    "message": "command 'rustup show' cites section 'rustup/index', which is not in the current index; the guidance was not fabricated — refresh the snapshot to resolve it",
    "data": { "key": "rustup show", "section": "rustup/index" }
  }
}
```

### 3.6 `get_command_guidance "cargo foobar"` → a not-in-catalog key (the catalog-boundary probe)

`cargo foobar` is **not** a catalog key. The server refuses it with a typed
error and the catalog's full set of **supported** keys as discoverable
alternatives — proving the key is a documentation *selector*, never a shell line:

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "error": {
    "code": -32002,
    "message": "no command guidance for 'cargo foobar'; available commands: [cargo new, cargo init, cargo metadata, cargo locate-project, cargo tree, cargo add, cargo remove, cargo update, cargo generate-lockfile, cargo fetch, cargo check, cargo build, cargo run, cargo test, cargo bench, cargo fmt, cargo clippy, cargo doc, cargo fix, cargo package, cargo publish, rustup show, rustup toolchain, rustup override, rustup component, rustup target, rustc, rustfmt, rustdoc]",
    "data": {
      "requested": "cargo foobar",
      "supported": ["cargo new", "cargo init", "cargo metadata", "cargo locate-project", "cargo tree", "cargo add", "cargo remove", "cargo update", "cargo generate-lockfile", "cargo fetch", "cargo check", "cargo build", "cargo run", "cargo test", "cargo bench", "cargo fmt", "cargo clippy", "cargo doc", "cargo fix", "cargo package", "cargo publish", "rustup show", "rustup toolchain", "rustup override", "rustup component", "rustup target", "rustc", "rustfmt", "rustdoc"]
    }
  }
}
```

> **Reading:** the `cargo foobar` refusal (JSON-RPC `-32002`,
> `RESOURCE_NOT_FOUND`, pre-`2026-07-28`) names the request and the full
> supported-key set. That supported list — which *includes* `cargo build`,
> `cargo test`, and `rustup show` — is the trial's evidence that those three are
> the catalog's advertised command keys, of which only `cargo build`'s cited
> section is present in the representative seed.

---

## 4. Retrieved command guidance and canonical citations (AC1)

The trial records the *actual* retrieved guidance for the one command that
resolves — `cargo build` — and the typed refusals for the two advertised commands
that do not. The `cargo build` entry's fields, verbatim from §3.3:

| Field | Value |
|-------|-------|
| `entry.key` | `cargo build` |
| `entry.purpose` | `Compile the current package, producing its binary or library artifacts.` |
| `entry.syntax` | `cargo build [OPTIONS]` |
| `entry.examples` | `cargo build` compiles the current package …; `cargo build --release` builds an optimized release profile. |
| `entry.prerequisites` | a manifest (`Cargo.toml`) is present in the working directory or an ancestor of it |
| `entry.compatibility` | (two qualifications, incl. "run `cargo build --help` for the version-specific option list"; selection flags `-p`/`-w`/`--target`/`--features`/`--all-features`; `--offline`/`--locked`; no program-argument forwarding after `--`) |
| `entry.effect_notes` | `artifact_write` — produces the final build artifacts under `target/`; `dependency_download` — may download uncached dependencies |
| `sections[0].id` | `cargo/commands%2Fcargo-build` |
| `sections[0].source` | `cargo` |
| `sections[0].doc_title` | `The Cargo Book` |
| `sections[0].content_hash` | `c128-ffa550364dc212527d904d59fe85fe84` |
| `sections[0].canonical_url` | `https://doc.rust-lang.org/cargo/commands/cargo-build.html` |
| `sections[0].freshness` | `unknown` (the seed has never checked upstream) |
| `sections[0].citations[0]` | `note: "canonical upstream document"`, `title: "The Cargo Book"`, `url: https://doc.rust-lang.org/cargo/commands/cargo-build.html` |
| `sections[0].citations[1]` | `note: "command key: cargo build"` |
| `sections[0].citations[2]` | `note: "command key: cargo test"` |
| `sections[0].citations[3]` | `note: "qualification: Run \`cargo build --help\` for the authoritative, version-specific option list."` |

**The "actual commands / exit statuses / fixes" the agent records (honestly):**
because the server does not execute commands, the trial records — for each
command — the guidance retrieved, the command the agent *would* run through its
own terminal, and the *retrieval's* outcome (a JSON-RPC `result` or `error`),
**not** the exit status of a command the server ran.

| Command | Retrieved guidance | What the agent would run (its own terminal) | Retrieval outcome the trial records | Fix |
|---------|-------------------|---------------------------------------------|-------------------------------------|-----|
| `cargo build` | full entry + the `cargo/commands%2Fcargo-build` section (§3.3) | `cargo build --release` (build), then `cargo build` | **Resolved** — `result` with entry + section + citations | none |
| `cargo test` | *not retrievable* on the seed | `cargo test` (once its guidance resolves) | **Unresolved** — `error` `-32603`, `section: cargo/commands%2Fcargo-test` | **refresh the snapshot** (`sync`) so `cargo/commands%2Fcargo-test` (+ `rustdoc/documentation-tests`) is present |
| `rustup show` | *not retrievable* on the seed | `rustup show` / `rustup show --active-toolchain` | **Unresolved** — `error` `-32603`, `section: rustup/index` | **refresh the snapshot** (`sync`) so `rustup/index` is present |
| `cargo foobar` | *not in the catalog* | — (nothing to run) | **Unknown key** — `error` `-32002` with the supported-key list | pick a supported key from the `supported` array |

The trial does **not** assert that `cargo build`/`cargo test`/`rustup show`
*ran*; it asserts the guidance was *retrieved* (where resolvable) and *refused*
(without fabrication) where it was not. Execution is the agent's own out-of-trial
step.

---

## 5. Findings — success achieved, limits recorded

**Success scenario achieved (plan §1, implementation/review half).** The agent's
retrieval session *can* reach the guidance for an applicable build command and
cite the original section: `get_command_guidance "cargo build"` returns the
command's catalog entry — its purpose, documented syntax, examples,
prerequisites, the compatibility qualifications, and the separately-labeled
effect notes — together with its cited section's body, `canonical_url`,
`content_hash`, `freshness`, and citations. The agent reads the guidance and
would run `cargo build` through its own terminal, as the success scenario
describes.

**The documentation-assistance-vs-execution distinction (AC2), recorded not
over-claimed.** The trial makes the boundary explicit and asserts it:

- **The server retrieves documentation; the agent executes.** The
  `initialize` `instructions` state verbatim that the server *"retrieves official
  Rust documentation only: it does not run commands, does not touch the project
  filesystem, and does not reach the network"* and *"does not execute commands or
  observe their results automatically."* The trial records the *retrieval's*
  JSON-RPC result/error, never the exit status of a command it ran. No real
  project command was executed in this trial.
- **No fabricated guidance; unsupported / skipped configurations reported.**
  Of the seed's three advertised command keys, **exactly one** (`cargo build`)
  resolves on the representative seed; the other two (`cargo test`,
  `rustup show`) return a **typed `UnresolvedSection` error** (`-32603`) naming
  the exact missing section and the fix (*"the guidance was not fabricated —
  refresh the snapshot to resolve it"*). These are the trial's *skipped /
  unsupported configurations*: the seed advertises the keys but does not carry
  the full catalog's cited sections, so the guidance is unavailable until the
  snapshot is refreshed. The trial records this honestly rather than pretending
  the commands resolved.
- **The catalog boundary is a selector, not a shell line.** A not-in-catalog key
  (`cargo foobar`) is refused with a typed `UnknownKey` error (`-32002`) and the
  full supported-key list; no shell operator is ever interpolated into a process
  (the [053] catalog validates the key at the `CommandKey` boundary, and a
  module-level source scan asserts no process / filesystem / network primitive
  appears in `get_command_guidance`).
- **Not automatically correct.** The trial demonstrates that the MCP
  *retrieves* the guidance and *returns its citations, compatibility
  qualifications, and effect notes* — it does **not** demonstrate that a command
  run on the strength of that retrieval is correct or side-effect-free. The
  effect notes describe *possible* effects and are not permission controls, and
  per [§11](../../plan.md#11-technical-decisions--risks-and-unknowns) and the
  server's own `instructions`, the MCP *"provides evidence, not a guarantee that
  all guidance has been followed."*

**Retrieval limits recorded (AC2), not asserted away:**

1. **Two of three advertised commands do not resolve on the representative seed.**
   `cargo test` (cites `cargo/commands%2Fcargo-test` + `rustdoc/documentation-
   tests`) and `rustup show` (cites `rustup/index`) fail with `UnresolvedSection`
   because the representative seed ships one section per collection
   (`cargo/commands%2Fcargo-build`, `rustup/common-install-problems`, …), not the
   full catalog's cited sections. This is a corpus-coverage limit of the *seed*,
   not the tool: a real `sync` of the upstream Cargo/rustup books would resolve
   them. The trial records the typed errors as the honest, non-fabricated
   outcome.
2. **Freshness is `unknown`.** The seed's `as_of`/`last_checked`/
   `upstream_modified` are all `unknown` (the representative seed has never
   checked upstream). The trial reports this verdict honestly; it does not claim
   the guidance is current.
3. **Compatibility is qualified, not absolute.** The `cargo build` guidance
   carries the qualification *"run `cargo build --help` for the version-specific
   option list"* — the agent must confirm version-specific options through its
   own terminal before relying on a version-specific claim. The trial records the
   qualification, not an unconditional claim.

---

## 6. How to reproduce

The session is encoded as a single test that drives the same spawned-stdio call
sequence over the bundled seed and asserts the retrieved guidance, the citations,
the compatibility qualifications and effect notes, and the typed refusals — so
the trial is **reproducible, not hand-claimed**:

```sh
cargo test --test trial_project_implementation
```

It reuses `tokio` + `serde_json` (the existing
[`tests/first_client.rs`](../../tests/first_client.rs) seam), adds **no**
dependency, and runs **offline**. To dump the exact request/response transcript
to a file (and diff it against §3):

```sh
RUST_DOCS_MCP_TRIAL_TRANSCRIPT=/tmp/project_trial.json \
  cargo test --test trial_project_implementation -- \
    --exact the_project_implementation_trial_retrieves_command_guidance_and_reports_skipped_configs
jq . /tmp/project_trial.json
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
  '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"project-implementation-agent","version":"0.0.1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_command_guidance","arguments":{"command":"cargo build"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_command_guidance","arguments":{"command":"cargo test"}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_command_guidance","arguments":{"command":"rustup show"}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_command_guidance","arguments":{"command":"cargo foobar"}}}' \
  | target/release/rust-docs-mcp serve --data-dir /tmp/rust-docs-mcp-069

# then close stdin (Ctrl-D); the server self-exits with no stdout traffic.
```

The responses match the transcript in §3: `cargo build` resolves; `cargo test`
and `rustup show` return `-32603` `UnresolvedSection` errors; `cargo foobar`
returns a `-32002` `UnknownKey` error with the supported-key list. A clean client
disconnect makes the server exit `0` with **no stdout protocol traffic** (the
diagnostics go to stderr).

---

## 8. Verification

| Check | Result |
|-------|--------|
| `cargo test --test trial_project_implementation` | the trial passes — `cargo build` retrieval + citations + qualifications + effect notes asserted; the two unresolved and the not-in-catalog refusals asserted; clean disconnect |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | clean, 0 warnings |
| Network in the trial | none — the bundled snapshot loads in-memory; an empty `--data-dir` keeps the host cache untouched |
| Stdout discipline | the trial asserts every stdout line is a JSON-RPC 2.0 message (including the `error`-bearing responses); the server's stderr carries the diagnostics only |
| Command execution | **none** — the trial is offline and hermetic; no project command was run. The "commands" are the guidance the agent would run through its own terminal |

> The full workspace test suite is **not** re-run for this task (it is already
> green and this change adds a self-contained trial test + a docs file; the known
> parallel-only flakiness in `ingest`/`writer_lock`/`corpus` is unrelated and
> passes in isolation). The trial's own assertions and `fmt`/`clippy` are the
> targeted checks. No behavior, dependency, MSRV, or `unsafe` change is made.

---

## 9. Files touched by this task

| File | Change |
| --- | --- |
| `docs/trials/project-implementation.md` | **Created** — this record of the project-implementation agent trial: the session, the real transcript, the retrieved `cargo build` guidance + citations, and the documented skipped/unsupported configurations and the documentation-assistance-vs-execution distinction. |
| `tests/trial_project_implementation.rs` | **Created** — the focused, hermetic, offline proof of the trial: `cargo build` resolves with citations/qualifications/effect notes; `cargo test`/`rustup show` return typed `UnresolvedSection` errors; `cargo foobar` returns a typed `UnknownKey` error; with `RUST_DOCS_MCP_TRIAL_TRANSCRIPT` transcript capture. No new dependency; no network. |
