//! The MCP protocol surface: the server handler, its stdio driver, and the
//! typed error mapping that turns `rmcp` errors into actionable diagnostics.
//!
//! Task [002] established the package shape. This module carries task
//! [**005**]: **a minimal, real `rmcp` MCP server** — the *lifecycle skeleton*
//! that task [003] wires to the `serve` CLI command and that task [050]
//! ("Run server over stdio") later extends with the catalog/section resources
//! and the five tools.
//!
//! What M1 proves here (and nothing more):
//!
//! * the server compiles and boots on the pinned rmcp **3.8.0** SDK;
//! * it advertises a stable implementation identity via [`ServerHandler::get_info`]
//!   (asserted by the synchronous `get_info` unit test in [`crate::tests`]);
//! * a client can negotiate a protocol version, exchange the handshake, and
//!   disconnect **cleanly**, after which the runtime task ends on its own —
//!   the "no orphaned processes" half of task [005], proven by the spawned-binary
//!   stdio test in [`tests/stdio_lifecycle.rs`], which runs the *real*
//!   `rust-docs-mcp serve` binary and drives it with the JSON-RPC protocol over
//!   genuine OS stdio.
//!
//! *Note on the client.* We deliberately do **not** stand up an in-process
//! `rmcp` client against a `tokio::io::duplex` pipe: the SDK's
//! `RunningService` tasks are `!Send` and bind to a `LocalSpawner`, so a
//! black-box client check would need to replicate the SDK's own runtime
//! setup. The spawned-binary test instead exercises the production path over
//! the real transport, which is the more authoritative check anyway.
//! What the M1 skeleton deliberately did **not** do — each item has since
//! landed in a later milestone, recorded here for provenance:
//!
//! * **Capabilities — superseded by M4 ([057]).** The M1 skeleton advertised
//!   *no* tool or resource capabilities. `[057]` wired the handler, and
//!   `get_info` now advertises **exactly** `tools` + `resources`
//!   (`ServerCapabilities::builder().enable_tools().enable_resources()`) and
//!   dispatches all five tools and the two catalog/section resource URIs.
//!   "Advertise only what is implemented" ([plan.md](../plan.md) §6) still
//!   governs: nothing else (`prompts`, `completions`, `logging`,
//!   `experimental`, `extensions`) is advertised, and no transport other than
//!   stdio is built. The full client + protocol matrix and the unsupported
//!   combinations are recorded in
//!   [`docs/COMPATIBILITY.md`](../docs/COMPATIBILITY.md) §10 (task [058]).
//! * **Corpus / data directory — superseded by M2/M4 ([018]-[022], [050]).**
//!   The M1 skeleton touched no corpus; the immutable snapshot load and the
//!   "no valid snapshot → exit without serving an empty server" gate landed in
//!   M2/M4.
//! * **CLI stdio wiring — superseded by [003].** The `serve` CLI command runs
//!   the server over genuine OS stdio; the spawned-binary tests
//!   ([`tests/stdio_lifecycle.rs`](../../tests/stdio_lifecycle.rs) and
//!   [`tests/stdio_mcp.rs`](../../tests/stdio_mcp.rs)) drive that production
//!   path with a raw JSON-RPC 2.0 client.
//!
//! # Error mapping
//!
//! Runtime/startup failures (transport init, a transport-level error, runtime
//! construction) are lifted into the project's typed
//! [`AppError`](crate::error::AppError) via `runtime_start`, preserving the
//! underlying cause for M4 diagnostics ([task 004](../plan.md#task-004)).
//! The [`rmcp`](https://docs.rs/rmcp) protocol-level errors that *clients* see
//! (e.g. an unsupported protocol version) are not mapped here: they are returned
//! *within* the protocol, not surfaced through the start path.

use std::process::ExitCode;

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, ErrorCode, Implementation, ListResourcesResult,
        ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, ResourceContents, ServerCapabilities, ServerConfig,
    },
    service::{RequestContext, RunningService},
};

