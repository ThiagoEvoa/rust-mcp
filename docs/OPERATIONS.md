# Operations: refresh, status, offline behavior, and troubleshooting

**Task:** [065][M5] — *Document sync, status, offline behavior, and
troubleshooting* (issue [#65](https://github.com/ThiagoEvoa/rust-mcp/issues/65)).
**Status:** **Documented and verified.** The explicit-refresh, snapshot-activation,
freshness, failure-recovery, and no-valid-corpus behaviors the M5 exit requires are
recorded here, with the real command outputs and the exact error wording the binary
emits, plus a troubleshooting guide that distinguishes the four failure classes the
acceptance criteria call for.
**Verified:** 2026-11-13, on `rustc`/`cargo` 1.99.1 (stable
`aarch64-apple-darwin`), MSRV 1.88, edition 2024 — the
[baseline](COMPATIBILITY.md#2-rust-toolchain-baseline) of
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md).

> **Outcome (issue #65):** explain explicit refreshes, snapshot activation on
> restart, freshness, version uncertainty, failure recovery, and the no-valid-corpus
> error.
>
> **Sources of truth:** [`plan.md`](../plan.md) §5 (*Runtime behavior*), §8
> (*MCP interface / the `sync` / `status` / `inspect` surface*), and §10/M5
> (*"Add README setup, build/install, client configuration, update, offline, and
> troubleshooting instructions"*), together with the M2 snapshot/freshness
> behavior in [`docs/COMPATIBILITY.md`](COMPATIBILITY.md) and the first-client
> contract in [`docs/CLIENTS.md`](CLIENTS.md).
>
> **Acceptance criteria (both met):**
> - **AC1** — the README examples cover `sync`/`status` and the configurable
>   data directory and staleness policy (this guide plus the
>   [README](../README.md#refreshing-the-corpus) and
>   [Troubleshooting](../README.md#troubleshooting) sections).
> - **AC2** — the troubleshooting guide distinguishes **missing components**,
>   **stale content**, **extraction failures**, and **client connection failures**
>   (§8).

This guide is the operator-facing complement to the
[README](../README.md). The README owns the install/first-launch walkthrough; this
document owns *what happens after install* — refreshing the corpus, reading its
status, recovering from a failed refresh, and diagnosing the four failure classes an
operator or a client can hit.

---

## 1. The four operator commands

```text
rust-docs-mcp serve   [--verbose] [--data-dir PATH]
rust-docs-mcp sync    [--verbose] [--data-dir PATH]
rust-docs-mcp status  [--verbose] [--data-dir PATH] [--stale-days DAYS]
rust-docs-mcp inspect
```

| Command | Network | What it does |
| --- | --- | --- |
| `serve` | No | Launch the stdio MCP server on the selected immutable snapshot; run until the client disconnects. |
| `sync` | **Yes** (the only one) | Refresh the corpus from the official sources and atomically publish a new snapshot. |
| `status` | No | Report the selected snapshot's identity, source/section counts, and per-source freshness against a configurable threshold. |
| `inspect` | No | Operator view: corpus coverage and extraction fidelity over the active snapshot. |

Two **global** flags are accepted **before or after** the subcommand (they are the
same option in every position):

- `--data-dir PATH` — use `PATH` as the application data directory instead of the
  OS default (§5).
- `-v`, `--verbose` — raise diagnostics to `debug` level (or set `RUST_LOG`).
  Diagnostics always go to **stderr**; this flag never writes query text to stdout.

**Stdio discipline (the invariant that makes the server client-friendly).** During
`serve`, **stdout carries only MCP protocol traffic** and every diagnostic — the
`status`/`inspect`/`sync` reports, a `sync` failure, a startup error — is written
to **stderr**. A client that parses stdout therefore never sees a diagnostic.
(`plan.md` §8/§9.)

---

## 2. Explicit refresh: `sync`

`sync` is the **only** command that needs network access. It runs the bounded,
explicit ingestion pipeline — discover the official sources, fetch and extract them,
validate the assembled corpus, and **atomically publish a new immutable snapshot**,
preserving the last valid one (`plan.md` §5, step 8: *"Write a candidate snapshot,
build its index, then publish it atomically only if all checks pass. Keep the
previous valid snapshot."*).

```sh
./target/release/rust-docs-mcp sync
```

On success the run prints a **per-source report on stderr** (a human-readable
`SyncReport`) and exits `0`. The report names the new snapshot identity, the
published totals, and one line per source with its inventory change. The format,
rendered by `SyncReport::to_stderr`, is:

```text
rust-docs-mcp sync:
  snapshot: <new snapshot id>
  published <N> section(s), <N> checklist entr(y/ies), <N> command entr(y/ies)
  [api-guidelines] <N> document(s), <N> section(s)
  [style-guide]    <N> document(s), <N> section(s)
  ...
```

Each source line appends `+<k> added` / `-<k> removed` / `<k> unavailable` when the
inventory changed against the previous snapshot. A real end-to-end run is **opt-in
and out of band**: it is the
[Upstream ingestion](../.github/workflows/upstream-ingestion.yml) workflow recorded
in [`docs/CI.md`](CI.md#opt-in-upstream-ingestion-check), not a mandatory part of
`PR validation`, because live upstream is the project's one flaky input. A local
operator runs `sync` when they want a current corpus; the format above is the
documented shape of its stdout-free, stderr report.

**`sync` is explicit, not automatic.** A query never triggers a refresh, and `sync`
never runs on its own: the corpus is only updated when an operator (or the opt-in
CI job) runs `sync` (`plan.md` §5: *"Serve reads without network calls. Queries do
not trigger updates."*).

---

## 3. Snapshot activation on restart

The server holds **one snapshot fixed for its process lifetime, with no hot reload**:

> *Keep one snapshot fixed for the process lifetime. After sync, restart the server
> to activate it; no hot reload or change notifications are needed in the MVP.*
> — `plan.md` §5

So the sequence to get a fresh corpus in front of a running client is:

1. Run `rust-docs-mcp sync` — it publishes a new immutable snapshot under the data
   directory.
2. **Restart `rust-docs-mcp serve`** — the new process loads the just-published
   snapshot. A `serve` process already running keeps the snapshot it started with;
   it does not observe the new file on disk.

The snapshot the process serves is the **latest valid published snapshot** in the
data directory; if there is none, the **validated snapshot embedded in the binary**
is the offline fallback (§5, §7). A published snapshot is the one written at
`<data-dir>/corpus/manifest.json`; a later `serve`/`status`/`inspect` reads it.

---

## 4. Freshness, version uncertainty, and the configurable staleness policy

`status` reports the snapshot a process would serve and judges each source's
freshness **offline** — it makes no network request. Staleness is **metadata, not a
gate**: a stale source is still fully reported and still served, so stale
documentation remains usable offline (`plan.md` §5: *"Staleness is metadata and does
not prevent offline use."*).

### 4.1 Reading status

```sh
./target/release/rust-docs-mcp status
```

```text
rust-docs-mcp status:
  snapshot: rust-docs-bundled-seed
  source(s): 8, section(s): 8
  freshness threshold: 7 day(s) (default)
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

(Real output, 2026-11-13; the per-run tracing prefix
`<timestamp> <level> ... rust-docs-mcp: ` is omitted for readability, and the report
continues with one block per source.) Three **distinct** times travel with each
source and are never conflated (`plan.md` §5: *"Distinguish download time, last
check time, and known upstream modification time"*):

| Field | Meaning |
| --- | --- |
| `last checked` | The last successful **upstream check** — the staleness input. `unknown` when the source has not been checked yet. |
| `fetched` | When this content was **downloaded**. A conditional (HTTP 304) check advances `last checked` *without* moving this or the content hash. |
| `upstream` | The known **upstream modification time**, when available; otherwise `unknown`. |
| `content` | The captured-content **hash** — an identity of the bytes, not a compiler version or a proof of an upstream publication date. |
| `stale` | The verdict: `fresh`, `stale`, or `unknown`. |

### 4.2 The configurable staleness policy

The staleness threshold is **configurable** — `--stale-days DAYS` overrides the
plan's seven-day default (`plan.md` §5: *"make the threshold configurable"*):

```sh
./target/release/rust-docs-mcp status --stale-days 14
```

```text
rust-docs-mcp status:
  snapshot: rust-docs-bundled-seed
  source(s): 8, section(s): 8
  freshness threshold: 14 day(s) (configured)
  ...
```

The report shows the window it judged against and notes whether it is the default
(`7 day(s) (default)`) or configured (`14 day(s) (configured)`). `--stale-days`
takes a **non-negative** integer; a missing or negative value is an actionable
`clap` parse error on stderr (exit `2`), e.g.:

```text
error: a value is required for '--stale-days <DAYS>' but none was supplied

Usage: rust-docs-mcp status [OPTIONS]
```

A source checked more than `DAYS` ago reports `stale: stale` under that window and
`stale: fresh` under a wider one; the *same* source is both, depending on the
threshold. `--stale-days` is a `status` flag — `inspect` has no staleness window.

### 4.3 Unknown freshness and version uncertainty

Two values are **preserved as `unknown`, never guessed** (`plan.md` §5/§139):

- **Unknown freshness.** A source with no known `last checked` yields `stale:
  unknown` — the verdict is *not* guessed. On the bundled seed every source is
  `unknown` (it has never checked upstream; `sync` stamps the times).
- **Unknown version compatibility.** Compatibility is one of `stable`,
  `nightly-only`, `rolling`, or `unknown`; when upstream does not pin a version it is
  reported as `unknown` rather than assumed. **A commit-pinned rustfmt document is
  not automatically documentation for an installed stable release.** When
  compatibility is uncertain, check the **local tool versions and the local
  `--help`** through your own terminal before relying on a version-specific claim
  (`plan.md` §5; carried in the server's `instructions`, see
  [`docs/COMPATIBILITY.md`](COMPATIBILITY.md)).

---

## 5. The configurable data directory

The server reads a snapshot from an **OS-appropriate application data directory**:

- **Unix:** `$XDG_DATA_HOME/rust-docs-mcp`, falling back to
  `$HOME/.local/share/rust-docs-mcp`.
- **Windows:** `%APPDATA%\rust-docs-mcp`.

Override it with the **global** `--data-dir PATH` flag (accepted before *or* after
the subcommand) for tests or portable installations:

```sh
./target/release/rust-docs-mcp --data-dir /tmp/rust-docs-mcp serve
# equivalent to:
./target/release/rust-docs-mcp serve --data-dir /tmp/rust-docs-mcp
```

`sync` writes a new snapshot under the resolved data directory; a later
`serve`/`status`/`inspect` reads it. **With no local snapshot a command falls back
to the embedded bundled snapshot**, so an empty `--data-dir` still serves offline:

```sh
$ ./target/release/rust-docs-mcp status --data-dir /tmp/empty   # no local snapshot yet
rust-docs-mcp status:
  snapshot: rust-docs-bundled-seed
  source(s): 8, section(s): 8
  freshness threshold: 7 day(s) (default)
  ...
# exit 0 — served from the bundled fallback
```

If no `--data-dir` is given and the platform exposes **no** default location, the
command fails with an actionable error that names the remedy rather than writing to
an unexpected place:

```text
no `--data-dir` was supplied and this platform exposes no default application data directory (set `--data-dir PATH` explicitly)
```

---

## 6. Failure recovery

`sync` is built so a bad refresh can never leave the operator with a broken corpus.
Any fetch, discovery, extraction, or validation failure **aborts the run *before*
the atomic promotion**, so the **last valid snapshot is preserved**
(`plan.md` §5: *"failed updates leave the previous corpus usable"*; AC2). A `sync`
that fails to fetch or parse an expected document never publishes a partial or
broken corpus.

On failure `sync` prints a **source-named diagnostic on stderr** and exits
**non-zero**, so a broken source is never presented as a successful refresh. The
diagnostic is `rust-docs-mcp: sync failed: <reason>`, where `<reason>` names the
failure:

| Failure | Diagnostic (`sync failed: …`) |
| --- | --- |
| The fetch client could not start (e.g. the registry is unreachable) | `building the fetch client failed: …` |
| A per-document fetch failed (transport, boundary rejection, retry cap, a non-success status) | `a fetch failed: …` |
| A source's document URLs could not be discovered on the live path | `discovery failed: …` |
| A source fetched but **failed to parse/extract** | `extracting source \`<id>\` failed: <reason>` |
| A configured source id is not one of the eight approved collections | `source id \`<id>\` is not one of the approved collections` |
| The candidate snapshot failed validation or could not be published | `publishing the snapshot failed: …` |

The `extracting source \`<id>\` failed: …` line is the key one: it names the
**exact source** that broke and the extractor's reason, so an operator can see
*which* collection failed and why, while the other seven collections stay served.

**A changed upstream layout produces a *visible* sync failure, never a silent empty
corpus** (`plan.md` §5: *"A changed layout must produce a visible sync failure
instead of silently publishing an empty corpus"*). The rustfmt rendered site, which
requires JavaScript and yields only an empty HTML shell, is intentionally **not** a
fetch boundary — rustfmt is ingested only from its approved raw Markdown
(`README.md`/`Configurations.md` at a resolved commit), so an empty shell can never
look like a successful ingestion.

---

## 7. The no-valid-corpus error

When **no valid snapshot exists** — neither a valid local published snapshot nor a
valid bundled fallback — the binary **must not start an apparently-healthy empty
server**. Instead it exits **non-zero** with an **actionable message on stderr**
directing the operator to run `sync` (`plan.md` §5: *"When no valid snapshot exists,
exit with an actionable message on stderr directing the operator to run sync. Do not
start an apparently healthy empty server."*). In the code this is
`LoadError::NoValidCorpus`.

The message the load path produces (a single line; shown wrapped for width), with
`<reason>` the source-specific cause:

```text
no valid corpus is available (local published snapshot: <reason>)
(no bundled snapshot was provided).
Run `rust-docs-mcp sync` to download and publish a local snapshot,
or restore the bundled fallback.
```

The middle reports the **specific reason for each source that was attempted** — the
local published snapshot when a data directory was read, and the bundled snapshot
when one was supplied (otherwise `(no bundled snapshot was provided)`). The two
reasons the real binary produces, captured 2026-11-13 (via the load path used by
`status`/`inspect`):

- **Absent local, no bundle** — `status`/`inspect` with no valid local and no
  bundle:

  ```text
  no valid corpus is available (local published snapshot: local published snapshot: reading published snapshot: No such file or directory (os error 2)) (no bundled snapshot was provided). Run `rust-docs-mcp sync` to download and publish a local snapshot, or restore the bundled fallback.
  ```

- **Corrupt local, no bundle** — a local file that is present but not a valid
  snapshot:

  ```text
  no valid corpus is available (local published snapshot: local published snapshot: schema or serialization error: JSON error: expected value at line 1 column 1) (no bundled snapshot was provided). Run `rust-docs-mcp sync` to download and publish a local snapshot, or restore the bundled fallback.
  ```

**In the current build this path does not fire in normal operation**, because a
validated snapshot is **always embedded in the binary**
([task 031](https://github.com/ThiagoEvoa/rust-mcp/issues/31)): a fresh install —
even with an empty `--data-dir` (§5) — falls back to the embedded snapshot and serves
offline. The error is the guarantee for a configuration that would **omit the
bundle** (a stripped build, or a future build that ships without a bundle). The
recover step is the same in every case: **run `rust-docs-mcp sync`, then restart
`serve`**.

---

## 8. Troubleshooting

The four failure classes the acceptance criteria call for, distinguished by their
**symptom** and **diagnostic**. In every case the **last valid corpus is preserved**
and, unless a bundle-less build is involved, the server still serves the bundled
snapshot — so a bad refresh is a *diagnostic*, not a *downtime*.

### 8.1 Missing components — a required source could not be fetched

- **Symptom.** `sync` fails before it publishes; the source never reaches the corpus.
- **Diagnostic (stderr).** A fetch/discovery/client diagnostic:
  `building the fetch client failed: …`, `a fetch failed: …`, or
  `discovery failed: …`. A `status` afterward shows the affected source missing or
  with no `last checked`.
- **Fix.** Re-run `sync` (the opt-in live path — a real run is the
  [Upstream ingestion](../.github/workflows/upstream-ingestion.yml) job in
  [`docs/CI.md`](CI.md#opt-in-upstream-ingestion-check)). Check the network and the
  source's boundary (the source must resolve within its fixed host/path rules in
  `src/registry.rs`; an off-boundary URL is rejected, not fetched). A missing
  collection is a **refresh** problem: the previous corpus (or the bundled snapshot)
  keeps serving until the source is restored.

### 8.2 Stale content — a source is older than the threshold

- **Symptom.** `status` reports `stale: stale` for one or more sources (their last
  successful upstream check is older than the window). The content is **still served
  and fully reported** — staleness never blocks a read.
- **Diagnostic (stderr).** The `status` per-source block: `stale: stale` under the
  configured window (e.g. the default `7 day(s)`).
- **Fix.** Re-run `sync`, then **restart `serve`** (§3) to serve the refreshed
  snapshot. To *change the verdict* rather than the corpus, widen the window with
  `status --stale-days <N>` (§4.2): the same source is `stale` at `7` and `fresh` at
  `30`. Staleness is metadata for the operator and the agent; it is not an error.

### 8.3 Extraction failures — a source fetched but failed to parse

- **Symptom.** `sync` fetched a source but could not extract it, so the run aborts
  *before* promotion and the last valid corpus is preserved (§6).
- **Diagnostic (stderr).** `rust-docs-mcp: sync failed: extracting source
  \`<id>\` failed: <reason>` — the source id *and* the extractor's reason, naming the
  exact collection that broke.
- **Inspect the per-source report.** Read the `sync` stderr: the failing source is
  named, and the seven that succeeded are unaffected. A **changed upstream layout**
  is the usual cause — it surfaces as this visible failure, **never** as a silent
  empty corpus (§6).
- **Fix.** Nothing to recover — the previous corpus (or the bundled snapshot) still
  serves. Re-run `sync` once the source is restorable; a **persistent layout break**
  needs a parser/adapter update for that collection, which is an implementation task,
  not an operator step.

### 8.4 Client connection failures — stdio / transport

- **Symptom.** A client cannot connect, the server process exits, or the client sees
  something other than clean protocol traffic. This is a **client/transport** problem,
  not a corpus problem — see [`docs/CLIENTS.md`](CLIENTS.md) for the first-client
  contract and the captured `initialize` transcript.
- **Wrong path or arguments.** The client launches the server by its **absolute
  executable path** with `serve` (and optional `--data-dir`/`--verbose`) in the
  standard `mcpServers` shape ([`docs/CLIENTS.md` §2](CLIENTS.md#2-ac1--the-generic-stdio-client-configuration)).
  A relative `command`, a missing `serve`, or a non-existent path is the most common
  cause. Re-point `command` at the absolute path from the
  [install walkthrough](../README.md#install-clean-install-walkthrough).
- **Polluted stdout.** If the client sees diagnostics where it expects protocol
  traffic, the stream has been conflated. During `serve` diagnostics go to **stderr**
  and **stdout carries only MCP traffic** (§1). Do not redirect the server's stderr
  into its stdout, and run the `serve` subcommand (not `sync`/`status`/`inspect`,
  which print human reports to stderr and exit) in the client's stdio.
- **The server exited.** A clean client disconnect ends `serve` with exit `0`
  (the transport's read half terminating on EOF is the authoritative
  end-of-session signal). A **non-zero** exit with an actionable line on **stderr**
  is a **startup failure** — read stderr first (a no-valid-corpus error in a
  bundle-less build, §7; or a transport-start failure). The server never panics on
  external input: every startup failure is a typed, actionable diagnostic plus a
  non-zero exit.

---

## 9. Quick reference

| Want to… | Run |
| --- | --- |
| See the snapshot and per-source freshness | `rust-docs-mcp status` |
| Change the staleness verdict | `rust-docs-mcp status --stale-days 14` |
| See coverage and extraction fidelity | `rust-docs-mcp inspect` |
| Get a current corpus | `rust-docs-mcp sync` (then **restart** `serve`) |
| Point at a specific data directory | add `--data-dir PATH` to any command |
| Get more diagnostics | add `--verbose` (or set `RUST_LOG`); they go to stderr |
| Recover from a failed refresh | the last valid corpus is preserved; re-run `sync` (§6, §8.1/§8.3) |
| Recover from a no-valid-corpus exit | `rust-docs-mcp sync`, then restart `serve` (§7) |

---

## 10. Files touched by this task

| File | Change |
| --- | --- |
| `docs/OPERATIONS.md` | **Created** — this record: explicit refresh, snapshot activation on restart, freshness and version uncertainty, the configurable data directory and staleness policy, failure recovery, the no-valid-corpus error, and the four-class troubleshooting guide (AC1/AC2). |
| `README.md` | Extended — a concrete `status --stale-days` example, a *Freshness and version uncertainty* note, a *Troubleshooting* section that names the four classes and links here, and `docs/OPERATIONS.md` added to the docs index. |
| `docs/CI.md` | Referenced as the home of the opt-in upstream-ingestion `sync` job; unchanged. |
| `docs/CLIENTS.md` | Referenced as the client-connection source of truth for §8.4; unchanged. |
