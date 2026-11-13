//! End-to-end agent trial for task `[069][M5]` — *"Run the project-implementation
//! agent trial"*.
//!
//! # What this trial is
//!
//! This is the plan's §1 *success scenario* (its implementation/review half)
//! exercised as a test:
//!
//! > When implementing or reviewing the project, it retrieves documentation for
//! > the applicable build, formatting, lint, test, and documentation commands,
//! > then executes them through its own terminal tools.
//!
//! The test **is** that agent's retrieval session, driven through the same
//! spawned-stdio JSON-RPC 2.0 seam as [`tests/first_client.rs`],
//! [`tests/stdio_mcp.rs`], and [`tests/trial_library_api_design.rs`]: the real
//! `rust-docs-mcp serve` binary over genuine OS stdio, over the **embedded
//! bundled seed** (`rust-docs-bundled-seed`), **offline** (an empty `--data-dir`;
//! no network, no host cache, no new dependency — `tokio` + `serde_json` only).
//! It records the *actual* retrieved command guidance, citations, compatibility
//! qualifications, and effect notes the plan's §10/M5 trial asks for, and asserts
//! them so the trial is **reproducible, not hand-claimed**.
//!
//! The human-readable transcript — every request and its real response — is
//! recorded in [`docs/trials/project-implementation.md`]
//! (../docs/trials/project-implementation.md).
//!
//! # The session (the trial's calls, in order)
//!
//! 1. `initialize` → negotiate `2025-11-25`, advertise the `tools` + `resources`
//!    capabilities, and carry the agent-workflow `instructions`.
//! 2. `notifications/initialized` → complete the handshake.
//! 3. `tools/list` → the agent discovers the five tools, including
//!    `get_command_guidance`.
//! 4. `get_command_guidance "cargo build"` → the **one** seed command whose
//!    full-catalog citation resolves in the representative seed: its catalog
//!    entry (purpose, syntax, examples, prerequisites, compatibility
//!    qualifications, effect notes) plus its cited section, canonical URL,
//!    content hash, and citations. This is the guidance the agent would run via
//!    *its own* terminal.
//! 5. `get_command_guidance "cargo test"` → the seed *advertises* `cargo test`
//!    in `list_sources`, but over the full catalog its citation
//!    (`cargo/commands%2Fcargo-test`) is **not in the representative seed**, so
//!    the server returns a typed `UnresolvedSection` error — the guidance is
//!    **not fabricated**. This is a *skipped/unsupported configuration* the
//!    trial records (AC2), not a failure to over-claim.
//! 6. `get_command_guidance "rustup show"` → the same, for `rustup show`
//!    (cites `rustup/index`, absent from the seed).
//! 7. `get_command_guidance "cargo foobar"` → a command **not in the catalog**:
//!    a typed `UnknownKey` error naming the full set of supported keys — the
//!    catalog-boundary probe that also proves the key is a *selector*, never a
//!    shell line.
//! 8. Clean client disconnect → the server self-exits.
//!
//! # What the trial does *not* claim (AC2)
//!
//! The trial *retrieves* command documentation and *records its citations,
//! compatibility qualifications, and effect notes*. It does **not** run any of
//! the commands: the server "retrieves official Rust documentation only: it does
//! not run commands, does not touch the project filesystem, and does not reach
//! the network", and it "does not execute commands or observe their results
//! automatically". The agent executes `cargo build`/`cargo test`/`rustup show`
//! through *its own* terminal tools *outside this trial*; the trial records the
//! guidance the agent would run and the *exit status* of the retrieval itself
//! (a JSON-RPC `result` for `cargo build`, a typed `error` for the unresolved and
//! unknown keys), never the exit status of a command the server ran. The server's
//! own `instructions` state the boundary verbatim: *"The MCP provides evidence,
//! not a guarantee that all guidance has been followed."* The trial asserts that
//! phrasing and the unresolved/unknown findings it observes; it does not over-claim
//! that every advertised command resolves on the representative seed.

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

/// The one seed command whose full-catalog citation *does* resolve in the
/// representative seed (the trial's success path).
const CARGO_BUILD: &str = "cargo build";

