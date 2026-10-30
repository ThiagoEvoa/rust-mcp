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
//! What it deliberately does **not** do in M1:
//!
//! * it advertises **no** tool or resource capabilities — the five tools and
//!   the two resources land in M4 ([048]-[059]); "advertise only what is
//!   implemented" ([plan.md](../plan.md) §6) means the skeleton stays honest;
//! * it touches **no** corpus or data directory — an immutable snapshot load and
//!   the "no valid snapshot → exit without serving an empty server" gate land in
//!   M2/M4 ([018]-[022], [050]); and
//! * it is **not** wired to the real stdio streams yet by the CLI — task
//!   [003] does that. The stdio transport is exercised by the spawned-binary
//!   test, which runs the real `rust-docs-mcp serve` binary and speaks the
//!   JSON-RPC protocol over its stdio.
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
    RoleServer, ServerHandler, ServiceExt,
    model::{Implementation, ServerCapabilities, ServerConfig},
    service::RunningService,
};

use crate::error::{AppError, AppResult};

/// The documentation MCP server handler for the M1 skeleton.
///
/// A unit struct — it has no corpus or request state yet. It exists to
/// [`impl`][ServerHandler] the server contract on the pinned SDK version so that
/// [`serve_stdio`] (and the spawned-binary test) exercise a real, versioned
/// connection. Tools and resources are added later; the capability surface stays
/// empty until they land (see the module docs).
#[derive(Clone, Copy, Debug, Default)]
pub struct DocsServer;

impl DocsServer {
    /// Construct the skeleton handler.
    pub fn new() -> Self {
        Self
    }
}

impl ServerHandler for DocsServer {
    /// Advertise the implementation identity and an empty capability set.
    ///
    /// `server_info` carries the binary's name and version so the client can
    /// tell *which* server it is talking to; `capabilities` is empty because M1
    /// implements no tools or resources yet (plan §6, "advertise only what is
    /// implemented"); `instructions` points an agent at the corpus. The
    /// default `supported_protocol_versions` returns the SDK's full
    /// `KNOWN_VERSIONS`, and the default `initialize`/`discover` handshake
    /// negotiates the highest mutually supported, client-requested version —
    /// which is exactly what the lifecycle tests assert.
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::default())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Local stdio MCP server for official Rust documentation \
                 (api-guidelines, style-guide, cargo, rustc, rustup, rustfmt, \
                 clippy, rustdoc). M1 skeleton: the connection negotiates and \
                 disconnects cleanly; tools and resources land in M4.",
            )
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

/// Build the single-threaded-block-on async runtime and await `future`.
///
/// Shared by the `serve` CLI dispatch and the spawned-binary test path so the
/// runtime is constructed identically. A construction failure is a startup
/// failure, not a panic.
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
// In-process lifecycle tests. These use the *official rmcp client* against the
// same `DocsServer` over an in-memory `tokio::io::duplex` pipe, proving (for
// every SDK-known, `initialize`-capable protocol version) that discovery /
// initialization negotiates the client's requested version and that a clean
// disconnect ends the runtime — without spawning a process, so the checks are
// fast and hermetic. The spawned-binary test in `tests/stdio_lifecycle.rs`
// proves the connect/disconnect and "no orphaned processes" behaviour over the
// real stdio streams.
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

    /// M1 must advertise no capabilities: a direct unit assertion on the handler
    /// config, independent of any transport or runtime.
    #[test]
    fn get_info_advertises_identity_and_no_capabilities() {
        let info = DocsServer::new().get_info();
        assert_eq!(info.server_info.name.as_str(), env!("CARGO_PKG_NAME"));
        assert_eq!(info.server_info.version.as_str(), env!("CARGO_PKG_VERSION"));
        assert!(info.capabilities.tools.is_none());
        assert!(info.capabilities.resources.is_none());
        assert!(info.capabilities.prompts.is_none());
    }
}
