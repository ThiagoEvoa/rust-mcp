//! Performance measurement harness for task `[071][M5]` — *"Measure
//! full-corpus startup, retrieval latency, and memory"* — extended by issue
//! #175 to cover **all five tools** and the multi-section guidance regression.
//!
//! This test drives the **real** compiled `rust-docs-mcp` binary over a spawned
//! OS-stdio session (the same seam as [`stdio_mcp.rs`](stdio_mcp.rs)) and
//! records three numbers the plan's §11 provisional targets name:
//!
//! 1. **Startup** — process spawn → first `initialize` response, over a fixed
//!    number of fresh processes (min / median / p95 / max).
//! 2. **Retrieval p95** — the p95 round-trip latency of **every tool**
//!    (`list_sources`, `search_docs`, `get_section`, `get_api_checklist`, and
//!    `get_command_guidance`, including the worst supported command and a
//!    continuation page) over fixed sets, each issued a fixed number of times.
//! 3. **Memory** — the peak resident set size of the serving process, sampled
//!    during the retrieval session and reported as an approximation of peak RSS.
//!
//! The numbers are compared against the plan's provisional targets (two-second
//! startup, sub-100-ms retrieval) and the comparison is recorded in
//! [`docs/PERFORMANCE.md`](../docs/PERFORMANCE.md).
//!
//! # Gating: this harness never slows the normal suite
//!
//! The test is `#[ignore]`d, so the ordinary `cargo test --workspace` run skips
//! it entirely (zero overhead). It is **additionally** guarded by the
//! `RUST_DOCS_MCP_PERF` environment variable, so that even an explicit
//! `--ignored` run is a fast no-op unless the operator opts in. To measure:
//!
//! ```sh
//! cargo build --release --locked
//! RUST_DOCS_MCP_PERF=1 cargo test --release --test perf -- --ignored --nocapture
//! ```
//!
//! The `--release` matters: `env!("CARGO_BIN_EXE_rust-docs-mcp")` resolves to the
//! binary the *current* `cargo` invocation built, so a release run measures the
//! release binary and a debug run measures the debug binary (the harness reports
//! which it measured). A JSON copy of the report can be written to the path named
//! by `RUST_DOCS_MCP_PERF_REPORT`.
//!
//! # Corpus, determinism, and hermeticity
//!
//! The corpus is the **bundled seed** — the `rust-docs-bundled-seed` snapshot the
//! `serve` binary always loads at compile time (`crate::bundled`) — by default,
//! and, when the operator pins `RUST_DOCS_MCP_PERF_DATA_DIR`, a published
//! snapshot in that data dir (a "larger" corpus). Each corpus is measured the
//! same way and reported with **its own identity**, so a number can never be
//! attributed to the wrong corpus. The measurement is fixed, offline, and
//! hermetic: no network, a per-run empty `--data-dir` (or the operator-pinned
//! one), a fixed protocol version, and fixed query/section sets and iteration
//! counts. The corpora measured and their caveats are stated in
//! [`docs/PERFORMANCE.md`](../docs/PERFORMANCE.md).

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;

/// The per-request and drain budget. A localhost stdio round-trip is sub-second;
/// this generous budget keeps the harness fast while tolerating CI load.
const BUDGET: Duration = Duration::from_secs(20);

/// The protocol version the harness negotiates — the highest `initialize`-capable
/// version the server negotiates to, matching the other spawned-stdio tests.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// The active bundled snapshot's id — the seed corpus `serve` always loads.
const SNAPSHOT_ID: &str = "rust-docs-bundled-seed";

/// The number of fresh processes whose startup is measured. A fixed count keeps
/// the measurement reproducible; a small warmup is discarded first so the OS
/// loader cache does not skew the first sample.
const STARTUP_RUNS: usize = 10;

/// The number of retrieval iterations per query (search) / section (get_section).
/// A fixed count keeps the p95 stable run-to-run.
const RETRIEVAL_ITERS: usize = 30;

/// The number of iterations per command in the guidance worst-command sweep.
/// Fewer than `RETRIEVAL_ITERS`: the sweep covers every command, so a small count
/// keeps the harness fast while still finding the slowest command.
const GUIDANCE_COMMAND_ITERS: usize = 10;

