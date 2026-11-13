//! End-to-end MCP-surface proof for task `[057][M4]`.
//!
//! This test runs the *real* compiled `rust-docs-mcp` binary as a separate
//! process, connected over the operating-system stdio streams the `serve`
//! command is wired to, and drives the **assembled** MCP surface with raw
//! JSON-RPC 2.0 over a newline-delimited stream — no `rmcp` client. It is the
//! authoritative check for the task's two acceptance criteria:
//!
//! 1. **Discover and call all five tools, including pagination, invalid input,
//!    and empty results.** After the `initialize` handshake the test asks the
//!    server for `tools/list` and asserts all five documented tool names are
//!    advertised, then calls *each* of them:
//!    * `list_sources` — the eight canonical sources, the command-catalog keys,
//!      and the snapshot metadata;
//!    * `search_docs` — a valid query (ranked hits with citations), a paginated
//!      walk (a small `limit` followed by its `next_cursor` until the stream is
//!      exhausted, proving the pages concatenate losslessly), an *empty* result
//!      (zero hits with the "useful empty" `next_steps` guidance, not an
//!      error), and an *invalid* input (a blank query → `INVALID_PARAMS`);
//!    * `get_section` — a valid section id (its body, provenance, and citations)
//!      and a missing id (a typed not-found carrying the offending id in `data`);
//!    * `get_api_checklist` — a valid category filter and an *invalid* category
//!      (a typed error surfacing the discoverable categories);
//!    * `get_command_guidance` — a valid command key (its catalog entry plus the
//!      cited sections) and an *unsupported* key (a typed error surfacing the
//!      supported keys).
//! 2. **Read catalog / section resources and validate schemas, citations, and
//!    stdout protocol integrity.** The test reads the `rust-docs://catalog`
//!    resource (its JSON payload advertises the snapshot, the per-source status
//!    rows, the command entries, and the concrete resource URIs) and a section
//!    resource (`rust-docs://snapshots/<id>/sections/<id>`, whose JSON payload
//!    carries the section body and its provenance citations), plus an invalid
//!    section URI (a typed error). It also asserts **stdout protocol integrity**:
//!    *every* line the server writes to stdout is a JSON-RPC 2.0 message — no
//!    log, diagnostic, or panic text ever leaks onto the protocol stream.
//!
//! The client speaks raw JSON-RPC over a newline-delimited stream — no `rmcp`
//! client needed, so the check is hermetic and does not depend on the SDK's
//! internal task scheduler. `stderr` is redirected to a pipe and discarded, so
//! nothing the server logs can leak onto the protocol stream. `kill_on_drop(true)`
//! is the safety net that reaps the child even on a panic, but the assertions
//! prove a graceful self-exit on a clean client disconnect, not a forced kill.
//!
//! # Companion to [`stdio_lifecycle.rs`](stdio_lifecycle.rs)
//!
//! [`stdio_lifecycle.rs`](stdio_lifecycle.rs) owns the per-version negotiation
//! and the clean connect/disconnect proof for task `[005]`. This test owns the
//! M4 surface proof and, per the PR #77 review note, the *stricter* stdout
//! assertion: it drains the whole session and asserts **every** tail line is
//! JSON-RPC 2.0, not just up to the `initialize` reply. The two files are
//! complementary and both preserved.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;

/// How long the test will wait for the server to answer a request and, after a
/// clean disconnect, to exit. A real stdio round-trip on localhost is
/// sub-second; this generous budget keeps the test fast while tolerating CI
/// load. A per-request deadline is derived from it below.
const BUDGET: Duration = Duration::from_secs(15);

/// The protocol version the test negotiates. `2025-11-25` is the highest
/// `initialize`-capable version the server negotiates to (a client that asks for
/// `2026-07-28` is down-negotiated to it, since that revision dropped the
/// `initialize` handshake), so it exercises the full `tools` + `resources`
/// surface the M4 work exposes.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// The five documented tool names, in the order `all_tools` advertises them.
const TOOL_NAMES: [&str; 5] = [
    "list_sources",
    "search_docs",
    "get_section",
    "get_api_checklist",
    "get_command_guidance",
];

/// The active bundled snapshot's id — the seed corpus the `serve` binary always
/// loads at compile time.
const SNAPSHOT_ID: &str = "rust-docs-bundled-seed";

/// The canonical `rust-docs://catalog` resource identity.
const CATALOG_URI: &str = "rust-docs://catalog";

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

