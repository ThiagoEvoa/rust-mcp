//! End-to-end proof of the **first MCP client configuration** for task
//! `[064][M5]` — *"Document and verify the first MCP client configuration"*.
//!
//! # What this is
//!
//! The chosen **first client** is the **raw JSON-RPC 2.0 client over stdio**
//! recorded in [`docs/COMPATIBILITY.md`](../docs/COMPATIBILITY.md) §10.3 — the
//! as-built M4 first client that [`stdio_mcp.rs`](stdio_mcp.rs) also exercises.
//! It is a hand-written client over a newline-delimited stream: **no `rmcp`
//! client** (the `rmcp` `client` feature stays off), **no new dependency**, and
//! **no network**. It exercises the *production* stdio path — the same bytes and
//! the same process boundary a real external agent hits — which is why it is the
//! contract-establishing client for development.
//!
//! This test is the **one tested configuration** for that client: it drives the
//! exact JSON-RPC sequence that a user's `mcpServers` block produces
//! (`command` + `args: ["serve"]`), then proves the three operations of the
//! acceptance criterion — **list sources, search, and retrieve a section with a
//! citation**:
//!
//! 1. `list_sources` returns the eight canonical sources *and* the active
//!    snapshot id (`rust-docs-bundled-seed`).
//! 2. `search_docs` for a query that matches the seed corpus (`"C-CONV"`)
//!    returns ranked hits, each backed by a non-empty `citations` array.
//! 3. `get_section` for a valid id (`"api-guidelines/about"`) returns the
//!    section body *with its provenance citations and canonical URL*.
//!
//! The check is hermetic: a per-test temp `--data-dir`, and `serve` loads the
//! bundled snapshot in-memory (no network, no host cache). `stderr` is piped and
//! discarded, so a diagnostic can never leak onto the protocol stream.
//! `kill_on_drop(true)` is the safety net that reaps the child on a panic, but
//! the assertions prove a graceful self-exit on a clean client disconnect, not a
//! forced kill.
//!
//! # Transcript
//!
//! When the environment variable `RUST_DOCS_MCP_TRANSCRIPT` names a path, the
//! test writes the **request/response transcript** of the whole session (the
//! `initialize` handshake plus the three tool calls, with their real responses)
//! as a pretty JSON array to that path. That is how the AC2 evidence in
//! [`docs/CLIENTS.md`](../docs/CLIENTS.md) is reproduced:
//!
//! ```sh
//! cargo test --test first_client -- --nocapture \
//!   # set RUST_DOCS_MCP_TRANSCRIPT to a path first, e.g.:
//!   RUST_DOCS_MCP_TRANSCRIPT=/tmp/first_client_transcript.json \
//!   cargo test --test first_client -- --exact \
//!     first_client_configuration_lists_searches_and_retrieves_with_citations
//! jq . /tmp/first_client_transcript.json
//! ```
//!
//! The authoritative, full-surface automated proof (all five tools and both
//! resources, plus stdout protocol integrity) remains
//! [`stdio_mcp.rs`](stdio_mcp.rs); this file is the focused, transcript-capturing
//! proof of exactly the three AC2 operations.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;

/// How long the test waits for a response and, after a clean disconnect, for the
/// server to exit. A real stdio round-trip on localhost is sub-second; this
/// generous budget keeps the test fast while tolerating CI load.
const BUDGET: Duration = Duration::from_secs(15);

/// The protocol version the client negotiates. `2025-11-25` is the highest
/// `initialize`-capable version the server negotiates to (a client that asks for
/// `2026-07-28` is down-negotiated to it, since that revision dropped the
/// `initialize` handshake), so it exercises the full `tools` + `resources`
/// surface.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// The active bundled snapshot's id — the seed corpus `serve` always loads.
const SNAPSHOT_ID: &str = "rust-docs-bundled-seed";

/// The eight canonical source collections, in registry order.
const SOURCES: [&str; 8] = [
    "api-guidelines",
    "style-guide",
    "cargo",
    "rustc",
    "rustup",
    "rustfmt",
    "clippy",
    "rustdoc",
];

/// The query and section the AC2 evidence uses: an exact guideline-code search
/// that matches the seed corpus, and the section it resolves to.
const SEARCH_QUERY: &str = "C-CONV";
const SECTION_ID: &str = "api-guidelines/about";

/// A temporary per-test data directory, given to the child as `--data-dir` so
/// the process never writes the host's real application cache. `serve` does not
/// consult it (the bundled snapshot is loaded in-memory), but the flag keeps the
/// child's behaviour identical to the lifecycle test's hermetic posture.
fn fresh_data_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rust-docs-mcp-first-client-{}-{}",
        std::process::id(),
        stamp,
    ))
}

/// One captured request/response pair for the transcript.
struct Exchange {
    request: Value,
    response: Value,
}

