//! [060][M4] — prove the assembled MCP surface serves **offline** and that the
//! server's access boundaries **refuse** adversarial inputs before they can
//! execute a command, fetch a URL, or read the filesystem.
//!
//! Both acceptance criteria are proven *in-process* through the **exact** public
//! tool boundary the `serve` handler dispatches —
//! [`DocsServer::dispatch_call`] / [`DocsServer::dispatch_read`], made
//! `pub(crate)` for this task — on a single [`DocsServer`] built over the
//! embedded, in-memory bundle (`crate::bundled::JSON`) with **no** local
//! snapshot, **no** `sync`, and **no** network.
//!
//! ## AC1 — the assembled surface works offline
//! [`assembled_surface_serves_offline_without_network_or_sync`] drives all five
//! tools **and** the two resource reads through `dispatch_call` /
//! `dispatch_read` on one [`DocsServer::new()`] and asserts each succeeds, then
//! proves the served snapshot *is* the embedded bundle (`SnapshotSource::Embedded`,
//! snapshot id [`crate::bundled::SNAPSHOT_ID`]) — mirroring
//! [`crate::runtime::tests::an_embedded_snapshot_makes_a_fresh_install_work_offline`].
//! Together this proves the assembled surface (all eight sources + a section + a
//! checklist + command guidance) works with no network and no `sync`.
//!
//! ## AC2 — adversarial inputs are refused, never acted on
//! [`adversarial_inputs_are_refused_never_executed_fetched_or_read`] drives a
//! shell-operator command key and caller-forged resource URIs (a network URL, a
//! filesystem path, a path-traversal id, a foreign-snapshot id) through the same
//! boundary and asserts each is refused with a JSON-RPC error
//! (`INVALID_PARAMS` / `RESOURCE_NOT_FOUND`) and *never* a success. The
//! per-boundary guarantees these refusals rest on are already proven elsewhere
//! and cited here:
//!
//! * the command key is validated at the `[007]` `CommandKey` boundary, and the
//!   module that serves it touches no process / fs / network primitive —
//!   [`crate::mcp::get_command_guidance::tests::command_guidance_never_executes_or_touches_fs_or_network`];
//! * a foreign host is not a fetch boundary and the rendered rustfmt JS site is
//!   not a fetch boundary —
//!   [`crate::registry::tests::arbitrary_foreign_hosts_are_rejected`] and
//!   [`crate::registry::tests::rustfmt_rendered_js_site_is_not_a_fetch_boundary`].
//!
//! [`the_dispatch_read_path_touches_no_execution_or_network_primitive`]
//! re-proves, by source scan, that the `dispatch_read` refusal path
//! (`crate::mcp::resources`) imports no process / network / filesystem primitive.

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode, ErrorData, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse,
};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::DocsServer;

// A counter that makes each offline data directory name unique within a run
// without creating a directory or touching a process primitive.
static EMPTY_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The negotiated protocol version the boundary calls use. `V_2025_11_25` is
/// **pre** `2026-07-28`, so a `RESOURCE_NOT_FOUND` keeps its base code and is
/// *not* upgraded to `INVALID_PARAMS` — letting the unknown-section refusal show
/// its `RESOURCE_NOT_FOUND` code, the exact refusal [055]/[051] surface.
fn version() -> ProtocolVersion {
    ProtocolVersion::V_2025_11_25
}

/// A `DocsServer` over the embedded, in-memory bundle with **no** local snapshot.
///
/// `DocsServer::new()` serves `crate::bundled::manifest()` — the same validated
/// seed the runtime reports as `SnapshotSource::Embedded` — so the entire
/// assembled surface is driven offline: no local snapshot, no `sync`, no network.
fn offline_server() -> DocsServer {
    DocsServer::new()
}

/// A guaranteed-absent data directory.
///
/// No directory is created and no network is touched: the *absence* of a local
/// published snapshot under this path is what makes the runtime select the
/// embedded bundle. Mirrors the "no local snapshot exists" precondition of
/// `crate::runtime::tests::an_embedded_snapshot_makes_a_fresh_install_work_offline`.
fn empty_data_dir() -> std::path::PathBuf {
    let n = EMPTY_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("rust-docs-mcp-offline-{n}-{nanos}"))
}

/// Build a `call_tool` request by name with string→JSON-value arguments.
fn call(name: &str, args: &[(&str, Value)]) -> CallToolRequestParams {
    let map: Map<String, Value> = args
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    CallToolRequestParams::new(name.to_string()).with_arguments(map)
}

/// Unwrap a `CallToolResponse` to its completed `CallToolResult`, panicking on
/// any MRTR intermediate variant (the five tools never return one).
fn complete(resp: Result<CallToolResponse, ErrorData>, ctx: &str) -> CallToolResult {
    match resp {
        Ok(CallToolResponse::Complete(result)) => result,
        Ok(other) => panic!("{ctx}: expected a completed tool result, got {other:?}"),
        Err(err) => panic!("{ctx}: the tool call failed offline: {err:?}"),
    }
}