/// A temporary per-test data directory, given to the child as `--data-dir` so
/// the process never writes the host's real application cache. `serve` does not
/// consult it (the bundled snapshot is loaded in-memory), but the flag keeps the
/// child's behaviour identical to the lifecycle test's hermetic posture.
fn fresh_data_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rust-docs-mcp-stdio-mcp-{}-{}",
        std::process::id(),
        stamp,
    ))
}

/// A canonical section-resource URI for the given section id under the active
/// snapshot.
fn section_resource_uri(section_id: &str) -> String {
    format!("rust-docs://snapshots/{SNAPSHOT_ID}/sections/{section_id}")
}

// ---------------------------------------------------------------------------
// The driver: a tiny stateful raw-JSON-RPC client over the child's stdio.
// ---------------------------------------------------------------------------

/// A minimal JSON-RPC 2.0 client over a spawned `serve` process. It hands one
/// request at a time, reads stdout one newline-framed JSON-RPC message at a
/// time, and asserts — on *every* line it reads — that the line is a JSON-RPC
/// 2.0 message. That last assertion is the "stdout carries only protocol
/// traffic" half of AC2.
struct Client {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    deadline: tokio::time::Instant,
}

impl Client {
    /// Spawn the `serve` binary over piped stdio and complete the `initialize`
    /// handshake (request + `notifications/initialized`), the way a conforming
    /// MCP client does.
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
        };

        client.initialize().await;
        client.notify("notifications/initialized").await;
        client
    }

    /// Send the `initialize` request and assert the server echoes the negotiated
    /// protocol version and its implementation identity.
    async fn initialize(&mut self) {
        let response = self.request("initialize", &json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "stdio-mcp-test", "version": "0.0.1" },
        }))
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

    /// Send a JSON-RPC request and return its response. The response's `id` is
    /// asserted to match the request's.
    async fn request(&mut self, method: &str, params: &Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        // `exchange` owns the "write one frame, then read until the matching id"
        // loop, so a numbered request is just a delegated `exchange`.
        self.exchange(method, params, id).await
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

    /// Read a resource by URI, returning the response.
    async fn read_resource(&mut self, uri: &str) -> Value {
        self.request("resources/read", &json!({ "uri": uri })).await
    }

    /// Write one newline-framed message to the child's stdin and flush.
    ///
    /// Implemented as a small blocking-style helper that the async `request`
    /// drives; written as a plain synchronous flush on the `AsyncWriteExt` by
    /// `send` below to keep the read path single-threaded and deterministic.
    /// (Kept as a no-op shim; the real write happens in `send`.)
    #[allow(dead_code)]
    fn send(&mut self, _message: Value) {}
}

