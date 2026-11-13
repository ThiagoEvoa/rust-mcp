//! End-to-end agent trial for task `[070][M5]` — *"Run the Rust review agent
//! trial"*.
//!
//! # What this trial is
//!
//! This is the plan's §1 *success scenario* (its **review** half) exercised as a
//! test:
//!
//! > When reviewing Rust code, the agent retrieves relevant documentation via the
//! > server's tools and produces review findings that identify concrete code
//! > locations and relevant source citations.
//!
//! The test **is** that review agent's retrieval session, driven through the same
//! spawned-stdio JSON-RPC 2.0 seam as [`tests/first_client.rs`],
//! [`tests/stdio_mcp.rs`], [`tests/trial_library_api_design.rs`], and
//! [`tests/trial_project_implementation.rs`]: the real `rust-docs-mcp serve`
//! binary over genuine OS stdio, over the **embedded bundled seed**
//! (`rust-docs-bundled-seed`), **offline** (an empty `--data-dir`; no network, no
//! host cache, no new dependency — `tokio` + `serde_json` only). It records the
//! *actual* retrieved evidence and canonical citations the plan's §10/M5 trial
//! asks for, and asserts them so the trial is **reproducible, not hand-claimed**.
//!
//! The human-readable transcript — every request and its real response — is
//! recorded in [`docs/trials/review.md`](../docs/trials/review.md).
//!
//! # The session (the trial's calls, in order)
//!
//! 1. `initialize` → negotiate `2025-11-25`, advertise the `tools` + `resources`
//!    capabilities, and carry the agent-workflow `instructions`.
//! 2. `notifications/initialized` → complete the handshake.
//! 3. `tools/list` → the agent discovers the five tools, including
//!    `get_api_checklist` and `search_docs`.
//! 4. `search_docs "C-CONV"` → the *exact-guideline-code* search, the reliable
//!    path to the conversion guideline's section: it surfaces
//!    `api-guidelines/about`, carrying the `C-CONV` guideline-code citation.
//! 5. `get_api_checklist { category: "Conversions and From" }` → the checklist
//!    entry for the conversion guideline: `C-CONV` "Prefer `From`/`Into` for
//!    conversions", explaining `api-guidelines/about`.
//! 6. `get_section "api-guidelines/about"` → the section the agent cites: its
//!    body, `canonical_url`, `content_hash`, and citations (including the
//!    `guideline code: C-CONV` note).
//! 7. `search_docs "lint"` → the reliable path to the **Clippy lint** the
//!    reviewer would cite: it surfaces `clippy/configuration` (the seed's one
//!    Clippy section), carrying the "Clippy is a nightly component …"
//!    qualification citation.
//! 8. `get_section "clippy/configuration"` → the Clippy section the agent cites:
//!    its body, `canonical_url`, `content_hash`, and citations (the canonical
//!    document + the nightly-component qualification).
//! 9. `get_section "style-guide/intro"` → a **style-guide** section the agent
//!    cites for a style finding: its body, `canonical_url`, `content_hash`, and
//!    the canonical-document citation.
//! 10. `search_docs "needless_return"` → a **coverage-limitation probe** (AC2):
//!     a *specific* Clippy lint whose section the representative seed does **not**
//!     ship — a useful `0 hit` result with `next_steps`, the recorded gap rather
//!     than a fabricated lint section.
//! 11. Clean client disconnect → the server self-exits.
//!
//! # What the trial does *not* claim (AC2)
//!
//! The trial *retrieves* documentation and *records its citations*. It does
//! **not** run any check: the server "retrieves official Rust documentation only:
//! it does not run commands, does not touch the project filesystem, and does not
//! reach the network", and it "does not execute commands or observe their results
//! automatically". So a finding's **check outcome** — that a snippet *actually*
//! violates `C-CONV`, triggers a Clippy lint, or breaks a style convention — is
//! an **unverified assumption** the review agent infers from the retrieved
//! guideline plus the snippet; it is **not** an observed result of a Clippy or
//! build run. What the trial *observes* is the retrieval itself: the sections,
//! their `content_hash` / `canonical_url` / `citations`, the `0 hit` for a lint
//! absent from the seed, and the server's stated "evidence, not a guarantee"
//! boundary. The trial asserts both halves and the coverage gap it records; it
//! does not over-claim a check result or complete recall.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;

