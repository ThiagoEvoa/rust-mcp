//! [056][M4] — the server's `instructions` text, advertised on every
//! `initialize` handshake via [`crate::mcp::DocsServer::get_info`].
//!
//! This is the artifact that tells the calling agent how to use the server. It
//! documents the plan's [§7 "Agent workflow and client integration"](../../../plan.md)
//! sequence, and it draws the product's two hard boundaries:
//!
//! * **Retrieval is not execution.** The MCP *retrieves* documentation (the five
//!   tools and the catalog/section resources); the agent *executes* Rust and Cargo
//!   commands through its own terminal tools and reads their exit status and output.
//!   The server never runs a command or observes its result. (plan §6: "`get_command_guidance`
//!   ... Returns documentation and never executes the command.")
//! * **Stale/unknown compatibility is surfaced, not hidden.** Staleness is metadata
//!   that does not stop offline use; unavailable version compatibility is exposed as
//!   `unknown`, and the agent checks local tool versions and local `--help` when it is
//!   uncertain. (plan §5: "Staleness is metadata and does not prevent offline use";
//!   "expose unavailable version compatibility as unknown".)
//! * **The workflow adapts to the repository.** The agent inspects the project's own
//!   instructions, manifests, feature/target policy, toolchain, lint/formatter
//!   configuration, and CI *first*, then adapts. There is no universal checklist and
//!   no compliance guarantee. (plan §7: "There is no universal command list that
//!   proves a Rust project correct"; "The MCP provides evidence, not a guarantee that
//!   all guidance has been followed.")
//!
//! The text lives here as a single `&'static str` so it has exactly one owner and the
//! `get_info` wiring points at it by reference. The `#[cfg(test)]` module below pins
//! the acceptance-critical substrings (not the full prose) so a later rewording that
//! preserves the boundary cannot silently drop a required claim.
//!
//! This module adds no capability, no tool, and no network surface: it is the only
//! artifact of task [056]. It touches no corpus, reads no filesystem, and executes no
//! command — consistent with the empty capability set the M1/M4 skeleton advertises.

/// The `instructions` string advertised by
/// [`crate::mcp::DocsServer::get_info`].
///
/// It is a `&'static str` (not a `String`) so `get_info` can hand it to
/// `ServerConfig::with_instructions` by reference without allocation, and so the
/// `#[cfg(test)]` module can compare `get_info()`'s advertised text against this
/// exact constant.
pub const INSTRUCTIONS: &str = r#"You are a coding agent with a Rust documentation MCP server available. The server retrieves official Rust documentation only: it does not run commands, does not touch the project filesystem, and does not reach the network. Retrieve documentation from it; do everything else through your own tools.

Responsibilities: retrieval versus execution
- The MCP retrieves documentation. Its five tools (list_sources, search_docs, get_section, get_api_checklist, get_command_guidance) and its catalog/section resources return source-backed text, citations, freshness, and compatibility metadata from one immutable snapshot. The MCP does not execute commands or observe their results automatically: you run Rust and Cargo commands through your own terminal tools and inspect their exit status and output yourself.
- A fetched example or a command key such as `cargo test` is documentation about a command, never a sign the server ran it: never present a retrieved example as something this server executed.
- Distinguish upstream documentation from the separately labeled maintainer effect notes (curated command metadata) some entries carry. Effect notes describe possible side effects; they are not permission controls and they do not guarantee that a build or test command has no side effects.

Staleness and version compatibility
- Staleness is metadata, not a block. A source whose last successful upstream check is older than the configured threshold (seven days by default) is marked stale but is still served, so stale documentation remains usable offline.
- Unavailable version compatibility is exposed as unknown. Compatibility is one of stable, nightly-only, rolling, or unknown; when the upstream does not pin a version, treat it as unknown. A commit-pinned rustfmt document is not automatically documentation for an installed stable release. When compatibility is uncertain, check the local tool versions and the local --help output through your terminal before relying on a version-specific claim.

