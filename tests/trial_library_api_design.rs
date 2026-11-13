//! End-to-end agent trial for task `[068][M5]` — *"Run the library API-design
//! agent trial"*.
//!
//! # What this trial is
//!
//! This is the plan's §1 *success scenario* exercised as a test:
//!
//! > An agent designing a library searches for conversion naming, reads the
//! > relevant API guideline with its examples, and cites the original section.
//!
//! The test **is** that agent's retrieval session, driven through the same
//! spawned-stdio JSON-RPC 2.0 seam as [`tests/first_client.rs`] and
//! [`tests/stdio_mcp.rs`]: the real `rust-docs-mcp serve` binary over genuine
//! OS stdio, over the **embedded bundled seed** (`rust-docs-bundled-seed`),
//! **offline** (an empty `--data-dir`; no network, no host cache, no new
//! dependency — `tokio` + `serde_json` only). It records the *actual* retrieved
//! evidence and canonical citations the plan's §10/M5 trial asks for, and asserts
//! them so the trial is **reproducible, not hand-claimed**.
//!
//! The human-readable transcript — every request and its real response — is
//! recorded in [`docs/trials/library-api-design.md`](../docs/trials/library-api-design.md).
//!
//! # The session (the trial's calls, in order)
//!
//! 1. `initialize` → negotiate `2025-11-25`, advertise the `tools` + `resources`
//!    capabilities, and carry the agent-workflow `instructions`.
//! 2. `notifications/initialized` → complete the handshake.
//! 3. `tools/list` → the agent discovers the five tools, including
//!    `get_api_checklist`.
//! 4. `search_docs "From TryFrom conversion naming"` → the agent's
//!    *natural-language* attempt at the conversion-naming question. On the seed
//!    corpus this is a **low-recall miss** (it matches the style guide's
//!    "naming" mention, not the conversion guideline); the trial records that and
//!    asserts the reliable path below.
//! 5. `search_docs "C-CONV"` → the *exact-guideline-code* search, the reliable
//!    path: it surfaces `api-guidelines/about`, the section that carries the
//!    `C-CONV` guideline code.
//! 6. `get_api_checklist { category: "Conversions and From" }` → the checklist
//!    entry for the conversion guideline: `C-CONV` "Prefer `From`/`Into` for
//!    conversions", explaining `api-guidelines/about`.
//! 7. `get_api_checklist { }` (all categories) → the seed's single entry, proving
//!    the checklist walk is whole.
//! 8. `get_section "api-guidelines/about"` → the original section the agent
//!    *cites*: its body, `canonical_url`, `content_hash`, `source`, and
//!    citations (including the `guideline code: C-CONV` note).
//! 9. Clean client disconnect → the server self-exits.
//!
//! # What the trial does *not* claim (AC2)
//!
//! The trial retrieves documentation and records its citations. It does **not**
//! claim that any code generated on the strength of the retrieved guideline is
//! *automatically compliant*. The seed ships only the `api-guidelines` *intro*
//! (one representative section per source), which *names* `C-CONV` but does not
//! expand the `From`/`Into`/`TryFrom` rule; and the retrieved section's
//! `freshness` is `unknown` (the seed has never checked upstream). The server's
//! own `instructions` state the boundary verbatim: *"The MCP provides evidence,
//! not a guarantee that all guidance has been followed."* The agent interprets
//! and applies the guidance itself; the server retrieves, it does not certify.
//! These limits are asserted in `retrieval_defects_are_recorded` and
//! `the_server_states_it_provides_evidence_not_a_guarantee`.

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
/// guideline's section (the reliable path; see the module docs for why the
/// natural-language query misses on the seed).
const CODE_QUERY: &str = "C-CONV";

/// The agent's natural-language attempt at the conversion-naming question.
const NATURAL_QUERY: &str = "From TryFrom conversion naming";

