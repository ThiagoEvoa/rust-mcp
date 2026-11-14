//! End-to-end snapshot-selection proof for issue #145 (task [P1][QA]).
//!
//! The reported bug: `serve` loaded the embedded bundled snapshot at startup and
//! ignored the local runtime snapshot that `status` reports, so every tool and
//! resource answered from the bundle instead of the operator's selected snapshot.
//!
//! These tests spawn the REAL `rust-docs-mcp` binary over genuine OS stdio and
//! drive it with a raw JSON-RPC 2.0 client, so each assertion traverses the full
//! path `serve --data-dir X` (CLI) → `crate::mcp::serve` (startup snapshot
//! selection) → the handler (the five tools and the two resources). They never
//! construct the handler directly, per the task's constraint.
//!
//! Coverage of the task's acceptance criteria:
//!  * a valid local snapshot whose identity differs from the bundle is served by
//!    *all five tools and both resources* (criterion 1);
//!  * a missing or invalid local corpus falls back to the embedded seed and
//!    reports why on stderr (criterion 2);
//!  * a running process stays on its original snapshot until it is restarted,
//!    after which the new snapshot is selected (criterion 3);
//!  * `status` and the MCP server report the *same* snapshot for one data
//!    directory (criterion 4).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::time::timeout;

/// The snapshot id of the bundled seed — the identity `serve` *wrongly* reported
/// for every request before the fix.
const BUNDLED_SNAPSHOT_ID: &str = "rust-docs-bundled-seed";
/// The MCP protocol version this client negotiates.
const PROTOCOL_VERSION: &str = "2025-11-25";
/// The catalog resource URI (the server's live source-status view).
const CATALOG_URI: &str = "rust-docs://catalog";
/// An upper bound for every child operation, so a hung server fails loudly.
const BUDGET: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Corpus fixtures.
// ---------------------------------------------------------------------------

/// The committed bundled snapshot the test derives its fixtures from — changing
/// only the top-level `snapshot_id` yields a distinct, valid snapshot that shares
/// every section, hash, and cross-reference with the seed (the loader re-parses
/// and re-validates, so semantic validity is preserved).
fn bundled_snapshot_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data/bundled/snapshot.json")
}

/// Write a valid local snapshot to `<data_dir>/corpus/manifest.json` carrying the
/// given `snapshot_id`, derived from the bundled snapshot by changing *only* the
/// top-level `snapshot_id`.
fn write_local_snapshot(data_dir: &Path, snapshot_id: &str) {
    let raw = std::fs::read(bundled_snapshot_path()).expect("read the bundled snapshot fixture");
    let mut manifest: Value =
        serde_json::from_slice(&raw).expect("parse the bundled snapshot fixture");
    manifest
        .as_object_mut()
        .expect("the bundled snapshot is a JSON object")
        .insert("snapshot_id".to_string(), json!(snapshot_id));
    let bytes = serde_json::to_vec(&manifest).expect("serialize the modified snapshot");

    let corpus_dir = data_dir.join("corpus");
    std::fs::create_dir_all(&corpus_dir).expect("create the data directory's corpus folder");
    std::fs::write(corpus_dir.join("manifest.json"), bytes).expect("publish the local snapshot");
}

/// A fresh, unique, empty data directory (a per-process unique temp dir,
/// following the `stdio_lifecycle`/`stdio_mcp` convention so concurrent test
/// processes never collide). Given to the child as `--data-dir`, it keeps the
/// process off the host's real `~/.cache`.
fn fresh_data_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rust-docs-mcp-serve-snapshot-{}-{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&dir).expect("create the temporary data directory");
    dir
}

// ---------------------------------------------------------------------------
// The JSON-RPC-over-stdio client (drives the real binary).
// ---------------------------------------------------------------------------

/// A child `rust-docs-mcp serve` process driven over OS stdio, with its stderr
/// captured so a test can assert *why* the server chose its snapshot.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: tokio::io::BufReader<ChildStdout>,
    stderr: ChildStderr,
    next_id: u64,
}