Workflow: adapt it to the repository
There is no universal command list that proves a Rust project correct: this server and you determine the applicable checks from the project. Do not treat the steps below as a checklist to run blindly, and do not assume the server guarantees compliance: the MCP provides evidence, not a guarantee that all guidance has been followed.
1. Inspect the repository first, using your filesystem tools: its own instructions, manifests and workspace membership, feature/target policy, toolchain files, lint/formatter configuration, and CI. Then adapt the remaining steps to what that project actually requires.
2. Call list_sources to learn coverage, freshness, and documentation versions; inspect local tool versions through your terminal when command compatibility matters.
3. Search relevant design or style topics with search_docs (or read a section with get_section, or read checklist entries with get_api_checklist), or fetch a known command with get_command_guidance. Read the full relevant section before relying on a short excerpt.
4. Choose commands for the actual task and the project's supported configurations, using project-specific commands and documented options. A review may use a formatting check mode; implementation may apply formatting within its authorized scope.
5. Execute through your existing terminal tools, inspect exit status and output, and fix or report issues as the task permits. The MCP does not execute commands or observe their results automatically.
6. Re-run the affected checks after your changes; report the commands you actually ran, the configurations covered, the failures, and the checks you skipped. Cite the documentation when you explain a command choice or a finding.

For library creation, additionally request API checklist entries (get_api_checklist) to guide your own design review. Installing the server alone does not make every agent consult it; these instructions decide when its tools are used."#;