/// How long the trial waits for a response and, after a clean disconnect, for the
/// server to exit. A real stdio round-trip on localhost is sub-second; this
/// generous budget keeps the trial fast while tolerating CI load.
const BUDGET: Duration = Duration::from_secs(15);

/// The protocol version the client negotiates — the highest
/// `initialize`-capable version the server negotiates to.
const PROTOCOL_VERSION: &str = "2025-11-25";

/// The active bundled snapshot's id — the seed corpus `serve` always loads.
const SNAPSHOT_ID: &str = "rust-docs-bundled-seed";

/// The exact-guideline-code search that reliably surfaces the conversion
/// guideline's section (the reliable path a reviewer would use).
const CONVERSION_QUERY: &str = "C-CONV";

/// The checklist category the conversion-naming guideline sits in.
const CONVERSION_CATEGORY: &str = "Conversions and From";

/// The section the conversion finding cites (the `api-guidelines` intro that
/// carries the `C-CONV` guideline code).
const API_GUIDELINES_SECTION: &str = "api-guidelines/about";
/// Its captured content identity and canonical upstream URL (the citation the
/// agent records).
const API_GUIDELINES_HASH: &str = "c128-ffa531f24bfdd996c9ef3d1a076dd56a";
const API_GUIDELINES_URL: &str = "https://rust-lang.github.io/api-guidelines/about.html";
/// The guideline-code citation note the seed's section carries.
const API_GUIDELINES_NOTE: &str = "guideline code: C-CONV";

/// The search query that reliably surfaces the seed's one Clippy section.
const CLIPPY_QUERY: &str = "lint";
/// The Clippy section the lint finding cites.
const CLIPPY_SECTION: &str = "clippy/configuration";
/// Its captured content identity and canonical upstream URL.
const CLIPPY_HASH: &str = "c128-ffa5381340fae3e97c387c8bd729fcbc";
const CLIPPY_URL: &str = "https://doc.rust-lang.org/clippy/configuration.html";
/// The nightly-component qualification citation note the seed's section carries.
const CLIPPY_QUAL_NOTE: &str = "qualification: Clippy is a nightly component; a lint may be added or removed between releases.";

/// The style-guide section the style finding cites.
const STYLE_SECTION: &str = "style-guide/intro";
/// Its captured content identity and canonical upstream URL.
const STYLE_HASH: &str = "c128-ffa549d71811a6f0c1df98d4204e09dc";
const STYLE_URL: &str = "https://doc.rust-lang.org/nightly/style-guide/intro.html";

/// A *specific* Clippy lint the coverage-limitation probe searches for. The
/// representative seed ships only the `clippy/configuration` section, not the
/// per-lint catalog, so this lint's section is absent — a `0 hit` the trial
/// records as a coverage gap rather than a fabricated section.
const MISSING_LINT_QUERY: &str = "needless_return";

/// The five tools the trial expects the agent to discover.
const TOOLS: [&str; 5] = [
    "list_sources",
    "search_docs",
    "get_section",
    "get_api_checklist",
    "get_command_guidance",
];

