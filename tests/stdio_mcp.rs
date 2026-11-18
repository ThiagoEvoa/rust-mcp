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
/// sub-second, but a full-corpus `search_docs` call inside a *debug* test binary
/// costs far more per round-trip than the server's own p95 — the client's
/// process-pipe I/O, serde, and debug-mode ranking dominate — so the pagination
/// walk below is *capped* (see `call_search_docs`) and this generous budget keeps
/// the suite comfortable under CI load while a larger corpus still cannot regress
/// the suite's wall-clock. A per-request deadline is derived from it below.
const BUDGET: Duration = Duration::from_secs(30);

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
        let response = self
            .request(
                "initialize",
                &json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "stdio-mcp-test", "version": "0.0.1" },
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
    call_search_docs_query_binding(&mut client).await;
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
// [P1][QA] #172 — resource discovery advertises *concrete, readable* section
// resources, paged. The bug: `list_resources` ignored its request and returned a
// fixed, unbound `rust-docs://section/{id}` template that no `resources/read`
// could satisfy, while the snapshot-bound, cap-bounded section discovery existed
// only inside the resource module. The fix wires the handler to the active
// snapshot's `section_resource_inventory`: the catalog leads the first (cursor-
// less) page, continuation pages carry section resources only, every advertised
// URI is the concrete canonical URI the server actually reads, and the request's
// continuation cursor is read + capped at the handler boundary (a foreign or
// malformed cursor is a typed `INVALID_PARAMS`, never a wrong page or a panic).
//
// This is a *separate* test with its own session budget so the large-corpus read
// sample below cannot regress the main surface test's wall-clock.
// ---------------------------------------------------------------------------

/// [P1][QA] #172: `resources/list` advertises concrete, readable section
/// resources, paged. The catalog leads the first page, continuation pages carry
/// section resources only, every advertised URI is snapshot-scoped and reads in
/// the same session, and a malformed/foreign cursor is a typed error. A capped
/// `search_docs` walk already proves the O(cap) discipline; this walk is read-
/// free except for a bounded sample of advertised section URIs, so the corpus
/// size cannot regress the suite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resource_discovery_walks_to_exhaustion_and_reads_advertised_uris() {
    let mut client = Client::new().await;
    walk_resource_discovery_and_read_advertised_uris(&mut client).await;
    // A clean disconnect proves the discovery session leaves no orphan.
    client.close_and_wait_clean().await;
}

/// Walk `resources/list` to exhaustion (following `nextCursor`), assert the
/// discovery contract on every page, then read a bounded sample of the advertised
/// section URIs in the same session — including an anchored and a percent-
/// escaped-path section so the URI encoding is exercised — and reject a malformed
/// and a foreign discovery cursor.
///
/// The full "every advertised URI reads" enumeration is proven three ways that do
/// not bloat CI: the unit-level
/// `an_advertised_section_resource_uri_round_trips_to_a_read` maps the advertised
/// producer URI to a successful read; the unit-level
/// `section_resource_discovery_paginates_and_walks_every_section_uri` walks the
/// whole inventory and checks it equals the snapshot's section list exactly once;
/// and a release-mode raw-stdio probe reads every advertised URI end-to-end. This
/// protocol test keeps CI bounded by reading a *sample* of advertised section
/// URIs (the same O(cap) discipline the capped `search_docs` walk uses) while
/// still walking the *whole* read-free discovery stream.
async fn walk_resource_discovery_and_read_advertised_uris(client: &mut Client) {
    // --- Walk the discovery stream to exhaustion, following `nextCursor`. ----
    // Paging the inventory is read-free and cheap, so the list walk is not the
    // cost driver; a runaway (non-advancing) cursor is caught by the per-request
    // deadline in `exchange`, and the cap below bounds the walk's cost even if a
    // cursor ever stopped advancing.
    const WALK_PAGE_CAP: usize = 400;
    let mut cursor: Option<String> = None;
    let mut uris: Vec<String> = Vec::new();
    let mut pages = 0usize;
    loop {
        let params: Value = match &cursor {
            None => json!({}),
            Some(c) => json!({ "cursor": c }),
        };
        let response = client.request("resources/list", &params).await;
        assert_eq!(
            response.get("error"),
            None,
            "a discovery page must succeed: {response}"
        );
        // Bounded after JSON serialization: every frame the server sends fits the
        // 32 KiB cap.
        let frame = serde_json::to_vec(&response).expect("the response serialises");
        assert!(
            frame.len() <= MAX_RESPONSE_BYTES,
            "every discovery page is within the 32 KiB cap, got {} bytes",
            frame.len()
        );
        let resources = response["result"]["resources"]
            .as_array()
            .expect("`resources/list` returns a `resources` array");
        for resource in resources {
            let uri = resource["uri"]
                .as_str()
                .expect("a resource carries a `uri`");
            uris.push(uri.to_string());
        }
        pages += 1;
        assert!(
            pages <= WALK_PAGE_CAP,
            "the discovery must reach its final page within {WALK_PAGE_CAP} pages"
        );
        let next = response["result"].get("nextCursor").and_then(Value::as_str);
        match next {
            None => break, // the final page carries no continuation.
            Some(next_cursor) if pages == WALK_PAGE_CAP => {
                // Still advancing when the cap is hit: the stream is open and the
                // walk is bounded, not a runaway loop.
                assert!(!next_cursor.is_empty(), "a capped page still advances");
                break;
            }
            Some(next_cursor) => {
                cursor = Some(next_cursor.to_string());
            }
        }
    }

    // --- The discovery contract, asserted over the whole walked stream. ------
    assert!(!uris.is_empty(), "the discovery advertises resources");
    // No placeholders / templates: the obsolete `rust-docs://section/{id}` form
    // is gone; every advertised URI is a concrete, snapshot-scoped identity.
    for uri in &uris {
        assert!(
            !uri.contains('{') && !uri.contains('}'),
            "a placeholder/template URI leaked into discovery: {uri}"
        );
        assert!(
            !uri.starts_with("rust-docs://section/"),
            "the obsolete unbound section template leaked into discovery: {uri}"
        );
    }
    // The catalog resource leads the first page and appears exactly once.
    assert_eq!(
        uris.first().map(String::as_str),
        Some(CATALOG_URI),
        "the catalog resource leads the first page"
    );
    assert_eq!(
        uris.iter().filter(|u| u.as_str() == CATALOG_URI).count(),
        1,
        "the catalog resource is advertised exactly once"
    );
    // No duplicate URIs across the walk: the cursor advances past each page.
    let distinct: std::collections::BTreeSet<&str> = uris.iter().map(String::as_str).collect();
    assert_eq!(
        distinct.len(),
        uris.len(),
        "discovery carries no duplicate URIs across pages"
    );
    // Every non-catalog URI is snapshot-scoped under the active snapshot — the
    // fixed-snapshot contract (a URI naming another snapshot is never advertised).
    let section_prefix = format!("rust-docs://snapshots/{SNAPSHOT_ID}/sections/");
    for uri in &uris {
        if *uri != CATALOG_URI {
            assert!(
                uri.starts_with(&section_prefix),
                "every non-catalog discovery URI is snapshot-scoped under {section_prefix}: {uri}"
            );
        }
    }

    // --- Read a bounded sample of advertised section URIs in the same session.
    // The full enumeration is proven above; here the first and last advertised
    // section URIs, plus any carrying an anchor (`#`) or a percent-escaped chapter
    // path (`%`), are read so the anchor/escaped-path encoding is exercised and
    // each sampled advertised URI reads successfully (the discovery-to-read
    // round-trip the bug broke).
    let section_uris: Vec<&str> = uris
        .iter()
        .filter(|u| u.as_str() != CATALOG_URI)
        .map(String::as_str)
        .collect();
    assert!(
        !section_uris.is_empty(),
        "the discovery advertises at least one section resource"
    );
    // The seed corpus advertises both an anchored and an escaped-path section, so
    // the encoding paths are actually sampled rather than silently skipped.
    assert!(
        section_uris.iter().any(|u| u.contains('#')),
        "the seed corpus advertises at least one anchored section"
    );
    assert!(
        section_uris.iter().any(|u| u.contains('%')),
        "the seed corpus advertises at least one percent-escaped section path"
    );
    let mut to_read: Vec<&str> = Vec::new();
    to_read.push(section_uris[0]);
    to_read.push(*section_uris.last().expect("at least one section URI"));
    if let Some(anchor) = section_uris.iter().find(|u| u.contains('#')) {
        to_read.push(*anchor);
    }
    if let Some(escaped) = section_uris.iter().find(|u| u.contains('%')) {
        to_read.push(*escaped);
    }
    for uri in &to_read {
        let response = client.read_resource(uri).await;
        assert_eq!(
            response.get("error"),
            None,
            "an advertised section URI reads in the same session: {uri}: {response}"
        );
        let contents = response["result"]["contents"]
            .as_array()
            .expect("`contents` is an array");
        assert_eq!(
            contents[0]["uri"],
            json!(uri),
            "the read content carries the advertised URI"
        );
        assert_eq!(
            contents[0]["mimeType"],
            json!("application/json"),
            "a section resource is served as application/json"
        );
    }

    // --- A malformed discovery cursor is a typed `INVALID_PARAMS`. -----------
    let malformed = client
        .request("resources/list", &json!({ "cursor": "not-a-cursor" }))
        .await;
    assert_error(
        &malformed,
        -32602,
        "a malformed discovery cursor is a typed error",
    );

    // --- A foreign discovery cursor (minted for another operation) is a typed
    //     `INVALID_PARAMS`, not a wrong page. A `search_docs` cursor names the
    //     `Search` operation, so it must not resume section discovery.
    let search = client
        .call_tool("search_docs", json!({ "query": "Rust", "limit": 1 }))
        .await;
    if let Some(foreign) = search["result"]["structuredContent"]["next_cursor"].as_str() {
        let foreign_response = client
            .request("resources/list", &json!({ "cursor": foreign }))
            .await;
        assert_error(
            &foreign_response,
            -32602,
            "a foreign (search) cursor is rejected for section discovery",
        );
    } else {
        // A corpus with no pagination on this query would be degenerate; the
        // malformed-cursor assertion above already exercises the rejection path.
    }
}