/// The section `cargo build`'s full-catalog entry cites, and that the seed
/// carries (so the guidance resolves).
const CARGO_BUILD_SECTION: &str = "cargo/commands%2Fcargo-build";

/// The captured content identity of the `cargo build` section the trial cites.
const CARGO_BUILD_SECTION_HASH: &str = "c128-ffa550364dc212527d904d59fe85fe84";

/// The canonical upstream URL of the `cargo build` section (the citation the
/// agent records).
const CARGO_BUILD_SECTION_URL: &str = "https://doc.rust-lang.org/cargo/commands/cargo-build.html";

/// The two seed-advertised commands whose full-catalog citations are *not* in the
/// representative seed — the trial's unresolved "skipped/unsupported"
/// configurations (AC2).
const CARGO_TEST: &str = "cargo test";
const CARGO_TEST_SECTION: &str = "cargo/commands%2Fcargo-test";
const RUSTUP_SHOW: &str = "rustup show";
const RUSTUP_SHOW_SECTION: &str = "rustup/index";

/// A command key **not** in the catalog at all — the catalog-boundary probe that
/// proves the key is a selector, never a shell line (AC2).
const UNKNOWN_COMMAND: &str = "cargo foobar";

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
        "rust-docs-mcp-project-trial-{}-{}",
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
    /// documents).
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
            "clientInfo": { "name": "project-implementation-agent", "version": "0.0.1" },
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
    /// response **without** asserting success — the trial needs to assert on the
    /// error responses for the unresolved and unknown-key probes too. The caller
    /// inspects `error` / `result` itself.
    async fn call_tool_raw(&mut self, name: &str, arguments: Value) -> Value {
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
    /// matches the request — a `result`-bearing success or an `error`-bearing
    /// failure, matched by `id` either way.
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
    /// set. That is how the [`docs/trials/project-implementation.md`] transcript
    /// is regenerated:
    ///
    /// ```sh
    /// RUST_DOCS_MCP_TRIAL_TRANSCRIPT=/tmp/project_trial.json \
    ///   cargo test --test trial_project_implementation -- --exact \
    ///     the_project_implementation_trial_retrieves_command_guidance_and_reports_skipped_configs
    /// jq . /tmp/project_trial.json
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
            "task": "[069][M5] project-implementation agent trial",
            "client": "raw JSON-RPC 2.0 over stdio (tests/trial_project_implementation.rs)",
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

/// The full project-implementation trial: the plan's §1 implementation/review
/// half, driven through the spawned-stdio seam over the bundled seed. It asserts
/// the one command whose guidance resolves (`cargo build`), records the two
/// advertised-but-unresolved commands (`cargo test`, `rustup show`) and the
/// not-in-catalog probe (`cargo foobar`) the trial observes — all honestly, per
/// AC2 — and proves a clean disconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_project_implementation_trial_retrieves_command_guidance_and_reports_skipped_configs() {
    let (mut client, init) = Client::new().await;

    // ---- (1) initialize: the negotiated version, the capabilities, and the
    //         agent-workflow `instructions` the trial runs on. `Client::new`
    //         completes the handshake exactly once and returns that single
    //         `initialize` response, so these assertions run on exactly the
    //         handshake the transcript records (no redundant second exchange).
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
        tool_names.contains(&"get_command_guidance"),
        "the trial depends on the `get_command_guidance` tool being advertised"
    );

    // ---- (3) get_command_guidance "cargo build": the one seed command whose
    //         full-catalog citation resolves in the representative seed. This is
    //         the guidance the agent would run through its *own* terminal. ------
    let build = client
        .call_tool_raw("get_command_guidance", json!({ "command": CARGO_BUILD }))
        .await;
    assert_eq!(
        build.get("error"),
        None,
        "`cargo build` must resolve on the seed: {build}"
    );
    assert_eq!(
        build["result"]["structuredContent"]["snapshot_id"], SNAPSHOT_ID,
        "the guidance names the active snapshot"
    );
    assert_eq!(
        build["result"]["structuredContent"]["limit"], 5,
        "the default limit (five) is echoed"
    );
    assert_eq!(
        build["result"]["structuredContent"]["truncated"], false,
        "the one cited section fits on one page"
    );

    // The command entry: its documented metadata + the separately-labeled effect
    // notes + the compatibility qualifications the agent would read before
    // running the command.
    let entry = &build["result"]["structuredContent"]["entry"];
    assert_eq!(
        entry.get("key").and_then(Value::as_str),
        Some(CARGO_BUILD),
        "the entry round-trips its command key"
    );
    assert!(
        !entry["purpose"].as_str().unwrap().is_empty(),
        "the entry carries a documented purpose"
    );
    assert!(
        !entry["syntax"].as_array().unwrap().is_empty(),
        "the entry carries documented syntax"
    );
    assert!(
        !entry["examples"].as_array().unwrap().is_empty(),
        "the entry carries documented examples (never executed)"
    );
    assert!(
        !entry["prerequisites"].as_array().unwrap().is_empty(),
        "the entry carries documented prerequisites"
    );
    // Compatibility qualifications (AC1): the version/stability qualifications the
    // agent must read before trusting a version-specific claim.
    let compat = entry["compatibility"].as_array().unwrap();
    assert!(
        !compat.is_empty(),
        "the entry carries compatibility qualifications"
    );
    let compat_text = compat
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        compat_text.contains("--help"),
        "the guidance tells the agent to consult `--help` for the version-specific option list"
    );
    // Effect notes (AC2): the maintainer-authored side-effect notes are labeled
    // separately from the upstream content, in snake-case kinds.
    let effect_notes = entry["effect_notes"].as_array().unwrap();
    assert!(
        !effect_notes.is_empty(),
        "the entry carries separately-labeled effect notes"
    );
    let effect_kinds: Vec<&str> = effect_notes
        .iter()
        .filter_map(|note| note.get("kind").and_then(Value::as_str))
        .collect();
    assert!(
        effect_kinds.contains(&"artifact_write") && effect_kinds.contains(&"dependency_download"),
        "the effect notes name the artifact-write and dependency-download families; got {effect_kinds:?}"
    );
    for note in effect_notes {
        let kind = note["kind"].as_str().unwrap();
        assert!(
            kind.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "each effect-note kind is snake-case: {kind}"
        );
        assert!(
            !note["detail"].as_str().unwrap().is_empty(),
            "each effect note carries a human detail"
        );
    }

    // The cited section: its provenance the agent records, plus the command-key
    // and qualification citations.
    let sections = &build["result"]["structuredContent"]["sections"];
    assert_eq!(
        sections.as_array().unwrap().len(),
        1,
        "`cargo build` cites exactly one section on the seed"
    );
    let section = &sections[0];
    assert_eq!(
        section.get("id").and_then(Value::as_str),
        Some(CARGO_BUILD_SECTION),
        "the cited section is the Cargo Book `cargo build` chapter"
    );
    assert_eq!(
        section.get("source").and_then(Value::as_str),
        Some("cargo"),
        "the cited section names its source"
    );
    assert_eq!(
        section.get("content_hash").and_then(Value::as_str),
        Some(CARGO_BUILD_SECTION_HASH),
        "the cited section carries its captured content identity (provenance)"
    );
    assert_eq!(
        section.get("canonical_url").and_then(Value::as_str),
        Some(CARGO_BUILD_SECTION_URL),
        "the cited section carries its canonical upstream URL (provenance)"
    );
    assert_eq!(
        section.get("snapshot_id").and_then(Value::as_str),
        Some(SNAPSHOT_ID),
        "the cited section names the active snapshot"
    );
    // The seed's freshness verdict is `unknown` (the representative seed has never
    // checked upstream) — recorded, not fabricated.
    assert_eq!(
        section.get("freshness").and_then(Value::as_str),
        Some("unknown"),
        "the seed section's freshness is `unknown` (AC2: an unverified as_of)"
    );
    // The section's citations carry the canonical document, the `cargo build`
    // command key, and a qualification.
    let section_citations = section
        .get("citations")
        .and_then(Value::as_array)
        .expect("the cited section carries a citations array");
    assert!(
        !section_citations.is_empty(),
        "the cited section carries at least one citation"
    );
    let notes = section_citations
        .iter()
        .filter_map(|c| c.get("note").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        notes.contains(&"canonical upstream document"),
        "the citations carry the canonical upstream document; got {notes:?}"
    );
    assert!(
        notes.contains(&"command key: cargo build"),
        "the citations carry the `cargo build` command-key note; got {notes:?}"
    );
    assert!(
        notes.iter().any(|n| n.starts_with("qualification:")),
        "the citations carry a version qualification note; got {notes:?}"
    );

    // ---- (4) get_command_guidance "cargo test": the seed *advertises* this
    //         command in `list_sources`, but its full-catalog citation is not in
    //         the representative seed — a typed UnresolvedSection error, not a
    //         fabricated section. A *skipped/unsupported configuration* (AC2). --
    let test = client
        .call_tool_raw("get_command_guidance", json!({ "command": CARGO_TEST }))
        .await;
    assert_error_names_unresolved_section(&test, CARGO_TEST, CARGO_TEST_SECTION);

    // ---- (5) get_command_guidance "rustup show": the same, for the toolchain
    //         inspection command (its citation is absent from the seed too). ---
    let rustup = client
        .call_tool_raw("get_command_guidance", json!({ "command": RUSTUP_SHOW }))
        .await;
    assert_error_names_unresolved_section(&rustup, RUSTUP_SHOW, RUSTUP_SHOW_SECTION);

    // ---- (6) get_command_guidance "cargo foobar": a command not in the catalog
    //         at all — a typed UnknownKey error naming the supported keys. This
    //         is the catalog-boundary probe: the key is a *selector*, never a
    //         shell line (AC2). ----------------------------------------------
    let unknown = client
        .call_tool_raw(
            "get_command_guidance",
            json!({ "command": UNKNOWN_COMMAND }),
        )
        .await;
    assert_error_names_unknown_key(&unknown, UNKNOWN_COMMAND);

    // The trial's headline finding (AC2): of the three seed-advertised commands,
    // exactly one resolves; the other two are honestly reported as unresolved.
    skipped_configs_are_recorded(&build, &test, &rustup, &unknown);

    // Persist the request/response transcript for the docs, when requested.
    client.maybe_write_transcript("RUST_DOCS_MCP_TRIAL_TRANSCRIPT");

    // ---- Clean disconnect: close our stdin; the server self-exits. ---------
    client.close_and_wait_clean().await;
}