use crate::commands::CommandCatalog;
use crate::error::{AppError, AppResult};
use crate::model::SnapshotManifest;
use crate::provenance::{Instant, StaleThreshold};

/// Protocol / tool error mapping for task `[048][M4]` (AC2): maps the crate's
/// internal retrieval errors to version-aware `rmcp` [`rmcp::model::ErrorData`]
/// without leaking internal paths or stack traces. See that module for the
/// [`ErrorClass`](protocol_error::ErrorClass) /
/// [`ProtocolError`](protocol_error::ProtocolError) API and the `2026-07-28`
/// error-code upgrade.
pub mod protocol_error;

/// The `list_sources` MCP tool ([049][M4]): builds the catalog entry-point
/// response from the active snapshot.
pub mod list_sources;

/// The `search_docs` MCP tool ([050][M4]): answers a query/source/limit/cursor
/// request against the active snapshot by composing the [034]/[035] ranked,
/// exact-match, source-filtered search, the [037] bounded/cursor-paginated page,
/// the [038] query validation and "useful empty" guidance, and the [036] section
/// reads for excerpts and citations, mapping failures through the [048]
/// `ProtocolError` mapping, and assembling the result as a `CallToolResult`.
pub mod search_docs;

/// The `get_section` MCP tool ([051][M4]): resolves a server-issued section id
/// to a section's complete record — Markdown body, heading context, citations,
/// source freshness, and snapshot identity — served from the active snapshot,
/// paginating over-cap bodies through the [037] continuation cursor and mapping
/// a missing id through the [048] `ProtocolError` mapping.
pub mod get_section;

/// The `get_api_checklist` MCP tool ([052][M4]): lists the snapshot's
/// API-guideline checklist entries — each its upstream `code`, `title`,
/// `category`, checklist `location`, and explanatory section ids — optionally
/// filtered by a single category and paginated by a [037] continuation cursor,
/// composing the [036] checklist read (which verifies every explanation
/// resolves), mapping an unknown category and a dangling citation through the
/// [048] `ProtocolError` contract, and assembling the result as a
/// `CallToolResult`.
pub mod get_api_checklist;

/// The `get_command_guidance` MCP tool ([053][M4]): the fifth and final M4
/// tool. Reads the source-backed documentation for one canonical command key
/// (e.g. `cargo test`) — its catalog entry (purpose, documented syntax, examples,
/// prerequisites, version/stability qualifications, and the separately-labeled
/// maintainer effect notes) plus the source-backed sections it cites — served
/// from the active snapshot and paginated over-cap section bodies through a [037]
/// continuation cursor. It composes the [039]–[043] command catalog lookup
/// (which validates the key at the [007] boundary, so a shell operator or
/// unsupported key is refused, never executed), the [036] section read, and the
/// [037] cursor, mapping a malformed / unsupported key, a dangling citation, and a
/// bad cursor through the [048] `ProtocolError` contract, and assembling the
/// result as a `CallToolResult`. It retrieves documentation only — it executes no
/// command and touches no filesystem or network.
pub mod get_command_guidance;
pub mod resources;

/// The `instructions` text advertised on every `initialize` handshake by
/// [`DocsServer::get_info`] ([056][M4]): the plan's §7 "Agent workflow and
/// client integration" sequence, plus the retrieval-versus-execution and
/// stale/unknown-compatibility boundaries. This module is the single owner of the
/// `instructions` string the skeleton hands to
/// `ServerConfig::with_instructions`, keeping the capability surface empty until
/// handler dispatch lands later in M4.
pub mod instructions;

/// The typed MCP tool/resource schemas for task `[048][M4]` (AC1): the
/// request/response shapes and catalog/section resource payloads that the five
/// tools and two resources speak, plus a conformance check that a value
/// serialised by this crate agrees with its generated `schemars` schema. See
/// that module for the type catalogue and the agreement tests.
pub mod schema;