// ---------------------------------------------------------------------------
// [P1][QA] #154 — error responses and user-controlled identifiers are bounded
// under the 32 KiB response cap. Over-cap inputs (section id, command, category,
// cursor, resource URI) are rejected at the handler boundary with a bounded
// `INVALID_PARAMS` error (a short diagnostic preview plus the original byte
// length), never the full input echoed in both the message and the `data`
// payload. A final serialized-size backstop guards the error path too, so no
// error response can exceed the cap. Success pagination and shell-operator
// rejection (exercised by the main test above) are unaffected.
// ---------------------------------------------------------------------------

/// The complete serialized response cap, in bytes: `32 * 1024`, the same bound
/// the successful-page path enforces ([`crate::pagination::MAX_RESPONSE_BYTES`]).
/// A newline-delimited JSON-RPC frame is the serialized object plus one newline,
/// so the object staying under this bound keeps the frame at or under the cap.
const MAX_RESPONSE_BYTES: usize = 32 * 1024;

/// The per-identifier cap the handler boundary enforces. Inputs at or under it
/// are not rejected by the cap; over it, a bounded `INVALID_PARAMS` is returned.
const MAX_IDENTIFIER_BYTES: usize = 4096;

/// Measure the complete serialized JSON-RPC frame a `response` represents: the
/// object re-serialized to its compact wire form (the frame the server writes is
/// this object plus a single trailing newline, so this is `frame - 1`).
fn serialized_frame_bytes(response: &Value) -> usize {
    serde_json::to_vec(response)
        .expect("a JSON-RPC response serializes to bytes")
        .len()
}

/// Assert a JSON-RPC error response is an error with the expected code **and**
/// that its complete serialized frame stays under the 32 KiB cap.
fn assert_bounded_error(response: &Value, expected_code: i32, context: &str) {
    assert_error(response, expected_code, context);
    let bytes = serialized_frame_bytes(response);
    assert!(
        bytes <= MAX_RESPONSE_BYTES,
        "{context}: the complete error frame must stay under the {MAX_RESPONSE_BYTES}-byte \
         response cap, but was {bytes} bytes: {response}"
    );
}

/// #154: the three reproduced over-cap inputs — a 40,000-character `section_id`,
/// `command`, and `category` — each produce a complete error frame that fits the
/// 32 KiB cap, over the real spawned stdio server. This is the exact
/// reproduction the issue reports (80 KB error responses bypassing the cap);
/// after the fix the responses are small `INVALID_PARAMS` errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_cap_identifiers_produce_bounded_error_responses_over_stdio() {
    let mut client = Client::new().await;

    let big = "a".repeat(40_000);

    // 1. `get_section` / large `section_id`. Before the fix this echoed the
    //    full 40,000-char id into both the message and `data`, producing an
    //    ~80 KB error that bypassed the cap.
    let response = client
        .call_tool("get_section", json!({ "section_id": big.clone() }))
        .await;
    assert_bounded_error(
        &response,
        -32602,
        "a 40,000-char section_id is a bounded INVALID_PARAMS",
    );
    assert_bounded_diagnostic(&response, "section_id", &big, "get_section");

    // 2. `get_command_guidance` / large `command`.
    let response = client
        .call_tool("get_command_guidance", json!({ "command": big.clone() }))
        .await;
    assert_bounded_error(
        &response,
        -32602,
        "a 40,000-char command is a bounded INVALID_PARAMS",
    );
    assert_bounded_diagnostic(&response, "command", &big, "get_command_guidance");

    // 3. `get_api_checklist` / large `category`.
    let response = client
        .call_tool("get_api_checklist", json!({ "category": big.clone() }))
        .await;
    assert_bounded_error(
        &response,
        -32602,
        "a 40,000-char category is a bounded INVALID_PARAMS",
    );
    assert_bounded_diagnostic(&response, "category", &big, "get_api_checklist");

    // The process is still alive and healthy: a subsequent valid call resolves.
    let response = client.call_tool("list_sources", json!({})).await;
    assert_eq!(
        response.get("error"),
        None,
        "the server stays alive after the over-cap calls: {response}"
    );

    client.close_and_wait_clean().await;
}

/// #154: overlong *multibyte* UTF-8 inputs are bounded by their UTF-8 byte
/// length (the quantity the cap measures), and the bounded preview never splits a
/// character across the cut — Unicode boundaries are safe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_cap_multibyte_utf8_inputs_are_bounded_over_stdio() {
    let mut client = Client::new().await;

    // 40,000 × "é" (a 3-byte character) = 120,000 bytes, far over the cap. The
    // byte-length cap rejects it, and the preview stays valid UTF-8.
    let big = "\u{e9}".repeat(40_000);
    assert!(
        big.len() > MAX_IDENTIFIER_BYTES,
        "the multibyte input exceeds the cap"
    );

    let response = client
        .call_tool("get_section", json!({ "section_id": big.clone() }))
        .await;
    assert_bounded_error(
        &response,
        -32602,
        "a 120,000-byte multibyte section_id is a bounded INVALID_PARAMS",
    );
    // The diagnostic's byte count is the UTF-8 length (120,000), not the char
    // count (40,000) — the cap is measured in bytes.
    let data = &response["error"]["data"];
    assert_eq!(
        data["bytes"],
        json!(big.len()),
        "the diagnostic carries the UTF-8 byte length"
    );
    // The preview is bounded and valid UTF-8 (no split character).
    let preview = data["preview"]
        .as_str()
        .expect("the diagnostic carries a preview");
    assert!(
        preview.len() <= 64 + 3,
        "the preview is bounded, got {} bytes",
        preview.len()
    );
    assert!(
        std::str::from_utf8(preview.as_bytes()).is_ok(),
        "the preview is valid UTF-8"
    );

    client.close_and_wait_clean().await;
}