/// Assert a `get_command_guidance` response for `key` is a typed
/// `UnresolvedSection` error (JSON-RPC `-32603`, `INTERNAL_ERROR`) naming the
/// exact missing section — the honest, non-fabricated "skipped configuration" the
/// trial records for a seed-advertised command whose full-catalog citation the
/// representative seed does not carry.
fn assert_error_names_unresolved_section(response: &Value, key: &str, section: &str) {
    let error = response
        .get("error")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("`{key}` must return a typed error, not a result: {response}"));
    // JSON-RPC `-32603` is the `INTERNAL_ERROR` the [053] seam maps
    // `UnresolvedSection` to.
    assert_eq!(
        error.get("code").and_then(Value::as_i64),
        Some(-32603),
        "an unresolved citation maps to JSON-RPC INTERNAL_ERROR (-32603); got {error:?}"
    );
    let data = error
        .get("data")
        .and_then(Value::as_object)
        .unwrap_or_else(|| {
            panic!("the unresolved-section error names the offending citation: {error:?}")
        });
    assert_eq!(
        data.get("key").and_then(Value::as_str),
        Some(key),
        "the error names the refused command key"
    );
    assert_eq!(
        data.get("section").and_then(Value::as_str),
        Some(section),
        "the error names the exact missing section (no section is fabricated)"
    );
    // The message is actionable and leak-free: it names the section and the fix
    // (refresh the snapshot), and no internal path or backtrace leaks.
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains(section),
        "the message names the missing section: {message}"
    );
    for forbidden in ["/src/", "at 0x", "backtrace", "src/mcp"] {
        assert!(
            !message.contains(forbidden),
            "the error leaks no internal path or backtrace ({forbidden}): {message}"
        );
    }
}

