# Rust / SDK / Protocol Compatibility Baseline

**Task:** [001] M1 — Record the Rust, SDK, and protocol compatibility baseline (issue #1).
**Status:** Baseline recorded; **review round of 2026-10-29 addressed** (SEP-2164 error-code attribution corrected; discovery SEPs added; PR matrix refreshed) — **re-review pending.** No Cargo package exists yet — this
document is the authoritative record that task [002] consumes when it creates
`Cargo.toml` / `Cargo.lock` / `rust-toolchain.toml`.
**Verified:** 2026-10-29, on `rustc`/`cargo` 1.99.1 (stable `aarch64-apple-darwin`),
`rustup` 1.29.2. Every version and claim below was checked with the commands
listed in [Evidence](#8-evidence). Unverifiable items are marked `UNVERIFIED`;
nothing was guessed.
**Re-verified after review (2026-10-29):** the `KNOWN_VERSIONS` omission of
`V_2024_11_05`, the `tokio` `time`/feature-gate detail, the `reqwest`
`default-features = false` TLS note, and the `scraper` license status were each
re-checked against `rmcp` @ `rmcp-v3.8.0` (see [Evidence](#8-evidence)).

> Sources of truth used: `plan.md` §4 (core choices) and §10/M1; the MCP
> specification 2026-07-28 (`/specification/latest` also resolves to
> 2026-07-28, 2026-10-29); the official [rmcp Rust SDK](https://github.com/modelcontextprotocol/rust-sdk).

---

## 1. Headline decisions

| Concern | Record | Confidence |
| --- | --- | --- |
| MCP SDK | `rmcp` **3.8.0** (latest release; Apache-2.0; MSRV 1.88) | Confirmed |
| SDK macro twin | `rmcp-macros` **3.8.0** (pulled transitively via `rmcp` `macros`/`server`) | Confirmed |
| Rust toolchain | **stable** (current `rustc`/`cargo` 1.99.1; `rustup` 1.29.2) | Confirmed |
| Edition | **2024** | Confirmed — the SDK itself ships as edition 2024 |
| MSRV | **1.88** (equals the binding floor: `rmcp`/`rmcp-macros` `rust-version`) | Confirmed |
| Protocol(s) served | `KNOWN_VERSIONS` = `2024-11-05` *(oldest)*, `2025-03-26`, `2025-06-18`, `2025-11-25`, **`2026-07-28` (latest)**; SDK negotiates highest common | Confirmed |
| Spec reference | MCP **2026-07-28** (current latest spec) | Confirmed |
| First supported client | **rmcp's built-in in-process client / SDK test harness** | Confirmed |
| Real-world clients | Listed in §7 as **STILL TO TEST** (untested) | To-test |

---

## 2. Rust toolchain baseline

Confirmed environment (this machine, 2026-10-29):

```text
rustc 1.99.1 (58cf8f9c3 2026-10-13)   [stable-aarch64-apple-darwin, active, default]
cargo 1.99.1 (5f94df478 2026-08-27)
rustup 1.29.2 (2026-10-21)
installed target: aarch64-apple-darwin (+ x86_64-pc-windows-gnu)
```

| Item | Value | Note |
| --- | --- | --- |
| Channel | `stable` | `plan.md` §4: "Use stable Rust … do not depend on an unpinned Git branch." `rust-toolchain.toml` pins `channel = "stable"` (see §9); the exact patch version is left to the toolchain, not hardcoded. |
| Current stable | `1.99.1` | Recorded for provenance; not a hard pin. |
| Edition | **2024** | The SDK workspace declares `edition = "2024"` and builds on MSRV 1.88, so the whole dependency set supports it. Edition 2024 stabilized in Rust 1.85; our MSRV 1.88 ≥ 1.85. |
| MSRV | **1.88** | Set to the **binding** floor in our dependency set, which is `rmcp`/`rmcp-macros` `rust-version = 1.88`. All other core deps are comfortably below it (§3), so 1.88 is the correct, non-inflated floor. A "sensible stable floor" that still lets the SDK compile. |

MSRV rule (enforced later by task [005]/[061]): the project MSRV is always the
**maximum** `rust-version` of our direct dependencies. As of this baseline that
maximum is 1.88; if any pinned dependency raises its `rust-version`, our MSRV
must move up with it.

---

## 3. SDK and dependency version matrix

"Floor" = the minimum version the SDK manifest requires (from
`crates/rmcp/Cargo.toml` @ `rmcp-v3.8.0`). "Latest" = newest release observed
via `cargo info` on 2026-10-29. MSRV is each crate's `rust-version`.

| Crate | Intended pin (floor) | Latest observed | MSRV | License (crates.io) | Why it is in the set |
| --- | --- | --- | --- | --- | --- |
| `rmcp` | `3.8.0` | **3.8.0** | **1.88** | Apache-2.0 | Official MCP Rust SDK — protocol, schema, lifecycle, stdio transport. |
| `rmcp-macros` | `3.8.0` | **3.8.0** | **1.88** | Apache-2.0 | Tool/schema derive macros; twin version, pulled by `rmcp` `macros`+`server`. |
| `tokio` | `1` (rmcp uses `1`) | **1.54.1** | 1.85 | MIT | Async runtime; rmcp's **direct** dependency declares `features = ["sync", "macros", "rt", "time"]`. `io-util`/`io-std`/`net`/`process` are enabled **only** through the transport feature gates (`transport-async-rw` → `io-util`/`codec`, `transport-io` → `io-std`/`net`, `process-wrap` → `process`), not the base dependency. |
| `serde` | `1` | **1.0.229** | 1.56 | MIT OR Apache-2.0 | Corpus + tool payloads; `derive` + `rc` (rmcp requires these). |
| `serde_json` | `1` | **1.0.154** | 1.71 | MIT OR Apache-2.0 | JSON (de)serialization and MCP message payloads. |
| `schemars` | `1` (rmcp floor `1.0`; `chrono04` feature for the `server` feature) | **1.2.3** | 1.74 | MIT | **SDK-compatible JSON-Schema crate.** The `rmcp` `server` feature enables `dep:schemars`; tool/resource schemas are built with it. Major must be `1.x`. |
| `reqwest` | `0.13` (rmcp floor `0.13.2`, `default-features=false`, `json`+`stream`) | **0.13.5** | **1.85** | MIT OR Apache-2.0 | Bounded HTTP client for the `sync` ingestion path. TLS is **rustls**: rmcp sets `default-features = false` (so reqwest's own defaults — `default-tls`, `charset`, `http2`, `system-proxy` — are all **off**) and re-enables `reqwest?/rustls` via the `reqwest` feature. No system TLS dependency; offline-friendly. `http2`/`system-proxy` are **not** active in rmcp's usage. |
| `scraper` | `0.29` | **0.29.0** | **UNVERIFIED** — `rust-version` not declared on the crate | **ISC** (crates.io; task [006]) | HTML parser for `books.rs` article extraction. crates.io authoritatively declares the license **`ISC`** (`cargo info scraper`, 2026-10-29), so task [006] may record it confidently; the repo's `LICENSE` SPDX id is still unresolved via the GitHub API and remains part of the audit. Must be validated to build on MSRV 1.88 in task [002]. |
| `thiserror` | `2` | **2.0.21** | **1.77** | MIT OR Apache-2.0 | Typed error layer (`AppError`, task [004]); already a direct transitive dependency of `rmcp` and now a pinned direct dependency. |
| `tracing` | `0.1` | **0.1.44** | **1.65** | MIT | Structured-log facade behind the stderr-only, gated diagnostics (task [004]). |
| `tracing-subscriber` | `0.3` (`env-filter`) | **0.3.23** | **1.65** | MIT | Stderr, configurable-verbosity subscriber + the `RUST_LOG`/level base (task [004]). |

**Pinned by later M1 tasks (direct, resolved in `Cargo.lock`):** `clap` `4.6.7`
(CLI skeleton, task [003]) and the `thiserror` `2` / `tracing` `0.1` /
`tracing-subscriber` `0.3` diagnostic + typed-error closure (task [004]) are now
**direct, pinned** dependencies — their resolved versions and MSRVs are recorded in
the matrix above, all comfortably under the 1.88 floor. The remaining deferred
as-needed item is `sha2` / hashing (snapshot integrity — `rmcp` already pulls
`sha2` `0.11` under `request-state`; reuse its version), still named in
`plan.md` §8 but not yet a direct dependency. All are pulled by `rmcp` or are the
minimal choice named in `plan.md` §8.

**MSRV impact summary:** the binding constraint is `rmcp`/`rmcp-macros` at **1.88**.
Everything else is ≤ 1.85 (reqwest/tokio), 1.77 (thiserror), 1.74 (schemars), 1.71
(serde_json), 1.65 (tracing / tracing-subscriber), 1.56 (serde). `scraper`'s undeclared MSRV is the one open item: record it and
confirm it compiles on 1.88 during task [002] bootstrap. No dependency forces an
MSRV above 1.88.

**Feature selection for the stdio server (intended, refined in task [004]/[048]):**
`rmcp` with at least `server` + `macros` (tools/resources/handlers) and `client`
for the in-process test client. The heavy `transport-streamable-http-*` / `auth`
/ `server-side-http` features are **not** needed for a local stdio MVP and should
stay off by default to keep the dependency closure small and fast to build.

---

## 4. Protocol versions

**Spec:** MCP **2026-07-28** is the current latest specification — on 2026-10-29
`https://modelcontextprotocol.io/specification/latest` redirects to
`/specification/2026-07-28` (HTTP 200). This matches the plan's reference and
aligns with `rmcp`'s `LATEST`.

**What the chosen SDK supports** (`rmcp` `3.8.0`, `src/model.rs`):

```text
ProtocolVersion::V_2024_11_05 = "2024-11-05"   // oldest; still has an `initialize` handshake
ProtocolVersion::V_2025_03_26 = "2025-03-26"
ProtocolVersion::V_2025_06_18 = "2025-06-18"
ProtocolVersion::V_2025_11_25 = "2025-11-25"
ProtocolVersion::V_2026_07_28 = "2026-07-28"   // = LATEST, = STANDARD_HEADERS, = NO_INITIALIZE
ProtocolVersion::KNOWN_VERSIONS = [ V_2024_11_05, V_2025_03_26, V_2025_06_18, V_2025_11_25, V_2026_07_28 ]  // 5 elements
```

`KNOWN_VERSIONS` has **5** elements (`2024-11-05` is the oldest, `2026-07-28` the
latest). The server must advertise the full list; omitting `2024-11-05` would
make a client negotiating that version fail to connect. `2024-11-05` still has an
`initialize` handshake because it sits below `NO_INITIALIZE`.

**What the server will negotiate:** the SDK's default
`Service::supported_protocol_versions()` returns the **full `KNOWN_VERSIONS`**
list, so the server advertises every supported version and negotiates the
**highest common version** with the client. This honors `plan.md` §4 ("Let the
SDK handle version-specific discovery or initialization rather than inventing a
custom handshake") — we do **not** hardcode a single version.

**Critical behavior to carry into testing (task [003]/[057]/[058]):**
- **`2026-07-28` has no `initialize` handshake.** `ProtocolVersion::has_initialize()`
  is `true` for `2024-11-05` through `2025-11-25` (the whole `Known` list up to
  and including `LATEST_WITH_INITIALIZE = V_2025_11_25`, `2024-11-05` included) and
  `false` at/after `2026-07-28` (`NO_INITIALIZE = V_2026_07_28`; see `model.rs:218`).
  Newer clients therefore use **discovery** instead of the `initialize` handshake: the
  sessionless / per-request `_meta` model is **SEP-2567** (*"Sessionless MCP via Explicit
  State Handles"*, `model.rs:180,218`), which is complementary to **SEP-2575** (*"Make MCP
  Stateless"*, per SEP-2567 the one that drops `initialize`); the standard HTTP headers at
  this boundary are **SEP-2243** (*"HTTP standardization"*, `model.rs:176`).
  The SDK client observation is verbatim: *"2026-07-28 has no ping, and a cancelled send
  could leave a partial frame."*
- **`2026-07-28` changes error responses.** The server upgrades the
  **resource-not-found error code** from `-32002` to `INVALID_PARAMS` (`-32602`) for
  peers negotiating `2026-07-28` or newer, per **SEP-2164** (*"Standardize Resource Not
  Found Error Code"*, Final) — recorded in `rmcp` @ `rmcp-v3.8.0` at `model.rs:682`
  (the `ResourceNotFound`/`-32002` note). Error mapping therefore shifts at that boundary;
  Task [048] must map errors version-aware. *(The optional/omitted error-response `id` claim
  previously attributed here to SEP-2164 was removed as unsupported: SEP-2164 concerns only
  the error-code upgrade, and `rmcp` v3.8.0 cites no SEP/spec for an omittable error `id`
  at this boundary.)*

Consequences for our MVP: we must validate **both** a discovery-style client
(2026-07-28) and an `initialize`-style client (`2024-11-05`…`2025-11-25`, with the
`2024-11-05` floor), and error-mapping must branch on the negotiated version.

---

## 5. First supported client — CONFIRMED vs STILL TO TEST

Per `plan.md` §7: *"No client-specific integration is required to begin
development; the SDK test client establishes the initial contract."*

### 5.1 CONFIRMED baseline (establishes the initial contract)

- **rmcp's built-in in-process MCP client / SDK test harness** — the
  `rmcp::service::serve_client` / `ClientSession` used by the SDK's own
  integration tests (e.g. `crates/rmcp/tests/test_protocol_version_negotiation.rs`,
  `test_client_lifecycle_modes.rs`, `test_close_connection.rs`, `test_abandoned_request_cancel.rs`).
- **Why confirmed:** it is the *same library we run the server from*, so a client↔server
  round-trip over an in-memory `async-stdio` stream is self-validating with **no
  network and no external process**. This is the client for task [003]
  "Prove the stdio lifecycle with the official SDK client" and the M1 exit
  ("a client can connect to the server and disconnect cleanly; no diagnostic
  output contaminates stdout").
- **Status:** Confirmed as the contract-establishing client for development. It is
  a *unit of the SDK*, not an end-user client, so it proves protocol/lifecycle
  behavior — it does **not** prove that a real external agent can drive the server.

### 5.2 STILL TO TEST (explicitly untested — no claim of working support)

These are real-world/external clients that a user would actually launch. None has
been exercised against this baseline yet; each moves to "tested" in tasks
[058] (protocol/first-client matrix) / [064] (document & verify the first
client config). Candidates for the first external client:

1. **An rmcp-based example/standalone client binary launched over real stdio**
   (a separate process). Same SDK, but a real spawn + real EOF/shutdown path —
   still to run. *Status: STILL TO TEST.*
2. **A third-party agent/client that launches an MCP server over stdio** (e.g.
   a general-purpose coding agent with an MCP client, or the MCP reference
   client). The negotiated version may be an `initialize`-style one
   (`2024-11-05`…`2025-11-25`, with `2024-11-05` the floor) *or* a 2026-07-28
   discovery one — must be measured, not assumed. *Status: STILL TO TEST.*
3. **A second, independent runtime client** (a non-rmcp MCP implementation, e.g.
   TypeScript/Python-based) to catch any rmcp-specific assumptions.
   *Status: STILL TO TEST.*

Until §5.2 items pass a real spawn/EOF/cancel/exit-code run, the external-client
compatibility claim remains unverified. The confirmed contract (§5.1) is sufficient
to begin implementation.

---

## 6. HTML→Markdown approach (recorded intent, not a pin)

`plan.md` §5.4 / §8 / §12: pick the converter by **testing preservation of real
upstream examples** (headings, anchors, lists, tables, fenced Rust code,
`stability` callouts, relative links), not by version. Candidate: **`html2md`
`0.2.17`** (library) driving `scraper`'s DOM; a hand-rolled DOM→Markdown on
`scraper` is the fallback. Decision and version are made in task [013]
"Extract article content into faithful Markdown" and verified in [032].
**UNVERIFIED for our MSRV/edition** — validate during task [002] before pinning.

---

## 7. Decision record / assumptions

1. **Pin the latest stable rmcp release (3.8.0), not a git branch.** Satisfies
   `plan.md` §4 and AGENTS "do not add unpinned Git dependencies." rmcp's latest
   is a tagged release `rmcp-v3.8.0` (published 2026-10-26).
2. **MSRV = 1.88 = the dependency maximum.** Defensible because it is the
   binding floor (`rmcp`/`rmcp-macros` `rust-version`); nothing in the set forces
   higher, and it is a real stable toolchain.
3. **Edition 2024 is safe** because the SDK itself is edition 2024 / MSRV 1.88.
4. **Server advertises all `KNOWN_VERSIONS` (5 elements, `2024-11-05` … `2026-07-28`) and negotiates** — we do not invent a
   handshake. This means tests must cover both `initialize` (`2024-11-05` … `2025-11-25`)
   and discovery (`2026-07-28`) paths.
5. **Default HTTP client TLS = rustls** — rmcp sets `default-features = false` and
   re-enables `reqwest?/rustls` (disabling reqwest's own `http2`/`system-proxy`/`charset` defaults),
   so `sync` has no system-TLS coupling and the default build stays reproducible.
6. **`scraper` MSRV is undeclared** — carried as a risk into task [002]
   (build-time check); license exactness carried into task [006] audit. These are
   the two explicit "unknown" items; everything else is confirmed.
7. **Only `docs/COMPATIBILITY.md` + `rust-toolchain.toml` (+ the `plan.md` M1
   checkbox pointer) are created here.** `Cargo.toml`, `Cargo.lock`, `src/` are
   intentionally **not** created — that is task [002].

---

## 8. Evidence

Checked 2026-10-29 with the commands below. All remote URLs returned HTTP 200.

**Version confirmation (crates.io index via cargo, which sets a valid User-Agent —
the raw anonymous crates.io API returns 403 without one):**
- `cargo info rmcp` → `version: 3.8.0`, `license: Apache-2.0`, `rust-version: 1.88`,
  `homepage/repository: github.com/modelcontextprotocol/rust-sdk`, and the full
  feature/`schemars`/`reqwest` closure quoted in §3.
- `cargo info rmcp-macros` → `3.8.0`, `rust-version: 1.88` (twin).
- `cargo info tokio / serde / serde_json / schemars / reqwest / scraper` → latest
  versions and MSRVs in §3.
- `cargo search html2md / clap` → `html2md 0.2.17`, `clap 4.6.7` (§6, §3 as-needed).
- `rustc/cargo/rustup --version` and `rustup show` → §2 environment.

**rmcp release + source (GitHub API, tag `rmcp-v3.8.0`, published
`2026-10-26T22:04:00Z`):**
- `gh api repos/modelcontextprotocol/rust-sdk/tags` → latest tag `rmcp-v3.8.0`;
  full history `rmcp-v3.0.0 … rmcp-v3.8.0`.
- `crates/rmcp/Cargo.toml` @ `rmcp-v3.8.0` → floors: `serde 1`, `serde_json 1`,
  `tokio 1`, `schemars 1.0` (+`chrono04`), `reqwest 0.13.2`
  (`default-features=false`, `json`+`stream`, `rustls` via the `reqwest`
  feature), `thiserror 2`, `rmcp-macros = { workspace }` (= 3.8.0). Features:
  `default = [base64, macros, server]`; `server = [transport-async-rw, schemars,
  pastey, uuid]`.
- Root `Cargo.toml` @ `rmcp-v3.8.0` → `[workspace.package]` `edition = "2024"`,
  `rust-version = "1.88"`.
- `crates/rmcp/src/model.rs` @ tag → `ProtocolVersion` constants table in §4.
- `crates/rmcp/src/service.rs` → `default supported_protocol_versions()` returns
  `KNOWN_VERSIONS` (§4 negotiation).
- `crates/rmcp/src/service/client.rs` → 2026-07-28 no-ping /
  negotiate-≥`V_2026_07_28` behavior (§4).

**Spec (HTTP 200 on 2026-10-29):**
- `https://modelcontextprotocol.io/specification/2026-07-28` → 200;
  `.../latest` → 302 redirect to `.../2026-07-28` (current spec).
- Spec URL referenced by the plan and by rmcp source comments
  (`.../2026-07-28/basic#error-responses`, `.../2026-07-28/schema`).

**docs.rs pages (HTTP 200 on 2026-10-29):**
- `docs.rs/rmcp/3.8.0`, `docs.rs/rmcp-macros/3.8.0`, `docs.rs/schemars/1.2.3`.

**Unverified / unknown (honest limits):**
- `scraper` `rust-version` is **not declared** on the crate → MSRV UNVERIFIED;
  build-time check owed to task [002].
- `scraper` license: crates.io authoritatively declares **`ISC`** (confirmed via
  `cargo info scraper`, 2026-10-29), so task [006] may record it as `ISC`
  confidently. The repo's `LICENSE` SPDX id is **still unresolved** via the GitHub
  API — `GET /repos/rust-scraper/scraper/license` returns `spdx_id: null`
  (2026-10-29) — so that specific audit item remains owed, but the crates.io
  SPDX id is not.
- `KNOWN_VERSIONS` was re-verified against `crates/rmcp/src/model.rs` @
  `rmcp-v3.8.0` (2026-10-29): it has **5** elements incl. `V_2024_11_05 = "2024-11-05"`
  (lines 202–208); `has_initialize()` is `true` for `2024-11-05…2025-11-25` and
  `false` from `NO_INITIALIZE = V_2026_07_28` (line 228–229). rmcp's direct
  dependencies re-confirmed: `tokio` `features = ["sync","macros","rt","time"]`
  and `reqwest` `0.13.2, default-features = false, features = ["json","stream"]`
  with `reqwest` feature = `reqwest?/rustls`.
- SEP-citation corrections (2026-10-29, after reviewer findings on this PR): the SEP
  references in §4 were re-checked by pulling `rmcp` `3.8.0` from crates.io and the MCP
  SEPs from `seps/`. Confirmed: `model.rs:682` is the SEP-2164 resource-not-found code
  upgrade (`-32002` → `-32602`/`INVALID_PARAMS`; SEP-2164 *"Standardize Resource Not Found
  Error Code"*, Final), `model.rs:176` is SEP-2243 standard HTTP headers, and
  `model.rs:180,218` are SEP-2567 (per-request `_meta` / sessionless; complementary to
  SEP-2575 *"Make MCP Stateless"*). The earlier "request `id` is optional/omitted …
  (SEP-2164)" claim in §4 was **removed** as unsupported — no SEP or spec reference in
  `rmcp` v3.8.0 attributes an omittable error `id` to `2026-07-28`.
- The spec `/basic` page body is JS-rendered, so the on-page `protocol_version`
  string was not scraped directly; 2026-07-28-as-latest is established via the
  `/specification/latest` redirect (authoritative) plus rmcp's own `LATEST`
  constant.
- No external client (§5.2) has been run; only the in-process SDK client is
  confirmed.
- No Cargo build/`cargo check` was run — no package exists yet (that is
  task [002]). These entries are manifest/internet evidence, not a compile.

---

## 9. Files touched by this task

| File | Change |
| --- | --- |
| `docs/COMPATIBILITY.md` | **Created** — this authoritative baseline record. |
| `rust-toolchain.toml` | **Created** — records the `channel = "stable"` pin; exact patch version intentionally left to the toolchain. |
| `plan.md` | M1 first checklist item marked `[x]` with a pointer to this doc. All other content unchanged. |
| `Cargo.toml`, `Cargo.lock`, `src/` | **Not created** — belongs to task [002]. |