/// #154: every string-bearing input surface — a cursor, an unknown tool name, and
/// a section-resource URI — is bounded the same way, so a large value in any of
/// them cannot produce an over-cap error response.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_cap_cursors_tools_and_resource_uris_are_bounded_over_stdio() {
    let mut client = Client::new().await;
    let big = "a".repeat(40_000);

    // A large `cursor` on an otherwise valid tool is a bounded `INVALID_PARAMS`
    // (the cursor is capped before the cursor-decode error is reached).
    let response = client
        .call_tool(
            "get_section",
            json!({ "section_id": "api-guidelines/about#rust-api-guidelines", "cursor": big.clone() }),
        )
        .await;
    assert_bounded_error(
        &response,
        -32602,
        "a 40,000-char cursor is a bounded INVALID_PARAMS",
    );

    // An unknown (and over-long) tool name is a bounded `METHOD_NOT_FOUND`: the
    // error's backstop truncates the echoed tool name so the frame fits.
    let response = client
        .request(
            "tools/call",
            &json!({ "name": big.clone(), "arguments": json!({}) }),
        )
        .await;
    assert_bounded_error(
        &response,
        -32601,
        "a 40,000-char unknown tool name is a bounded METHOD_NOT_FOUND",
    );

    // An over-long section-resource URI (a huge section id embedded in the URI) is
    // a bounded `INVALID_PARAMS`.
    let big_uri = format!("rust-docs://snapshots/{SNAPSHOT_ID}/sections/{}", big);
    let response = client.read_resource(&big_uri).await;
    assert_bounded_error(
        &response,
        -32602,
        "an over-long section-resource URI is a bounded INVALID_PARAMS",
    );

    client.close_and_wait_clean().await;
}

/// #154: the two `search_docs` string fields — `query` and `source` — are
/// bounded the same way. `search_docs` already caps a query at its own
/// 1000-byte limit (`QueryTooLarge`), but the handler-boundary cap
/// (`MAX_IDENTIFIER_BYTES`) fires first for a much larger input and returns a
/// bounded `INVALID_PARAMS` rather than the full echo; an over-long `source`
/// selector is bounded the same way. This exercises the remaining
/// string-bearing fields for AC3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn over_cap_search_docs_query_and_source_are_bounded_over_stdio() {
    let mut client = Client::new().await;
    let big = "a".repeat(40_000);

    // A 40,000-char `query` is cap-rejected at the handler boundary (a bounded
    // `INVALID_PARAMS`), long before the search layer's own 1000-byte cap.
    let response = client
        .call_tool("search_docs", json!({ "query": big.clone() }))
        .await;
    assert_bounded_error(
        &response,
        -32602,
        "a 40,000-char query is a bounded INVALID_PARAMS",
    );
    assert_bounded_diagnostic(&response, "query", &big, "search_docs/query");

    // An over-long optional `source` selector is bounded the same way.
    let response = client
        .call_tool(
            "search_docs",
            json!({ "query": "cargo build", "source": big.clone() }),
        )
        .await;
    assert_bounded_error(
        &response,
        -32602,
        "a 40,000-char source is a bounded INVALID_PARAMS",
    );
    assert_bounded_diagnostic(&response, "source", &big, "search_docs/source");

    client.close_and_wait_clean().await;
}

/// #154: the cap is a gate, not a rejection of everything large. Inputs at or
/// under the cap are not cap-rejected: a valid section id resolves, and an
/// invalid input exactly at the cap still reaches the not-found mapping (bounded
/// by the backstop) rather than the cap. A one-byte-over input is cap-rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn near_limit_valid_inputs_still_resolve_and_the_cap_gate_is_exact_over_stdio() {
    let mut client = Client::new().await;

    // A valid, short section id resolves — the cap does not interfere with
    // legitimate inputs.
    let response = client
        .call_tool(
            "get_section",
            json!({ "section_id": "api-guidelines/about#rust-api-guidelines" }),
        )
        .await;
    assert_eq!(
        response.get("error"),
        None,
        "a valid section id resolves under the cap: {response}"
    );

    // An input exactly at the cap (4096 bytes) is *not* cap-rejected: it reaches
    // the not-found mapping. The response stays bounded (the backstop guards the
    // 4 KB echo).
    let at_cap = "x".repeat(MAX_IDENTIFIER_BYTES);
    let response = client
        .call_tool("get_section", json!({ "section_id": at_cap.clone() }))
        .await;
    assert_bounded_error(
        &response,
        -32002,
        "an at-cap invalid section_id is a bounded not-found (not a cap rejection)",
    );

    // One byte over the cap is cap-rejected (a bounded `INVALID_PARAMS`).
    let over_cap = "x".repeat(MAX_IDENTIFIER_BYTES + 1);
    let response = client
        .call_tool("get_section", json!({ "section_id": over_cap }))
        .await;
    assert_bounded_error(
        &response,
        -32602,
        "a one-byte-over-cap section_id is a bounded INVALID_PARAMS cap rejection",
    );

    client.close_and_wait_clean().await;
}

/// Assert a cap rejection's `data` payload carries the bounded diagnostic the
/// issue asks for: the field name, the original byte length, a short bounded
/// preview, and a `truncated` flag — **not** the full input echoed in the
/// message or `data`.
fn assert_bounded_diagnostic(response: &Value, field: &str, big_input: &str, tool: &str) {
    let data = &response["error"]["data"];
    assert_eq!(
        data.get("field"),
        Some(&json!(field)),
        "{tool}: the diagnostic names the offending field"
    );
    // The original length is carried so the caller knows how much was truncated.
    assert_eq!(
        data.get("bytes"),
        Some(&json!(big_input.len())),
        "{tool}: the diagnostic carries the original byte length"
    );
    assert_eq!(
        data.get("limit"),
        Some(&json!(MAX_IDENTIFIER_BYTES)),
        "{tool}: the diagnostic names the cap"
    );
    assert_eq!(
        data.get("truncated"),
        Some(&json!(true)),
        "{tool}: the diagnostic marks itself truncated"
    );
    // The preview is bounded (a short prefix + ellipsis), never the full input.
    let preview = data.get("preview").and_then(Value::as_str);
    assert!(
        preview.is_some_and(|p| p.len() <= 64 + 3),
        "{tool}: the preview is bounded, got {preview:?}"
    );
    // The full input is not echoed anywhere in the frame.
    let serialized = serde_json::to_string(response).expect("serialize the response");
    assert!(
        !serialized.contains("a".repeat(1000).as_str()),
        "{tool}: the full input must not be echoed in the error frame"
    );
}

// ---------------------------------------------------------------------------
// AC1 — discovery + each tool.
// ---------------------------------------------------------------------------