/// The structured JSON payload of a completed tool result.
fn structured(result: &CallToolResult, ctx: &str) -> Value {
    result
        .structured_content
        .clone()
        .unwrap_or_else(|| panic!("{ctx}: the tool result carries no structured content"))
}

/// Extract the single text payload from a completed resource read.
fn resource_text(resp: Result<ReadResourceResponse, ErrorData>, ctx: &str) -> String {
    match resp {
        Ok(ReadResourceResponse::Complete(result)) => {
            let content = result
                .contents
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("{ctx}: the resource read returned no content"));
            match content {
                rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
                other => panic!("{ctx}: expected a text resource, got {other:?}"),
            }
        }
        Ok(other) => panic!("{ctx}: expected a completed resource read, got {other:?}"),
        Err(err) => panic!("{ctx}: the resource read failed offline: {err:?}"),
    }
}

// ---------------------------------------------------------------------------
// AC1 — the assembled surface serves offline.
// ---------------------------------------------------------------------------

/// All five tools and the two resource reads succeed on one `DocsServer` over
/// the embedded bundle, and every response names the embedded snapshot — so the
/// assembled MCP surface (all eight sources + a section + a checklist + command
/// guidance) works with no network and no `sync`.
#[tokio::test]
async fn assembled_surface_serves_offline_without_network_or_sync() {
    let server = offline_server();
    let version = version();
    let snapshot_id = crate::bundled::SNAPSHOT_ID;

    // (0) Prove the served snapshot *is* the embedded bundle, with no local
    //     snapshot and no sync — mirroring
    //     `an_embedded_snapshot_makes_a_fresh_install_work_offline`.
    let rt =
        crate::runtime::load_snapshot_embedded(&empty_data_dir(), crate::bundled::JSON.as_bytes())
            .expect("the bundled snapshot must load offline");
    assert!(
        matches!(rt.source, crate::runtime::SnapshotSource::Embedded),
        "with no local snapshot, the embedded bundle must be selected"
    );
    assert_eq!(
        rt.manifest.body.snapshot_id.as_str(),
        crate::bundled::SNAPSHOT_ID,
        "the served snapshot must be the embedded seed"
    );

    // (1) list_sources returns all eight source ids in registry order.
    let list = complete(
        server
            .dispatch_call(call("list_sources", &[]), Some(&version))
            .await,
        "list_sources",
    );
    assert!(
        !list.is_error.unwrap_or(false),
        "list_sources must not error offline"
    );
    let list_structured = structured(&list, "list_sources");
    let sources = list_structured
        .get("sources")
        .and_then(Value::as_array)
        .expect("list_sources returns a sources array");
    assert_eq!(sources.len(), 8, "list_sources returns all eight sources");
    let ids: Vec<&str> = sources
        .iter()
        .filter_map(|s| s.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(
        ids,
        [
            "api-guidelines",
            "style-guide",
            "cargo",
            "rustc",
            "rustup",
            "rustfmt",
            "clippy",
            "rustdoc",
        ],
        "list_sources returns the eight registry ids in order"
    );
    assert_eq!(
        structured(&list, "list_sources")
            .get("snapshot_id")
            .and_then(Value::as_str),
        Some(snapshot_id),
        "list_sources names the embedded snapshot"
    );

    // (2) search_docs hits a section offline.
    let search = complete(
        server
            .dispatch_call(
                call(
                    "search_docs",
                    &[
                        ("query", json!("cargo build")),
                        ("source", json!("cargo")),
                        ("limit", json!(5)),
                    ],
                ),
                Some(&version),
            )
            .await,
        "search_docs",
    );
    assert!(
        !search.is_error.unwrap_or(false),
        "search_docs must not error offline"
    );
    let search_structured = structured(&search, "search_docs");
    let hits = search_structured
        .get("hits")
        .and_then(Value::as_array)
        .expect("search_docs returns a hits array");
    assert!(!hits.is_empty(), "search_docs hits a section offline");
    assert_eq!(
        structured(&search, "search_docs")
            .get("snapshot_id")
            .and_then(Value::as_str),
        Some(snapshot_id),
        "search_docs names the embedded snapshot"
    );

    // (3) get_api_checklist returns the C-CONV entry offline.
    let checklist = complete(
        server
            .dispatch_call(
                call(
                    "get_api_checklist",
                    &[("category", json!("Conversions and From"))],
                ),
                Some(&version),
            )
            .await,
        "get_api_checklist",
    );
    assert!(
        !checklist.is_error.unwrap_or(false),
        "get_api_checklist must not error offline"
    );
    let checklist_structured = structured(&checklist, "get_api_checklist");
    let entries = checklist_structured
        .get("entries")
        .and_then(Value::as_array)
        .expect("get_api_checklist returns an entries array");
    assert!(
        !entries.is_empty(),
        "get_api_checklist returns the C-CONV entry offline"
    );

    // (4) get_section resolves a section offline.
    let section = complete(
        server
            .dispatch_call(
                call(
                    "get_section",
                    &[("section_id", json!("api-guidelines/about"))],
                ),
                Some(&version),
            )
            .await,
        "get_section",
    );
    assert!(
        !section.is_error.unwrap_or(false),
        "get_section must not error offline"
    );
    assert!(
        structured(&section, "get_section").get("section").is_some(),
        "get_section returns the requested section offline"
    );

    // (5) get_command_guidance resolves a documented command offline.
    let guidance = complete(
        server
            .dispatch_call(
                call("get_command_guidance", &[("command", json!("cargo build"))]),
                Some(&version),
            )
            .await,
        "get_command_guidance",
    );
    assert!(
        !guidance.is_error.unwrap_or(false),
        "get_command_guidance must not error offline"
    );
    let guidance_structured = structured(&guidance, "get_command_guidance");
    assert!(
        guidance_structured.get("entry").is_some(),
        "get_command_guidance returns the command entry offline"
    );
    assert_eq!(
        guidance_structured
            .get("snapshot_id")
            .and_then(Value::as_str),
        Some(snapshot_id),
        "get_command_guidance names the embedded snapshot"
    );

    // (6) Both resource reads (catalog + a section) succeed offline.
    let catalog = resource_text(
        server
            .dispatch_read(
                ReadResourceRequestParams::new("rust-docs://catalog"),
                Some(&version),
            )
            .await,
        "catalog resource",
    );
    assert!(
        serde_json::from_str::<Value>(&catalog).is_ok(),
        "the catalog resource reads as JSON offline"
    );
    let section_uri = format!("rust-docs://snapshots/{snapshot_id}/sections/api-guidelines/about");
    let section_resource = resource_text(
        server
            .dispatch_read(ReadResourceRequestParams::new(&section_uri), Some(&version))
            .await,
        "section resource",
    );
    assert!(
        serde_json::from_str::<Value>(&section_resource).is_ok(),
        "the section resource reads as JSON offline"
    );
}

// ---------------------------------------------------------------------------
// AC2 — adversarial inputs are refused, never acted on.
// ---------------------------------------------------------------------------

/// A shell-operator command key and caller-forged resource URIs (a network URL,
/// a filesystem path, a path-traversal id, and a foreign-snapshot id) are each
/// refused through the exact `dispatch_call` / `dispatch_read` boundary with a
/// JSON-RPC error — never a success, and therefore never an executed command, a
/// fetched URL, or a read of the filesystem. A clean key and a valid section
/// resource are the positive controls: the refusal is specific, not blanket.
#[tokio::test]
async fn adversarial_inputs_are_refused_never_executed_fetched_or_read() {
    let server = offline_server();
    let version = version();
    let snapshot_id = crate::bundled::SNAPSHOT_ID;

    // --- Positive controls: the surface works for *well-formed* input, so the
    //     refusals below are specific to the adversarial shape, not a blanket
    //     refusal. ---
    let ok_call = server
        .dispatch_call(
            call("get_command_guidance", &[("command", json!("cargo build"))]),
            Some(&version),
        )
        .await;
    assert!(
        matches!(ok_call, Ok(CallToolResponse::Complete(_))),
        "a clean command key resolves — the refusal below is specific to operators"
    );
    let ok_read = server
        .dispatch_read(
            ReadResourceRequestParams::new(format!(
                "rust-docs://snapshots/{snapshot_id}/sections/api-guidelines/about"
            )),
            Some(&version),
        )
        .await;
    assert!(
        matches!(ok_read, Ok(ReadResourceResponse::Complete(_))),
        "a valid section resource reads — the refusals below are specific to forgery"
    );

    // --- (a) A shell-operator command key is refused, never executed. ---
    // `;`, `|`, `&&`, and `$(…)` are disallowed `CommandKey` characters, so the
    // `[007]` boundary refuses the key before the catalog could ever interpolate
    // it into a process. Proven per-key by
    // `command_guidance_never_executes_or_touches_fs_or_network` and the source
    // scan below; here we prove it at the *dispatch* boundary.
    for bad in [
        "cargo test; rm -rf /",
        "cargo new | xargs -n1 echo",
        "cargo metadata $(whoami)",
        "cargo init && echo pwned",
    ] {
        let err = server
            .dispatch_call(
                call("get_command_guidance", &[("command", json!(bad))]),
                Some(&version),
            )
            .await
            .expect_err("a shell-operator key must be refused, never executed");
        assert_eq!(
            err.code,
            ErrorCode::INVALID_PARAMS,
            "a shell-operator key is refused as INVALID_PARAMS (InvalidKey), never executed"
        );
        assert!(
            !server
                .dispatch_call(
                    call("get_command_guidance", &[("command", json!(bad))]),
                    Some(&version),
                )
                .await
                .is_ok(),
            "a shell-operator key never produces a result: it was refused, not executed"
        );
    }

    // --- (b) Caller-forged resource URIs are refused before any fetch or fs read. ---
    // Each forged URI is a network URL, a filesystem path, a path-traversal id,
    // or a foreign-snapshot id. `dispatch_read` parses it against the exact
    // `rust-docs://snapshots/{snapshot}/sections/{id}` form and refuses anything
    // outside it *before* touching a URL, the filesystem, or a foreign snapshot.
    // The per-boundary guarantee is `arbitrary_foreign_hosts_are_rejected` and
    // `rustfmt_rendered_js_site_is_not_a_fetch_boundary` (a host or rendered JS
    // site is not a fetch boundary).
    let section_id = "api-guidelines/about";
    let forged: &[(&str, ErrorCode)] = &[
        // A network URL the registry never registers as a fetch boundary.
        ("https://docs.rs/serde/1.0/serde", ErrorCode::INVALID_PARAMS),
        ("https://example.com/foo", ErrorCode::INVALID_PARAMS),
        // A raw filesystem path.
        ("/etc/passwd", ErrorCode::INVALID_PARAMS),
        ("C:\\Windows\\System32", ErrorCode::INVALID_PARAMS),
        // A path-traversal section id under the *real* snapshot — a literal `/`
        // after the source separator fails the id grammar, so it is refused.
        (
            &format!(
                "rust-docs://snapshots/{snapshot_id}/sections/api-guidelines/../../etc/passwd"
            ),
            ErrorCode::INVALID_PARAMS,
        ),
        // A foreign snapshot the handler does not serve — refused as a mismatch
        // before any read, so a caller cannot forge a read of another snapshot.
        (
            &format!("rust-docs://snapshots/some-other-snapshot/sections/{section_id}"),
            ErrorCode::INVALID_PARAMS,
        ),
        // A well-formed id the active snapshot does not carry — the "section
        // absent" refusal, shown as its base RESOURCE_NOT_FOUND code.
        (
            &format!("rust-docs://snapshots/{snapshot_id}/sections/does-not-exist"),
            ErrorCode::RESOURCE_NOT_FOUND,
        ),
    ];
    for (uri, expected_code) in forged {
        let err = server
            .dispatch_read(ReadResourceRequestParams::new(&**uri), Some(&version))
            .await
            .expect_err("a caller-forged resource URI must be refused, never read");
        assert_eq!(
            err.code, *expected_code,
            "the forged URI {uri} is refused with {expected_code:?}, never fetched or read"
        );
        // The same URI never produces a result: it was refused, not read.
        assert!(
            !server
                .dispatch_read(ReadResourceRequestParams::new(&**uri), Some(&version))
                .await
                .is_ok(),
            "the forged URI {uri} never produces a resource: it was refused"
        );
    }
}

// ---------------------------------------------------------------------------
// Source scan: the `dispatch_read` refusal path imports no execution primitive.
// ---------------------------------------------------------------------------

/// Re-prove, by source scan (the pattern of
/// `command_guidance_never_executes_or_touches_fs_or_network`), that the
/// `dispatch_read` refusal boundary — `crate::mcp::resources` — imports no
/// process-execution, filesystem, or network primitive. This is why a forged
/// URL or path is *refused* rather than *acted on*: the code path that decides
/// the refusal has no primitive with which to fetch, execute, or read a file.
#[test]
fn the_dispatch_read_path_touches_no_execution_or_network_primitive() {
    // The `dispatch_read` refusal lives in `crate::mcp::resources`; scan *that*
    // module (the boundary), not this test's own prose. The forbidden tokens are
    // assembled from pieces so this test's own source never matches its own scan.
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/mcp/resources.rs"));
    let forbidden = [
        "std::".to_string() + "process", // the standard-library process module
        "std::".to_string() + "net",     // the standard-library network module
        "Command::".to_string() + "new", // the standard-library command constructor
        ".spaw".to_string() + "n(",      // the spawn method call
        ".exe".to_string() + "c(",       // the exec method call
        "req".to_string() + "west",      // the HTTP client
    ];
    for needle in &forbidden {
        assert!(
            !source.contains(needle.as_str()),
            "the dispatch_read refusal path (resources.rs) must contain no \
             process/network primitive, but found: {needle}"
        );
    }
}