/// The `get_command_guidance` page size used for the worst-command and
/// continuation measurements. A large limit exercises the multi-section guidance
/// path the issue #175 regression was about (the largest guidance input is
/// `rustfmt`, which cites ~50 sections); `15` keeps each page under the response
/// cap while remaining a genuinely large page.
const LARGE_GUIDANCE_LIMIT: u32 = 15;

/// The fixed `get_api_checklist` categories. The shipped corpus carries a single
/// checklist category ("Naming"); the unfiltered "all" read is always timed too.
const CHECKLIST_CATEGORIES: [&str; 1] = ["Naming"];

/// The fixed `search_docs` query set. Each query is expected to match the seed
/// corpus; the harness asserts non-empty hits so a zero-hit "fast" result can
/// never masquerade as a real retrieval.
const SEARCH_QUERIES: [&str; 8] = [
    "C-CONV",
    "naming convention",
    "cargo build",
    "compiler command-line",
    "install rustup",
    "rustfmt options",
    "clippy lint",
    "rustdoc documentation",
];

/// The fixed `get_section` id set — one real section per source, by the
/// canonical id the *shipped* corpus actually stores (issue #156: the harness
/// measured stale, non-existent ids). Each is asserted real by
/// `perf_corpus_section_ids_are_real`, so the set cannot silently drift.
const SECTION_IDS: [&str; 8] = [
    "api-guidelines/about#rust-api-guidelines",
    "style-guide/expressions#array-accesses-indexing-and-slicing",
    "cargo/commands%2Fcargo-add#cargo-add1",
    "rustc/command-line-arguments#--cap-lints-set-the-most-restrictive-lint-level",
    "rustup/concepts%2Findex#concepts",
    "rustfmt/Configurations.md",
    "clippy/clippy-catalog#clone_on_copy",
    "rustdoc/command-line-arguments#--cfg-passing-configuration-flags",
];

/// A minimal raw-JSON-RPC 2.0 client over a spawned `serve` process. It reuses
/// the pattern the M4 proof owns in [`stdio_mcp.rs`](stdio_mcp.rs): one request
/// at a time, one newline-framed response at a time, no `rmcp` client.
struct Client {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    deadline: tokio::time::Instant,
}

/// A temporary per-measurement data directory, given to the child as `--data-dir`
/// so the process never writes the host's real application cache. `serve` loads
/// the bundled snapshot in-memory and does not consult it, but the flag keeps the
/// child's behaviour identical to the other spawned-stdio tests.
fn fresh_data_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rust-docs-mcp-perf-{}-{}",
        std::process::id(),
        stamp,
    ))
}