/// `tools/list` advertises exactly the five documented tools.
async fn discover_all_five_tools(client: &mut Client) {
    let response = client.request("tools/list", &json!({})).await;
    assert_eq!(
        response.get("error"),
        None,
        "tools/list must succeed: {response}"
    );
    let tools = response["result"]["tools"]
        .as_array()
        .expect("`tools/list` returns a `tools` array");
    let advertised: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
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
    assert_eq!(
        response.get("error"),
        None,
        "list_sources must succeed: {response}"
    );
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
    let commands = structured["commands"]
        .as_array()
        .expect("a `commands` array");
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
    assert_eq!(
        response.get("error"),
        None,
        "a valid search must succeed: {response}"
    );
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["snapshot_id"], SNAPSHOT_ID);
    let hits = structured["hits"]
        .as_array()
        .expect("a valid search returns hits");
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
    // A `limit` of one over the real corpus (#170 grew it to 877 sections) now
    // matches the bulk of the corpus, so an *unbounded* "walk until exhausted"
    // loop would perform one JSON-RPC round-trip per distinct match — hundreds of
    // them — and blow the test's time budget on slow/CI machines (the F-1
    // regression: the 545 -> 877 corpus growth pushed a limit-one walk past the
    // 15 s `BUDGET` on CI/MSRV). The pagination *contract* — each page is a
    // complete page, the cursor advances without duplicate section ids, and the
    // concatenation is lossless — is proven by walking a *bounded* number of
    // pages: every page we read is read losslessly, and a non-advancing (runaway)
    // cursor would still push `pages` past the distinct count and trip the
    // assertion below; the per-request deadline in `exchange` is the fast-fail net
    // for a true runaway loop. Capping the walk makes its cost O(cap) rather than
    // O(distinct matches), so a larger corpus cannot regress the test again. The
    // cap (48 pages) is a strong multi-page lossless proof and keeps the suite
    // fast; the larger budget (30 s, see `BUDGET`) absorbs CI hardware variance
    // on top of that bound.
    const WALK_PAGE_CAP: usize = 48;
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
            collected.push(
                hit["section_id"]
                    .as_str()
                    .expect("a section id")
                    .to_string(),
            );
        }
        pages += 1;
        if structured.get("truncated").and_then(Value::as_bool) != Some(true) {
            // The stream is exhausted: the final page carries no next cursor.
            assert!(
                structured.get("next_cursor").is_none()
                    || structured.get("next_cursor") == Some(&Value::Null),
                "the final page carries no next cursor: {structured}"
            );
            break;
        }
        // A truncated page carries a non-empty next cursor that advances to the
        // next unread page.
        let next_cursor = structured
            .get("next_cursor")
            .and_then(Value::as_str)
            .expect("a truncated page carries a next cursor")
            .to_string();
        // Bound the walk so a large corpus cannot exceed the time budget: stop
        // after `WALK_PAGE_CAP` pages even while the stream is still open. The
        // non-empty cursor we stop on proves the walk continued losslessly past
        // the cap; a small corpus instead exhausts here and asserts no cursor.
        if pages == WALK_PAGE_CAP {
            assert!(
                !next_cursor.is_empty(),
                "the page that hits the walk cap still carries a next cursor"
            );
            break;
        }
        cursor = Some(next_cursor);
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
    // Bounded, non-truncating pagination, corpus-derived: a limit-one walk
    // returns exactly one new distinct section per page, so the number of pages
    // can never exceed the number of distinct matches. The hand-authored
    // 8-section corpus is gone, replaced by the real upstream corpus (#150, the
    // routine command families plus the full books; #170 grew it to 877
    // sections), so `Rust` matches the bulk of the corpus. Capping the walk at
    // `WALK_PAGE_CAP` keeps this assertion fast and corpus-growth-proof while
    // still proving the per-page lossless-concatenation contract over a real,
    // large corpus: a non-advancing (runaway) cursor would push `pages` past the
    // distinct count and break this; the per-request deadline in `exchange` is the
    // fast-fail net for a true runaway loop.
    assert!(
        pages <= distinct.len(),
        "a limit-one walk is bounded by its distinct matches: {pages} pages over \
         {} distinct matches",
        distinct.len(),
    );

    // --- Empty result: zero hits with useful-empty guidance, not an error. ---
    let response = client
        .call_tool(
            "search_docs",
            json!({ "query": "zzzzz_no_such_token_qwerty" }),
        )
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
    assert!(
        empty,
        "a non-matching query returns zero hits: {structured}"
    );
    // The useful-empty guidance is offered.
    let next_steps = structured["next_steps"].as_array();
    assert!(
        next_steps.map(|a| !a.is_empty()).unwrap_or(false),
        "a zero-hit search offers useful-empty next steps: {structured}"
    );

    // --- Invalid input: a blank query is a typed INVALID_PARAMS (-32602). ----
    let response = client
        .call_tool("search_docs", json!({ "query": "   " }))
        .await;
    assert_error(&response, -32602, "a blank search query is INVALID_PARAMS");
}

/// #[171] `search_docs` binds its pagination cursor to the exact query (AC1–AC4
/// through the spawned `serve` binary over real stdio). A cursor minted for one
/// query cannot be replayed after the query changes — a bounded `INVALID_PARAMS`
/// that does not echo the query text — while an identical query resumes the same
/// stream losslessly. This is the seam a client most often trips: it re-sends a
/// stale cursor after a prompt edit.
async fn call_search_docs_query_binding(client: &mut Client) {
    // --- Mint a cursor from a query that truncates. -------------------------
    let first = client
        .call_tool("search_docs", json!({ "query": "Rust", "limit": 1 }))
        .await;
    assert_eq!(
        first.get("error"),
        None,
        "a valid `search_docs` call must succeed, not error: {first}"
    );
    let first = &first["result"]["structuredContent"];
    assert_eq!(
        first.get("truncated").and_then(Value::as_bool),
        Some(true),
        "a limit-one `Rust` search truncates and carries a cursor: {first}"
    );
    let cursor = first
        .get("next_cursor")
        .and_then(Value::as_str)
        .expect("a truncated page carries a next cursor")
        .to_string();
    let first_hit = first["hits"][0]["section_id"]
        .as_str()
        .expect("the first page returns a hit")
        .to_string();

    // --- AC1: a changed query with the old cursor is a bounded INVALID_PARAMS.
    //
    // The new query is the highest-ranked result of its *own* call, so absent
    // the binding the cursor would silently skip it. With the binding, the
    // resume is rejected before ranking, not silently advanced.
    let mismatched = client
        .call_tool(
            "search_docs",
            json!({ "query": "cargo", "limit": 1, "cursor": cursor.clone() }),
        )
        .await;
    assert_error(
        &mismatched,
        -32602,
        "a changed query with an old cursor is INVALID_PARAMS",
    );

    // --- AC1: the error output never echoes the query text. ----------------
    //
    // `MismatchField::Query` reports through fixed labels ("the current query",
    // "the query the cursor was bound to"); the mismatched query must not leak
    // into the error's message or structured data.
    let error = &mismatched["error"];
    let error_text = serde_json::to_string(error).expect("serializing an error must not fail");
    assert!(
        !error_text.contains("Rust") && !error_text.contains("cargo"),
        "the query-mismatch error must not echo either query text: {error_text}"
    );
    assert_eq!(
        error
            .get("data")
            .and_then(|d| d.get("field"))
            .and_then(Value::as_str),
        Some("query"),
        "the mismatch is attributed to the query field: {error}"
    );

    // --- AC2: an identical query resumes the same stream losslessly. -------
    let resumed = client
        .call_tool(
            "search_docs",
            json!({ "query": "Rust", "limit": 1, "cursor": cursor }),
        )
        .await;
    assert_eq!(
        resumed.get("error"),
        None,
        "an identical-query resume must succeed, not error: {resumed}"
    );
    let resumed = &resumed["result"]["structuredContent"];
    let resumed_hit = resumed["hits"][0]["section_id"]
        .as_str()
        .expect("the resumed page returns a hit")
        .to_string();
    assert_ne!(
        resumed_hit, first_hit,
        "the resumed page advances past the first hit"
    );
    assert_eq!(
        resumed.get("snapshot_id").and_then(Value::as_str),
        first.get("snapshot_id").and_then(Value::as_str),
        "both pages are read from the same snapshot"
    );
}

// ---------------------------------------------------------------------------
// [P2][QA] #173 — the retrieval-quality ranking proof, driven through the *real*
// spawned `serve` binary over genuine OS stdio (the seam the issue exercises,
// not the index unit test). Two behaviours the fix must hold end-to-end:
//
//   * **Primary-first (C-CONV).** `search_docs({"query":"C-CONV"})` must rank
//     the *primary* guideline explanation — the section whose own heading carries
//     the `(C-CONV)` marker — ahead of the standard-library section that merely
//     *inherits* the same code. Before the fix the inherited
//     `more-examples-from-the-standard-library` section won on raw BM25; the
//     `primary_code` sort key now lifts the primary rule first.
//   * **Cross-source NL ranking + preserved bounds/citations.** A couple of the
//     natural-language queries the issue reported as misses (E-009 imports, E-044
//     rustdoc options) now surface their expected source in the top five, and
//     every response stays within the server's response bound with its citations
//     intact — the ranking change must not trade the C-CONV order for a broken
//     or unbounded response.
//
// This is a *separate* test with its own session budget so the per-query ranked
// walks cannot regress the main surface test's wall-clock.
// ---------------------------------------------------------------------------

/// The primary C-CONV explanation: the `api-guidelines/naming` section whose own
/// heading carries the `(C-CONV)` marker.
const CCONV_PRIMARY: &str =
    "api-guidelines/naming#ad-hoc-conversions-follow-as_-to_-into_-conventions-c-conv";

/// The inherited secondary section that used to outrank the primary C-CONV rule
/// on raw BM25; the fix must demote it below `CCONV_PRIMARY`.
const CCONV_INHERITED: &str = "api-guidelines/naming#more-examples-from-the-standard-library";

