# Repository instructions

These instructions apply throughout this repository. Follow the user's current scope and any applicable instructions in more specific directories. A request to answer a question or edit documentation does not automatically start project implementation.

## Project and sources of truth

- Repository: [ThiagoEvoa/rust-mcp](https://github.com/ThiagoEvoa/rust-mcp).
- Implementation plan: [plan.md](plan.md).
- Kanban board: [Rust MCP, Project 6](https://github.com/users/ThiagoEvoa/projects/6/views/1), owned by `ThiagoEvoa`.
- Repository issues define individual deliverables, acceptance criteria, and dependencies. The board records their current lifecycle, priority, and size.

Read the plan and relevant issues before implementation. Inspect the current code, manifests, toolchain, CI, and working-tree changes; do not infer implementation progress from the plan alone. Preserve other contributors' work.

If an issue and the plan disagree, check their history and the user's latest direction. Resolve routine details independently and record consequential decisions. Ask about a material scope conflict only when the available evidence cannot resolve it. Keep approved changes consistent across the plan and affected issues.

## Selecting the next task

When asked to build this project or continue board-driven implementation:

1. Read the live board and issue state. Retrieve every page needed; do not treat a partial item list or a remembered status as the full backlog.
2. Follow the numeric implementation sequence in titles such as `[001][M1]`. This sequence is distinct from the GitHub issue number. Use the current board priorities and milestone assignments as additional context.
3. Read the candidate issue's complete body, linked dependencies, and relevant plan sections. Confirm that prerequisite deliverables are accepted and available in the working base; a closed issue or a Ready label alone is insufficient evidence.
4. Choose the lowest-numbered unfinished task whose dependencies are complete. Prefer finishing an already-started task you own before taking another. Do not take over another contributor's active task without coordinating.
5. If the earliest task is blocked, identify the blocker and proceed to the next eligible task when it is within the requested scope. Do not bypass a prerequisite by pretending it is complete.
6. Continue through eligible tasks for a project-wide build request. Stop at the requested boundary for a single-task request, or when genuinely blocked; do not expand a bounded request into the entire project.

All planned priorities are required MVP work: P0 covers bootstrap/corpus foundations, P1 retrieval/MCP behavior, and P2 packaging/acceptance. P2 does not mean optional. Keep changes small and independently verifiable; avoid combining unrelated cards into one change.

## GitHub access and board maintenance

Use available GitHub tools. If they do not support Projects, use the authenticated `gh` CLI. Discover live project, field, item, and option IDs instead of assuming cached IDs remain valid. Useful read commands are:

```sh
gh project view 6 --owner ThiagoEvoa --format json
gh project field-list 6 --owner ThiagoEvoa --format json
gh project item-list 6 --owner ThiagoEvoa --limit 200 --format json
gh issue view ISSUE_NUMBER --repo ThiagoEvoa/rust-mcp
```

Check reported totals and paginate or increase limits when necessary. Re-read the target before a mutation. Reuse existing issues and project items; GitHub automation may already have linked an issue. After a failed or interrupted write, inspect remote state before retrying so successful work is not duplicated.

When implementing a board task, maintain its status and concise verification evidence within the user's authorization. Use the board's actual workflow:

| Status | Meaning |
| --- | --- |
| Backlog | Captured work with unfinished prerequisites or unresolved readiness details. |
| Ready | Dependencies are satisfied and the acceptance criteria are actionable. |
| In progress | Implementation has actually started. |
| In review | A concrete deliverable awaits the review or integration required by the repository. |
| Done | Acceptance criteria, applicable checks, and required review/integration are satisfied. |

Move the selected card to In progress when work begins. Keep blockers visible in the issue and handoff; do not invent a Blocked option when the board has none. A blocked task that has already started remains In progress with an explicit blocker; an unstarted blocked task remains Backlog.

Do not mark Done merely because code was written, a test was started, or a local change exists while required review/integration remains pending. Keep issue closure and board status consistent when completing work. Update newly unblocked cards only after verifying their prerequisites. A milestone or broad plan checkbox is complete only when all of its required deliverables have evidence.

If GitHub access is unavailable, report the actual limitation. Continue useful local work supported by the available task requirements, record proposed status updates in the handoff, and never claim the remote board was updated. Respect the current authorization for commits, pushes, PRs, merges, and publication; board-driven implementation does not by itself require an automatic merge or release.

## Implementing a task

Before editing, identify the acceptance criteria, affected behavior, dependency contracts, and appropriate verification seam. Implement the smallest complete solution and keep its tests with the corresponding behavior. Use issue references in commits or PRs when those operations are part of the authorized workflow.

- Follow the existing architecture. Start with the single Cargo package and modules described in the plan; introduce additional crates or abstraction layers only for a concrete need.
- Use the recorded stable toolchain, edition, MSRV, and released SDK baseline. Verify version-sensitive behavior against official documentation for the selected versions. Do not silently change these baselines or add unpinned Git dependencies.
- Use typed errors and preserve useful context. External input and network/parser failures must not cause avoidable panics. Keep implementation details private unless callers need them.
- Make ownership, side effects, async cancellation, and shutdown explicit. Avoid unnecessary cloning, blocking async handlers, speculative generics, and unjustified unsafe code.
- Preserve original source wording, examples, links, and qualifications. Keep maintainer-authored retrieval hints and command effect notes separate from upstream documentation.
- Before this MCP is usable, consult the official sources linked in the plan directly. After it is usable, exercise its retrieval tools during relevant work and check local tool versions when documentation compatibility is uncertain.

## Product boundaries to preserve

The MVP is a local stdio documentation server covering `api-guidelines`, `style-guide`, `cargo`, `rustc`, `rustup`, `rustfmt`, `clippy`, and `rustdoc`.

- Keep the five planned tools: `list_sources`, `search_docs`, `get_section`, `get_api_checklist`, and `get_command_guidance`, plus the planned catalog/section resources.
- The MCP retrieves documentation. The calling agent executes Cargo and other commands through its own terminal tools. Do not add command execution, project filesystem access, code review execution, or arbitrary URL ingestion to the server as an incidental implementation choice.
- Serve queries from one immutable snapshot without network calls. Refresh through explicit sync; validate all required collections before atomic publication and preserve the last valid corpus.
- Enforce the source registry and redirect boundaries, including the specific rustfmt Markdown adapter and Clippy catalog alias. Do not execute upstream JavaScript or documentation examples.
- Preserve provenance, citations, freshness, and unknown version compatibility. Enforce the plan's input/output bounds and explicit pagination; do not silently truncate examples or fabricate missing guidance.
- Keep serve stdout exclusively for MCP messages and diagnostics on stderr. No external LLM, API key, project upload, or telemetry is required.
- Preserve upstream licensing and attribution when bundling or distributing content.

Scope changes require the user's direction or an already-approved requirement; do not silently broaden the product to finish a card.

## Verification and completion

Run the focused checks that demonstrate the task's acceptance criteria, then the applicable repository checks. Once the Cargo package and lockfile exist, the planned baseline is:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test --doc --locked
```

Follow the repository's recorded feature, target, MSRV, and lint policies. Test supported configurations affected by the change; do not assume an all-features pass proves every supported combination. Cargo tool installation and network requirements follow the agent environment's existing permissions.

Normal tests use local fixtures and temporary directories. Live upstream ingestion and real-client/agent trials are explicit integration work. Verify protocol-facing changes through the assembled stdio client seam when appropriate. Do not require Rust checks before Rust code exists or for a documentation-only change; review document consistency and links instead.

Before reporting a task complete:

1. Inspect the final diff and verify every acceptance criterion with concrete evidence.
2. Record the commands actually run, toolchain/configuration, outcomes, and material skipped checks. A skipped or failing required check is not a pass.
3. Confirm the deliverable is available to dependent work and required review/integration is complete. Leave the card In review when those steps remain.
4. Update the issue/board consistently when authorized, and verify the resulting remote state.

Finish each work session with a concise handoff: task links, changes, verification results, current board status, blockers, and the next eligible task. Make unfinished work explicit so another agent can continue without reconstructing the session.
