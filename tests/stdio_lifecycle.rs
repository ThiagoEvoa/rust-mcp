//! End-to-end lifecycle proof for task `[005][M1]`.
//!
//! This test runs the *real* compiled `rust-docs-mcp` binary as a separate
//! process, connected over the operating-system stdio streams the `serve`
//! command is wired to. It is the authoritative check for the task's two
//! acceptance criteria:
//!
//! 1. **Client discovery/initialization matches the chosen protocol version.**
//!    For each known, `initialize`-capable version, the test acts as an MCP
//!    client: it writes a single `initialize` request naming that version and
//!    asserts the server's reply echoes the exact `protocolVersion`.
//! 2. **Clean connect and disconnect without non-protocol stdout or orphaned
//!    processes.** After the handshake the test closes its side of the child's
//!    stdin, which is an EOF the stdio transport treats as the end of the
//!    session. The process must then exit *on its own*, within a bounded time,
//!    with a clean exit code. `kill_on_drop(true)` (a `Command` builder flag)
//!    is the safety net that reaps the child even on panic, but the assertion
//!    proves a graceful self-exit, not a forced kill. `stderr` is redirected to
//!    a pipe and then discarded, so nothing the server logs can leak onto the
//!    protocol stream; every stdout line is parsed and must be a JSON-RPC 2.0
//!    message.
//!
//! The client speaks raw JSON-RPC over a newline-delimited stream — no rmcp
//! client needed, so the check is hermetic and does not depend on the SDK's
//! internal task scheduler.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;

/// How long the test will wait for the server to answer and, after a clean
/// disconnect, to exit. A real stdio handshake on localhost is sub-second; this
/// generous budget keeps the test fast while tolerating CI load.
const BUDGET: Duration = Duration::from_secs(15);

/// A temporary per-test data directory, given to the child as `--data-dir` so
/// the process never writes the host's real `~/.cache`.
fn fresh_data_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rust-docs-mcp-stdio-lifecycle-{}-{}",
        std::process::id(),
        stamp,
    ))
}

// ---------------------------------------------------------------------------
// Test.
// ---------------------------------------------------------------------------

/// The authoritative lifecycle proof for task [005]. For each known,
/// `initialize`-capable protocol version, it runs the real binary as a separate
/// process over genuine stdio, exercises the production `serve` path, and proves
/// the two acceptance criteria. See the module docs for the reasoning.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_negotiates_each_known_version_and_disconnects_cleanly() {
    for protocol_version in ["2024-11-05", "2025-03-26", "2025-06-18"] {
        serve_negotiates_version_and_disconnects_cleanly(protocol_version).await;
    }
}

async fn serve_negotiates_version_and_disconnects_cleanly(protocol_version: &str) {
    let data_dir = fresh_data_dir();
    // Start the real MCP server on its own process, piping its stdio so we can
    // observe exactly what it writes and feed it the client handshake.
    // `kill_on_drop(true)` is a *builder* flag: the OS reaps the child even if
    // this test panics or the wait times out — the no-orphan guarantee, beyond
    // the clean self-exit we assert below.
    let mut child = Command::new(env!("CARGO_BIN_EXE_rust-docs-mcp"))
        .arg("serve")
        .arg("--data-dir")
        .arg(&data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Diagnostics would appear on stderr, which we drop after proving stdout
        // carries only protocol traffic.
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the test harness must be able to launch the `serve` binary");

    let mut stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");

    // (1) Clean connect: a conforming client opens with `initialize` and then
    //     its `notifications/initialized` notification.
    write_json(&mut stdin, &initialize_request_json(protocol_version)).await;

    // The server must answer on stdout, and *only* with a JSON-RPC 2.0 message
    // (no logs, no diagnostics, no panic text) within the budget. `read_line`
    // lives on `AsyncBufReadExt`, so wrap the piped stdout in a `BufReader`.
    let mut reader = tokio::io::BufReader::new(stdout);
    let response = read_initialize_response(&mut reader).await;

    assert_eq!(
        response.get("error").and_then(serde_json::Value::as_str),
        None,
        "the initialization handshake must succeed, not error: {response}"
    );
    let result = response
        .get("result")
        .expect("`initialize` must return a result object");

    // The negotiated version must match the one we asked for, and the server
    // must identify itself by its binary name.
    assert_eq!(
        result
            .get("protocolVersion")
            .and_then(serde_json::Value::as_str),
        Some(protocol_version),
        "the server must echo the protocol version the client selected ({protocol_version})"
    );
    assert_eq!(
        result
            .pointer("/serverInfo/name")
            .and_then(serde_json::Value::as_str),
        Some(env!("CARGO_PKG_NAME")),
        "the response must carry the documented server identity"
    );

    // Complete the initialize lifecycle with its `notifications/initialized`
    // notification, the way a conforming client does.
    write_json(&mut stdin, &initialized_notification()).await;

    // (2) Clean disconnect: close *our* side of stdin (an EOF for the server).
    // The server must then end on its own, within the remaining budget, with a
    // clean exit — the concrete "no orphaned process" claim.
    drop(stdin);

    match tokio::time::timeout(BUDGET, child.wait()).await {
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

/// Read stdout one JSON object per line until the line whose `id` matches the
/// request (`1`). Every line must be a JSON-RPC 2.0 message, and every read must
/// arrive within the remaining budget; a premature EOF is a disconnected server
/// and a timeout are both failures, not clean runs.
async fn read_initialize_response<R: AsyncBufReadExt + Unpin + Send>(reader: &mut R) -> Value {
    let deadline = tokio::time::Instant::now() + BUDGET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for the `initialize` response from the server");
        }

        let mut line = String::new();
        match tokio::time::timeout(remaining, reader.read_line(&mut line)).await {
            // `read_line` resolves to `Ok(0)` only at end-of-stream.
            Ok(Ok(0)) => {
                panic!(
                    "the server closed stdout before answering `initialize` — a disconnected server"
                );
            }
            Ok(Ok(_)) => { /* a line was read; fall through to parse it */ }
            Ok(Err(error)) => panic!("reading the child's stdout failed: {error}"),
            Err(_) => panic!("timed out waiting for the `initialize` response from the server"),
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Every stdout line must be a JSON-RPC 2.0 message. This is the concrete
        // "stdout carries only protocol traffic" assertion.
        let value: Value = serde_json::from_str(trimmed).expect("every stdout line must be JSON");
        assert_eq!(
            value.get("jsonrpc").and_then(Value::as_str),
            Some("2.0"),
            "a non-JSON-RPC object leaked onto stdout: {trimmed}"
        );
        if value.get("id").and_then(Value::as_u64) == Some(1) {
            return value;
        }
    }
}

/// Write one newline-framed message and flush it to the child's stdin.
async fn write_json<W: AsyncWriteExt + Unpin + Send>(writer: &mut W, message: &Value) {
    writer
        .write_all(frame(message).as_bytes())
        .await
        .expect("writing a JSON-RPC request to the child's stdin must not fail");
    writer
        .flush()
        .await
        .expect("flushing the child's stdin must not fail");
}

// ---------------------------------------------------------------------------
// Request builders.
// ---------------------------------------------------------------------------

fn initialize_request_json(protocol_version: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": protocol_version,
            "capabilities": {},
            "clientInfo": {
                "name": "stdio-lifecycle-test",
                "version": "0.0.1",
            }
        }
    })
}

fn initialized_notification() -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    })
}

/// Encode one JSON-RPC message as a newline-delimited frame.
fn frame(message: &Value) -> String {
    let serialized =
        serde_json::to_string(message).expect("encoding a hand-built JSON value must not fail");
    format!("{serialized}\n")
}