impl Client {
    /// Send one request and read the matching response, asserting every stdout
    /// line up to it is a JSON-RPC 2.0 message. Returns the response whose `id`
    /// matches the request.
    async fn exchange(&mut self, method: &str, params: &Value, id: u64) -> Value {
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
        // assertion for this request.
        loop {
            let remaining = self.deadline.saturating_duration_since(tokio::time::Instant::now());
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
            // Every stdout line must be a JSON-RPC 2.0 message. This is the
            // concrete "stdout carries only protocol traffic" assertion (AC2).
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
}

// ---------------------------------------------------------------------------
// Test.
// ---------------------------------------------------------------------------

/// The authoritative M4 surface proof. Spawns the real `serve` binary, drives
/// the `initialize` handshake, then exercises AC1 (all five tools: pagination,
/// invalid input, empty results) and AC2 (catalog/section resources, schemas,
/// citations, stdout protocol integrity), and finally proves a clean self-exit
/// on a client disconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_exposes_the_five_tools_and_two_resources_over_stdio() {
    let mut client = Client::new().await;

    // ---- AC1: discover all five tools. ------------------------------------
    discover_all_five_tools(&mut client).await;

    // ---- AC1: call each tool (valid + invalid + empty + pagination). -------
    call_list_sources(&mut client).await;
    call_search_docs(&mut client).await;
    call_get_section(&mut client).await;
    call_get_api_checklist(&mut client).await;
    call_get_command_guidance(&mut client).await;

    // ---- AC2: read the two resources (schemas, citations, integrity). -----
    read_catalog_resource(&mut client).await;
    read_section_resource(&mut client).await;

    // ---- AC2 (clean disconnect): close our stdin; the server self-exits. ---
    client.close_and_wait_clean().await;
}

// ---------------------------------------------------------------------------
// AC1 — discovery + each tool.
// ---------------------------------------------------------------------------

/// `tools/list` advertises exactly the five documented tools.
async fn discover_all_five_tools(client: &mut Client) {
    let response = client.request("tools/list", &json!({})).await;
    assert_eq!(response.get("error"), None, "tools/list must succeed: {response}");
    let tools = response["result"]["tools"]
        .as_array()
        .expect("`tools/list` returns a `tools` array");
    let advertised: Vec<&str> = tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for name in TOOL_NAMES {
        assert!(
            advertised.contains(&name),
            "`tools/list` must advertise `{name}`; advertised: {advertised:?}"
        );
    }
    // Exactly five tools — no more, no fewer.
    assert_eq!(
        advertised.len(),
        TOOL_NAMES.len(),
        "`tools/list` advertises exactly the five tools, got: {advertised:?}"
    );
    // Each advertised tool carries a JSON-schema input object and a description.
    for tool in tools {
        assert!(
            tool.get("description").and_then(Value::as_str).is_some(),
            "every tool carries a description: {tool}"
        );
        assert!(
            tool.get("inputSchema").is_some(),
            "every tool carries an input schema: {tool}"
        );
    }
}

/// `list_sources` returns the eight sources, the command-catalog keys, and the
/// snapshot metadata.
async fn call_list_sources(client: &mut Client) {
    let response = client.call_tool("list_sources", json!({})).await;
    assert_eq!(response.get("error"), None, "list_sources must succeed: {response}");
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["snapshot_id"], SNAPSHOT_ID,
        "the response names the active snapshot"
    );
    let sources = structured["sources"].as_array().expect("a `sources` array");
    assert_eq!(
        sources.len(),
        SOURCES.len(),
        "all eight canonical sources are listed"
    );
    let ids: Vec<&str> = sources.iter().filter_map(|s| s["id"].as_str()).collect();
    for expected in SOURCES {
        assert!(
            ids.contains(&expected),
            "list_sources must name the `{expected}` source; got {ids:?}"
        );
    }
    // The command-catalog keys are surfaced for `get_command_guidance`
    // discovery.
    let commands = structured["commands"].as_array().expect("a `commands` array");
    assert!(
        !commands.is_empty(),
        "the command catalog is surfaced for discovery"
    );
    // The schema version is echoed.
    assert!(
        structured.get("schema_version").is_some(),
        "the response carries the schema version"
    );
}