impl Client {
    /// Spawn the `serve` binary over piped stdio and complete the `initialize`
    /// handshake + `notifications/initialized`, the way a conforming MCP client
    /// does, against an explicit `--data-dir`. The shipped corpus passes a fresh
    /// empty dir (the bundled fallback `serve` loads); the "documented larger
    /// corpus" (issue #175 AC2) passes a published-snapshot dir so the same
    /// harness measures a different corpus the same way. Returns the client
    /// together with the `initialize` response and the instant the response was
    /// fully read, so the caller can measure startup.
    async fn new_with_data_dir(data_dir: &std::path::Path) -> (Self, Value, tokio::time::Instant) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rust-docs-mcp"))
            .arg("serve")
            .arg("--data-dir")
            .arg(data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Diagnostics are redirected to stderr and discarded, so a log the
            // server emits can never leak onto the protocol stream.
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("the perf harness must be able to launch the `serve` binary");

        // Take the piped handles out *before* moving `child` into the struct.
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
            "clientInfo": { "name": "perf-harness", "version": "0.0.1" },
        });
        let response = client.request("initialize", &params).await;
        client.notify("notifications/initialized").await;
        (client, response, tokio::time::Instant::now())
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

    /// Send a JSON-RPC request and return its response, asserting the id matches.
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

    /// Call a tool by name with the given arguments object, returning the
    /// response.
    async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            &json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// The child pid, for RSS sampling.
    fn pid(&self) -> u32 {
        self.child.id().unwrap_or(0)
    }

    /// Close the client's stdin and wait for a clean self-exit, the way the
    /// other spawned-stdio tests do (see [`stdio_mcp.rs`](stdio_mcp.rs)): drop
    /// the stdin writer (the EOF that ends the session), **drain the child's
    /// remaining stdout to EOF** so its write end never fills and blocks the
    /// child's exit, then assert a clean, successful self-exit within the drain
    /// budget. `kill_on_drop` reaps a child that does not exit, but the
    /// assertion makes a clean exit the expected outcome — a harness that times
    /// out must not silently count as a completed measurement (criterion 5).
    async fn close(mut self) {
        // Dropping the piped stdin writer closes its write end — the EOF that
        // ends the session.
        drop(self.stdin);

        // Drain the child's remaining stdout to EOF so a full pipe cannot block
        // the child on a write.
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

/// The minimum number of samples a percentile needs to be meaningful.
const MIN_SAMPLES: usize = 2;

/// A summary of a set of millisecond samples: min / median / p95 / max.
struct Percentiles {
    min_ms: f64,
    median_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

/// Sort the samples and report min / median / p95 / max in milliseconds. A
/// nearest-rank percentile keeps the harness dependency-free and deterministic
/// for a fixed iteration count.
fn percentile_report(samples: &mut [f64]) -> Percentiles {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("finite samples compare"));
    let at = |pct: f64| -> f64 {
        if samples.is_empty() {
            0.0
        } else {
            let rank = (pct / 100.0 * samples.len() as f64).ceil() as usize;
            samples[rank.clamp(1, samples.len()) - 1]
        }
    };
    Percentiles {
        min_ms: samples.first().copied().unwrap_or(0.0),
        median_ms: at(50.0),
        p95_ms: at(95.0),
        max_ms: samples.last().copied().unwrap_or(0.0),
    }
}

/// Sample the child's resident set size (RSS) via the platform `ps` tool. macOS
/// `ps -o rss=` reports KiB. This is an approximation of peak RSS (sampled after
/// the corpus is loaded); the harness states the caveat. Returns `None` if the
/// process is gone or `ps` is unavailable.
fn sample_rss_kib(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    let out = std::process::Command::new("ps")
        .arg("-o")
        .arg("rss=")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let kib = s.trim();
    kib.parse::<u64>().ok().filter(|v| *v > 0)
}

/// Round to two decimals for a stable, human-readable report.
fn round(ms: f64) -> f64 {
    (ms * 100.0).round() / 100.0
}

/// Assert a `search_docs` result is non-empty, so a zero-hit "fast" path cannot
/// masquerade as a real retrieval. The real search response over the seed
/// carries at least one hit for these queries.
fn assert_non_empty_search(response: &Value, query: &str) {
    let text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !text.contains("no documentation") && !text.contains("no results"),
        "search_docs `{query}` returned no hits (unexpected for the seed)"
    );
}

/// Assert a `get_section` result is resolved (no typed error), so the fixed
/// `SECTION_IDS` are all real seed sections.
fn assert_section_ok(response: &Value, section: &str) {
    let is_error = response
        .get("error")
        .map(|e| e.get("code").and_then(Value::as_i64).is_some())
        .unwrap_or(false);
    assert!(
        !is_error,
        "get_section `{section}` returned an error (unexpected for the seed)"
    );
}

/// Assert a `get_api_checklist` result carries at least one entry, so a zero-entry
/// "fast" path cannot masquerade as a real checklist read.
fn assert_checklist_ok(response: &Value, category: &str) {
    let entries = response
        .pointer("/result/structuredContent/entries")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        entries >= 1,
        "get_api_checklist `{category}` returned {entries} entr(y/ies) (unexpected for the seed)"
    );
}

/// Assert a `get_command_guidance` result resolves a real command entry, so a
/// missing-command "fast" error path cannot masquerade as a guidance read.
fn assert_guidance_ok(response: &Value, command: &str, limit: u32) {
    let is_error = response
        .get("error")
        .map(|e| e.get("code").and_then(Value::as_i64).is_some())
        .unwrap_or(false);
    assert!(
        !is_error,
        "get_command_guidance `{command}` (limit {limit}) returned an error (unexpected for the seed)"
    );
}

