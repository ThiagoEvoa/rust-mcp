//! Performance measurement harness for task `[071][M5]` — *"Measure
//! full-corpus startup, retrieval latency, and memory"*.
//!
//! This test drives the **real** compiled `rust-docs-mcp` binary over a spawned
//! OS-stdio session (the same seam as [`stdio_mcp.rs`](stdio_mcp.rs)) and
//! records three numbers the plan's §11 provisional targets name:
//!
//! 1. **Startup** — process spawn → first `initialize` response, over a fixed
//!    number of fresh processes (min / median / p95 / max).
//! 2. **Retrieval p95** — the p95 round-trip latency of `search_docs` and
//!    `get_section` over a **fixed query set** and a **fixed section set**, each
//!    issued a fixed number of times.
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
//! RUST_DOCS_MCP_PERF=1 cargo test --release --test perf -- --ignored -- --nocapture
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
//! `serve` binary always loads at compile time (`crate::bundled`). It is fixed,
//! offline, and hermetic: no network, a per-run empty `--data-dir`, a fixed
//! protocol version, and fixed query/section sets and iteration counts. The
//! corpus measured and its caveats are stated in
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

/// The fixed `get_section` id set — the eight seed sections, by their canonical
/// ids.
const SECTION_IDS: [&str; 8] = [
    "api-guidelines/about",
    "style-guide/intro",
    "cargo/commands%2Fcargo-build",
    "rustc/introduction",
    "rustup/common-install-problems",
    "rustfmt/options",
    "clippy/configuration",
    "rustdoc/command-line-arguments",
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
    /// does. Returns the client together with the `initialize` response and the
    /// instant the response was fully read, so the caller can measure startup.
    async fn new() -> (Self, Value, tokio::time::Instant) {
        let data_dir = fresh_data_dir();
        let mut child = Command::new(env!("CARGO_BIN_EXE_rust-docs-mcp"))
            .arg("serve")
            .arg("--data-dir")
            .arg(&data_dir)
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
    /// other spawned-stdio tests do, so each measured process terminates. The
    /// wait is bounded: the server exits on stdin EOF, but if it does not we do
    /// not block the harness forever — `kill_on_drop` reaps the child on drop.
    async fn close(mut self) {
        let _ = self.stdin.shutdown().await;
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
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

/// The performance measurement the plan's §11 provisional targets name.
///
/// This is the gated entry point. It is `#[ignore]`d (so the normal suite skips
/// it) and additionally guarded by `RUST_DOCS_MCP_PERF` (so even an explicit
/// `--ignored` run is a fast no-op unless the operator opts in). When it runs, it
/// measures startup, retrieval p95, and peak RSS, prints a report to stderr, and
/// — when `RUST_DOCS_MCP_PERF_REPORT` names a path — writes a JSON copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in performance harness — run with `RUST_DOCS_MCP_PERF=1 cargo test --release --test perf -- --ignored -- --nocapture`; see docs/PERFORMANCE.md"]
async fn measure_startup_retrieval_and_memory() {
    // The env-var guard makes an explicit `--ignored` run a fast no-op unless the
    // operator opts in; the `#[ignore]` keeps the normal suite off the harness.
    if std::env::var_os("RUST_DOCS_MCP_PERF").is_none() {
        eprintln!(
            "[perf] RUST_DOCS_MCP_PERF is unset — skipping the measurement (fast no-op). \
             Run with `RUST_DOCS_MCP_PERF=1 cargo test --release --test perf -- --ignored -- --nocapture`."
        );
        return;
    }

    // ---- Startup: spawn -> first `initialize` response over fresh processes. ----
    // A small warmup discards the OS loader-cache skew on the first sample.
    for _ in 0..3 {
        let warm = Client::new().await.0;
        warm.close().await;
    }
    let mut startup_samples: Vec<f64> = Vec::with_capacity(STARTUP_RUNS);
    for _ in 0..STARTUP_RUNS {
        let spawn = tokio::time::Instant::now();
        let (client, _response, init_end) = Client::new().await;
        // `init_end` is captured right after `Client::new` returns (i.e. after
        // the `initialize` handshake + `notifications/initialized`), so it marks
        // the end of the startup window; the spawn start is `spawn`.
        startup_samples.push((init_end - spawn).as_secs_f64() * 1000.0);
        client.close().await;
    }
    let startup = percentile_report(&mut startup_samples);
    assert!(
        startup_samples.len() >= MIN_SAMPLES,
        "the startup measurement produced enough samples"
    );

    // ---- Retrieval p95 over the fixed query + section sets, one warm client. ----
    let (mut client, _resp, _t) = Client::new().await;

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

    // Sample the serving process's RSS once the corpus is loaded (the index is
    // built at startup, so this is a representative peak for the small seed).
    let peak_rss_kib = sample_rss_kib(client.pid());

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
    client.close().await;

    let search_p95 = percentile_report(&mut search_times).p95_ms;
    let section_p95 = percentile_report(&mut section_times).p95_ms;
    assert!(
        search_times.len() >= MIN_SAMPLES && section_times.len() >= MIN_SAMPLES,
        "the retrieval measurement produced enough samples"
    );

    // ---- Report + target comparison. ----
    let startup_target_ms = 2000.0;
    let retrieval_target_ms = 100.0;
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let report = json!({
        "ok": true,
        "profile": profile,
        "corpus": SNAPSHOT_ID,
        "host": std::env::consts::OS,
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
            "search_p95_ms": round(search_p95),
            "section_p95_ms": round(section_p95),
            "target_ms": retrieval_target_ms,
            "meets_target": search_p95 <= retrieval_target_ms
                && section_p95 <= retrieval_target_ms,
        },
        "memory": {
            "peak_rss_kib": peak_rss_kib,
            "note": "approximation: sampled once after the corpus is loaded; the seed is small, so this is close to peak",
        },
    });
    emit_report(&report);
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