impl Server {
    /// Spawn `rust-docs-mcp serve --data-dir <dir>`, wired for JSON-RPC over
    /// stdio, with stderr captured. The default logging level is `info`, so the
    /// "which snapshot is active, and why" diagnostic the server writes to stderr
    /// is captured (the task's "report why" requirement).
    async fn spawn(data_dir: &Path) -> Server {
        let mut child = Command::new(binary())
            .arg("serve")
            .arg("--data-dir")
            .arg(data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the rust-docs-mcp binary");

        // Take the stdio handles *before* moving `child` into the struct, so the
        // (non-`Copy`) `Child` is still available to move after its pipes are
        // detached.
        let stdin = child
            .stdin
            .take()
            .expect("the child's stdin is piped and taken");
        let stdout = child
            .stdout
            .take()
            .expect("the child's stdout is piped and taken");
        let stderr = child
            .stderr
            .take()
            .expect("the child's stderr is piped and taken");

        Server {
            child,
            stdin,
            stdout: tokio::io::BufReader::new(stdout),
            stderr,
            next_id: 0,
        }
    }

    /// Send a JSON-RPC request on a single line and read the matching response.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.write_line(&message).await;

        loop {
            let line = self
                .read_line()
                .await
                .unwrap_or_else(|| panic!("server closed stdout while awaiting {method}"));
            let value: Value =
                serde_json::from_str(&line).unwrap_or_else(|_| panic!("non-JSON line: {line:?}"));
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
            // A server-initiated notification or a response for another id is
            // skipped; the request's own response follows.
        }
    }

    /// Send a JSON-RPC notification (no `id`, no response expected).
    async fn notify(&mut self, method: &str, params: Value) {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_line(&message).await;
    }

    async fn write_line(&mut self, value: &Value) {
        let mut bytes = serde_json::to_vec(value).expect("serialize the request");
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .expect("write to the server's stdin");
        self.stdin.flush().await.expect("flush the server's stdin");
    }

    async fn read_line(&mut self) -> Option<String> {
        let mut line = String::new();
        let n = match timeout(BUDGET, self.stdout.read_line(&mut line)).await {
            Ok(Ok(n)) => n,
            Ok(Err(_)) => return None,
            Err(_) => panic!("the server took longer than {BUDGET:?} to respond"),
        };
        if n == 0 {
            return None;
        }
        Some(line.trim_end_matches(['\r', '\n']).to_string())
    }

    /// Complete the MCP handshake (initialize + `initialized` notification).
    async fn initialize(&mut self) {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "rust-docs-mcp-serve-snapshot-test", "version": "1" }
        });
        let response = self.request("initialize", params).await;
        assert_eq!(
            response.get("error"),
            None,
            "the server must initialize: {response}"
        );
        self.notify("notifications/initialized", json!({})).await;
    }

    /// Call `tools/call` and return the full response so the test can inspect
    /// the (structured) result or a typed error.
    async fn call_tool(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({ "name": name, "arguments": args }))
            .await
    }

    /// Read a resource by URI and return the full response.
    async fn read_resource(&mut self, uri: &str) -> Value {
        self.request("resources/read", json!({ "uri": uri })).await
    }

    /// Close our side of stdin (an EOF the stdio transport treats as the end of
    /// the session), wait for the process to exit, and return everything it wrote
    /// to stderr (the diagnostics that report *which* snapshot is served and why).
    async fn close_and_read_stderr(mut self) -> String {
        // Dropping the writer half closes the child's stdin, which ends the
        // session; the process then exits and closes its stderr.
        drop(self.stdin);
        let _ = timeout(BUDGET, self.child.wait()).await;
        let mut stderr = String::new();
        // The child has exited and closed its stderr, so this drains to EOF.
        let _ = timeout(BUDGET, self.stderr.read_to_string(&mut stderr)).await;
        stderr
    }
}

/// The `rust-docs-mcp` binary path: override with `RUST_DOCS_MCP_BINARY` so the
/// test can point at a specific build; otherwise use the test's own binary.
fn binary() -> PathBuf {
    std::env::var_os("RUST_DOCS_MCP_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_rust-docs-mcp")))
}

/// Every `snapshot_id` string anywhere in a response, so a test can assert the
/// server names exactly the expected snapshot and never the bundled seed.
fn snapshot_ids(response: &Value) -> Vec<String> {
    let mut found = Vec::new();
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    // Collect the `snapshot_id` of every object node, wherever it
                    // sits in the response tree.
                    if let ("snapshot_id", Value::String(id)) = (key.as_str(), child) {
                        out.push(id.clone());
                    }
                    walk(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, out)),
            _ => {}
        }
    }
    walk(response, &mut found);
    found
}