/// Assert a `list_sources` result is non-empty, so a zero-source "fast" path
/// cannot masquerade as a real catalog read.
fn assert_non_empty_list_sources(response: &Value) {
    let sources = response
        .pointer("/result/structuredContent/sources")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        sources >= 8,
        "list_sources returned {sources} source(s) (expected at least 8)"
    );
}

/// The performance measurement the plan's §11 provisional targets name, plus the
/// issue #175 AC2 extension: **all five tools**, the **worst supported command**,
/// and a **continuation page**, measured over the shipped corpus *and*, when
/// `RUST_DOCS_MCP_PERF_DATA_DIR` is pinned, a documented larger (published)
/// corpus — each with its own identity.
///
/// This is the gated entry point. It is `#[ignore]`d (so the normal suite skips
/// it) and additionally guarded by `RUST_DOCS_MCP_PERF` (so even an explicit
/// `--ignored` run is a fast no-op unless the operator opts in). When it runs, it
/// measures startup, per-tool retrieval p95, the worst-command guidance p95 and
/// its continuation, and peak RSS, prints a report to stderr, and — when
/// `RUST_DOCS_MCP_PERF_REPORT` names a path — writes a JSON copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in performance harness — run with `RUST_DOCS_MCP_PERF=1 cargo test --release --test perf -- --ignored --nocapture`; see docs/PERFORMANCE.md"]
async fn measure_startup_retrieval_and_memory() {
    // The env-var guard makes an explicit `--ignored` run a fast no-op unless the
    // operator opts in; the `#[ignore]` keeps the normal suite off the harness.
    if std::env::var_os("RUST_DOCS_MCP_PERF").is_none() {
        eprintln!(
            "[perf] RUST_DOCS_MCP_PERF is unset — skipping the measurement (fast no-op). \
             Run with `RUST_DOCS_MCP_PERF=1 cargo test --release --test perf -- --ignored --nocapture`."
        );
        return;
    }

    // ---- Corpus identity: the shipped corpus is asserted against the in-process
    // identity, so a corpus drift is a visible, deliberate re-pin rather than a
    // silent one. The numbers reported below are attributed to it. ----
    let shipped = rust_docs_mcp::acceptance::shipped_corpus_identity();
    assert_eq!(
        shipped.snapshot_id, SNAPSHOT_ID,
        "the perf harness must measure the shipped corpus"
    );
    assert_eq!(
        rust_docs_mcp::bundled::section_count(),
        shipped.section_count,
        "the in-process section count must agree with the reported corpus identity"
    );

    // Measure every corpus: the shipped (bundled) corpus always, and — when the
    // operator pins `RUST_DOCS_MCP_PERF_DATA_DIR` — a published "larger" corpus
    // the same way. Each is measured independently and reported with its own
    // identity, so a number is never attributed to the wrong corpus.
    let mut corpora: Vec<Value> = Vec::new();

    // (1) The shipped corpus: a fresh empty data dir loads the bundled fallback.
    let shipped_dir = fresh_data_dir();
    corpora.push(measure_corpus(&shipped_dir, "shipped", Some(&shipped)).await);

    // (2) The documented larger corpus, when pinned: a published snapshot in a
    //     data dir, measured the same way. Its identity is read from the served
    //     `list_sources` response (not the bundled one), so a different corpus is
    //     reported distinctly, never silently.
    if let Some(data_dir) = std::env::var_os("RUST_DOCS_MCP_PERF_DATA_DIR") {
        let path = std::path::PathBuf::from(data_dir);
        corpora.push(measure_corpus(&path, "data-dir", None).await);
    }

    // ---- Report + target comparison. ----
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    // The toolchain the measurement ran under (best-effort; a sentinel when
    // `rustc` is not on the PATH), so a number is attributed to a machine, not
    // just a corpus.
    let toolchain = rustc_version();
    let report = json!({
        "ok": true,
        "profile": profile,
        "host": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "toolchain": toolchain,
        "corpora": corpora,
    });
    emit_report(&report);
}