/// A temporary per-test data directory, given to the child as `--data-dir` so the
/// process never writes the host's real application cache. `serve` does not
/// consult it (the bundled snapshot is loaded in-memory), but the flag keeps the
/// child's behaviour identical to the lifecycle test's hermetic posture.
fn fresh_data_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rust-docs-mcp-review-trial-{}-{}",
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
/// request at a time, reads stdout one newline-framed JSON-RPC message at a time,
/// and records every request/response pair for the transcript. A response is
/// matched to its request by `id` (the server may answer out of order), and every
/// stdout line is asserted to be a JSON-RPC 2.0 message — the "stdout carries
/// only protocol traffic" half of the configuration.
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
    /// handshake + `notifications/initialized` exactly once, the way a conforming
    /// MCP client does. Returns the completed client together with that single
    /// `initialize` response, so a caller asserts on exactly the handshake the
    /// transcript records — without re-initializing (a second `initialize` would
    /// record a redundant exchange and desync the transcript from the session it
    /// documents; that desync was the [068] defect).
    async fn new() -> (Self, Value) {
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
            .expect("the trial harness must be able to launch the `serve` binary");

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

        let init = client.initialize().await;
        client.notify("notifications/initialized").await;
        (client, init)
    }

    /// Send the `initialize` request and assert the server echoes the negotiated
    /// protocol version and its implementation identity. Returns the response so
    /// the caller can inspect the advertised `instructions`.
    async fn initialize(&mut self) -> Value {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "rust-review-agent", "version": "0.0.1" },
        });
        let response = self.request("initialize", &params).await;
        assert_eq!(
            response.get("error"),
            None,
            "the initialization handshake must succeed, not error: {response}"
        );
        response
    }

    /// List the advertised tools.
    async fn list_tools(&mut self) -> Value {
        self.request("tools/list", &json!({})).await
    }

    /// Call a tool by name with the given arguments object, returning its
    /// response (asserting success — a valid search, checklist, and section read
    /// must all return a `result`, including the `0 hit` coverage probe).
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
    /// pairs) to `path`, when the `RUST_DOCS_MCP_TRIAL_TRANSCRIPT` variable is
    /// set. That is how the [`docs/trials/review.md`] transcript is regenerated:
    ///
    /// ```sh
    /// RUST_DOCS_MCP_TRIAL_TRANSCRIPT=/tmp/review_trial.json \
    ///   cargo test --test trial_review -- --exact \
    ///     the_review_agent_trial_retrieves_and_cites_review_evidence
    /// jq . /tmp/review_trial.json
    /// ```
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
            "task": "[070][M5] Rust review agent trial",
            "client": "raw JSON-RPC 2.0 over stdio (tests/trial_review.rs)",
            "protocol_version": PROTOCOL_VERSION,
            "snapshot_id": SNAPSHOT_ID,
            "exchanges": entries,
        });
        let pretty = serde_json::to_string_pretty(&document)
            .expect("serializing the transcript must not fail");
        std::fs::write(&path, format!("{pretty}\n"))
            .unwrap_or_else(|error| panic!("writing the transcript to {path:?} failed: {error}"));
    }
}

// ---------------------------------------------------------------------------
// The findings the review agent produces.
// ---------------------------------------------------------------------------

/// The status of a finding's **check outcome**, per AC2's observed-vs-unverified
/// split. The *retrieval* (that the guideline/lint/section was found and cited)
/// is **observed** — the server returned it. That the *snippet actually violates
/// the guideline / triggers the lint / breaks the convention* is an
/// **unverified assumption** — the review agent infers it from the retrieved
/// guideline plus the snippet; the server does **not** run Clippy or a build, so
/// no check outcome is observed in this trial.
#[derive(Debug, PartialEq, Eq)]
enum CheckStatus {
    /// The retrieval was observed; the snippet's check outcome is inferred.
    RetrievedNotChecked,
}

/// One review finding: a concrete review-target location (a representative
/// snippet + `file:line`, **described, not executed by the server**) paired with
/// the server-returned citation that justifies it.
struct Finding {
    /// A short label for the finding.
    id: &'static str,
    /// The concrete code location the finding points at (`file:line`).
    location: &'static str,
    /// The representative review-target snippet at that location. The server
    /// never sees or executes this text — it is the agent's own artifact.
    snippet: &'static str,
    /// The server-returned citation the finding records.
    source: &'static str,
    section_id: &'static str,
    canonical_url: &'static str,
    content_hash: &'static str,
    /// The guideline / lint / qualification note the citation carries.
    note: &'static str,
    /// The finding's check-outcome status (AC2).
    status: CheckStatus,
}

/// The three findings the trial's review agent produces, each pairing a concrete
/// code location with a server-returned citation.
fn findings() -> Vec<Finding> {
    vec![
        Finding {
            id: "F-1 conversion (C-CONV)",
            // The review target: a hand-rolled conversion that should be a
            // `From`/`Into` impl. Described, not executed.
            location: "src/units/meter.rs:14",
            snippet: "fn to_meter(value: f32) -> Meter { Meter { value } } // C-CONV: use From/Into",
            source: "api-guidelines",
            section_id: API_GUIDELINES_SECTION,
            canonical_url: API_GUIDELINES_URL,
            content_hash: API_GUIDELINES_HASH,
            note: API_GUIDELINES_NOTE,
            status: CheckStatus::RetrievedNotChecked,
        },
        Finding {
            id: "F-2 clippy lint",
            // The review target: a trailing `return` a Clippy lint would flag.
            // Described, not executed (the trial does not run `cargo clippy`).
            location: "src/parser/parse.rs:37",
            snippet: "fn parse(t: &[Token]) -> Result<Expr> { let e = build(t); return Ok(e); }",
            source: "clippy",
            section_id: CLIPPY_SECTION,
            canonical_url: CLIPPY_URL,
            content_hash: CLIPPY_HASH,
            note: CLIPPY_QUAL_NOTE,
            status: CheckStatus::RetrievedNotChecked,
        },
        Finding {
            id: "F-3 style",
            // The review target: a public fn whose name is not a noun phrase.
            location: "src/config/load.rs:8",
            snippet: "pub fn do_stuff() { /* load config */ } // style-guide: naming",
            source: "style-guide",
            section_id: STYLE_SECTION,
            canonical_url: STYLE_URL,
            content_hash: STYLE_HASH,
            note: "canonical upstream document",
            status: CheckStatus::RetrievedNotChecked,
        },
    ]
}