/// The checklist category the conversion-naming guideline sits in.
const CONVERSION_CATEGORY: &str = "Conversions and From";

/// The section the trial cites: the `api-guidelines` intro that carries the
/// `C-CONV` guideline code.
const SECTION_ID: &str = "api-guidelines/about";

/// The section's captured content identity (provenance the citation carries).
const SECTION_CONTENT_HASH: &str = "c128-ffa531f24bfdd996c9ef3d1a076dd56a";

/// The section's canonical upstream URL (the citation the agent records).
const SECTION_CANONICAL_URL: &str = "https://rust-lang.github.io/api-guidelines/about.html";

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
        "rust-docs-mcp-library-trial-{}-{}",
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

        client.initialize().await;
        client.notify("notifications/initialized").await;
        client
    }

    /// Send the `initialize` request and assert the server echoes the negotiated
    /// protocol version and its implementation identity. Returns the response so
    /// the caller can inspect the advertised `instructions`.
    async fn initialize(&mut self) -> Value {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "library-design-agent", "version": "0.0.1" },
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
    /// set. That is how the [`docs/trials/library-api-design.md`] transcript is
    /// regenerated:
    ///
    /// ```sh
    /// RUST_DOCS_MCP_TRIAL_TRANSCRIPT=/tmp/library_trial.json \
    ///   cargo test --test trial_library_api_design -- --exact \
    ///     the_library_api_design_trial_retrieves_and_cites_the_conversion_guideline
    /// jq . /tmp/library_trial.json
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
            "task": "[068][M5] library API-design agent trial",
            "client": "raw JSON-RPC 2.0 over stdio (tests/trial_library_api_design.rs)",
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
// The trial.
// ---------------------------------------------------------------------------

