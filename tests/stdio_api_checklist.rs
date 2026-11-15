//! Assembled stdio proof for issue #148 (task `[057][M4]`, checklist retrieval).
//!
//! Issue #148 fixed the API Guidelines ingestion so the checklist is parsed by
//! *document identity* (`…/checklist.html`) and a nonempty checklist is
//! *required* — before the fix, a successful refresh published a **zero-entry**
//! checklist, so `get_api_checklist` returned nothing even though the corpus was
//! healthy. The companion unit/integration tests in `src/ingest/pipeline.rs`
//! prove the pipeline now *publishes* a nonempty, explained checklist; this test
//! proves the other half of the acceptance criterion — that a published snapshot
//! carrying a nonempty checklist is **retrievable through `get_api_checklist`
//! over the assembled stdio path**.
//!
//! It runs the *real* compiled `rust-docs-mcp` binary as a separate process over
//! genuine OS stdio and drives the assembled MCP surface with raw JSON-RPC 2.0
//! over a newline-delimited stream — no `rmcp` client. It never constructs the
//! handler directly (per the task's constraint), so each assertion traverses the
//! full path `serve --data-dir X` (CLI) → `crate::mcp::serve` (startup snapshot
//! selection) → the `get_api_checklist` handler → the assembled `CallToolResult`.
//!
//! The published snapshot is derived from the committed bundled seed by changing
//! *only* the top-level `snapshot_id`, so it carries the seed's real `C-CONV`
//! checklist entry (category `Naming`, explanation
//! `api-guidelines/naming`) and the section that explains it. The loader
//! re-parses and re-validates, so semantic validity — including every
//! entry's `explanation_sections` resolving — is preserved.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;

/// How long the test will wait for the server to answer a request. A real stdio
/// round-trip on localhost is sub-second; this generous budget keeps the test
/// fast while tolerating CI load.
const BUDGET: Duration = Duration::from_secs(15);

/// The protocol version the test negotiates — the highest `initialize`-capable
/// version the server negotiates to, so it exercises the full `tools` surface.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// The distinct local snapshot id the test publishes — different from the bundled
/// seed, so a passing test proves the *local* snapshot (not the seed) was served.
const LOCAL_SNAPSHOT_ID: &str = "rust-docs-stdapi-148";

/// The checklist entry the published snapshot carries (the seed's real entry).
const CHECKLIST_CODE: &str = "C-CONV";
const CHECKLIST_CATEGORY: &str = "Naming";
const CHECKLIST_LOCATION: &str = "Naming#C-CONV";
const CHECKLIST_EXPLANATION: &str =
    "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv";

/// A per-process unique, empty data directory, given to the child as `--data-dir`
/// so the process never writes the host's real application cache.
fn fresh_data_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rust-docs-mcp-stdapi-148-{}-{}",
        std::process::id(),
        stamp,
    ));
    std::fs::create_dir_all(&dir).expect("create the temporary data directory");
    dir
}

/// The committed bundled snapshot the test derives its fixture from — changing
/// only the top-level `snapshot_id` yields a distinct, valid snapshot that shares
/// every section, hash, and checklist entry with the seed.
fn bundled_snapshot_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data/bundled/snapshot.json")
}

/// Write a valid published snapshot to `<data_dir>/corpus/manifest.json` carrying
/// `LOCAL_SNAPSHOT_ID`, derived from the bundled snapshot by changing *only* the
/// top-level `snapshot_id`. The loader re-parses and re-validates, so the
/// checklist entry and its `explanation_sections` are preserved and valid.
fn write_local_snapshot(data_dir: &Path) {
    let raw = std::fs::read(bundled_snapshot_path()).expect("read the bundled snapshot fixture");
    let mut manifest: Value =
        serde_json::from_slice(&raw).expect("parse the bundled snapshot fixture");
    manifest
        .as_object_mut()
        .expect("the bundled snapshot is a JSON object")
        .insert("snapshot_id".to_string(), json!(LOCAL_SNAPSHOT_ID));
    let bytes = serde_json::to_vec(&manifest).expect("serialize the modified snapshot");

    let corpus_dir = data_dir.join("corpus");
    std::fs::create_dir_all(&corpus_dir).expect("create the data directory's corpus folder");
    std::fs::write(corpus_dir.join("manifest.json"), bytes).expect("publish the local snapshot");
}

// ---------------------------------------------------------------------------
// The driver: a tiny stateful raw-JSON-RPC client over the child's stdio.
// ---------------------------------------------------------------------------

/// A minimal JSON-RPC 2.0 client over a spawned `serve` process. It hands one
/// request at a time, reads stdout one newline-framed JSON-RPC message at a
/// time, and asserts — on *every* line it reads — that the line is a JSON-RPC
/// 2.0 message, so nothing the server logs leaks onto the protocol stream.
struct Client {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    deadline: tokio::time::Instant,
}