// ---------------------------------------------------------------------------
// The trial.
// ---------------------------------------------------------------------------

/// The full Rust review agent trial: the plan's §1 review-half success scenario,
/// driven through the spawned-stdio seam over the bundled seed. It asserts that
/// the retrieval produces the evidence and canonical citations the findings
/// record (AC1), records the observed-vs-unverified split and the coverage gap
/// (AC2), and proves a clean disconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_review_agent_trial_retrieves_and_cites_review_evidence() {
    let (mut client, init) = Client::new().await;

    // ---- (1) initialize: the negotiated version, the capabilities, and the
    //         agent-workflow `instructions` the trial runs on. `Client::new`
    //         completes the handshake exactly once and returns that single
    //         `initialize` response, so these assertions run on exactly the
    //         handshake the transcript records (no redundant second exchange —
    //         the [068] defect).
    //         ----------------------------------------------------------------
    assert_eq!(
        init["result"]
            .get("protocolVersion")
            .and_then(Value::as_str),
        Some(PROTOCOL_VERSION),
        "the handshake negotiates the version the client selected"
    );
    assert!(
        init["result"]["capabilities"].get("tools").is_some(),
        "the handshake advertises the tools capability"
    );
    assert!(
        init["result"]["capabilities"].get("resources").is_some(),
        "the handshake advertises the resources capability"
    );
    assert_eq!(
        init["result"]
            .pointer("/serverInfo/name")
            .and_then(Value::as_str),
        Some(env!("CARGO_PKG_NAME")),
        "the handshake carries the documented server identity"
    );
    // AC2 boundary, stated by the server itself: the retrieval-versus-execution
    // boundary the trial must not over-claim.
    let instructions = init["result"]
        .get("instructions")
        .and_then(Value::as_str)
        .expect("the handshake advertises the agent-workflow instructions");
    the_server_states_it_provides_evidence_not_a_guarantee(instructions);

    // ---- (2) tools/list: the agent discovers the five tools. ----------------
    let tools = client.list_tools().await;
    assert_eq!(tools.get("error"), None, "tools/list must succeed: {tools}");
    let tool_names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns a `tools` array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for expected in TOOLS {
        assert!(
            tool_names.contains(&expected),
            "tools/list must advertise the `{expected}` tool; got {tool_names:?}"
        );
    }
    assert!(
        tool_names.contains(&"get_api_checklist") && tool_names.contains(&"search_docs"),
        "the trial depends on `get_api_checklist` and `search_docs` being advertised"
    );

    // ---- (3) C-CONV: the reliable exact-guideline-code search surfaces the
    //         conversion guideline's section, with its `C-CONV` citation. ------
    let conv_search = client
        .call_tool("search_docs", json!({ "query": CONVERSION_QUERY }))
        .await;
    assert_eq!(
        conv_search.get("error"),
        None,
        "the code search must succeed: {conv_search}"
    );
    let conv_hits = conv_search["result"]["structuredContent"]["hits"]
        .as_array()
        .expect("the code search returns a `hits` array");
    let cited = conv_hits
        .iter()
        .find(|hit| hit.get("section_id").and_then(Value::as_str) == Some(API_GUIDELINES_SECTION))
        .expect("the exact-code search surfaces the `{API_GUIDELINES_SECTION}` section");
    assert_eq!(
        cited.get("source").and_then(Value::as_str),
        Some("api-guidelines"),
        "the cited section names its source"
    );
    assert_citations_carry_note(cited, API_GUIDELINES_NOTE, "the code-search hit");

    // ---- (4) get_api_checklist "Conversions and From": the checklist entry
    //         for the conversion guideline, explaining `api-guidelines/about`.
    let checklist = client
        .call_tool(
            "get_api_checklist",
            json!({ "category": CONVERSION_CATEGORY }),
        )
        .await;
    assert_eq!(
        checklist.get("error"),
        None,
        "a valid category must resolve: {checklist}"
    );
    let entries = checklist["result"]["structuredContent"]["entries"]
        .as_array()
        .expect("get_api_checklist returns an `entries` array");
    assert_eq!(
        checklist["result"]["structuredContent"]["snapshot_id"], SNAPSHOT_ID,
        "the checklist names the active snapshot"
    );
    let c_conv = entries
        .iter()
        .find(|entry| entry.get("code").and_then(Value::as_str) == Some("C-CONV"))
        .expect("the conversion category lists the `C-CONV` entry");
    assert_eq!(
        c_conv.get("category").and_then(Value::as_str),
        Some(CONVERSION_CATEGORY),
        "the `C-CONV` entry sits in the conversion category"
    );
    let explains: Vec<&str> = c_conv["explanation_sections"]
        .as_array()
        .expect("the entry names its explanation sections")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        explains.contains(&API_GUIDELINES_SECTION),
        "the `C-CONV` entry explains the `{API_GUIDELINES_SECTION}` section; got {explains:?}"
    );

    // ---- (5) get_section "api-guidelines/about": the section the conversion
    //         finding cites — its body, provenance, and citations. ------------
    let api_section = client
        .call_tool(
            "get_section",
            json!({ "section_id": API_GUIDELINES_SECTION }),
        )
        .await;
    let s = &api_section["result"]["structuredContent"]["section"];
    assert_section_identity(
        s,
        "api-guidelines",
        API_GUIDELINES_SECTION,
        API_GUIDELINES_HASH,
        API_GUIDELINES_URL,
    );
    assert!(
        s["body"]
            .as_str()
            .is_some_and(|body| body.contains("C-CONV")),
        "the section body is the guideline intro that names `C-CONV`"
    );
    assert_citations_carry_note(s, API_GUIDELINES_NOTE, "the cited section");

    // ---- (6) Clippy: the reliable search surfaces the seed's one Clippy
    //         section, with its nightly-component qualification citation. -----
    let clippy_search = client
        .call_tool("search_docs", json!({ "query": CLIPPY_QUERY }))
        .await;
    assert_eq!(
        clippy_search.get("error"),
        None,
        "the lint search must succeed: {clippy_search}"
    );
    let clippy_hits = clippy_search["result"]["structuredContent"]["hits"]
        .as_array()
        .expect("the lint search returns a `hits` array");
    let clippy_cited = clippy_hits
        .iter()
        .find(|hit| hit.get("section_id").and_then(Value::as_str) == Some(CLIPPY_SECTION))
        .expect("the lint search surfaces the `{CLIPPY_SECTION}` section");
    assert_eq!(
        clippy_cited.get("source").and_then(Value::as_str),
        Some("clippy"),
        "the cited section names its `clippy` source"
    );
    assert_citations_carry_note(clippy_cited, CLIPPY_QUAL_NOTE, "the lint-search hit");

    // ---- (7) get_section "clippy/configuration": the section the lint finding
    //         cites — its body, provenance, and the qualification citation. ----
    let clippy_section = client
        .call_tool("get_section", json!({ "section_id": CLIPPY_SECTION }))
        .await;
    let c = &clippy_section["result"]["structuredContent"]["section"];
    assert_section_identity(c, "clippy", CLIPPY_SECTION, CLIPPY_HASH, CLIPPY_URL);
    assert_citations_carry_note(c, CLIPPY_QUAL_NOTE, "the cited clippy section");

    // ---- (8) get_section "style-guide/intro": the section the style finding
    //         cites — its body, provenance, and the canonical citation. --------
    let style_section = client
        .call_tool("get_section", json!({ "section_id": STYLE_SECTION }))
        .await;
    let g = &style_section["result"]["structuredContent"]["section"];
    assert_section_identity(g, "style-guide", STYLE_SECTION, STYLE_HASH, STYLE_URL);
    assert_citations_carry_url(g, STYLE_URL, "the cited style section");

    // ---- (9) Coverage-limitation probe (AC2): a *specific* Clippy lint whose
    //         section the representative seed does not ship — a useful `0 hit`
    //         with `next_steps`, the recorded gap rather than a fabricated
    //         section. -------------------------------------------------------
    let missing = client
        .call_tool("search_docs", json!({ "query": MISSING_LINT_QUERY }))
        .await;
    coverage_limitation_is_recorded(&missing, MISSING_LINT_QUERY);

    // ---- (10) AC1: every finding's citation is the one the server returned.
    assert_findings_cite_retrieved_sections(
        &findings(),
        &api_section,
        &clippy_section,
        &style_section,
    );

    // ---- (11) AC2: the observed-vs-unverified split is asserted, not blurred.
    observed_vs_unverified_are_separated(&findings());

    // Persist the request/response transcript for the docs, when requested.
    client.maybe_write_transcript("RUST_DOCS_MCP_TRIAL_TRANSCRIPT");

    // ---- Clean disconnect: close our stdin; the server self-exits. ---------
    client.close_and_wait_clean().await;
}