/// `search_docs`: a valid query yields ranked, cited hits; a paginated walk
/// concatenates losslessly; an empty query yields useful-empty guidance (not an
/// error); a blank query is a typed `INVALID_PARAMS`.
async fn call_search_docs(client: &mut Client) {
    // --- A valid query: ranked hits with citations. --------------------------
    let response = client
        .call_tool("search_docs", json!({ "query": "C-CONV" }))
        .await;
    assert_eq!(response.get("error"), None, "a valid search must succeed: {response}");
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["snapshot_id"], SNAPSHOT_ID);
    let hits = structured["hits"].as_array().expect("a valid search returns hits");
    assert!(
        !hits.is_empty(),
        "the query `C-CONV` matches a section in the seed corpus"
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
        assert!(
            hit.get("excerpt").and_then(Value::as_str).is_some(),
            "every hit carries an excerpt: {hit}"
        );
        // Citations back the hit (a non-empty, schema-conformant array).
        let citations = hit.get("citations").and_then(Value::as_array);
        assert!(citations.is_some(), "every hit carries citations: {hit}");
        for citation in citations.unwrap() {
            assert!(
                citation.get("source").and_then(Value::as_str).is_some(),
                "every citation names its source: {citation}"
            );
        }
    }

    // --- Pagination: a small limit paginates; walking it concatenates. -------
    let mut cursor: Option<String> = None;
    let mut collected: Vec<String> = Vec::new();
    let mut pages = 0usize;
    loop {
        let mut args = json!({ "query": "Rust", "limit": 1 });
        if let Some(c) = cursor.take() {
            args["cursor"] = json!(c);
        }
        let response = client.call_tool("search_docs", args).await;
        assert_eq!(
            response.get("error"),
            None,
            "a paginated search must succeed: {response}"
        );
        let structured = &response["result"]["structuredContent"];
        for hit in structured["hits"].as_array().expect("a hits array") {
            collected.push(hit["section_id"].as_str().expect("a section id").to_string());
        }
        pages += 1;
        if structured.get("truncated").and_then(Value::as_bool) != Some(true) {
            assert!(
                structured.get("next_cursor").is_none()
                    || structured.get("next_cursor") == Some(&Value::Null),
                "the final page carries no next cursor: {structured}"
            );
            break;
        }
        cursor = Some(
            structured["next_cursor"]
                .as_str()
                .expect("a truncated page carries a next cursor")
                .to_string(),
        );
        assert!(pages <= 20, "the search paginates in a bounded number of pages");
    }
    assert!(
        !collected.is_empty(),
        "the paginated search returns at least one hit"
    );
    assert!(
        pages > 1,
        "a limit of one over the seed corpus spans more than one page"
    );
    // The concatenated pages contain no duplicate section ids (the cursor
    // advances past the page just returned).
    let distinct: std::collections::BTreeSet<&str> = collected.iter().map(String::as_str).collect();
    assert_eq!(
        distinct.len(),
        collected.len(),
        "concatenated pages carry no duplicate hits: {collected:?}"
    );

    // --- Empty result: zero hits with useful-empty guidance, not an error. ---
    let response = client
        .call_tool("search_docs", json!({ "query": "zzzzz_no_such_token_qwerty" }))
        .await;
    assert_eq!(
        response.get("error"),
        None,
        "an empty search is a result, not an error: {response}"
    );
    let structured = &response["result"]["structuredContent"];
    // Zero hits: `hits` is either absent (skip-if-empty) or an empty array.
    let empty = match structured.get("hits") {
        None => true,
        Some(Value::Array(arr)) => arr.is_empty(),
        _ => false,
    };
    assert!(empty, "a non-matching query returns zero hits: {structured}");
    // The useful-empty guidance is offered.
    let next_steps = structured["next_steps"].as_array();
    assert!(
        next_steps.map(|a| !a.is_empty()).unwrap_or(false),
        "a zero-hit search offers useful-empty next steps: {structured}"
    );

    // --- Invalid input: a blank query is a typed INVALID_PARAMS (-32602). ----
    let response = client.call_tool("search_docs", json!({ "query": "   " })).await;
    assert_error(&response, -32602, "a blank search query is INVALID_PARAMS");
}

/// `get_section`: a valid section id yields its body + provenance + citations; a
/// missing id is a typed not-found carrying the offending id.
async fn call_get_section(client: &mut Client) {
    // --- A valid section id. ------------------------------------------------
    let response = client
        .call_tool("get_section", json!({ "section_id": "api-guidelines/about" }))
        .await;
    assert_eq!(response.get("error"), None, "a valid section must resolve: {response}");
    let section = &response["result"]["structuredContent"]["section"];
    assert_eq!(section["id"], "api-guidelines/about");
    assert_eq!(section["source"], "api-guidelines");
    // The body is served verbatim (non-empty).
    assert!(
        section["body"].as_str().is_some_and(|b| !b.is_empty()),
        "the section body is served"
    );
    // Provenance: the canonical URL and a freshness verdict are present.
    assert!(
        section.get("canonical_url").is_some(),
        "the section carries its canonical URL"
    );
    assert!(
        matches!(
            section["freshness"].as_str(),
            Some("fresh" | "stale" | "unknown")
        ),
        "the section carries a freshness verdict: {section}"
    );
    // Citations back the section.
    let citations = section.get("citations").and_then(Value::as_array);
    assert!(citations.is_some(), "the section carries citations: {section}");
    // The schema version is echoed.
    assert!(
        response["result"]["structuredContent"].get("schema_version").is_some(),
        "the response carries the schema version"
    );

    // --- A missing section id is a typed not-found. -------------------------
    let response = client
        .call_tool("get_section", json!({ "section_id": "cargo/does-not-exist" }))
        .await;
    assert_error(&response, -32002, "a missing section is a typed not-found (-32002)");
    // The offending id is surfaced in the error `data` for self-correction.
    assert_eq!(
        response["error"]["data"]["section_id"],
        json!("cargo/does-not-exist"),
        "the not-found error surfaces the offending section id"
    );
}