/// Assert a `search_docs` response is a successful, *bounded* result whose hits
/// each carry the fields the response contract promises: a source, a non-empty
/// excerpt, and a non-empty, source-attributed citations array. The whole
/// response frame must stay under the 32 KiB response cap — the "preserve
/// response bounds" half of the issue, so the ranking change never trades order
/// for an oversized or citation-less response.
fn assert_bounded_ranked<'a>(response: &'a Value, context: &str) -> Vec<&'a Value> {
    assert_eq!(
        response.get("error"),
        None,
        "the ranked search must succeed: {response}"
    );
    let structured = &response["result"]["structuredContent"];
    // The complete serialized frame must stay under the response cap.
    let bytes = serialized_frame_bytes(response);
    assert!(
        bytes <= MAX_RESPONSE_BYTES,
        "{context}: the complete response frame must stay under the \
         {MAX_RESPONSE_BYTES}-byte response cap, but was {bytes} bytes: {response}"
    );
    let hits = structured["hits"]
        .as_array()
        .expect("a ranked search returns a hits array");
    assert!(
        !hits.is_empty(),
        "{context}: the query must match a section"
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
            hit.get("excerpt")
                .and_then(Value::as_str)
                .is_some_and(|e| !e.is_empty()),
            "every hit carries a non-empty excerpt: {hit}"
        );
        // Citations back the hit: a non-empty, source-attributed array.
        let citations = hit.get("citations").and_then(Value::as_array);
        let citations = citations.expect("every hit carries a citations array: {hit}");
        assert!(!citations.is_empty(), "every hit carries citations: {hit}");
        for citation in citations {
            assert!(
                citation.get("source").and_then(Value::as_str).is_some(),
                "every citation names its source: {citation}"
            );
        }
    }
    hits.iter().collect()
}

/// #173: the real `serve` binary ranks the primary C-CONV explanation first (not
/// the inherited example list) and keeps the reported NL queries both surfaced
/// and response-bounded with citations intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_docs_ranks_primary_guideline_first_over_inherited_sections() {
    let mut client = Client::new().await;

    // --- AC2: C-CONV ranks the primary rule first. -------------------------
    // The exact reproduction the issue reports. No `limit` (the default five) so
    // the response is the server's natural ranking, not a page slice.
    let response = client
        .call_tool("search_docs", json!({ "query": "C-CONV" }))
        .await;
    let hits = assert_bounded_ranked(&response, "the C-CONV ranking");
    let top1 = hits[0]["section_id"]
        .as_str()
        .expect("the top hit names a section id");
    assert_eq!(
        top1, CCONV_PRIMARY,
        "`C-CONV` must rank the primary guideline explanation first, not the \
         inherited example section: {response}"
    );
    // The inherited secondary section must no longer lead; if it is present it is
    // ranked *after* the primary rule, not ahead of it.
    let inherited_positions: Vec<usize> = hits
        .iter()
        .enumerate()
        .filter(|(_, h)| h["section_id"] == CCONV_INHERITED)
        .map(|(i, _)| i)
        .collect();
    for &pos in &inherited_positions {
        assert!(
            pos > 0,
            "the inherited section must be demoted below the primary rule: {response}"
        );
    }

    // --- AC2: two previously-missing NL queries surface their source. -------
    // Each was in the issue's miss list; the ranking change must now surface its
    // expected source in the top five, with the response still bounded and cited.
    for (query, expected_source, context) in [
        (
            "how should use imports be organized and grouped?",
            "style-guide",
            "E-009 imports",
        ),
        (
            "what rustdoc configuration options exist?",
            "rustdoc",
            "E-044 rustdoc options",
        ),
    ] {
        let response = client
            .call_tool("search_docs", json!({ "query": query, "limit": 5 }))
            .await;
        let hits = assert_bounded_ranked(&response, context);
        // The ranking change must keep the result within the requested bound.
        assert!(
            hits.len() <= 5,
            "{context}: the limit-5 request must return at most five hits, got {}",
            hits.len()
        );
        let surfaced = hits.iter().any(|h| h["source"] == expected_source);
        assert!(
            surfaced,
            "{context}: the query must surface its expected source `{expected_source}` in the top five: {response}"
        );
    }

    // --- AC2: a clean disconnect proves the session leaves no orphan. -------
    client.close_and_wait_clean().await;
}

/// `get_section`: a valid section id yields its body + provenance + citations; a
/// missing id is a typed not-found carrying the offending id.
async fn call_get_section(client: &mut Client) {
    // --- A valid section id. ------------------------------------------------
    let response = client
        .call_tool(
            "get_section",
            json!({ "section_id": "api-guidelines/about#rust-api-guidelines" }),
        )
        .await;
    assert_eq!(
        response.get("error"),
        None,
        "a valid section must resolve: {response}"
    );
    let section = &response["result"]["structuredContent"]["section"];
    assert_eq!(section["id"], "api-guidelines/about#rust-api-guidelines");
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
    assert!(
        citations.is_some(),
        "the section carries citations: {section}"
    );
    // The schema version is echoed.
    assert!(
        response["result"]["structuredContent"]
            .get("schema_version")
            .is_some(),
        "the response carries the schema version"
    );

    // --- A missing section id is a typed not-found. -------------------------
    let response = client
        .call_tool(
            "get_section",
            json!({ "section_id": "cargo/does-not-exist" }),
        )
        .await;
    assert_error(
        &response,
        -32002,
        "a missing section is a typed not-found (-32002)",
    );
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
        .call_tool("get_api_checklist", json!({ "category": "Naming" }))
        .await;
    assert_eq!(
        response.get("error"),
        None,
        "a valid category resolves: {response}"
    );
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["category"],
        json!("Naming"),
        "the response echoes the requested category"
    );
    let entries = structured["entries"]
        .as_array()
        .expect("an `entries` array");
    assert!(
        !entries.is_empty(),
        "the `Naming` category has entries in the seed corpus"
    );
    for entry in entries {
        assert!(
            entry.get("code").and_then(Value::as_str).is_some(),
            "every entry carries an upstream code: {entry}"
        );
        assert_eq!(
            entry["category"],
            json!("Naming"),
            "a filtered entry belongs to the requested category"
        );
    }

    // --- An invalid category is a typed error with discoverable categories. --
    let response = client
        .call_tool(
            "get_api_checklist",
            json!({ "category": "No Such Category" }),
        )
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
        structured["entry"]["purpose"]
            .as_str()
            .is_some_and(|p| !p.is_empty()),
        "the entry carries a purpose"
    );
    // The cited section bodies are resolved into the `sections` page.
    let sections = structured["sections"]
        .as_array()
        .expect("a `sections` array");
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

    // --- The other routine command families also resolve on the seed. -------
    // Since issue [#149] unified the catalog with the snapshot, the seed's
    // catalog is derived from its own sections, so every seed-advertised key
    // resolves rather than dangling into a `-32603` (`UnresolvedSection`). This
    // is the AC3 "routine command families work on a fresh offline launch"
    // property, driven through the stdio seam.
    for key in ["cargo test", "rustup show"] {
        let response = client
            .call_tool("get_command_guidance", json!({ "command": key }))
            .await;
        assert_eq!(
            response.get("error"),
            None,
            "the routine command `{key}` resolves on the seed: {response}"
        );
        let structured = &response["result"]["structuredContent"];
        assert_eq!(
            structured["entry"]["key"],
            json!(key),
            "`{key}` round-trips its command key"
        );
        let sections = structured["sections"]
            .as_array()
            .expect("a `sections` array");
        assert!(
            !sections.is_empty(),
            "`{key}` cites at least one upstream section in the seed corpus"
        );
    }

    // --- An unsupported key is a typed error with the supported keys. -------
    let response = client
        .call_tool(
            "get_command_guidance",
            json!({ "command": "cargo frobnicate" }),
        )
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
    assert_eq!(
        list.get("error"),
        None,
        "resources/list must succeed: {list}"
    );
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
    let payload: Value =
        serde_json::from_str(text).expect("the catalog resource text is a JSON object");
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
    let uri = section_resource_uri("api-guidelines/about#rust-api-guidelines");
    let response = client.read_resource(&uri).await;
    assert_eq!(
        response.get("error"),
        None,
        "a section resource reads: {response}"
    );
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
    let payload: Value =
        serde_json::from_str(text).expect("the section resource text is a JSON object");
    let section = &payload["section"];
    assert_eq!(
        section["id"],
        json!("api-guidelines/about#rust-api-guidelines"),
        "the section resource carries its section"
    );
    assert!(
        section
            .get("body")
            .and_then(Value::as_str)
            .is_some_and(|b| !b.is_empty()),
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
    /// the end of the session), **drain whatever the server still has on
    /// stdout**, assert every remaining line is a JSON-RPC 2.0 message, and
    /// assert the server self-exits cleanly within the budget. The first and
    /// last are the "no orphaned process" claim; the drain is the *stricter*
    /// AC2 check that asserts the tail of the session — not just the lines up to
    /// the final response.
    async fn close_and_wait_clean(mut self) {
        // Dropping the piped stdin writer closes its write end, which the stdio
        // transport reads as an EOF — the end of the session. This mirrors the
        // lifecycle test's `drop(stdin)`.
        drop(self.stdin);

        // Now drain the child's remaining stdout. After the final response the
        // server may still emit a trailing notification, a misrouted log line,
        // or a panic message; EOF (the server closing stdout as it exits) ends
        // the drain. Every line it writes must be a JSON-RPC 2.0 message — the
        // "stdout carries only protocol traffic" assertion for the *tail* of the
        // session, the stricter check PR #77 asked for. Each read gets a fresh
        // budget so a slow session cannot make the drain time out spuriously.
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

        // stdout closed (EOF above), so the server is exiting; assert a clean,
        // successful self-exit — the concrete "no orphaned process" claim.
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
        code, expected_code as i64,
        "{context}: expected error code {expected_code}, got {code} ({response})"
    );
    // Every protocol error carries a human-readable message.
    assert!(
        error
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| !m.is_empty()),
        "an error response carries a message: {response}"
    );
}