impl Client {
    /// Spawn `rust-docs-mcp serve --data-dir <dir>` over piped stdio and
    /// complete the `initialize` handshake (request + `notifications/initialized`).
    async fn new(data_dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rust-docs-mcp"))
            .arg("serve")
            .arg("--data-dir")
            .arg(data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Diagnostics are redirected to stderr and then discarded, so a log
            // the server emits can never leak onto the protocol stream.
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("the test harness must be able to launch the `serve` binary");

        // Take the piped handles out *before* moving `child` into the struct, so
        // we never borrow a value we are about to move (E0382).
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        let mut client = Self {
            child,
            stdin,
            reader: tokio::io::BufReader::new(stdout),
            next_id: 0,
            deadline: tokio::time::Instant::now() + BUDGET,
        };

        client.initialize().await;
        client.notify("notifications/initialized").await;
        client
    }

    /// Send the `initialize` request and assert the server echoes the negotiated
    /// protocol version and its implementation identity.
    async fn initialize(&mut self) {
        let response = self
            .request(
                "initialize",
                &json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "stdapi-148-test", "version": "0.0.1" },
                }),
            )
            .await;
        assert_eq!(
            response.get("error"),
            None,
            "the initialization handshake must succeed, not error: {response}"
        );
        let result = response
            .get("result")
            .expect("`initialize` must return a result object");
        assert_eq!(
            result.get("protocolVersion").and_then(Value::as_str),
            Some(PROTOCOL_VERSION),
            "the server must echo the protocol version the client selected"
        );
        assert_eq!(
            result.pointer("/serverInfo/name").and_then(Value::as_str),
            Some(env!("CARGO_PKG_NAME")),
            "the handshake must carry the documented server identity"
        );
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn notify(&mut self, method: &str) {
        let message = json!({ "jsonrpc": "2.0", "method": method });
        let frame = serde_json::to_string(&message)
            .expect("encoding a hand-built JSON value must not fail");
        self.stdin
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("writing a JSON-RPC notification to the child's stdin must not fail");
        self.stdin
            .flush()
            .await
            .expect("flushing the child's stdin must not fail");
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

    /// Send one request and read the matching response, asserting every stdout
    /// line up to it is a JSON-RPC 2.0 message.
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
            .expect("writing a JSON-RPC request to the child's stdin must not fail");
        self.stdin
            .flush()
            .await
            .expect("flushing the child's stdin must not fail");

        // Read lines until the one whose `id` matches. Every line must be a
        // JSON-RPC 2.0 message — the "stdout carries only protocol traffic"
        // assertion.
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
                    "the server closed stdout before answering `{method}` (id {id}) — \
                     a disconnected server"
                ),
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

    /// Close the client and wait for the server to self-exit on the resulting
    /// EOF, proving a graceful disconnect (not a forced kill). Returns the exit
    /// status so the caller can assert a clean, successful exit.
    async fn expect_clean_exit(mut self) -> std::process::ExitStatus {
        // Releasing the client's stdin sends EOF to the server's stdio reader,
        // after which the `serve` loop returns and the process exits on its own.
        drop(self.stdin);
        drop(self.reader);
        match tokio::time::timeout(BUDGET, self.child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => panic!("waiting on the child failed: {error}"),
            Err(_) => panic!("the child did not exit within {BUDGET:?} — a likely orphan"),
        }
    }
}

// ---------------------------------------------------------------------------
// Test.
// ---------------------------------------------------------------------------

/// The assembled stdio proof for issue #148: a published snapshot carrying a
/// nonempty, explained checklist is **retrievable through `get_api_checklist`**
/// over the assembled stdio path. Before #148 a healthy refresh published a
/// zero-entry checklist, so this retrieval would have returned an empty `entries`
/// array; the fix guarantees the entry is present and its explanation resolves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_api_checklist_retrieves_the_published_checklist_over_stdio() {
    let data_dir = fresh_data_dir();
    write_local_snapshot(&data_dir);

    let mut client = Client::new(&data_dir).await;

    // AC4: list every checklist entry (no category filter). The response carries
    // the published snapshot's checklist — the same entry the pipeline now
    // publishes — and its `explanation_sections` resolve.
    let response = client.call_tool("get_api_checklist", json!({})).await;

    // The call succeeded (no `error`), and the structured content is present.
    assert_eq!(
        response.get("error"),
        None,
        "get_api_checklist must succeed on a healthy published checklist: {response}"
    );
    let result = response
        .get("result")
        .expect("get_api_checklist must return a result object");

    // The response's structured content carries the `entries` array.
    let structured = result
        .get("structuredContent")
        .expect("the result must carry structured content");
    let entries = structured
        .get("entries")
        .and_then(Value::as_array)
        .expect("the structured content must carry an `entries` array");

    // The nonempty checklist is retrievable: the `C-CONV` entry is present.
    let c_conv = entries
        .iter()
        .find(|entry| entry.get("code").and_then(Value::as_str) == Some(CHECKLIST_CODE))
        .expect("the published checklist's C-CONV entry must be retrievable");

    // The entry is the same one the published snapshot carries.
    assert_eq!(
        c_conv.get("category").and_then(Value::as_str),
        Some(CHECKLIST_CATEGORY),
        "the C-CONV entry must carry its upstream category"
    );
    assert_eq!(
        c_conv.get("location").and_then(Value::as_str),
        Some(CHECKLIST_LOCATION),
        "the C-CONV entry must carry its upstream location"
    );

    // And its explanation resolves to the section that explains it — the
    // continuation-aware mapping the pipeline populates and the read verifies.
    let explanation = c_conv
        .get("explanation_sections")
        .and_then(Value::as_array)
        .expect("the C-CONV entry must carry its explanation sections");
    assert!(
        explanation
            .iter()
            .any(|s| s.as_str() == Some(CHECKLIST_EXPLANATION)),
        "the C-CONV entry must resolve to the {CHECKLIST_EXPLANATION} explanation"
    );

    // The response reports the *local* snapshot identity, proving the published
    // (local) snapshot was served, not the embedded seed.
    let reported_id = structured.get("snapshot_id").and_then(Value::as_str);
    assert_eq!(
        reported_id,
        Some(LOCAL_SNAPSHOT_ID),
        "get_api_checklist must report the published local snapshot identity"
    );

    // The server self-exits cleanly when the client disconnects — no orphaned
    // process is left behind.
    let status = client.expect_clean_exit().await;
    assert!(
        status.success(),
        "after a clean client disconnect the server must exit successfully"
    );
}