/// Assert a `get_command_guidance` response for `command` is a typed `UnknownKey`
/// error (JSON-RPC `-32002`, `RESOURCE_NOT_FOUND`) naming the request *and* the
/// catalog's full set of supported keys — the catalog-boundary probe that proves
/// the key is a documentation selector, never a shell line (AC2).
fn assert_error_names_unknown_key(response: &Value, command: &str) {
    let error = response
        .get("error")
        .and_then(Value::as_object)
        .unwrap_or_else(|| {
            panic!("`{command}` must return a typed error, not a result: {response}")
        });
    // JSON-RPC `-32002` is the `RESOURCE_NOT_FOUND` the [053] seam maps
    // `UnknownKey` to for a pre-`2026-07-28` peer.
    assert_eq!(
        error.get("code").and_then(Value::as_i64),
        Some(-32002),
        "an unknown key maps to JSON-RPC RESOURCE_NOT_FOUND (-32002); got {error:?}"
    );
    let data = error
        .get("data")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("the unknown-key error names the request: {error:?}"));
    assert_eq!(
        data.get("requested").and_then(Value::as_str),
        Some(command),
        "the error preserves the refused request"
    );
    let supported = data
        .get("supported")
        .and_then(Value::as_array)
        .expect("the error names the catalog's supported keys as discoverable alternatives");
    let keys: Vec<&str> = supported.iter().filter_map(Value::as_str).collect();
    assert!(
        keys.contains(&"cargo build")
            && keys.contains(&"cargo test")
            && keys.contains(&"rustup show"),
        "the alternatives name the catalog's advertised command keys; got {keys:?}"
    );
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains(command),
        "the message names the refused request: {message}"
    );
    for forbidden in ["/src/", "at 0x", "backtrace", "src/mcp"] {
        assert!(
            !message.contains(forbidden),
            "the error leaks no internal path or backtrace ({forbidden}): {message}"
        );
    }
}