/// The documentation MCP server handler.
///
/// It owns the single immutable snapshot (plan §5) it serves for its entire
/// lifetime, plus the command catalog the `get_command_guidance` tool and the
/// catalog resource read over, and an `as_of` instant and staleness
/// `threshold` the per-source freshness verdict is judged against. All state is
/// `Clone + Send + Sync`, so the handler is cheaply clonable and safe to share
/// across the connection. It advertises the `tools` and `resources`
/// capabilities and dispatches all five tools and the two resource URIs.
#[derive(Clone, Debug)]
pub struct DocsServer {
    /// The immutable snapshot the handler serves for its entire lifetime.
    manifest: SnapshotManifest,
    /// The command catalog the `get_command_guidance` tool and catalog resource read over.
    catalog: CommandCatalog,
    /// The "now" the per-source freshness verdict is judged against.
    as_of: Instant,
    /// The staleness threshold the per-source freshness verdict is judged against.
    threshold: StaleThreshold,
}

impl DocsServer {
    /// Build the handler over the shipped, in-memory bundled snapshot with a
    /// "now" read from the wall clock and the default staleness threshold.
    ///
    /// The bundled snapshot is always present (it is a compile-time-embedded,
    /// validated corpus), so this is the server's always-valid offline fallback:
    /// it never starts an apparently-healthy empty server (plan §5). Used by the
    /// `serve` CLI dispatch and by the transport-independent unit tests.
    pub fn new() -> Self {
        Self {
            manifest: crate::bundled::manifest(),
            catalog: CommandCatalog::combined(),
            as_of: Instant::from_system_time(std::time::SystemTime::now()),
            threshold: StaleThreshold::default(),
        }
    }
}

impl ServerHandler for DocsServer {
    /// Advertise the implementation identity and the `tools` + `resources`
    /// capabilities.
    ///
    /// `server_info` carries the binary's name and version so the client can
    /// tell *which* server it is talking to; `capabilities` now advertises the
    /// `tools` and `resources` surfaces the handler dispatches (plan §6,
    /// "advertise only what is implemented"); `instructions` advertises the
    /// plan's §7 agent workflow (retrieval versus execution, stale/unknown
    /// compatibility, adapt-to-the-repository), owned by
    /// [`crate::mcp::instructions::INSTRUCTIONS`]. The default
    /// `supported_protocol_versions` returns the SDK's full `KNOWN_VERSIONS`,
    /// and the default `initialize`/`discover` handshake negotiates the highest
    /// mutually supported, client-requested version — which is exactly what the
    /// lifecycle tests assert.
    fn get_info(&self) -> ServerConfig {
        // Advertise *exactly* the two capabilities this handler dispatches —
        // `tools` + `resources` — and nothing else (plan §6, "advertise only
        // what is implemented"). Notably **not** advertised: `prompts`,
        // `completions`, `logging`, `experimental`, and `extensions` (rmcp
        // omits any capability we do not `.enable_*()` via
        // `skip_serializing_if = "Option::is_none"`), and no transport other
        // than stdio is built. The full client + protocol matrix and the
        // unsupported combinations are recorded in `docs/COMPATIBILITY.md` §10
        // (task [058]); the unit test `get_info_advertises_exactly_tools_and_resources`
        // guards this set.
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(crate::mcp::instructions::INSTRUCTIONS)
    }

    /// List the five documentation tools. The `list_tools` cursor is accepted
    /// but unused: the catalog is small and returned whole.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            crate::mcp::schema::all_tools(),
        ))
    }

    /// List the two resource identities (`rust-docs://catalog` and
    /// `rust-docs://sections/<source>/<path>`).
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(
            crate::mcp::schema::all_resources(),
        ))
    }

    /// Route a tool call to its module and lift its typed error into a
    /// version-aware [`ErrorData`].
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let version = context.protocol_version();
        self.dispatch_call(request, version.as_ref()).await
    }

    /// Route a `read_resource` request to the catalog or section resource view
    /// and render it as a JSON text resource.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let version = context.protocol_version();
        self.dispatch_read(request, version.as_ref()).await
    }
}