/// The full library API-design trial: the plan's §1 success scenario, driven
/// through the spawned-stdio seam over the bundled seed. It asserts the
/// retrieval produces the expected evidence and canonical citations, records the
/// retrieval defects (AC2), and proves a clean disconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_library_api_design_trial_retrieves_and_cites_the_conversion_guideline() {
    let mut client = Client::new().await;

    // ---- (1) initialize: the negotiated version, the capabilities, and the
    //         agent-workflow `instructions` the trial runs on. ----------------
    let init = client.initialize().await;
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
        tool_names.contains(&"get_api_checklist"),
        "the trial depends on the `get_api_checklist` tool being advertised"
    );

    // ---- (3) search_docs: the agent's natural-language conversion-naming
    //         attempt. On the seed it is a low-recall miss (recorded in AC2);
    //         the assertion is that retrieval *works* (a cited hit returns). ---
    let natural = client
        .call_tool("search_docs", json!({ "query": NATURAL_QUERY }))
        .await;
    assert_eq!(
        natural.get("error"),
        None,
        "a valid search must succeed: {natural}"
    );
    let natural_hits = natural["result"]["structuredContent"]["hits"]
        .as_array()
        .expect("a valid search returns a `hits` array");
    assert!(
        !natural_hits.is_empty(),
        "the natural-language query must return at least one hit on the seed corpus"
    );
    for hit in natural_hits {
        assert!(
            hit.get("section_id").and_then(Value::as_str).is_some(),
            "every hit names a section id: {hit}"
        );
        let citations = hit
            .get("citations")
            .and_then(Value::as_array)
            .expect("every hit carries a citations array: {hit}");
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

    // ---- (4) search_docs "C-CONV": the reliable exact-code path surfaces the
    //         conversion guideline's section, with its `C-CONV` citation. ------
    let code = client
        .call_tool("search_docs", json!({ "query": CODE_QUERY }))
        .await;
    assert_eq!(
        code.get("error"),
        None,
        "the code search must succeed: {code}"
    );
    let code_hits = code["result"]["structuredContent"]["hits"]
        .as_array()
        .expect("the code search returns a `hits` array");
    assert!(
        !code_hits.is_empty(),
        "the exact guideline code `{CODE_QUERY}` matches a section on the seed corpus"
    );
    // The conversion guideline's section must be among the hits, and its
    // citations must carry the `C-CONV` guideline code (the provenance the agent
    // cites).
    let cited = code_hits
        .iter()
        .find(|hit| hit.get("section_id").and_then(Value::as_str) == Some(SECTION_ID))
        .expect("the exact-code search surfaces the `{SECTION_ID}` section");
    assert_eq!(
        cited.get("source").and_then(Value::as_str),
        Some("api-guidelines"),
        "the cited section names its source"
    );
    assert_citations_carry_code(cited, "C-CONV", "the code-search hit");

    // ---- (5) get_api_checklist "Conversions and From": the checklist entry
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
    // The entry's explanation section is the section the trial then reads.
    let explains: Vec<&str> = c_conv["explanation_sections"]
        .as_array()
        .expect("the entry names its explanation sections")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        explains.contains(&SECTION_ID),
        "the `C-CONV` entry explains the `{SECTION_ID}` section; got {explains:?}"
    );

    // ---- (6) get_api_checklist {} (all): the seed's checklist walk is whole —
    //         it is exactly this one `C-CONV` entry on the representative seed.
    let all = client.call_tool("get_api_checklist", json!({})).await;
    assert_eq!(
        all.get("error"),
        None,
        "the all-categories read must succeed: {all}"
    );
    let all_entries = all["result"]["structuredContent"]["entries"]
        .as_array()
        .expect("the all-categories read returns an `entries` array");
    assert_eq!(
        all_entries
            .iter()
            .filter(|entry| entry.get("code").and_then(Value::as_str) == Some("C-CONV"))
            .count(),
        1,
        "the all-categories walk reaches the `C-CONV` entry exactly once"
    );

    // ---- (7) get_section "api-guidelines/about": the original section the
    //         agent cites — its body, provenance, and citations. --------------
    let section = client
        .call_tool("get_section", json!({ "section_id": SECTION_ID }))
        .await;
    assert_eq!(
        section.get("error"),
        None,
        "the section must resolve: {section}"
    );
    let s = &section["result"]["structuredContent"]["section"];
    assert_eq!(
        s["id"], SECTION_ID,
        "the section resolves to the requested id"
    );
    assert_eq!(
        s["source"], "api-guidelines",
        "the section carries its source"
    );
    assert!(
        s["body"]
            .as_str()
            .is_some_and(|body| body.contains("C-CONV")),
        "the section body is the guideline intro that names `C-CONV`"
    );
    // Canonical citations (AC1): source, section_id, canonical_url, as_of
    // (freshness), content_hash.
    assert_eq!(
        s.get("canonical_url").and_then(Value::as_str),
        Some(SECTION_CANONICAL_URL),
        "the section carries its canonical upstream URL (provenance)"
    );
    assert_eq!(
        s.get("content_hash").and_then(Value::as_str),
        Some(SECTION_CONTENT_HASH),
        "the section carries its captured content identity (provenance)"
    );
    assert_eq!(
        s.get("snapshot_id").and_then(Value::as_str),
        Some(SNAPSHOT_ID),
        "the section names the active snapshot"
    );
    assert_eq!(
        section["result"]["structuredContent"]["schema_version"], "1",
        "the section carries the snapshot's schema version"
    );
    assert_eq!(
        section["result"]["structuredContent"]["truncated"], false,
        "a seed section is not truncated"
    );
    // The seed's freshness verdict is `unknown` (the representative seed has
    // never checked upstream) — recorded, not fabricated.
    assert_eq!(
        s.get("freshness").and_then(Value::as_str),
        Some("unknown"),
        "the seed section's freshness is `unknown` (AC2: an unverified as_of)"
    );
    // The section's citations carry the canonical document and the `C-CONV`
    // guideline-code note.
    assert_citations_carry_code(s, "C-CONV", "the cited section");
    assert_citations_carry_url(s, SECTION_CANONICAL_URL, "the cited section");

    // Record the retrieval defects explicitly (AC2): the natural-language query
    // did not surface the conversion guideline, and the cited section names but
    // does not expand the `C-CONV` rule.
    retrieval_defects_are_recorded(natural_hits, &s["body"]);

    // Persist the request/response transcript for the docs, when requested.
    client.maybe_write_transcript("RUST_DOCS_MCP_TRIAL_TRANSCRIPT");

    // ---- Clean disconnect: close our stdin; the server self-exits. ---------
    client.close_and_wait_clean().await;
}