/// AC2 — the trial's headline finding, recorded rather than over-claimed:
///
/// * **one** of the three seed-advertised commands (`cargo build`) *resolves* —
///   its guidance, citations, compatibility qualifications, and effect notes are
///   returned in full;
/// * the other two (`cargo test`, `rustup show`) are **honestly reported as
///   unresolved** — a typed `UnresolvedSection` error, not a fabricated section
///   — because the representative seed does not carry their full-catalog
///   citations;
/// * the not-in-catalog probe (`cargo foobar`) is **honestly reported as
///   unknown** with the supported keys — the catalog boundary.
///
/// This asserts the trial did **not** silently claim that every advertised
/// command resolves on the seed: the gap is real and documented, not fabricated
/// away.
fn skipped_configs_are_recorded(build: &Value, test: &Value, rustup: &Value, unknown: &Value) {
    assert!(
        build.get("error").is_none(),
        "`cargo build` resolves on the seed — the trial's success path"
    );
    assert!(
        test.get("error").is_some(),
        "`cargo test` is an unresolved configuration the trial records, not a hidden miss"
    );
    assert!(
        rustup.get("error").is_some(),
        "`rustup show` is an unresolved configuration the trial records, not a hidden miss"
    );
    assert!(
        unknown.get("error").is_some(),
        "the not-in-catalog probe is a recorded unsupported configuration"
    );
    // Exactly one of the three advertised commands resolves; the other two do
    // not — the trial's honest, recorded finding.
    let resolves = [
        (CARGO_BUILD, build),
        (CARGO_TEST, test),
        (RUSTUP_SHOW, rustup),
    ];
    let resolving_count = resolves
        .iter()
        .filter(|(_, response)| response.get("error").is_none())
        .count();
    assert_eq!(
        resolving_count, 1,
        "exactly one of the three seed-advertised commands resolves on the representative seed"
    );
}

/// AC2 — the server's own `instructions` state the retrieval-versus-execution
/// boundary the trial must not over-claim: the MCP *retrieves* documentation and
/// *provides evidence, not a guarantee* that guidance was followed. Asserting the
/// exact phrasing pins the boundary the trial respects — it retrieves and cites,
/// it does not execute or certify.
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