/// Assert a section value's identity fields: the requested `id`, its `source`,
/// its captured `content_hash`, and its `canonical_url` (the provenance the
/// citation carries).
fn assert_section_identity(
    section: &Value,
    source: &str,
    id: &str,
    content_hash: &str,
    canonical_url: &str,
) {
    assert_eq!(
        section.get("id").and_then(Value::as_str),
        Some(id),
        "the section resolves to the requested id"
    );
    assert_eq!(
        section.get("source").and_then(Value::as_str),
        Some(source),
        "the section carries its source"
    );
    assert_eq!(
        section.get("content_hash").and_then(Value::as_str),
        Some(content_hash),
        "the section carries its captured content identity (provenance)"
    );
    assert_eq!(
        section.get("canonical_url").and_then(Value::as_str),
        Some(canonical_url),
        "the section carries its canonical upstream URL (provenance)"
    );
    assert_eq!(
        section.get("snapshot_id").and_then(Value::as_str),
        Some(SNAPSHOT_ID),
        "the section names the active snapshot"
    );
    // The seed's freshness verdict is `unknown` (the representative seed has
    // never checked upstream) — recorded, not fabricated.
    assert_eq!(
        section.get("freshness").and_then(Value::as_str),
        Some("unknown"),
        "the seed section's freshness is `unknown` (AC2: an unverified as_of)"
    );
}