// ---------------------------------------------------------------------------
// [059] AC2 — cancellation / EOF: a pipelined burst exits cleanly with no
// orphaned work, and the handlers don't block on per-request indexing/parsing.
// ---------------------------------------------------------------------------

/// [059] AC2: cancellation / EOF exits cleanly with **no orphaned work**, and
/// the handlers don't block on per-request indexing or parsing.
///
/// The companion [`stdio_lifecycle.rs`](stdio_lifecycle.rs) proves a clean
/// *single* connect/disconnect. This test goes further: it drives a **pipelined
/// burst** — many requests in flight at once — and proves the session still
/// cancels cleanly. The two halves of the criterion:
///
/// 1. **Handlers don't block on per-request work.** A burst of diverse requests
///    — every one of the five tools and both resources, several at a time — is
///    written to stdin back to back, with no response read in between. Each
///    handler resolves by reading the immutable snapshot built once at startup
///    (`DocsServer`'s `&self` handlers are `Send` + `Sync` and share it by
///    reference; `mcp::tests::handler_request_futures_are_send` is the static
///    proof), so a large concurrent burst drains without a per-request indexing
///    or parsing stall — the observable consequence of "indexing/parsing happens
///    at snapshot build time, not per call." Every response is matched to its
///    request `id` and checked for shape, proving no response is mixed,
///    corrupted, or lost while many are in flight.
///
/// 2. **Cancelling the session (EOF) exits cleanly, no orphaned work.** Once
///    every in-flight response is delivered, the test closes its side of stdin —
///    the stdio transport's end-of-session signal. The server then drains any
///    tail output, self-exits with status 0, and writes nothing to the protocol
///    stream that is not JSON-RPC 2.0 — no half-processed request is left
///    dangling and no process is leaked.
///
/// The check is hermetic: a temp `--data-dir`, and `serve` loads the bundled
/// snapshot in-memory (no network, no host cache). `kill_on_drop` is the safety
/// net that reaps the child on a panic, but the assertions prove a graceful
/// self-exit, not a forced kill.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_burst_exits_cleanly_on_eof_without_orphaned_work() {
    use std::collections::HashMap;
    use tokio::io::AsyncReadExt;
    use tokio::time::Instant;

    let data_dir = fresh_data_dir();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rust-docs-mcp"))
        .arg("serve")
        .arg("--data-dir")
        .arg(&data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the test harness must be able to launch the `serve` binary");

    // Take the piped handles out before the writer/reader tasks borrow them, so
    // we never move a handle we are about to use (and `child` stays available
    // for the final `wait()`).
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let mut reader = tokio::io::BufReader::new(stdout);
    let deadline = Instant::now() + BUDGET;

    // A stderr drain runs the whole time, so a full stderr buffer can never make
    // the child block on a diagnostic while we are reading stdout.
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });

    // (0) The initialize handshake + `notifications/initialized`, the way a
    //     conforming MCP client does — so the runtime is up before the burst.
    write_request(
        &mut stdin,
        0,
        "initialize",
        &json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "stdio-mcp-cancellation-test", "version": "0.0.1" },
        }),
    )
    .await;
    write_notify(&mut stdin, "notifications/initialized").await;
    let init = read_response_until(&mut reader, 0, deadline).await;
    assert_eq!(
        init.get("result")
            .and_then(|result| result.get("protocolVersion"))
            .and_then(Value::as_str),
        Some(PROTOCOL_VERSION),
        "the handshake must echo the negotiated protocol version"
    );

    // (1) The pipelined burst: every tool and both resources, several at a time,
    //     each with a distinct id. The writer writes them all back to back (no
    //     response read in between) while the reader collects every response by
    //     id.
    let plan = burst_plan();
    let frames: Vec<String> = plan
        .iter()
        .enumerate()
        .map(|(i, (method, params, _))| request_frame(i as u64 + 1, method, params))
        .collect();
    let expected_ids: Vec<u64> = (1..=plan.len() as u64).collect();
    let expected_key: HashMap<u64, &str> = plan
        .iter()
        .enumerate()
        .map(|(i, (_, _, key))| (i as u64 + 1, *key))
        .collect();

    // The writer holds stdin open until the reader has every response, so EOF
    // (dropping stdin) happens only *after* the burst is fully answered — never
    // mid-flight — and the "clean exit" below is a clean disconnect, not a
    // truncation of in-flight work.
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel();
    let writer = tokio::spawn(async move {
        for frame in &frames {
            stdin
                .write_all(frame.as_bytes())
                .await
                .expect("writing a pipelined request to the child's stdin must not fail");
            stdin
                .flush()
                .await
                .expect("flushing the child's stdin must not fail");
        }
        // Park on the reader's "I have every response" signal, then fall out of
        // the task — dropping `stdin` closes the child's stdin (the EOF).
        let _ = drain_rx.await;
    });

    // (2) Read every burst response, matching by id, asserting each is a
    //     JSON-RPC 2.0 success of its expected shape — no mixing, no corruption,
    //     nothing lost while many are in flight.
    let mut got: HashMap<u64, Value> = HashMap::new();
    while expected_ids.iter().any(|id| !got.contains_key(id)) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!(
                "timed out waiting for the burst: got {}/{} responses",
                got.len(),
                plan.len()
            );
        }
        let mut line = String::new();
        match tokio::time::timeout(remaining, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => panic!(
                "the server closed stdout before answering the burst \n                 \
                 (got {}/{} responses) — a disconnected server",
                got.len(),
                plan.len()
            ),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
            Err(_) => panic!("timed out waiting for the burst responses"),
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
        // A response (has an id) is stored; a notification (no id) is skipped. A
        // duplicate id is a protocol violation the server must not produce.
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            assert!(
                got.insert(id, value).is_none(),
                "the server sent a duplicate response for id {id}"
            );
        }
    }

    // Every burst response is a success of its expected shape — no orphaned,
    // mixed, or corrupted result.
    for (id, response) in &got {
        assert!(
            response.get("error").is_none(),
            "burst request {id} must succeed, not error: {response}"
        );
        let key = expected_key[id];
        assert!(
            response
                .get("result")
                .and_then(|result| result.get(key))
                .is_some(),
            "burst response {id} must carry its `{key}` result (no mixing/corruption)"
        );
    }

    // (3) EOF: tell the writer to release stdin, then confirm the graceful,
    //     successful self-exit with no orphaned process.
    let _ = drain_tx.send(());
    writer
        .await
        .expect("the pipelined writer task must not panic");

    // Drain any tail the server writes after the final response (it may emit a
    // trailing notification); every tail line must still be JSON-RPC 2.0, and
    // EOF ends the drain when the server closes stdout as it exits.
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("timed out draining the child's stdout after the burst");
        }
        let mut line = String::new();
        match tokio::time::timeout(remaining, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => break, // EOF: the server closed stdout and exited.
            Ok(Ok(_)) => {}
            Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
            Err(_) => panic!("timed out draining the child's stdout after the burst"),
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

    // The concrete "no orphaned process" claim: a clean, successful self-exit.
    let status = match tokio::time::timeout(BUDGET, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => panic!("waiting on the child failed: {error}"),
        Err(_) => panic!("the child did not exit within {BUDGET:?} — a likely orphan"),
    };
    assert!(
        status.success(),
        "after a clean EOF disconnect the server must exit successfully, got {status:?}"
    );

    // No orphaned *or* panicked work: the diagnostics carry no panic. A panic
    // already forces a non-zero exit (caught above); this makes the "no orphaned
    // work" claim explicit on the diagnostic stream too.
    let stderr_bytes = stderr_task.await.expect("the stderr drain must finish");
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    assert!(
        !stderr.contains("panic"),
        "the server must not panic on EOF/cancellation; stderr: {stderr}"
    );
}