/// Assert a section/hit value's `citations` array carries a `guideline code:
/// C-CONV` note citation — the provenance the trial cites.
fn assert_citations_carry_code(value: &Value, code: &str, ctx: &str) {
    let expected = format!("guideline code: {code}");
    let citations = value
        .get("citations")
        .and_then(Value::as_array)
        .expect("{ctx} carries a citations array: {value}");
    let found = citations
        .iter()
        .filter_map(|citation| citation.get("note").and_then(Value::as_str))
        .any(|note| note == expected);
    assert!(
        found,
        "{ctx} must carry a `guideline code: {code}` citation; got {citations:?}"
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

/// AC2 — record the retrieval defects the trial observes, rather than over-claim:
///
/// * the agent's *natural-language* conversion-naming query does **not** surface
///   the `api-guidelines` conversion guideline's section — on the representative
///   seed it matches the style guide's "naming" mention instead (a low-recall
///   miss). The reliable path is the exact-code search / the checklist, asserted
///   above.
/// * the cited section *names* `C-CONV` but does not *expand* the
///   `From`/`Into`/`TryFrom` rule (the seed ships only the `api-guidelines`
///   intro, one representative section per source); the full rule lives upstream.
///
/// This asserts the trial did **not** silently claim complete recall or a
/// complete rule — the gap is real and documented, not fabricated away.
fn retrieval_defects_are_recorded(natural_hits: &[Value], cited_body: &Value) {
    // The natural-language query must not have surfaced the conversion
    // guideline's section: the seed has no section whose text contains the
    // conversion-naming rule, so the reliable path is the checklist/exact-code,
    // not free-text search.
    let natural_surfaced_guideline = natural_hits
        .iter()
        .any(|hit| hit.get("section_id").and_then(Value::as_str) == Some(SECTION_ID));
    assert!(
        !natural_surfaced_guideline,
        "the natural-language query unexpectedly surfaced `{SECTION_ID}` — the \
         retrieval defect the trial records would have closed; re-check the seed corpus"
    );
    // The cited section names `C-CONV` but does not expand the rule: it carries
    // the code as an example, not a `From`/`Into`/`TryFrom` naming rule or
    // example. This is the "names but does not expand" gap the trial records.
    let body = cited_body
        .as_str()
        .expect("the cited section has a body string");
    assert!(
        body.contains("C-CONV"),
        "the cited section names the `C-CONV` code"
    );
    // The rule text (a `From`/`Into`/`TryFrom` naming convention) is absent from
    // the seed section — the detailed guidance is upstream, not in the seed.
    let expands_rule = ["tryfrom", "prefer `from`", "implement `from`", "use `into`"]
        .iter()
        .any(|needle| body.to_lowercase().contains(needle));
    assert!(
        !expands_rule,
        "the cited seed section unexpectedly expanded the conversion rule — the \
         documented gap (names but does not expand) would have closed"
    );
}

/// AC2 — the server's own `instructions` state the retrieval-versus-execution
/// boundary the trial must not over-claim: the MCP *retrieves* documentation and
/// *provides evidence, not a guarantee* that guidance was followed. Asserting the
/// exact phrasing pins the boundary the trial respects — it retrieves and cites,
/// it does not certify that any generated code is automatically compliant.
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