impl DocsServer {
    /// Dispatch a `call_tool` request by tool name. Deserializes the request
    /// arguments into the tool's typed request struct, runs the module, and
    /// builds the `CallToolResult`. Each tool's typed error is lifted into a
    /// version-aware [`ErrorData`] via [`protocol_error::map`]; a malformed
    /// arguments object is an `INVALID_PARAMS` error; an unknown tool name is a
    /// `METHOD_NOT_FOUND` error. A blank query, an unknown source, or a cursor
    /// mismatch surface as the tool's own protocol error so the client receives
    /// the actionable guidance rather than a panicking server (plan §5).
    async fn dispatch_call(
        &self,
        request: CallToolRequestParams,
        version: Option<&rmcp::model::ProtocolVersion>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let result = match request.name.as_ref() {
            crate::mcp::list_sources::TOOL_NAME => {
                // No request fields; the response is derived purely from the
                // manifest snapshot.
                let resp =
                    crate::mcp::list_sources::response(&self.manifest, self.as_of, self.threshold)
                        .map_err(|e| protocol_error::map(e, version))?;
                crate::mcp::list_sources::result(&resp)
                    .map_err(|e| protocol_error::map(e, version))?
            }
            crate::mcp::search_docs::TOOL_NAME => {
                let req =
                    self.parse_args::<crate::mcp::schema::SearchDocsRequest>("search_docs", &args)?;
                let resp = crate::mcp::search_docs::search(&self.manifest, &req)
                    .map_err(|e| protocol_error::map(e, version))?;
                crate::mcp::search_docs::result(&resp)
                    .map_err(|e| protocol_error::map(e, version))?
            }
            crate::mcp::get_section::TOOL_NAME => {
                let req =
                    self.parse_args::<crate::mcp::schema::GetSectionRequest>("get_section", &args)?;
                let resp = crate::mcp::get_section::get_section(
                    &self.manifest,
                    &req,
                    self.as_of,
                    self.threshold,
                )
                .map_err(|e| protocol_error::map(e, version))?;
                crate::mcp::get_section::result(&resp)
                    .map_err(|e| protocol_error::map(e, version))?
            }
            crate::mcp::get_api_checklist::TOOL_NAME => {
                let req = self.parse_args::<crate::mcp::schema::GetApiChecklistRequest>(
                    "get_api_checklist",
                    &args,
                )?;
                let resp = crate::mcp::get_api_checklist::checklist(&self.manifest, &req)
                    .map_err(|e| protocol_error::map(e, version))?;
                crate::mcp::get_api_checklist::result(&resp)
                    .map_err(|e| protocol_error::map(e, version))?
            }
            crate::mcp::get_command_guidance::TOOL_NAME => {
                let req = self.parse_args::<crate::mcp::schema::GetCommandGuidanceRequest>(
                    "get_command_guidance",
                    &args,
                )?;
                let resp = crate::mcp::get_command_guidance::get_command_guidance(
                    &self.catalog,
                    &self.manifest,
                    &req,
                    self.as_of,
                    self.threshold,
                )
                .map_err(|e| protocol_error::map(e, version))?;
                crate::mcp::get_command_guidance::result(&resp)
                    .map_err(|e| protocol_error::map(e, version))?
            }
            other => {
                return Err(ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    format!("unknown tool: {other}"),
                    None,
                ));
            }
        };
        Ok(result.into())
    }

    /// Dispatch a `read_resource` request. The catalog resource returns the
    /// `catalog` resource view as JSON; any other `rust-docs://sections/...`
    /// URI is rendered as a single-section resource. An unknown source or an
    /// unavailable section surfaces as a `RESOURCE_NOT_FOUND` protocol error via
    /// the module's own typed error.
    async fn dispatch_read(
        &self,
        request: ReadResourceRequestParams,
        version: Option<&rmcp::model::ProtocolVersion>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let uri = request.uri;
        let structured = if uri == crate::mcp::resources::CATALOG_URI {
            let view = crate::mcp::resources::catalog_resource_view(
                &self.manifest,
                &self.catalog,
                None,
                None,
                self.as_of,
                self.threshold,
            )
            .map_err(|e| protocol_error::map(e, version))?;
            crate::mcp::resources::catalog_resource_result(&view)
                .map_err(|e| protocol_error::map(e, version))?
        } else {
            let view = crate::mcp::resources::read_section_resource(
                &self.manifest,
                &uri,
                self.as_of,
                self.threshold,
            )
            .map_err(|e| protocol_error::map(e, version))?;
            // Render the section view as the resource's JSON payload. The
            // `structured_content` is built the same way the [056] resource
            // layer would, so the catalog resource and the section resource
            // share one JSON shape.
            serde_json::to_value(crate::mcp::schema::SectionResourceView { section: view })
                .map_err(|e| {
                    ErrorData::internal_error(format!("encode section resource: {e}"), None)
                })?
        };
        let text = serde_json::to_string(&structured)
            .map_err(|e| ErrorData::internal_error(format!("render resource text: {e}"), None))?;
        Ok(ReadResourceResponse::from(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type("application/json"),
        ])))
    }

    /// Deserialize the `call_tool` arguments object into the tool's typed
    /// request struct, surfacing a malformed object as an `INVALID_PARAMS`
    /// protocol error carrying the failing tool's name.
    fn parse_args<T: for<'de> serde::Deserialize<'de>>(
        &self,
        tool: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<T, ErrorData> {
        serde_json::from_value(serde_json::Value::Object(args.clone())).map_err(|e| {
            ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                format!("the '{tool}' request arguments could not be parsed: {e}"),
                Some(serde_json::json!({ "tool": tool })),
            )
        })
    }
}