/// Assert a section/hit value's `citations` array carries a citation with the
/// given `note` — the provenance the trial cites.
fn assert_citations_carry_note(value: &Value, note: &str, ctx: &str) {
    let citations = value
        .get("citations")
        .and_then(Value::as_array)
        .expect("{ctx} carries a citations array: {value}");
    let found = citations
        .iter()
        .filter_map(|citation| citation.get("note").and_then(Value::as_str))
        .any(|got| got == note);
    assert!(
        found,
        "{ctx} must carry a citation with note `{note}`; got {citations:?}"
    );
}

/// Assert a section value's `citations` array carries the canonical upstream
/// URL — the provenance citation the trial records.
fn assert_citations_carry_url(value: &Value, url: &str, ctx: &str) {
    let citations = value
        .get("citations")
        .and_then(Value::as_array)
        .expect("{ctx} carries a citations array: {value}");
    let found = citations
        .iter()
        .filter_map(|citation| citation.get("url").and_then(Value::as_str))
        .any(|got| got == url);
    assert!(
        found,
        "{ctx} must carry a citation with the canonical URL {url}; got {citations:?}"
    );
}

/// AC1 — every finding's recorded citation (source, `section_id`, `canonical_url`,
/// `content_hash`, and the guideline/lint/qualification note) is exactly the one
/// the server returned for that section. This is the "findings identify concrete
/// code locations **and** relevant source citations" criterion: the location is
/// the agent's artifact; the citation is the server's, asserted verbatim.
fn assert_findings_cite_retrieved_sections(
    findings: &[Finding],
    api_section: &Value,
    clippy_section: &Value,
    style_section: &Value,
) {
    for finding in findings {
        let fid = finding.id;
        let section = match finding.section_id {
            API_GUIDELINES_SECTION => &api_section["result"]["structuredContent"]["section"],
            CLIPPY_SECTION => &clippy_section["result"]["structuredContent"]["section"],
            STYLE_SECTION => &style_section["result"]["structuredContent"]["section"],
            other => panic!("finding {fid} cites an unknown section id: {other}"),
        };
        assert_eq!(
            section.get("source").and_then(Value::as_str),
            Some(finding.source),
            "finding {fid}'s source matches the retrieved section"
        );
        assert_eq!(
            section.get("id").and_then(Value::as_str),
            Some(finding.section_id),
            "finding {fid}'s section id matches the retrieved section"
        );
        assert_eq!(
            section.get("canonical_url").and_then(Value::as_str),
            Some(finding.canonical_url),
            "finding {fid}'s canonical url matches the retrieved section"
        );
        assert_eq!(
            section.get("content_hash").and_then(Value::as_str),
            Some(finding.content_hash),
            "finding {fid}'s content hash matches the retrieved section"
        );
        assert_citations_carry_note(section, finding.note, &format!("finding {fid}"));
        // The finding carries a concrete code location (a `file:line` the agent
        // points at, plus the snippet it reviewed) — AC1's "concrete code
        // location" half.
        assert!(
            finding.location.contains(':') && !finding.snippet.is_empty(),
            "finding {fid} must carry a concrete `file:line` location and a snippet"
        );
    }
}