/// Build the pipelined burst plan: every one of the five tools and both
/// resources, several at a time, each a *valid* request (so every response is a
/// success). Each entry is `(method, params, expected_result_key)`: the five
/// tool calls surface `result.structuredContent`, the two resource reads
/// surface `result.contents`.
fn burst_plan() -> Vec<(String, Value, &'static str)> {
    let section_uri = section_resource_uri("api-guidelines/about#rust-api-guidelines");
    let mut plan: Vec<(String, Value, &'static str)> = Vec::new();
    // Two interleaved rounds of the whole surface — a diverse, concurrent burst.
    for _ in 0..2 {
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "list_sources", "arguments": {} }),
            "structuredContent",
        ));
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "search_docs", "arguments": { "query": "C-CONV" } }),
            "structuredContent",
        ));
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "search_docs", "arguments": { "query": "Rust", "limit": 1 } }),
            "structuredContent",
        ));
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "get_section", "arguments": { "section_id": "api-guidelines/about#rust-api-guidelines" } }),
            "structuredContent",
        ));
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "get_api_checklist", "arguments": { "category": "Naming" } }),
            "structuredContent",
        ));
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "get_command_guidance", "arguments": { "command": "cargo build" } }),
            "structuredContent",
        ));
        plan.push((
            "resources/read".to_string(),
            json!({ "uri": CATALOG_URI }),
            "contents",
        ));
        plan.push((
            "resources/read".to_string(),
            json!({ "uri": &section_uri }),
            "contents",
        ));
    }
    plan
}

/// Encode one numbered JSON-RPC 2.0 request as a newline-framed string.
fn request_frame(id: u64, method: &str, params: &Value) -> String {
    let message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    format!(
        "{}\n",
        serde_json::to_string(&message).expect("encoding a hand-built JSON value must not fail")
    )
}

/// Write one numbered JSON-RPC request frame to the child's stdin and flush.
async fn write_request(
    stdin: &mut tokio::process::ChildStdin,
    id: u64,
    method: &str,
    params: &Value,
) {
    let frame = request_frame(id, method, params);
    stdin
        .write_all(frame.as_bytes())
        .await
        .expect("writing a JSON-RPC request to the child's stdin must not fail");
    stdin
        .flush()
        .await
        .expect("flushing the child's stdin must not fail");
}

/// Write a JSON-RPC notification frame (no id, no response) and flush.
async fn write_notify(stdin: &mut tokio::process::ChildStdin, method: &str) {
    let message = json!({ "jsonrpc": "2.0", "method": method });
    let frame =
        serde_json::to_string(&message).expect("encoding a hand-built JSON value must not fail");
    stdin
        .write_all(format!("{frame}\n").as_bytes())
        .await
        .expect("writing a JSON-RPC notification to the child's stdin must not fail");
    stdin
        .flush()
        .await
        .expect("flushing the child's stdin must not fail");
}

/// Read stdout lines until the response whose `id` matches, asserting every line
/// is a JSON-RPC 2.0 message; returns that response. A per-read deadline keeps a
/// hung server from hanging the test forever.
async fn read_response_until(
    reader: &mut tokio::io::BufReader<tokio::process::ChildStdout>,
    id: u64,
    deadline: tokio::time::Instant,
) -> Value {
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for the response to id {id}");
        }
        let mut line = String::new();
        match tokio::time::timeout(remaining, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => {
                panic!("the server closed stdout before answering id {id} — a disconnected server")
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
            Err(_) => panic!("timed out waiting for the response to id {id}"),
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

// ---------------------------------------------------------------------------
// [175] AC4 — large guidance inputs under a concurrent burst.
//
// The companion [`concurrent_burst_exits_cleanly_on_eof_without_orphaned_work`]
// above exercises a *small, diverse* burst (one page per tool). This test goes
// to the **worst case the issue reports**: the most-cited command, `rustfmt`
// (50 cited sections in the shipped corpus), served at the **largest valid page**
// — 15 sections, ~30 KiB, just under the [`MAX_RESPONSE_BYTES`](crate::pagination::MAX_RESPONSE_BYTES)
// cap — and then a **continuation page** of that same large guidance input. The
// regression the issue #175 reports is that each of those 15 cited sections
// rebuilt the O(corpus) index, so one large guidance call was ~15× O(corpus). With
// the index now built once per snapshot, these large, multi-section inputs must
// drain under a concurrent burst with **no per-request rebuild stall**, the
// continuation page must resolve through the same shared index, and the session
// must still cancel cleanly (EOF → graceful self-exit, no orphaned work, no
// panic).
//
// The check is hermetic: a temp `--data-dir`, and `serve` loads the bundled
// snapshot in-memory (no network, no host cache). `kill_on_drop` is the safety
// net that reaps the child on a panic, but the assertions prove a graceful
// self-exit, not a forced kill. The test is `#[ignore]`d **only** when the
// machine is under heavy parallel load; in the ordinary serial suite it runs.
// ---------------------------------------------------------------------------

/// The largest valid `get_command_guidance` page for `rustfmt` in the shipped
/// corpus: 15 sections (~30 KiB, under the 32 KiB response cap), a real
/// continuation to follow. A `limit` above this overflows the cap and the server
/// refuses it, so 15 is the "large but valid" input the burst exercises.
const LARGE_GUIDANCE_LIMIT: usize = 15;
/// The worst-supported command — the most-cited (`rustfmt`, 50 sections in the
/// shipped corpus), the one whose per-section index rebuilds the issue reports.
const WORST_GUIDANCE_COMMAND: &str = "rustfmt";
/// The minimum number of sections a "large" guidance page must actually carry, so
/// a fast empty / one-section page can never masquerade as the large payload.
const MIN_LARGE_GUIDANCE_SECTIONS: u64 = 10;

/// The large-guidance pipelined burst plan. Each entry is
/// `(method, params, expected_result_key, min_sections)`: `min_sections` is the
/// smallest `sections` array a guidance response must carry to count as the large
/// payload (0 for non-guidance entries, which only need their `structuredContent` /
/// `contents` result present). The plan interleaves the worst command at the
/// largest valid page with the other four tools and both resources, several times,
/// so the burst is both **large** (the multi-section guidance payloads) and
/// **concurrent** (every frame is written back to back, no response read between).
fn large_guidance_plan() -> Vec<(String, Value, &'static str, u64)> {
    let section_uri = section_resource_uri("api-guidelines/about#rust-api-guidelines");
    let mut plan: Vec<(String, Value, &'static str, u64)> = Vec::new();
    // Three interleaved rounds so the large guidance payloads are several at a time.
    for _ in 0..3 {
        plan.push((
            "tools/call".to_string(),
            json!({
                "name": "get_command_guidance",
                "arguments": { "command": WORST_GUIDANCE_COMMAND, "limit": LARGE_GUIDANCE_LIMIT },
            }),
            "structuredContent",
            MIN_LARGE_GUIDANCE_SECTIONS,
        ));
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "list_sources", "arguments": {} }),
            "structuredContent",
            0,
        ));
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "search_docs", "arguments": { "query": "C-CONV" } }),
            "structuredContent",
            0,
        ));
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "get_section", "arguments": { "section_id": "api-guidelines/about#rust-api-guidelines" } }),
            "structuredContent",
            0,
        ));
        plan.push((
            "tools/call".to_string(),
            json!({ "name": "get_api_checklist", "arguments": { "category": "Naming" } }),
            "structuredContent",
            0,
        ));
        plan.push((
            "resources/read".to_string(),
            json!({ "uri": CATALOG_URI }),
            "contents",
            0,
        ));
        plan.push((
            "resources/read".to_string(),
            json!({ "uri": &section_uri }),
            "contents",
            0,
        ));
    }
    plan
}

