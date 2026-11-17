//! Retrieval *acceptance* over the shipped corpus (issue #156, plan §11).
//!
//! The eval harness (`crate::eval`, `src/eval.rs`) proves the ranking *mechanism*
//! over a small deterministic corpus. This test proves retrieval *quality* over
//! the real bundled corpus a fresh install actually serves (`crate::bundled`, the
//! `rust-docs-bundled-seed` snapshot): it runs the genuine 45-case eval dataset
//! over that corpus and records the numbers the plan's §11 bar names —
//! **≥90% top-5 recall** and **exact-match / canonical first place** — measured
//! honestly, with the gap to the target reported rather than hidden.
//!
//! ## What is measured
//!
//! * **Top-5 recall** — for every case, does some top-5 result belong to the
//!   source the query targets? (`crate::acceptance::AcceptanceReport::top5_recall`)
//! * **Canonical first** — for every exact-match case (a guideline code, command
//!   name, or lint id), does the top-1 hit carry the *genuine token* in the
//!   section's structured field? (`::canonical_first`)
//! * **Completeness** — every case produces a measurement; a skipped or unmeasured
//!   case never counts as a complete result (criterion 5).
//!
//! ## Honest, not gated to a pass
//!
//! The shipped corpus currently measures *below* the plan's 90% top-5 target
//! (see the emitted report and `docs/ACCEPTANCE.md`). This test therefore pins
//! the *measured* baseline as a regression guard and asserts the gap explicitly —
//! it does **not** assert that 90% is met. That is the honesty the task
//! requires: the layer reports what the corpus achieves and names the misses,
//! rather than reporting a green bar it does not clear.
//!
//! ## Two seams
//!
//! The recall is measured at the **index level** (`SectionIndex` over the
//! bundled manifest) — the honest seam, because the `search_docs` *server* path
//! layers pagination and a byte cap on top and can empty a result even when the
//! index holds a match (the "zero hits" the audit attributed to the corpus).
//! The spawned `acceptance_server_path_reconciles_with_index` test proves the
//! server path returns non-empty hits for the same queries over the *built
//! binary*, reconciling the two.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;

use rust_docs_mcp::acceptance::{Environment, measure, shipped_corpus_identity};

/// The active bundled snapshot's id — the seed corpus `serve` always loads and
/// the acceptance measurement is tied to.
const SNAPSHOT_ID: &str = "rust-docs-bundled-seed";

/// The per-request and drain budget. A localhost stdio round-trip is sub-second;
/// this generous budget keeps the test fast while tolerating CI load.
const BUDGET: Duration = Duration::from_secs(15);

/// The protocol version the harness negotiates, matching the other spawned-stdio
/// tests.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// The number of sections the shipped corpus carries. Pinned so a corpus change
/// is a visible, deliberate re-pin rather than a silent drift.
const SHIPPED_SECTION_COUNT: usize = 877;

// ---------------------------------------------------------------------------
// Seam 1: the index-level measurement over the shipped corpus.
// ---------------------------------------------------------------------------