/// Measure one corpus (issue #175 AC2): startup, **all five tools**, the **worst
/// supported command**'s guidance p95 and its **continuation page**, and peak
/// RSS. `expected_identity` pins the measurement to the shipped corpus when it is
/// the bundled one (a drift fails loudly); for a `data-dir` corpus it is `None`
/// and the identity is read from the served `list_sources` response instead.
async fn measure_corpus(
    data_dir: &std::path::Path,
    label: &str,
    expected_identity: Option<&rust_docs_mcp::acceptance::CorpusIdentity>,
) -> Value {
    // ---- Startup: spawn -> first `initialize` response over fresh processes. ----
    // A small warmup discards the OS loader-cache skew on the first sample.
    for _ in 0..3 {
        let warm = Client::new_with_data_dir(data_dir).await.0;
        warm.close().await;
    }
    let mut startup_samples: Vec<f64> = Vec::with_capacity(STARTUP_RUNS);
    for _ in 0..STARTUP_RUNS {
        let spawn = tokio::time::Instant::now();
        let (client, _response, init_end) = Client::new_with_data_dir(data_dir).await;
        // `init_end` is captured right after `Client::new_with_data_dir` returns
        // (i.e. after the `initialize` handshake + `notifications/initialized`),
        // so it marks the end of the startup window; the spawn start is `spawn`.
        startup_samples.push((init_end - spawn).as_secs_f64() * 1000.0);
        client.close().await;
    }
    let startup = percentile_report(&mut startup_samples);
    assert!(
        startup_samples.len() >= MIN_SAMPLES,
        "the startup measurement produced enough samples"
    );

    // ---- Retrieval p95 over all five tools, one warm client. ----
    let (mut client, _resp, _t) = Client::new_with_data_dir(data_dir).await;

    // The corpus identity the numbers are attributed to, read from the served
    // `list_sources` response so it is correct for *any* corpus (not just the
    // bundled one). The in-process identity is asserted equal for the shipped
    // corpus so a drift fails loudly.
    let list_sources_response = client.call_tool("list_sources", json!({})).await;
    assert_non_empty_list_sources(&list_sources_response);
    let corpus_identity = corpus_identity_from_response(&list_sources_response, expected_identity);

    // Enumerate the command catalog from the served `list_sources` so the
    // worst-command measurement tracks the corpus's real catalog, not a
    // hard-coded list that can drift.
    let commands: Vec<String> = list_sources_response
        .pointer("/result/structuredContent/commands")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !commands.is_empty(),
        "the served catalog must expose command keys"
    );

    // Prove the fixed sets actually retrieve (a zero-hit "fast" result must
    // never masquerade as a retrieval) before timing anything.
    for query in SEARCH_QUERIES {
        let response = client
            .call_tool("search_docs", json!({ "query": query, "limit": 5 }))
            .await;
        assert_non_empty_search(&response, query);
    }
    for section in SECTION_IDS {
        let response = client
            .call_tool("get_section", json!({ "section_id": section }))
            .await;
        assert_section_ok(&response, section);
    }
    for category in CHECKLIST_CATEGORIES {
        let response = client
            .call_tool("get_api_checklist", json!({ "category": category }))
            .await;
        assert_checklist_ok(&response, category);
    }
    for command in &commands {
        let response = client
            .call_tool(
                "get_command_guidance",
                json!({ "command": command, "limit": 1 }),
            )
            .await;
        assert_guidance_ok(&response, command, 1);
    }

    // Sample the serving process's RSS once the corpus is loaded (the index is
    // built at startup, so this is a representative peak for the corpus).
    let peak_rss_kib = sample_rss_kib(client.pid());

    // ---- (a) `list_sources` p95. ----
    let mut list_sources_times = Vec::with_capacity(RETRIEVAL_ITERS);
    for _ in 0..RETRIEVAL_ITERS {
        let start = tokio::time::Instant::now();
        let _ = client.call_tool("list_sources", json!({})).await;
        list_sources_times.push((tokio::time::Instant::now() - start).as_secs_f64() * 1000.0);
    }

    // ---- (b) `search_docs` p95 over the fixed query set. ----
    let mut search_times: Vec<f64> = Vec::with_capacity(SEARCH_QUERIES.len() * RETRIEVAL_ITERS);
    for query in SEARCH_QUERIES {
        for _ in 0..RETRIEVAL_ITERS {
            let start = tokio::time::Instant::now();
            let _ = client
                .call_tool("search_docs", json!({ "query": query, "limit": 5 }))
                .await;
            search_times.push((tokio::time::Instant::now() - start).as_secs_f64() * 1000.0);
        }
    }

    // ---- (c) `get_section` p95 over the fixed section set. ----
    let mut section_times: Vec<f64> = Vec::with_capacity(SECTION_IDS.len() * RETRIEVAL_ITERS);
    for section in SECTION_IDS {
        for _ in 0..RETRIEVAL_ITERS {
            let start = tokio::time::Instant::now();
            let _ = client
                .call_tool("get_section", json!({ "section_id": section }))
                .await;
            section_times.push((tokio::time::Instant::now() - start).as_secs_f64() * 1000.0);
        }
    }

    // ---- (d) `get_api_checklist` p95 over the unfiltered "all" read plus each
    //     named category. ----
    let mut checklist_times: Vec<f64> =
        Vec::with_capacity((CHECKLIST_CATEGORIES.len() + 1) * RETRIEVAL_ITERS);
    for _ in 0..RETRIEVAL_ITERS {
        let start = tokio::time::Instant::now();
        let _ = client.call_tool("get_api_checklist", json!({})).await;
        checklist_times.push((tokio::time::Instant::now() - start).as_secs_f64() * 1000.0);
    }
    for category in CHECKLIST_CATEGORIES {
        for _ in 0..RETRIEVAL_ITERS {
            let start = tokio::time::Instant::now();
            let _ = client
                .call_tool("get_api_checklist", json!({ "category": category }))
                .await;
            checklist_times.push((tokio::time::Instant::now() - start).as_secs_f64() * 1000.0);
        }
    }

    // ---- (e) `get_command_guidance` p95: a per-command sweep finds the worst
    //     supported command; the worst and its continuation page are then
    //     re-measured at the full iteration count. This is the guidance path the
    //     issue #175 regression was about. ----
    // First pass: a modest sweep over the whole catalog to find the slowest
    // command (its p95 over the sweep).
    let mut per_command_p95: Vec<(f64, String)> = Vec::with_capacity(commands.len());
    for command in &commands {
        let mut times = Vec::with_capacity(GUIDANCE_COMMAND_ITERS);
        for _ in 0..GUIDANCE_COMMAND_ITERS {
            let start = tokio::time::Instant::now();
            let _ = client
                .call_tool(
                    "get_command_guidance",
                    json!({ "command": command, "limit": LARGE_GUIDANCE_LIMIT }),
                )
                .await;
            times.push((tokio::time::Instant::now() - start).as_secs_f64() * 1000.0);
        }
        per_command_p95.push((percentile_report(&mut times).p95_ms, command.clone()));
    }
    // The worst command is the one with the highest p95 in the sweep.
    let (_worst_p95_ms, worst_command) = per_command_p95
        .iter()
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(ms, cmd)| (*ms, cmd.clone()))
        .expect("the command sweep produced at least one command");

    // Re-measure the worst command at the full iteration count (the headline
    // worst-case guidance number) and capture a real continuation cursor from a
    // truncated first page, so the continuation below resumes an actual large
    // guidance page.
    let mut worst_times = Vec::with_capacity(RETRIEVAL_ITERS);
    let mut first_page_next_cursor: Option<String> = None;
    for _ in 0..RETRIEVAL_ITERS {
        let start = tokio::time::Instant::now();
        let response = client
            .call_tool(
                "get_command_guidance",
                json!({ "command": worst_command, "limit": LARGE_GUIDANCE_LIMIT }),
            )
            .await;
        if first_page_next_cursor.is_none()
            && let Some(cursor) = response
                .pointer("/result/structuredContent/next_cursor")
                .and_then(Value::as_str)
        {
            first_page_next_cursor = Some(cursor.to_string());
        }
        worst_times.push((tokio::time::Instant::now() - start).as_secs_f64() * 1000.0);
    }
    let worst_guidance_p95 = percentile_report(&mut worst_times).p95_ms;

    // The continuation page of the worst command (a second, resumed large page).
    let continuation_p95 = match first_page_next_cursor {
        Some(cursor) => {
            let mut times = Vec::with_capacity(RETRIEVAL_ITERS);
            for _ in 0..RETRIEVAL_ITERS {
                let start = tokio::time::Instant::now();
                let _ = client
                    .call_tool(
                        "get_command_guidance",
                        json!({ "command": worst_command, "limit": LARGE_GUIDANCE_LIMIT, "cursor": cursor }),
                    )
                    .await;
                times.push((tokio::time::Instant::now() - start).as_secs_f64() * 1000.0);
            }
            Some(percentile_report(&mut times).p95_ms)
        }
        None => None,
    };
    client.close().await;

    let list_sources_p95 = percentile_report(&mut list_sources_times).p95_ms;
    let search_p95 = percentile_report(&mut search_times).p95_ms;
    let section_p95 = percentile_report(&mut section_times).p95_ms;
    let checklist_p95 = percentile_report(&mut checklist_times).p95_ms;
    assert!(
        list_sources_times.len() >= MIN_SAMPLES
            && search_times.len() >= MIN_SAMPLES
            && section_times.len() >= MIN_SAMPLES
            && checklist_times.len() >= MIN_SAMPLES
            && worst_times.len() >= MIN_SAMPLES,
        "the retrieval measurement produced enough samples"
    );

    // ---- Guidance block + target comparison. ----
    let startup_target_ms = 2000.0;
    let retrieval_target_ms = 100.0;
    // The guidance path is the issue #175 regression: the worst command must now
    // be well under the retrieval target (sub-100 ms), where it was ~3.4 s
    // before the fix. The `meets_target` verdict covers every timed tool.
    let guidance_meets_target = worst_guidance_p95 <= retrieval_target_ms
        && continuation_p95.is_none_or(|c| c <= retrieval_target_ms);

    let per_command = per_command_p95
        .iter()
        .map(|(ms, cmd)| json!({ "command": cmd, "p95_ms": round(*ms) }))
        .collect::<Vec<_>>();

    json!({
        "label": label,
        "corpus": corpus_identity,
        "startup": {
            "runs": STARTUP_RUNS,
            "min_ms": round(startup.min_ms),
            "median_ms": round(startup.median_ms),
            "p95_ms": round(startup.p95_ms),
            "max_ms": round(startup.max_ms),
            "target_ms": startup_target_ms,
            "meets_target": startup.p95_ms <= startup_target_ms,
        },
        "retrieval": {
            "iters_per_item": RETRIEVAL_ITERS,
            "queries": SEARCH_QUERIES.len(),
            "sections": SECTION_IDS.len(),
            "list_sources_p95_ms": round(list_sources_p95),
            "search_p95_ms": round(search_p95),
            "section_p95_ms": round(section_p95),
            "checklist_p95_ms": round(checklist_p95),
            "target_ms": retrieval_target_ms,
            "meets_target": list_sources_p95 <= retrieval_target_ms
                && search_p95 <= retrieval_target_ms
                && section_p95 <= retrieval_target_ms
                && checklist_p95 <= retrieval_target_ms,
        },
        "guidance": {
            "commands_measured": commands.len(),
            "worst_command": worst_command,
            "worst_p95_ms": round(worst_guidance_p95),
            "continuation_p95_ms": continuation_p95.map(round),
            "target_ms": retrieval_target_ms,
            "meets_target": guidance_meets_target,
            "per_command_p95_ms": per_command,
            "note": "issue #175: the worst command's guidance is the multi-section regression; \
                     it is now sub-100 ms (was ~3.4 s before the index reuse fix)"
        },
        "memory": {
            "peak_rss_kib": peak_rss_kib,
            "note": "approximation: sampled once after the corpus is loaded",
        },
    })
}