/// Assert a `tools/call` response carries a result (no JSON-RPC error) and that
/// *every* `snapshot_id` it reports equals `expected` — and, when `not_bundled`
/// is true, never the bundled seed (the core regression of issue #145).
fn assert_serves_snapshot(response: &Value, expected: &str, not_bundled: bool) {
    assert_eq!(
        response.get("error"),
        None,
        "the call must succeed (no JSON-RPC error): {response}"
    );
    let ids = snapshot_ids(response);
    assert!(
        !ids.is_empty(),
        "the response must name its snapshot; got no snapshot_id: {response}"
    );
    for id in &ids {
        assert_eq!(id, expected, "a tool reported an unexpected snapshot id");
        assert!(
            !not_bundled || id != BUNDLED_SNAPSHOT_ID,
            "the server must NOT report the bundled seed; got it — the #145 regression"
        );
    }
}

// ---------------------------------------------------------------------------
// Acceptance 1: a valid local snapshot is served by all five tools and both
// resources.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn serve_serves_the_local_snapshot_for_all_tools_and_resources() {
    const LOCAL_ID: &str = "audit-local-snapshot";
    let data_dir = fresh_data_dir();
    write_local_snapshot(&data_dir, LOCAL_ID);

    let mut server = Server::spawn(&data_dir).await;
    server.initialize().await;

    // All five tools answer from the local snapshot, never the seed.
    let list = server.call_tool("list_sources", json!({})).await;
    assert_serves_snapshot(&list, LOCAL_ID, true);

    let search = server
        .call_tool("search_docs", json!({ "query": "C-CONV" }))
        .await;
    assert_serves_snapshot(&search, LOCAL_ID, true);

    let section = server
        .call_tool(
            "get_section",
            json!({ "section_id": "api-guidelines/about" }),
        )
        .await;
    assert_serves_snapshot(&section, LOCAL_ID, true);

    let checklist = server
        .call_tool(
            "get_api_checklist",
            json!({ "category": "Conversions and From" }),
        )
        .await;
    assert_serves_snapshot(&checklist, LOCAL_ID, true);

    let guidance = server
        .call_tool("get_command_guidance", json!({ "command": "cargo build" }))
        .await;
    assert_serves_snapshot(&guidance, LOCAL_ID, true);

    // The catalog resource names the local snapshot.
    let catalog = server.read_resource(CATALOG_URI).await;
    assert_eq!(
        catalog.get("error"),
        None,
        "the catalog resource must read: {catalog}"
    );
    let contents = catalog["result"]["contents"]
        .as_array()
        .expect("the catalog resource returns a contents array");
    let text = contents[0]["text"].as_str().expect("a JSON text payload");
    let payload: Value = serde_json::from_str(text).expect("the catalog text is JSON");
    assert_eq!(
        payload.get("snapshot_id"),
        Some(&json!(LOCAL_ID)),
        "the catalog resource must name the local snapshot"
    );

    // The section resource serves a section that exists in the local snapshot
    // (the section ids are identical across the derived snapshot).
    let section_uri = format!("rust-docs://snapshots/{LOCAL_ID}/sections/api-guidelines/about");
    let section_resource = server.read_resource(&section_uri).await;
    assert_eq!(
        section_resource.get("error"),
        None,
        "the section resource must read: {section_resource}"
    );
    let contents = section_resource["result"]["contents"]
        .as_array()
        .expect("the section resource returns a contents array");
    let text = contents[0]["text"].as_str().expect("a JSON text payload");
    let payload: Value = serde_json::from_str(text).expect("the section resource text is JSON");
    assert_eq!(
        payload["section"]["id"],
        json!("api-guidelines/about"),
        "the section resource serves the requested section"
    );

    // The server reports, on stderr, that it is serving the *local* snapshot
    // (and not the bundled seed).
    let stderr = server.close_and_read_stderr().await;
    assert!(
        stderr.contains("local snapshot") && stderr.contains(LOCAL_ID),
        "the server must report the local snapshot on stderr; got: {stderr}"
    );
    assert!(
        !stderr.contains("embedded bundled snapshot"),
        "the server must not fall back to the bundle when a valid local snapshot exists; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 2: a missing or invalid local corpus falls back to the embedded
// seed and reports why.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_local_corpus_falls_back_to_embedded_and_reports_why() {
    // The data directory exists but has no `corpus/manifest.json`, so the
    // loader falls back to the embedded seed.
    let data_dir = fresh_data_dir();

    let mut server = Server::spawn(&data_dir).await;
    server.initialize().await;

    let list = server.call_tool("list_sources", json!({})).await;
    assert_serves_snapshot(&list, BUNDLED_SNAPSHOT_ID, false);

    // The diagnostic states the fallback reason.
    let stderr = server.close_and_read_stderr().await;
    assert!(
        stderr.contains("embedded bundled snapshot"),
        "the server must report that it fell back to the embedded seed; got: {stderr}"
    );
}