// ---------------------------------------------------------------------------
// Tests.
//
// The assertions pin the acceptance-critical *substrings and keywords* of
// [INSTRUCTIONS] rather than its full prose, so a later rewording that preserves
// each required boundary still passes. They are grouped by acceptance criterion:
// AC1 (retrieval-vs-execution and stale/unknown compatibility) and
// AC2 (adapt-to-repository, no universal checklist, evidence-not-guarantee).
// The final test pins the wiring: get_info() must advertise exactly this text.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use rmcp::ServerHandler;

    use super::INSTRUCTIONS;
    use crate::mcp::DocsServer;

    // --- AC1: retrieval is distinct from execution ------------------------

    /// The instructions name documentation retrieval and terminal execution as
    /// distinct responsibilities (AC1).
    #[test]
    fn ac1_retrieval_and_execution_are_distinct() {
        assert!(
            INSTRUCTIONS.contains("retrieves documentation"),
            "must name documentation retrieval",
        );
        assert!(
            INSTRUCTIONS.contains("terminal tools"),
            "must name the agent's terminal-execution responsibility",
        );
        assert!(
            INSTRUCTIONS.contains("exit status"),
            "must tell the agent to inspect exit status / output",
        );
    }

    /// The instructions state that the MCP does not execute commands and does not
    /// observe their results (AC1).
    #[test]
    fn ac1_mcp_does_not_execute_or_observe() {
        assert!(
            INSTRUCTIONS.contains("does not execute commands"),
            "must state the MCP does not execute commands",
        );
        assert!(
            INSTRUCTIONS.contains("observe their results"),
            "must state the MCP does not observe command results",
        );
    }

    /// The instructions warn that a fetched example is not something the server ran
    /// (AC1: never present a fetched example as executed).
    #[test]
    fn ac1_fetched_examples_are_not_server_executed() {
        assert!(
            INSTRUCTIONS
                .contains("never present a retrieved example as something this server executed"),
            "must forbid presenting a fetched example as server-executed",
        );
    }

    /// The instructions say staleness is metadata and does not prevent offline use
    /// (AC1).
    #[test]
    fn ac1_staleness_is_metadata_not_a_block() {
        assert!(
            INSTRUCTIONS.contains("Staleness is metadata"),
            "must say staleness is metadata",
        );
        assert!(
            INSTRUCTIONS.contains("still served"),
            "a stale source is still served",
        );
        assert!(
            INSTRUCTIONS.contains("usable offline"),
            "stale documentation remains usable offline",
        );
    }

    /// The instructions surface unknown compatibility across stable / nightly-only /
    /// rolling / unknown and direct the agent to local versions and `--help` when
    /// uncertain (AC1).
    #[test]
    fn ac1_unknown_compatibility_is_surfaced() {
        for label in ["stable", "nightly-only", "rolling", "unknown"] {
            assert!(
                INSTRUCTIONS.contains(label),
                "must surface the compatibility label {label:?}",
            );
        }
        assert!(
            INSTRUCTIONS.contains("exposed as unknown"),
            "must say unavailable compatibility is exposed as unknown",
        );
        assert!(
            INSTRUCTIONS.contains("commit-pinned rustfmt"),
            "must note a commit-pinned rustfmt doc is not a stable-release doc",
        );
        assert!(
            INSTRUCTIONS.contains("local tool versions"),
            "must direct the agent to local tool versions when uncertain",
        );
        assert!(
            INSTRUCTIONS.contains("--help"),
            "must direct the agent to local --help when uncertain",
        );
    }

    // --- AC2: adapt to the repository; evidence, not a guarantee ----------

    /// The instructions tell the agent to inspect the repository first and adapt
    /// (AC2).
    #[test]
    fn ac2_inspect_repository_first_and_adapt() {
        assert!(
            INSTRUCTIONS.contains("Inspect the repository first"),
            "must inspect the repository first",
        );
        for token in [
            "filesystem tools",
            "workspace membership",
            "feature/target policy",
            "toolchain files",
            "lint/formatter",
            "CI",
        ] {
            assert!(
                INSTRUCTIONS.contains(token),
                "must inspect {token:?} before adapting",
            );
        }
    }

    /// The instructions distinguish upstream documentation from curated effect notes
    /// (AC2).
    #[test]
    fn ac2_upstream_documentation_vs_effect_notes() {
        assert!(
            INSTRUCTIONS.contains("upstream documentation"),
            "must name upstream documentation",
        );
        assert!(
            INSTRUCTIONS.contains("effect notes"),
            "must name the curated effect notes",
        );
        assert!(
            INSTRUCTIONS.contains("curated command metadata"),
            "must label the effect notes as curated command metadata",
        );
    }

    /// The instructions present no universal checklist and no compliance guarantee
    /// (AC2: evidence, not a guarantee).
    #[test]
    fn ac2_no_universal_checklist_no_guarantee() {
        assert!(
            INSTRUCTIONS.contains("no universal command list"),
            "must refuse to present a universal checklist",
        );
        assert!(
            INSTRUCTIONS.contains("evidence, not a guarantee"),
            "must state it provides evidence, not a guarantee",
        );
    }

    /// For library creation the agent may request API checklist entries (AC2).
    #[test]
    fn ac2_library_creation_may_request_checklist() {
        assert!(
            INSTRUCTIONS.contains("API checklist entries"),
            "must allow requesting API checklist entries for library creation",
        );
        assert!(
            INSTRUCTIONS.contains("get_api_checklist"),
            "must name the get_api_checklist tool",
        );
    }

    // --- Wiring: get_info() advertises exactly this text ------------------

    /// `get_info()` must advertise exactly the reworked instructions text, so the
    /// text the client receives on `initialize` is the text pinned above (not a stale
    /// M1 placeholder).
    #[test]
    fn get_info_advertises_the_reworked_instructions() {
        let info = DocsServer::new().get_info();
        assert_eq!(
            info.instructions.as_deref(),
            Some(INSTRUCTIONS),
            "get_info() must advertise the reworked INSTRUCTIONS",
        );
        // [057] wires the handler dispatch, so the capability surface now
        // advertises the tools and resources the server implements (plan §6,
        // "advertise only what is implemented"); this task still reworks only the
        // instructions text.
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.resources.is_some());
    }
}