/// Build the corpus-identity block the numbers are attributed to, from the
/// served `list_sources` response so it is correct for any corpus. When
/// `expected_identity` is given (the shipped corpus), the served identity is
/// asserted equal to it, so a corpus drift fails loudly instead of silently.
fn corpus_identity_from_response(
    list_sources_response: &Value,
    expected_identity: Option<&rust_docs_mcp::acceptance::CorpusIdentity>,
) -> Value {
    let snapshot_id = list_sources_response
        .pointer("/result/structuredContent/snapshot_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let corpus_hash = list_sources_response
        .pointer("/result/structuredContent/corpus_hash")
        .and_then(Value::as_str)
        .map(str::to_string);
    let sources = list_sources_response
        .pointer("/result/structuredContent/sources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Per-source section counts sum to the corpus's total section count.
    let section_count: usize = sources
        .iter()
        .filter_map(|s| s.get("section_count").and_then(Value::as_u64))
        .map(|n| usize::try_from(n).unwrap_or(0))
        .sum();
    let command_count = list_sources_response
        .pointer("/result/structuredContent/commands")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    if let Some(expected) = expected_identity {
        assert_eq!(
            &snapshot_id, &expected.snapshot_id,
            "the served corpus identity must match the in-process shipped identity"
        );
        assert_eq!(
            section_count, expected.section_count,
            "the served section count must match the in-process shipped identity"
        );
        json!({
            "label": "shipped-bundled-seed",
            "snapshot_id": expected.snapshot_id,
            "parser_version": expected.parser_version,
            "section_count": expected.section_count,
            "content_fingerprint": expected.content_fingerprint,
            "corpus_hash": corpus_hash,
            "command_count": command_count,
            "sources": expected.sources,
        })
    } else {
        json!({
            "label": "data-dir-selected",
            "snapshot_id": snapshot_id,
            "section_count": section_count,
            "corpus_hash": corpus_hash,
            "command_count": command_count,
            "sources": sources
                .iter()
                .filter_map(|s| {
                    s.get("id")
                        .or_else(|| s.get("source"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>(),
        })
    }
}

/// The fast, always-on guard that the harness's fixed `SECTION_IDS` name *real*
/// shipped sections, not the stale placeholders the harness measured before issue
/// #156. Runs in the ordinary suite (not `#[ignore]`d), so a corrupted or
/// regenerated corpus that drops one of the fixed ids fails here even when the
/// heavy measurement is skipped. A missing id is a hard failure, never a silent
/// one.
#[test]
fn perf_corpus_section_ids_are_real() {
    assert_eq!(
        rust_docs_mcp::bundled::section_count(),
        rust_docs_mcp::acceptance::shipped_corpus_identity().section_count,
        "the corpus the fixed ids resolve against must be the shipped corpus"
    );
    for section in SECTION_IDS {
        assert!(
            rust_docs_mcp::bundled::has_section(section),
            "fixed section id {section:?} is not in the shipped corpus — re-pin it to a \
             real section id (the harness measured stale ids before issue #156)"
        );
    }
}

/// The best-effort `rustc --version` string the measurement ran under, or a
/// sentinel when `rustc` is not on the PATH. A number is still recorded (with the
/// sentinel) rather than failing, so the harness never blocks on a missing tool.
fn rustc_version() -> String {
    std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "rustc (unavailable on PATH)".to_string())
}

/// Print the report to stderr and, when `RUST_DOCS_MCP_PERF_REPORT` names a path,
/// write a pretty JSON copy there. Writing a diagnostic to stderr (never stdout)
/// keeps the harness consistent with the server's stdio-exclusivity contract.
fn emit_report(report: &Value) {
    let pretty =
        serde_json::to_string_pretty(report).expect("serializing the report must not fail");
    eprintln!(
        "\n=== rust-docs-mcp performance report ===\n{pretty}\n=============================="
    );

    if let Some(path) = std::env::var_os("RUST_DOCS_MCP_PERF_REPORT") {
        std::fs::write(&path, format!("{pretty}\n"))
            .unwrap_or_else(|error| panic!("writing the report to {path:?} failed: {error}"));
    }
}