/// A minimal JSON-RPC 2.0 client over a spawned `serve` process. It hands one
/// request at a time, reads stdout one newline-framed JSON-RPC message at a
/// time, and records every request/response pair for the transcript. A response
/// is matched to its request by `id` (the server may answer out of order), and
/// every stdout line is asserted to be a JSON-RPC 2.0 message — the "stdout
/// carries only protocol traffic" half of the configuration.
struct Client {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    deadline: tokio::time::Instant,
    transcript: Vec<Exchange>,
}

impl Client {
    /// Spawn the `serve` binary over piped stdio and complete the `initialize`
    /// handshake + `notifications/initialized`, the way a conforming MCP client
    /// does.
    async fn new() -> Self {
        let data_dir = fresh_data_dir();
        let mut child = Command::new(env!("CARGO_BIN_EXE_rust-docs-mcp"))
            .arg("serve")
            .arg("--data-dir")
            .arg(&data_dir)
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
            transcript: Vec::new(),
        };

        client.initialize().await;
        client.notify("notifications/initialized").await;
        client
    }

    /// Send the `initialize` request and assert the server echoes the negotiated
    /// protocol version and its implementation identity.
    async fn initialize(&mut self) {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "first-client", "version": "0.0.1" },
        });
        let response = self.request("initialize", &params).await;
        assert_eq!(
            response.get("error"),
            None,
            "the initialization handshake must succeed, not error: {response}"
        );
        assert_eq!(
            response["result"]
                .get("protocolVersion")
                .and_then(Value::as_str),
            Some(PROTOCOL_VERSION),
            "the server must echo the protocol version the client selected"
        );
        assert_eq!(
            response["result"]
                .pointer("/serverInfo/name")
                .and_then(Value::as_str),
            Some(env!("CARGO_PKG_NAME")),
            "the handshake must carry the documented server identity"
        );
    }

    /// Call a tool by name with the given arguments object, returning its
    /// response.
    async fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            &json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn notify(&mut self, method: &str) {
        let message = json!({ "jsonrpc": "2.0", "method": method });
        self.write_frame(&message).await;
    }

    /// Send one numbered request, read the matching response, and record the
    /// exchange for the transcript.
    async fn request(&mut self, method: &str, params: &Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let response = self.exchange(method, params, id).await;
        // Record the exchange (a clone) for the transcript, then return the
        // original to the caller.
        self.transcript.push(Exchange {
            request: self.last_frame(method, params, id),
            response: response.clone(),
        });
        response
    }

    /// Rebuild the exact frame a numbered request is sent as, so the transcript
    /// records the request the client actually wrote.
    fn last_frame(&self, method: &str, params: &Value, id: u64) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    /// Write one newline-framed JSON-RPC message and flush.
    async fn write_frame(&mut self, message: &Value) {
        let frame =
            serde_json::to_string(message).expect("encoding a hand-built JSON value must not fail");
        self.stdin
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("writing a JSON-RPC message to the child's stdin must not fail");
        self.stdin
            .flush()
            .await
            .expect("flushing the child's stdin must not fail");
    }

    /// Send one request and read the matching response, asserting every stdout
    /// line up to it is a JSON-RPC 2.0 message. Returns the response whose `id`
    /// matches the request.
    async fn exchange(&mut self, method: &str, params: &Value, id: u64) -> Value {
        self.write_frame(&self.last_frame(method, params, id)).await;

        // Read lines until the one whose `id` matches. Every line must be a
        // JSON-RPC 2.0 message — the "stdout carries only protocol traffic"
        // assertion for this request.
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
            // Every stdout line must be a JSON-RPC 2.0 message.
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

    /// Close our side of the child's stdin (an EOF the stdio transport treats as
    /// the end of the session), drain whatever the server still writes, assert
    /// every remaining line is JSON-RPC 2.0, and assert a clean self-exit.
    async fn close_and_wait_clean(mut self) {
        drop(self.stdin);

        loop {
            let mut line = String::new();
            match tokio::time::timeout(BUDGET, self.reader.read_line(&mut line)).await {
                Ok(Ok(0)) => break, // EOF: the server closed stdout and exited.
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
                Err(_) => {
                    panic!("timed out draining the child's stdout after the final response")
                }
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value =
                serde_json::from_str(trimmed).expect("every tail stdout line must be JSON");
            assert_eq!(
                value.get("jsonrpc").and_then(Value::as_str),
                Some("2.0"),
                "a non-JSON-RPC object leaked onto the tail of stdout: {trimmed}"
            );
        }

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

    /// Write the captured transcript (a pretty JSON array of request/response
    /// pairs) to `path`, when the `RUST_DOCS_MCP_TRANSCRIPT` variable is set.
    fn maybe_write_transcript(&self, path_hint: &str) {
        let Some(path) = std::env::var_os(path_hint) else {
            return;
        };
        let entries: Vec<Value> = self
            .transcript
            .iter()
            .map(|exchange| {
                json!({
                    "request": exchange.request,
                    "response": exchange.response,
                })
            })
            .collect();
        let document = json!({
            "task": "[064][M5] first MCP client configuration",
            "client": "raw JSON-RPC 2.0 over stdio (tests/first_client.rs)",
            "protocol_version": PROTOCOL_VERSION,
            "exchanges": entries,
        });
        let pretty = serde_json::to_string_pretty(&document)
            .expect("serializing the transcript must not fail");
        std::fs::write(&path, format!("{pretty}\n"))
            .unwrap_or_else(|error| panic!("writing the transcript to {path:?} failed: {error}"));
    }
}

// ---------------------------------------------------------------------------
// Test.
// ---------------------------------------------------------------------------

/// The one tested configuration for the first client: drive the exact
/// `initialize` → `list_sources` → `search_docs` → `get_section` sequence and
/// prove the three AC2 operations, then a clean disconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_client_configuration_lists_searches_and_retrieves_with_citations() {
    let mut client = Client::new().await;

    // ---- AC2(1): list_sources returns the eight sources + the snapshot id. --
    let sources = client.call_tool("list_sources", json!({})).await;
    assert_eq!(
        sources.get("error"),
        None,
        "list_sources must succeed: {sources}"
    );
    let structured = &sources["result"]["structuredContent"];
    assert_eq!(
        structured["snapshot_id"], SNAPSHOT_ID,
        "list_sources names the active snapshot"
    );
    let source_ids: Vec<&str> = structured["sources"]
        .as_array()
        .expect("list_sources returns a `sources` array")
        .iter()
        .filter_map(|source| source["id"].as_str())
        .collect();
    assert_eq!(
        source_ids.len(),
        SOURCES.len(),
        "list_sources returns all eight canonical sources"
    );
    for expected in SOURCES {
        assert!(
            source_ids.contains(&expected),
            "list_sources must name the `{expected}` source; got {source_ids:?}"
        );
    }

    // ---- AC2(2): search_docs returns ranked hits, each cited. ---------------
    let search = client
        .call_tool("search_docs", json!({ "query": SEARCH_QUERY }))
        .await;
    assert_eq!(
        search.get("error"),
        None,
        "a valid search must succeed: {search}"
    );
    let structured = &search["result"]["structuredContent"];
    assert_eq!(
        structured["snapshot_id"], SNAPSHOT_ID,
        "search_docs names the active snapshot"
    );
    let hits = structured["hits"]
        .as_array()
        .expect("a valid search returns a `hits` array");
    assert!(
        !hits.is_empty(),
        "the query `{SEARCH_QUERY}` matches a section in the seed corpus"
    );
    for hit in hits {
        assert!(
            hit.get("section_id").and_then(Value::as_str).is_some(),
            "every hit names a section id: {hit}"
        );
        assert!(
            hit.get("source").and_then(Value::as_str).is_some(),
            "every hit names its source: {hit}"
        );
        // Every hit is backed by a non-empty, schema-conformant citation array.
        let citations = hit
            .get("citations")
            .and_then(Value::as_array)
            .expect("every hit carries citations: {hit}");
        assert!(
            !citations.is_empty(),
            "every hit carries at least one citation: {hit}"
        );
        for citation in citations {
            assert!(
                citation.get("source").and_then(Value::as_str).is_some(),
                "every citation names its source: {citation}"
            );
        }
    }

    // ---- AC2(3): get_section returns a section *with a citation*. -----------
    let section = client
        .call_tool("get_section", json!({ "section_id": SECTION_ID }))
        .await;
    assert_eq!(
        section.get("error"),
        None,
        "a valid section must resolve: {section}"
    );
    let structured = &section["result"]["structuredContent"]["section"];
    assert_eq!(
        structured["id"], SECTION_ID,
        "the section resolves to the requested id"
    );
    assert_eq!(
        structured["source"],
        json!("api-guidelines"),
        "the section carries its source"
    );
    assert!(
        structured["body"]
            .as_str()
            .is_some_and(|body| !body.is_empty()),
        "the section body is served"
    );
    // Provenance: the canonical URL is present and the section is cited.
    assert!(
        structured.get("canonical_url").is_some(),
        "the section carries its canonical URL (provenance)"
    );
    let citations = structured
        .get("citations")
        .and_then(Value::as_array)
        .expect("the section carries citations: {structured}");
    assert!(
        !citations.is_empty(),
        "the section carries at least one citation"
    );
    for citation in citations {
        assert!(
            citation.get("source").and_then(Value::as_str).is_some(),
            "every section citation names its source: {citation}"
        );
    }

    // Persist the request/response transcript for the docs, when requested.
    client.maybe_write_transcript("RUST_DOCS_MCP_TRANSCRIPT");

    // ---- Clean disconnect: close our stdin; the server self-exits. ---------
    client.close_and_wait_clean().await;
}