/// Start the MCP server over the process's real stdio and run until the client
/// disconnects.
///
/// This opens the standard input/output as the protocol transport, hands the
/// split halves to [`DocsServer`], and blocks until the connection ends — which
/// for a stdio server happens when the client closes its end (the canonical MCP
/// shutdown trigger), or on a transport error. On a clean disconnect it returns
/// [`Ok`]; on a start/transport failure it returns [`AppError::RuntimeStart`]
/// carrying the cause so the CLI boundary can emit an actionable diagnostic to
/// stderr and exit non-zero rather than panic on external input (plan §5).
///
/// Signal-driven shutdown (Ctrl-C) is intentionally not implemented in M1: the
/// stdio transport's read half terminating on EOF is the authoritative
/// end-of-session signal, and wiring an OS signal source is a cross-platform
/// concern left to task [050]/M4. A comment marks the seam.
///
/// [`AppError::RuntimeStart`]: crate::error::AppError::RuntimeStart
pub async fn serve_stdio() -> AppResult<()> {
    tracing::info!("rust-docs-mcp: starting the stdio MCP runtime (M1 skeleton)");

    // `stdio()` adapts the process's std streams to the async transport. `?`
    // here is fine: transport construction is the one failure this function
    // owns, and a startup error must reach the CLI boundary as a typed value.
    let (reader, writer) = rmcp::transport::stdio();

    let server: RunningService<RoleServer, DocsServer> = DocsServer::new()
        .serve((reader, writer))
        .await
        .map_err(|error| {
            AppError::runtime_start("the stdio transport failed to start", Some(Box::new(error)))
        })?;

    // Wait for *natural* end-of-session (transport closure / EOF / error) rather
    // than cancelling a live connection. `waiting()` consumes the running
    // service and resolves when its background task ends; a transport error
    // there is lifted to a startup failure so we never present a crashed
    // runtime as a clean exit.
    match server.waiting().await {
        Ok(reason) => {
            tracing::debug!(?reason, "stdio MCP runtime ended on client disconnect");
            Ok(())
        }
        Err(join_error) => Err(AppError::runtime_start(
            "the stdio MCP runtime task ended unexpectedly",
            Some(Box::new(join_error)),
        )),
    }
}