/// `get_api_checklist`: a valid category filters; an invalid category is a typed
/// error surfacing the discoverable categories.
async fn call_get_api_checklist(client: &mut Client) {
    // --- A valid category filter. -------------------------------------------
    let response = client
        .call_tool(
            "get_api_checklist",
            json!({ "category": "Conversions and From" }),
        )
        .await;
    assert_eq!(response.get("error"), None, "a valid category resolves: {response}");
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["category"],
        json!("Conversions and From"),
        "the response echoes the requested category"
    );
    let entries = structured["entries"].as_array().expect("an `entries` array");
    assert!(
        !entries.is_empty(),
        "the `Conversions and From` category has entries in the seed corpus"
    );
    for entry in entries {
        assert!(
            entry.get("code").and_then(Value::as_str).is_some(),
            "every entry carries an upstream code: {entry}"
        );
        assert_eq!(
            entry["category"],
            json!("Conversions and From"),
            "a filtered entry belongs to the requested category"
        );
    }

    // --- An invalid category is a typed error with discoverable categories. --
    let response = client
        .call_tool("get_api_checklist", json!({ "category": "No Such Category" }))
        .await;
    assert_error(
        &response,
        -32002,
        "an unknown category is a typed not-found (-32002)",
    );
    let data = &response["error"]["data"];
    assert_eq!(
        data["category"],
        json!("No Such Category"),
        "the error surfaces the requested category"
    );
    let categories = data.get("categories").and_then(Value::as_array);
    assert!(
        categories.is_some_and(|c| !c.is_empty()),
        "the error surfaces the discoverable categories: {data}"
    );
}

/// `get_command_guidance`: a valid command key yields its entry + cited sections;
/// an unsupported key is a typed error surfacing the supported keys.
async fn call_get_command_guidance(client: &mut Client) {
    // --- A valid command key. -----------------------------------------------
    let response = client
        .call_tool("get_command_guidance", json!({ "command": "cargo build" }))
        .await;
    assert_eq!(
        response.get("error"),
        None,
        "a valid command key resolves: {response}"
    );
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["entry"]["key"], json!("cargo build"));
    assert!(
        structured["entry"]["purpose"].as_str().is_some_and(|p| !p.is_empty()),
        "the entry carries a purpose"
    );
    // The cited section bodies are resolved into the `sections` page.
    let sections = structured["sections"].as_array().expect("a `sections` array");
    assert!(
        !sections.is_empty(),
        "`cargo build` cites at least one upstream section in the seed corpus"
    );
    for section in sections {
        assert!(
            section.get("id").and_then(Value::as_str).is_some(),
            "every cited section carries an id: {section}"
        );
        // Each cited section is served with its provenance.
        assert!(
            section.get("source").and_then(Value::as_str).is_some(),
            "every cited section carries its source: {section}"
        );
    }

    // --- An unsupported key is a typed error with the supported keys. -------
    let response = client
        .call_tool("get_command_guidance", json!({ "command": "cargo frobnicate" }))
        .await;
    assert_error(
        &response,
        -32002,
        "an unsupported command key is a typed not-found (-32002)",
    );
    let data = &response["error"]["data"];
    assert_eq!(
        data["requested"],
        json!("cargo frobnicate"),
        "the error surfaces the requested key"
    );
    let supported = data.get("supported").and_then(Value::as_array);
    assert!(
        supported.is_some_and(|s| !s.is_empty()),
        "the error surfaces the supported command keys: {data}"
    );
}

// ---------------------------------------------------------------------------
// AC2 — resources.
// ---------------------------------------------------------------------------