/// AC2 — the trial's headline separation, recorded rather than blurred:
///
/// * **Observed** is the *retrieval* — every finding's citation is the section
///   the server actually returned (asserted above), plus the `0 hit` for the
///   specific lint the seed does not ship.
/// * **Unverified** is each finding's *check outcome* — that the snippet actually
///   violates `C-CONV`, triggers the Clippy lint, or breaks the style
///   convention. The server does not run Clippy or a build, so no check outcome
///   is observed; the trial records the finding as "retrieved, not checked".
///
/// This asserts the trial did **not** silently claim a check result or auto-
/// verification it did not perform.
fn observed_vs_unverified_are_separated(findings: &[Finding]) {
    assert!(
        !findings.is_empty(),
        "the trial records at least one finding"
    );
    for finding in findings {
        // Every finding's status is the honest "retrieved, not checked" verdict —
        // the trial never records a finding as an observed check result.
        let fid = finding.id;
        assert_eq!(
            finding.status,
            CheckStatus::RetrievedNotChecked,
            "finding {fid}'s check outcome is an unverified assumption, not an observed result"
        );
    }
}

/// AC2 — the coverage-limitation probe: a *specific* Clippy lint (`needless_
/// return`) whose section the representative seed does not ship returns a
/// **useful empty** — `0 hits` with `next_steps` — not a fabricated lint
/// section. This is the "a lint whose section is not in the representative seed"
/// gap the trial records.
fn coverage_limitation_is_recorded(response: &Value, query: &str) {
    assert_eq!(
        response.get("error"),
        None,
        "a search for an absent lint is a useful empty, not an error: {response}"
    );
    let sc = &response["result"]["structuredContent"];
    let hits = sc
        .get("hits")
        .and_then(Value::as_array)
        .expect("a search returns a `hits` array");
    assert!(
        hits.is_empty(),
        "the specific lint `{query}` has no section in the representative seed (recorded gap)"
    );
    // The useful empty still offers discoverable next steps.
    let next_steps = sc
        .get("next_steps")
        .and_then(Value::as_array)
        .expect("a useful empty carries `next_steps`");
    assert!(
        !next_steps.is_empty(),
        "a useful empty offers `next_steps` to the agent"
    );
    // It still names the active snapshot (so the agent knows the gap is a
    // corpus-coverage limit of *this* snapshot, not a broken tool).
    assert_eq!(
        sc.get("snapshot_id").and_then(Value::as_str),
        Some(SNAPSHOT_ID),
        "the useful empty names the active snapshot"
    );
}

/// AC2 — the server's own `instructions` state the retrieval-versus-execution
/// boundary the trial must not over-claim: the MCP *retrieves* documentation and
/// *provides evidence, not a guarantee* that guidance was followed. Asserting the
/// exact phrasing pins the boundary the trial respects — it retrieves and cites,
/// it does not execute or certify a check result.
fn the_server_states_it_provides_evidence_not_a_guarantee(instructions: &str) {
    assert!(
        instructions.contains("retrieves official Rust documentation only"),
        "the server states it retrieves documentation only: {instructions}"
    );
    assert!(
        instructions.contains("does not execute commands"),
        "the server states it does not execute commands: {instructions}"
    );
    assert!(
        instructions.to_lowercase().contains("not a guarantee"),
        "the server states it provides evidence, not a guarantee: {instructions}"
    );
}