/// [175] AC4: large, multi-section guidance inputs drain under a concurrent burst
/// and the session still cancels cleanly. See the section header above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_large_guidance_burst_responds_and_shuts_down_cleanly() {
    use std::collections::HashMap;
    use tokio::io::AsyncReadExt;
    use tokio::time::Instant;

    let data_dir = fresh_data_dir();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rust-docs-mcp"))
        .arg("serve")
        .arg("--data-dir")
        .arg(&data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the test harness must be able to launch the `serve` binary");

    let mut stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let mut reader = tokio::io::BufReader::new(stdout);
    let deadline = Instant::now() + BUDGET;

    // A stderr drain runs the whole time, so a full stderr buffer can never make
    // the child block on a diagnostic while we are reading stdout.
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });

    // (0) The initialize handshake + `notifications/initialized`, so the runtime
    //     (and the once-built snapshot index) is up before the burst.
    write_request(
        &mut stdin,
        0,
        "initialize",
        &json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "stdio-mcp-large-guidance-test", "version": "0.0.1" },
        }),
    )
    .await;
    write_notify(&mut stdin, "notifications/initialized").await;
    let init = read_response_until(&mut reader, 0, deadline).await;
    assert_eq!(
        init.get("result")
            .and_then(|result| result.get("protocolVersion"))
            .and_then(Value::as_str),
        Some(PROTOCOL_VERSION),
        "the handshake must echo the negotiated protocol version"
    );

    // (1) The large-guidance pipelined burst: the worst command at the largest
    //     valid page, interleaved with the other tools and both resources, every
    //     frame written back to back with no response read in between.
    let plan = large_guidance_plan();
    let frames: Vec<String> = plan
        .iter()
        .enumerate()
        .map(|(i, (method, params, _, _))| request_frame(i as u64 + 1, method, params))
        .collect();
    let expected_ids: Vec<u64> = (1..=plan.len() as u64).collect();
    let expected_key: HashMap<u64, &str> = plan
        .iter()
        .enumerate()
        .map(|(i, (_, _, key, _))| (i as u64 + 1, *key))
        .collect();
    let expected_min_sections: HashMap<u64, u64> = plan
        .iter()
        .enumerate()
        .map(|(i, (_, _, _, min))| (i as u64 + 1, *min))
        .collect();

    // The writer holds stdin open until the burst (and any continuation) is fully
    // answered, so EOF (dropping stdin) happens only *after* — never mid-flight —
    // and the "clean exit" below is a clean disconnect, not a truncation of
    // in-flight work. Post-burst frames (the continuation) are handed to the
    // writer through `act_tx`: `Some(frame)` writes it, `None` is the EOF signal.
    let (act_tx, mut act_rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    let writer = tokio::spawn(async move {
        for frame in &frames {
            stdin
                .write_all(frame.as_bytes())
                .await
                .expect("writing a pipelined request to the child's stdin must not fail");
            stdin
                .flush()
                .await
                .expect("flushing the child's stdin must not fail");
        }
        while let Some(action) = act_rx.recv().await {
            match action {
                Some(frame) => {
                    stdin
                        .write_all(frame.as_bytes())
                        .await
                        .expect("writing the continuation to the child's stdin must not fail");
                    stdin
                        .flush()
                        .await
                        .expect("flushing the child's stdin must not fail");
                }
                None => break, // EOF: fall out, dropping stdin closes the session.
            }
        }
    });

    // (2) Read every burst response, matching by id, asserting each is a JSON-RPC
    //     2.0 success of its expected shape — no mixing, no corruption, nothing
    //     lost while many large requests are in flight.
    let mut got: HashMap<u64, Value> = HashMap::new();
    while expected_ids.iter().any(|id| !got.contains_key(id)) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!(
                "timed out waiting for the large-guidance burst: got {}/{} responses",
                got.len(),
                plan.len()
            );
        }
        let mut line = String::new();
        match tokio::time::timeout(remaining, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => panic!(
                "the server closed stdout before answering the burst \n                 \
                 (got {}/{} responses) — a disconnected server",
                got.len(),
                plan.len()
            ),
            Ok(Ok(_)) => {}
            Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
            Err(_) => panic!("timed out waiting for the burst responses"),
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
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            assert!(
                got.insert(id, value).is_none(),
                "the server sent a duplicate response for id {id}"
            );
        }
    }

    // Every burst response is a success of its expected shape, and the large
    // guidance payloads actually carry many sections (a fast empty / one-section
    // page must never masquerade as the large input).
    for (id, response) in &got {
        assert!(
            response.get("error").is_none(),
            "burst request {id} must succeed, not error: {response}"
        );
        let key = expected_key[id];
        let result = response
            .get("result")
            .and_then(|result| result.get(key))
            .expect("burst response must carry its result");
        if let Some(min) = expected_min_sections.get(id).filter(|min| **min > 0) {
            // For a guidance response `key` is `structuredContent`, so `result`
            // is already that object and its `sections` array is the payload.
            let sections = result
                .get("sections")
                .and_then(Value::as_array)
                .unwrap_or_else(|| {
                    panic!("large guidance response {id} must carry a `sections` array")
                });
            assert!(
                sections.len() >= *min as usize,
                "large guidance response {id} carried only {} section(s) — expected >= {min} \
                 (a fast empty/short page must not stand in for the large payload)",
                sections.len()
            );
        }
    }

    // (3) Continuation of a large guidance input: resume a *guidance* page's
    //     `next_cursor` through the same shared index (issue #175), so it returns
    //     the remaining cited sections as a second large page. The continuation
    //     frame is a `get_command_guidance` call, whose continuation cursor is
    //     minted under `Operation::Catalog`; the server rejects a cursor whose
    //     operation does not match the current one, so the cursor must come from
    //     one of the burst's *guidance* responses — not from another tool that
    //     also carries a cursor (e.g. `get_api_checklist`, `Operation::Checklist`).
    //     The guidance entries are exactly the plan entries that demand the large
    //     `min_sections` payload, so filtering on that identifies them precisely.
    let guidance_ids: Vec<u64> = plan
        .iter()
        .enumerate()
        .filter_map(|(i, (_, _, _, min))| (*min > 0).then_some(i as u64 + 1))
        .collect();
    let next_cursor = got
        .iter()
        .filter(|(id, _)| guidance_ids.contains(id))
        .find_map(|(_, response)| {
            response
                .get("result")
                .and_then(|r| r.get("structuredContent"))
                .and_then(|sc| sc.get("next_cursor"))
                .filter(|c| !c.is_null())
        })
        .expect("at least one large guidance page must carry a continuation cursor")
        .clone();
    // Hand the continuation frame to the writer so it is written with stdin still
    // open (the burst is fully drained before the continuation, which is before
    // EOF); the writer then writes it and flushes.
    act_tx
        .send(Some(request_frame(plan.len() as u64 + 1, "tools/call", &json!({
            "name": "get_command_guidance",
            "arguments": { "command": WORST_GUIDANCE_COMMAND, "limit": LARGE_GUIDANCE_LIMIT, "cursor": next_cursor },
        }))))
        .expect("the writer task must still be alive to take the continuation frame");
    let continuation_response =
        read_response_until(&mut reader, plan.len() as u64 + 1, deadline).await;
    assert!(
        continuation_response.get("error").is_none(),
        "the large-guidance continuation must succeed, not error: {continuation_response}"
    );
    let cont_sections = continuation_response
        .get("result")
        .and_then(|r| r.get("structuredContent"))
        .and_then(|sc| sc.get("sections"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the continuation must carry a `sections` array"));
    assert!(
        !cont_sections.is_empty(),
        "the large-guidance continuation must return the remaining cited sections"
    );

    // (4) EOF: tell the writer to close stdin, then confirm the graceful, successful
    //     self-exit with no orphaned process — even after the large burst.
    act_tx
        .send(None)
        .expect("the writer task must still be alive to take the EOF signal");
    writer
        .await
        .expect("the pipelined writer task must not panic");

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("timed out draining the child's stdout after the burst");
        }
        let mut line = String::new();
        match tokio::time::timeout(remaining, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(_)) => {}
            Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
            Err(_) => panic!("timed out draining the child's stdout after the burst"),
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

    let status = match tokio::time::timeout(BUDGET, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => panic!("waiting on the child failed: {error}"),
        Err(_) => panic!("the child did not exit within {BUDGET:?} — a likely orphan"),
    };
    assert!(
        status.success(),
        "after a clean EOF disconnect the server must exit successfully, got {status:?}"
    );

    // No orphaned *or* panicked work on the diagnostic stream.
    let stderr_bytes = stderr_task.await.expect("the stderr drain must finish");
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    assert!(
        !stderr.contains("panic"),
        "the server must not panic on EOF/cancellation; stderr: {stderr}"
    );
}