/// The genuine 45-case eval dataset is measured over the *shipped* corpus: the
/// top-5 recall and the canonical-first numbers are recorded, the gap to the
/// plan's 90% target is asserted explicitly (so the test can never masquerade as
/// a 90%-met pass), and the full report is emitted.
#[test]
fn acceptance_recall_measured_over_shipped_corpus() {
    let manifest = rust_docs_mcp::bundled::manifest();
    let report = measure(&manifest, &Environment::current());

    // Emit the report so the gap is visible in the test output.
    eprintln!("\n{}", report.render());

    // Criterion 5: a harness that skips or fails a case must not report a
    // complete result. Every case must be measured with no recorded error.
    assert!(
        report.all_measured(),
        "a case was skipped or unmeasured — the result is incomplete: {}",
        report.render()
    );

    // The corpus the numbers are tied to is the shipped one, not a fixture.
    assert_eq!(
        report.corpus.snapshot_id, SNAPSHOT_ID,
        "the acceptance measurement must run over the shipped corpus"
    );
    assert_eq!(
        report.corpus.section_count, SHIPPED_SECTION_COUNT,
        "the measured corpus must be the full shipped corpus"
    );

    // Canonical first place is the MET bar: every exact-match case ranks a hit
    // that carries its genuine token first.
    let (canonical_pass, canonical_total) = report.canonical_first();
    assert_eq!(
        canonical_pass,
        canonical_total,
        "every exact-match case must rank its canonical section first; misses:\n{}",
        report
            .canonical_misses()
            .iter()
            .map(|c| format!("  - {} ({})", c.id, c.query))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Top-5 recall is measured honestly. The shipped corpus does NOT yet clear
    // the plan's 90% target; the test pins the *measured* baseline as a
    // regression guard and asserts the gap, rather than faking a pass.
    let (recall_pass, recall_total) = report.top5_recall();
    assert_eq!(
        (recall_pass, recall_total),
        (34, 45),
        "the measured top-5 recall baseline is 34/45; re-pin this if the corpus \
         is intentionally changed, and record the new number in docs/ACCEPTANCE.md"
    );
    // The gap is asserted explicitly so this can never read as a 90%-met pass.
    assert!(
        report.top5_recall_pct() < 90.0,
        "top-5 recall ({:.1}%) has reached the plan's 90% target — update this \
         assertion and docs/ACCEPTANCE.md to record the passing state",
        report.top5_recall_pct()
    );
}

/// The shipped corpus identity is stable and matches what `serve` reports: the
/// `snapshot_id`, the section count, and the deterministic content fingerprint.
/// This is the fast, always-on guard that the fixed corpus a measurement is tied
/// to is the real shipped one, not a fixture or a partial sync.
#[test]
fn acceptance_corpus_identity_is_pinned() {
    let identity = shipped_corpus_identity();
    assert_eq!(identity.snapshot_id, SNAPSHOT_ID);
    assert_eq!(identity.section_count, SHIPPED_SECTION_COUNT);
    assert!(
        !identity.parser_version.is_empty(),
        "the shipped corpus records a parser version"
    );
    // The fingerprint is deterministic (FNV-1a is order-stable and dependency-
    // free): two calls over the same bundled corpus agree.
    assert_eq!(
        identity.content_fingerprint,
        shipped_corpus_identity().content_fingerprint,
        "the corpus content fingerprint must be deterministic"
    );
    assert_eq!(
        identity.sources.len(),
        8,
        "the shipped corpus carries all eight registered sources"
    );
}

// ---------------------------------------------------------------------------
// Seam 2: the server path over the built binary (refresh → restart → retrieve).
// ---------------------------------------------------------------------------

/// A minimal raw-JSON-RPC 2.0 client over a spawned `serve` process, the same
/// pattern the M4 proof owns in [`stdio_mcp.rs`](stdio_mcp.rs) and the perf
/// harness in [`perf.rs`](perf.rs): one request at a time, one newline-framed
/// response at a time, no `rmcp` client.
struct Client {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    deadline: tokio::time::Instant,
}

impl Client {
    /// Spawn `serve` over a fresh, empty `--data-dir` (so it loads the bundled
    /// fallback — the shipped corpus) and complete the `initialize` handshake.
    async fn new() -> Self {
        let data_dir = fresh_data_dir();
        let mut child = Command::new(env!("CARGO_BIN_EXE_rust-docs-mcp"))
            .arg("serve")
            .arg("--data-dir")
            .arg(&data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Diagnostics go to /dev/null: the acceptance test does not inspect
            // stderr, and a piped-but-undrained stderr can block the child on a
            // full pipe (the failure mode the clean close guards against).
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("the acceptance test must be able to launch the `serve` binary");

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        let mut client = Self {
            child,
            stdin,
            reader: tokio::io::BufReader::new(stdout),
            next_id: 0,
            deadline: tokio::time::Instant::now() + BUDGET,
        };

        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "acceptance-test", "version": "0.0.1" },
        });
        client.request("initialize", &params).await;
        client.notify("notifications/initialized").await;
        client
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn notify(&mut self, method: &str) {
        let message = json!({ "jsonrpc": "2.0", "method": method });
        let frame = serde_json::to_string(&message)
            .expect("encoding a hand-built JSON value must not fail");
        self.stdin
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("writing a JSON-RPC notification must not fail");
        self.stdin
            .flush()
            .await
            .expect("flushing the child's stdin must not fail");
    }

    /// Send a JSON-RPC request and return its matching response.
    async fn request(&mut self, method: &str, params: &Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let frame = serde_json::to_string(&message)
            .expect("encoding a hand-built JSON value must not fail");
        self.stdin
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("writing a JSON-RPC request must not fail");
        self.stdin
            .flush()
            .await
            .expect("flushing the child's stdin must not fail");

        loop {
            let remaining = self
                .deadline
                .saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                panic!("timed out waiting for the response to `{method}` (id {id})");
            }
            let mut line = String::new();
            match tokio::time::timeout(remaining, self.reader.read_line(&mut line)).await {
                Ok(Ok(0)) => {
                    panic!("the server closed stdout before answering `{method}` (id {id})")
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
                Err(_) => panic!("timed out waiting for the response to `{method}` (id {id})"),
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value =
                serde_json::from_str(trimmed).expect("every stdout line must be JSON");
            assert_eq!(
                value.get("jsonrpc").and_then(Value::as_str),
                Some("2.0"),
                "a non-JSON-RPC object leaked onto stdout: {trimmed}"
            );
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }

    /// Call a tool by name with the given arguments object.
    async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            &json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// Close stdin and wait for a clean self-exit, mirroring the proven
    /// [`stdio_mcp.rs`](stdio_mcp.rs) pattern: drop the stdin writer (the stdio
    /// transport reads it as EOF, the end of the session), **drain the child's
    /// remaining stdout to EOF** (after the final response the server may still
    /// emit a trailing line; not draining leaves its stdout pipe full and blocks
    /// the child on a write), then assert a clean, successful self-exit within
    /// the budget. A hung child is reaped by `kill_on_drop`, but the assertion
    /// makes a clean exit the expected outcome, not an ignored one.
    async fn close(mut self) {
        // Dropping the piped stdin writer closes its write end — the EOF that
        // ends the session.
        drop(self.stdin);

        // Drain the child's remaining stdout to EOF so its write end never fills
        // and blocks the child's exit.
        loop {
            let mut line = String::new();
            match tokio::time::timeout(BUDGET, self.reader.read_line(&mut line)).await {
                Ok(Ok(0)) => break, // EOF: the server closed stdout and is exiting.
                Ok(Ok(_)) => continue,
                Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
                Err(_) => panic!("timed out draining the child's stdout after the final response"),
            }
        }

        // stdout closed, so the server is exiting; assert a clean, successful
        // self-exit — the concrete "no orphaned process" claim.
        match tokio::time::timeout(BUDGET, self.child.wait()).await {
            Ok(Ok(status)) => {
                assert!(
                    status.success(),
                    "after a clean client disconnect the server must exit successfully, got {status:?}"
                );
            }
            Ok(Err(error)) => panic!("waiting on the child failed: {error}"),
            Err(_) => panic!("the child did not exit within {BUDGET:?} — a likely orphan"),
        }
    }
}

/// A temporary per-call data directory, given to the child as `--data-dir` so the
/// process never touches the host's application cache. Empty, so `serve` loads
/// the bundled fallback (the shipped corpus).
fn fresh_data_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rust-docs-mcp-accept-{}-{}",
        std::process::id(),
        stamp
    ))
}