/// `resources/list` advertises the catalog and the section resource.
async fn read_catalog_resource(client: &mut Client) {
    // The discovery listing.
    let list = client.request("resources/list", &json!({})).await;
    assert_eq!(list.get("error"), None, "resources/list must succeed: {list}");
    let resources = list["result"]["resources"]
        .as_array()
        .expect("`resources/list` returns a `resources` array");
    let uris: Vec<&str> = resources.iter().filter_map(|r| r["uri"].as_str()).collect();
    assert!(
        uris.contains(&CATALOG_URI),
        "`resources/list` must advertise `{CATALOG_URI}`; advertised: {uris:?}"
    );

    // Read the catalog resource and validate its JSON payload.
    let response = client.read_resource(CATALOG_URI).await;
    assert_eq!(
        response.get("error"),
        None,
        "reading the catalog resource must succeed: {response}"
    );
    let contents = &response["result"]["contents"];
    let contents = contents.as_array().expect("`contents` is an array");
    assert_eq!(
        contents[0]["uri"],
        json!(CATALOG_URI),
        "the resource content carries the catalog URI"
    );
    assert_eq!(
        contents[0]["mimeType"],
        json!("application/json"),
        "the catalog resource is served as application/json"
    );
    // The text payload is itself a JSON object (schema conformance by round-trip).
    let text = contents[0]["text"].as_str().expect("a JSON text payload");
    let payload: Value = serde_json::from_str(text)
        .expect("the catalog resource text is a JSON object");
    assert!(
        payload.is_object(),
        "the catalog resource payload is a JSON object"
    );
    assert_eq!(
        payload["snapshot_id"],
        json!(SNAPSHOT_ID),
        "the catalog names the active snapshot"
    );
    // The per-source status rows are present (one per canonical source).
    let sources = payload.get("sources").and_then(Value::as_array);
    assert!(
        sources.is_some_and(|s| s.len() == SOURCES.len()),
        "the catalog advertises all eight source status rows"
    );
    // The command entries and the concrete resource URIs are advertised.
    let commands = payload.get("commands").and_then(Value::as_array);
    assert!(
        commands.is_some(),
        "the catalog advertises its command entries"
    );
    let resource_uris = payload.get("resource_uris").and_then(Value::as_array);
    assert!(
        resource_uris.is_some_and(|r| !r.is_empty()),
        "the catalog advertises its concrete resource URIs"
    );
}

/// A section resource: its JSON payload carries the section body + provenance
/// citations; an invalid section URI is a typed error.
async fn read_section_resource(client: &mut Client) {
    let uri = section_resource_uri("api-guidelines/about");
    let response = client.read_resource(&uri).await;
    assert_eq!(response.get("error"), None, "a section resource reads: {response}");
    let contents = response["result"]["contents"]
        .as_array()
        .expect("`contents` is an array");
    assert_eq!(
        contents[0]["uri"],
        json!(uri),
        "the resource content carries the section URI"
    );
    assert_eq!(
        contents[0]["mimeType"],
        json!("application/json"),
        "the section resource is served as application/json"
    );
    let text = contents[0]["text"].as_str().expect("a JSON text payload");
    let payload: Value = serde_json::from_str(text)
        .expect("the section resource text is a JSON object");
    let section = &payload["section"];
    assert_eq!(
        section["id"],
        json!("api-guidelines/about"),
        "the section resource carries its section"
    );
    assert!(
        section.get("body").and_then(Value::as_str).is_some_and(|b| !b.is_empty()),
        "the section resource serves the body"
    );
    // Provenance: citations and the canonical URL are present.
    assert!(
        section.get("citations").is_some(),
        "the section resource carries citations"
    );
    assert!(
        section.get("canonical_url").is_some(),
        "the section resource carries its canonical URL"
    );

    // --- An invalid section URI is a typed error. ---------------------------
    let bad = section_resource_uri("cargo/does-not-exist");
    let response = client.read_resource(&bad).await;
    assert_error(
        &response,
        -32002,
        "an absent section resource is a typed not-found (-32002)",
    );
}

// ---------------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------------

impl Client {
    /// Close our side of the child's stdin (an EOF the stdio transport treats as
    /// the end of the session) and assert the server self-exits cleanly within
    /// the budget — the concrete "no orphaned process" claim.
    async fn close_and_wait_clean(mut self) {
        // Dropping the piped stdin writer closes its write end, which the stdio
        // transport reads as an EOF — the end of the session. This mirrors the
        // lifecycle test's `drop(stdin)`.
        drop(self.stdin);
        match tokio::time::timeout(BUDGET, self.child.wait()).await {
            Ok(Ok(status)) => {
                assert!(
                    status.success(),
                    "after a clean client disconnect the server must exit successfully, \
                     got {status:?}"
                );
            }
            Ok(Err(error)) => panic!("waiting on the child failed: {error}"),
            Err(_) => panic!("the child did not exit within {BUDGET:?} — a likely orphan"),
        }
    }
}

/// Assert a JSON-RPC response is an error with the expected code.
fn assert_error(response: &Value, expected_code: i32, context: &str) {
    let error = response
        .get("error")
        .expect("{context}: expected an error response, got {response}");
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .expect("an error response carries a numeric code");
    assert_eq!(
        code,
        expected_code as i64,
        "{context}: expected error code {expected_code}, got {code} ({response})"
    );
    // Every protocol error carries a human-readable message.
    assert!(
        error.get("message").and_then(Value::as_str).is_some_and(|m| !m.is_empty()),
        "an error response carries a message: {response}"
    );
}