/// Run the skeleton until a disconnect, and translate the end reason into a
/// process exit code.
///
/// [`serve_stdio`] returns typed errors for the CLI boundary; here it folds the
/// outcome into an [`ExitCode`] for `main`, preserving the "non-zero on
/// failure" contract of task [003]'s dispatch.
pub fn serve() -> AppResult<ExitCode> {
    run_runtime(serve_stdio())
}

/// Build a multi-threaded async runtime and block on `future` to completion.
///
/// Used on the `serve` CLI dispatch path ([`serve`](crate::mcp::serve) →
/// `main`) so the skeleton runtime is constructed in exactly one place. A
/// construction failure is a startup failure, not a panic — it is mapped to
/// [`AppError::Io`](crate::error::AppError::Io) for the CLI boundary.
fn run_runtime<F>(future: F) -> AppResult<ExitCode>
where
    F: std::future::Future<Output = AppResult<()>>,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_io()
        .enable_time()
        .thread_name("rust-docs-mcp")
        .build()
        .map_err(|error| AppError::io("building the async runtime for `serve`", error))?;
    runtime.block_on(future)?;
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Tests.
//
// Protocol-version negotiation and the clean connect/disconnect lifecycle are
// proven by the *spawned-binary* integration test in `tests/stdio_lifecycle.rs`
// against the real `rust-docs-mcp serve` process over genuine stdio streams —
// the authoritative check for task [005]'s two acceptance criteria.
//
// We deliberately do *not* stand up an in-process rmcp client here: the rmcp
// `RunningService` tasks are `!Send` and bind to a `LocalSpawner`, so exercising
// `ClientConfig::serve_with_lifecycle` requires the exact runtime/scheduler setup
// rmcp uses internally and is not a reliable black-box check from this crate.
// The spawned-binary test instead drives a real client over the wire, which both
// exercises the production path and proves no non-protocol stdout or orphaned
// process. This module keeps only the transport-independent `get_info` assertion.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use rmcp::ServerHandler;

    use super::DocsServer;

    /// The handler advertises the `tools` and `resources` capabilities alongside
    /// the server identity, independent of any transport or runtime.
    #[test]
    fn get_info_advertises_identity_and_capabilities() {
        let info = DocsServer::new().get_info();
        assert_eq!(info.server_info.name.as_str(), env!("CARGO_PKG_NAME"));
        assert_eq!(info.server_info.version.as_str(), env!("CARGO_PKG_VERSION"));
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.resources.is_some());
        assert!(info.capabilities.prompts.is_none());
    }

    /// The advertised capability set is *exactly* `tools` + `resources` — no
    /// other capability is advertised (AC2, [058]). This is the "advertise only
    /// what is implemented" invariant ([plan.md](../plan.md) §6): `prompts`,
    /// `completions`, `logging`, `experimental`, and `extensions` must all be
    /// absent so a client does not expect a surface the server never dispatches.
    #[test]
    fn get_info_advertises_exactly_tools_and_resources() {
        let info = DocsServer::new().get_info();
        // Exactly the two surfaces the handler dispatches.
        assert!(
            info.capabilities.tools.is_some(),
            "the handler dispatches the five tools, so `tools` must be advertised"
        );
        assert!(
            info.capabilities.resources.is_some(),
            "the handler dispatches the two catalog/section resources, so `resources` must be advertised"
        );
        // Nothing else is advertised.
        assert!(
            info.capabilities.prompts.is_none(),
            "no prompt surface is implemented; `prompts` must stay unadvertised"
        );
        assert!(
            info.capabilities.completions.is_none(),
            "no completion surface is implemented; `completions` must stay unadvertised"
        );
        assert!(
            info.capabilities.logging.is_none(),
            "no logging surface is implemented; `logging` must stay unadvertised"
        );
        assert!(
            info.capabilities.experimental.is_none(),
            "no experimental surface is implemented; `experimental` must stay unadvertised"
        );
        assert!(
            info.capabilities.extensions.is_none(),
            "no MCP extensions are implemented; `extensions` must stay unadvertised"
        );
    }
}
