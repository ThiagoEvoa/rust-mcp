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

#![allow(clippy::needless_range_loop)]
// The measurement logic is wired in the following commits; the client and the
// fixed query/section sets are already declared so the first commit is a
// compiling, gated skeleton. This allow is removed once the harness is complete.
#![allow(dead_code)]

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
const STARTUP_RUNS: usize = 15;

/// The number of retrieval iterations per query (search) / section (get_section).
/// A fixed count keeps the p95 stable run-to-run.
const RETRIEVAL_ITERS: usize = 200;

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
                Ok(Ok(0)) => panic!(
                    "the server closed stdout before answering `{method}` (id {id})"
                ),
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
                Err(_) => panic!("timed out waiting for the response to `{method}` (id {id})"),
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(trimmed).expect("every stdout line must be JSON");
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
        self.request("tools/call", &json!({ "name": name, "arguments": arguments }))
            .await
    }
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

    // Measurement + report emission are added in the following commits. This
    // gated skeleton proves the harness compiles and is skipped by default.
    let report = json!({ "ok": true });
    emit_report(&report);
}

/// Print the report to stderr and, when `RUST_DOCS_MCP_PERF_REPORT` names a path,
/// write a pretty JSON copy there. Writing a diagnostic to stderr (never stdout)
/// keeps the harness consistent with the server's stdio-exclusivity contract.
fn emit_report(report: &Value) {
    let pretty =
        serde_json::to_string_pretty(report).expect("serializing the report must not fail");
    eprintln!("\n=== rust-docs-mcp performance report ===\n{pretty}\n==============================");

    if let Some(path) = std::env::var_os("RUST_DOCS_MCP_PERF_REPORT") {
        std::fs::write(&path, format!("{pretty}\n")).unwrap_or_else(|error| {
            panic!("writing the report to {path:?} failed: {error}")
        });
    }
}