#[tokio::test]
async fn invalid_local_corpus_falls_back_to_embedded_and_reports_why() {
    // Publish a syntactically corrupt manifest: the loader rejects it and falls
    // back to the embedded seed, reporting why on stderr.
    let data_dir = fresh_data_dir();
    let corpus_dir = data_dir.join("corpus");
    std::fs::create_dir_all(&corpus_dir).expect("create the corpus folder");
    std::fs::write(corpus_dir.join("manifest.json"), b"not a snapshot manifest")
        .expect("write the corrupt manifest");

    let mut server = Server::spawn(&data_dir).await;
    server.initialize().await;

    let list = server.call_tool("list_sources", json!({})).await;
    assert_serves_snapshot(&list, BUNDLED_SNAPSHOT_ID, false);

    let stderr = server.close_and_read_stderr().await;
    assert!(
        stderr.contains("embedded bundled snapshot"),
        "an invalid local corpus must fall back to the embedded seed; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 3: a running process stays on its original snapshot until it is
// restarted; the restart selects the newly published snapshot.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_running_process_keeps_its_snapshot_until_restarted() {
    let data_dir = fresh_data_dir();
    write_local_snapshot(&data_dir, "audit-v1");

    let mut server = Server::spawn(&data_dir).await;
    server.initialize().await;

    // The running process reports v1.
    let first = server.call_tool("list_sources", json!({})).await;
    assert_serves_snapshot(&first, "audit-v1", true);

    // While it runs, publish a *different* snapshot to the same path.
    write_local_snapshot(&data_dir, "audit-v2");

    // No hot reload: the same running process still reports v1.
    let second = server.call_tool("list_sources", json!({})).await;
    assert_serves_snapshot(&second, "audit-v1", true);

    // End this process.
    let _ = server.close_and_read_stderr().await;

    // A *new* process selects the newly published snapshot.
    let mut restarted = Server::spawn(&data_dir).await;
    restarted.initialize().await;
    let third = restarted.call_tool("list_sources", json!({})).await;
    assert_serves_snapshot(&third, "audit-v2", true);

    let stderr = restarted.close_and_read_stderr().await;
    assert!(
        stderr.contains("audit-v2"),
        "the restarted process must report the newly published snapshot; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 4: `status` and the MCP server report the *same* snapshot for one
// data directory.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_and_mcp_identity_agree_for_the_same_data_dir() {
    const LOCAL_ID: &str = "audit-shared-snapshot";
    let data_dir = fresh_data_dir();
    write_local_snapshot(&data_dir, LOCAL_ID);

    // `status` reports the local snapshot's id on stderr.
    let output = Command::new(binary())
        .arg("status")
        .arg("--data-dir")
        .arg(&data_dir)
        .output()
        .await
        .expect("run the `status` subcommand");
    assert!(
        output.status.success(),
        "`status` must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status_stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        status_stderr.contains(LOCAL_ID),
        "`status` must report the local snapshot's id; got: {status_stderr}"
    );

    // The MCP server, for the same data directory, must report the same id.
    let mut server = Server::spawn(&data_dir).await;
    server.initialize().await;
    let list = server.call_tool("list_sources", json!({})).await;
    assert_serves_snapshot(&list, LOCAL_ID, true);

    let _ = server.close_and_read_stderr().await;
}