/// A sample of eval queries spanning every source, including several of the
/// top-5 *misses* (the natural-language queries the index ranks cross-source).
/// The point is not that each hits its expected source — the index-level test
/// records which miss — but that the *server path* returns non-empty hits for
/// all of them, reconciling the audit's "zero hits" concern with the index
/// level.
const SERVER_PATH_QUERIES: [&str; 10] = [
    "C-CONV",
    "cargo build",
    "clippy::unwrap_used",
    "how do I list or switch between toolchains?",
    "how do I run the formatter in check mode without writing?",
    "how do intra-documentation links work?",
    "how do I configure Clippy lint levels like allow and deny?",
    "how should Rust functions and types be named?",
    "how do I generate documentation with cargo doc?",
    "are rustfmt options stable across toolchain versions?",
];

/// The `search_docs` *server path* over the built binary returns non-empty hits
/// for every sampled query — reconciling the index-level measurement (which
/// finds matches the server's pagination/byte-cap could otherwise empty) with
/// what a real client observes. Each response also names the shipped snapshot,
/// so the client can detect drift and re-search.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_server_path_reconciles_with_index() {
    let mut client = Client::new().await;

    // The server names the shipped snapshot on a catalog read.
    let list = client.call_tool("list_sources", json!({})).await;
    assert_eq!(list.get("error"), None, "list_sources must succeed: {list}");
    assert_eq!(
        list.pointer("/result/structuredContent/snapshot_id")
            .and_then(Value::as_str),
        Some(SNAPSHOT_ID),
        "the server must name the shipped corpus on a catalog read"
    );

    // Every sampled query returns non-empty hits through the server path.
    for query in SERVER_PATH_QUERIES {
        let response = client
            .call_tool("search_docs", json!({ "query": query, "limit": 5 }))
            .await;
        assert_eq!(
            response.get("error"),
            None,
            "a valid search must succeed: {query} → {response}"
        );
        // The response names the shipped snapshot (drift detection).
        assert_eq!(
            response
                .pointer("/result/structuredContent/snapshot_id")
                .and_then(Value::as_str),
            Some(SNAPSHOT_ID),
            "search_docs must name the shipped corpus: {query}"
        );
        let hits = response
            .pointer("/result/structuredContent/hits")
            .and_then(Value::as_array);
        let hits = hits.unwrap_or_else(|| {
            panic!("search_docs for `{query}` returned no `hits` array: {response}")
        });
        assert!(
            !hits.is_empty(),
            "the server path returned zero hits for `{query}` — the audit's \
             \"zero hits\" concern; the index holds a match, so the server path \
             must not empty it"
        );
    }

    client.close().await;
}
