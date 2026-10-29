# Rust Documentation MCP — Project Plan

Date: 2026-10-28  
Status: Implementation plan; no application code exists yet.

## 1. Goal and confirmed scope

Build a Model Context Protocol (MCP) server in Rust that gives coding agents searchable, attributable access to official Rust design, style, and tooling documentation while they implement or review Rust projects.

The user confirmed:

- Run locally using the stdio transport.
- Start with documentation search and retrieval.
- Include official command and configuration documentation for Cargo, rustc, rustup, rustfmt, Clippy, and rustdoc alongside the original two guides.

The server supplies relevant documentation; the calling agent interprets it, writes code, and runs its own development tools. The server does not need an LLM, embeddings service, API key, or access to the agent's project files.

### Success scenario

An agent designing a library searches for conversion naming, reads the relevant API guideline with its examples, and cites the original section. When implementing or reviewing the project, it retrieves documentation for the applicable build, formatting, lint, test, and documentation commands, then executes them through its own terminal tools. It can also look up dependency, workspace, feature, target, and toolchain configuration. These documentation queries work offline using an identified snapshot.

## 2. Documentation sources and interpretation

| Source ID | Entry point | Coverage and interpretation |
| --- | --- | --- |
| `api-guidelines` | [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/about.html) | Include the introduction, checklist, and topical chapters. Preserve guideline codes such as `C-CONV`. These are API design recommendations, with particular relevance to library authors. |
| `style-guide` | [Rust Style Guide](https://doc.rust-lang.org/style-guide/index.html) | Include the introduction and chapters discovered through book navigation. Present these as default Rust style conventions; preserve any qualifications in the source. |
| `cargo` | [The Cargo Book](https://doc.rust-lang.org/cargo/) | Include the guide, reference, command manual, and appendices: project creation, manifests, workspaces, dependencies, features, targets, profiles, configuration, build/test/doc commands, lockfiles, packaging, and publishing. |
| `rustc` | [The rustc book](https://doc.rust-lang.org/rustc/) | Include compiler usage, options, diagnostics guidance, lints, targets, and test-harness documentation. Preserve stability qualifications; prefer Cargo for Cargo-managed projects. |
| `rustup` | [The rustup book](https://rust-lang.github.io/rustup/) | Include installation, toolchains, overrides, components, targets, and toolchain-file behavior. Record its independent documentation revision. |
| `rustfmt` | [Official rustfmt README](https://github.com/rust-lang/rustfmt#readme) and [configuration reference](https://rust-lang.github.io/rustfmt/) | Include `cargo fmt`/`rustfmt` usage, check mode, configuration, editions, and option stability. Ingest official Markdown sources at a recorded revision when the rendered site requires JavaScript. |
| `clippy` | [Clippy book](https://doc.rust-lang.org/clippy/) and [stable lint catalog](https://clippy.rust-lang.org/stable/index.html) | Include installation, usage, configuration, Cargo integration, lint groups, and individual lint explanations/examples. Retain lint IDs, known issues, and version/stability metadata where supplied. |
| `rustdoc` | [The rustdoc book](https://doc.rust-lang.org/rustdoc/) | Include documentation generation, Cargo integration, documentation tests, links, configuration, and documented command options. |

Treat book URLs as entrances to their complete chapter inventories, rather than indexing only the landing pages. The [API checklist](https://rust-lang.github.io/api-guidelines/checklist.html) provides an inventory of guideline codes; the [Cargo command index](https://doc.rust-lang.org/cargo/commands/index.html) supplies command discovery. Keep all eight source collections in the MVP, with explicit adapters for non-book material.

Keep source text separate from server-authored retrieval hints. Do not turn recommendations into universal requirements, claim every guideline applies to every project, or present this corpus as a complete Rust language reference. The Style Guide explains formatting conventions; rustfmt documentation explains how to use the formatter. Tool documentation supports command selection, but cannot establish that a command is appropriate for an unseen project.

### Discovery boundary

- Maintain a fixed source registry with permitted hosts, path prefixes, formats, entry points, and parser adapters. Discover chapters from book navigation, commands from official indexes, and individual lint sections from the Clippy catalog.
- Allow `doc.rust-lang.org/{style-guide,cargo,rustc,clippy,rustdoc}/` and `rust-lang.github.io/{api-guidelines,rustup,rustfmt}/` only within those exact path prefixes.
- Allow the Clippy catalog at `clippy.rust-lang.org/stable/`. Treat the legacy `rust-lang.github.io/rust-clippy/stable/` redirect as an explicit source-specific alias, not permission to crawl unrelated paths.
- For rustfmt, allow only the official repository's `README.md` and `Configurations.md` Markdown files at a resolved commit under `raw.githubusercontent.com/rust-lang/rustfmt/`; cite the corresponding commit-pinned GitHub file and heading. Do not crawl GitHub issues, arbitrary repository files, or follow the repository's links as new sources.
- Resolve relative links, normalize URLs, deduplicate fragments, and reject redirects outside the configured source boundaries. Record the requested and final URLs.
- Exclude other Rust books, general crate documentation, search scripts, themes, assets, and duplicate print editions. Preserve external hyperlinks without crawling them. Do not execute upstream JavaScript to ingest documentation; use an explicit official static-source adapter where needed.
- Capture each collection's document inventory in the snapshot. Fail an update when an expected document cannot be fetched or parsed; report added or removed documents for inspection. All eight required collections must validate before publishing a complete replacement snapshot.

## 3. MVP requirements

### Functional requirements

1. Publish a source catalog with coverage, snapshot identity, freshness, and provenance.
2. Search all eight sources by natural-language terms, Rust identifiers, headings, guideline codes, command names, flags, and lint IDs.
3. Filter search by source and return bounded results with useful excerpts and canonical URLs.
4. Retrieve complete logical sections, including code examples and links, through server-issued identifiers.
5. Retrieve API checklist entries with links to their explanatory sections.
6. Work offline from a bundled or previously synchronized snapshot.
7. Provide an explicit local command to refresh documentation and inspect update failures.
8. Expose the same corpus through MCP resources where supported, while keeping every essential operation available through tools.
9. Retrieve cited command guidance, including documented syntax, options, examples, prerequisites, and toolchain/stability qualifications.
10. Distinguish upstream documentation from curated command metadata, and expose unavailable version compatibility as unknown.

### Outside the MVP

- Reading, reviewing, editing, generating, or executing a caller's Rust code.
- Running Cargo, Clippy, rustfmt, or downloaded documentation examples.
- Hosted HTTP transport, authentication, multi-user operation, or a web interface.
- Arbitrary URL ingestion, collections outside the eight approved sources, and general crate API documentation.
- Semantic search, vector databases, generated answers, and automatic compliance scores.

## 4. Architecture and technical choices

Use one Cargo package with a binary and a small internal library. Keep protocol handling, corpus ingestion, and retrieval as modules; separate crates are unnecessary at this scale.

```text
Official documentation sites
         |
         | explicit `rust-docs-mcp sync`
         v
Discover -> Fetch -> Extract -> Validate -> Publish snapshot atomically
                                              |
Bundled fallback snapshot ---------------------+
                                              v
Agent / MCP client <--- stdio ---> MCP handlers -> Corpus + in-memory search
```

### Core choices

| Concern | Proposed choice | Reason |
| --- | --- | --- |
| MCP implementation | Official [`rmcp` Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) | Use the SDK's protocol, schema, lifecycle, and transport support. |
| Runtime | Tokio | Fits the official SDK and bounded asynchronous fetching. |
| Transport | stdio | Confirmed local deployment; client launches the binary. |
| Corpus format | Versioned JSON metadata plus normalized Markdown sections | Inspectable and portable across the eight official source collections. |
| Search | In-memory lexical index with BM25 ranking and explicit code/command/title boosts | Offline and deterministic; benchmark against the expanded corpus, including the lint catalog. |
| Updates | Explicit CLI sync into a new immutable snapshot | Network failures never interrupt queries against the active corpus. |
| Initial availability | Bundle a validated snapshot after upstream license checks | First launch can work without network access. |

Pin a released SDK version during implementation and commit `Cargo.lock`. Record the Rust toolchain, edition, and minimum supported Rust version after checking actual dependency requirements. Use stable Rust and edition 2024 if the selected dependency set supports that baseline; do not depend on an unpinned Git branch.

The [MCP specification](https://modelcontextprotocol.io/specification/2026-07-28) and the SDK must be checked together when implementing. Record supported protocol versions and test compatibility with the intended client. Let the SDK handle version-specific discovery or initialization rather than inventing a custom handshake.

### Alternatives considered

- **Fetch upstream on every query:** simple initially, but introduces network latency, outages, and changing answers within a session. Choose local snapshots instead.
- **Resources only:** useful for clients that expose document resources, but agent discovery varies by client. Choose tools for the primary workflow and resources for direct context attachment.
- **Embeddings or a full search service:** potentially useful for a larger corpus, but adds operational and evaluation costs. Start with lexical search and measure retrieval quality before expanding.

## 5. Ingestion, provenance, and freshness

### Ingestion pipeline

1. Use the source registry to fetch book navigation, command indexes, the Clippy catalog, and the approved rustfmt Markdown files; construct bounded document manifests.
2. Download pages with connection/request timeouts, a descriptive user agent, limited concurrency, response-size limits, and bounded retries for transient failures. Honor upstream rate-limit signals.
3. Extract the main article or parse the approved Markdown; remove navigation, scripts, styling, and repeated book chrome. Preserve command synopsis/options tables, stability callouts, and lint examples/known issues.
4. Convert the article to Markdown while preserving heading hierarchy, source anchors, lists, tables, links, and fenced Rust examples. Resolve relative links against the original page.
5. Split on semantic section boundaries. Keep examples with their surrounding explanation. Split oversized sections at paragraph or code-block boundaries with explicit continuation metadata.
6. Extract API guideline codes, command references, Clippy lint IDs, and documented stability/version information. Validate cross-references and curated command-to-section mappings; do not guess missing mappings or flag availability.
7. Validate chapter coverage, nonempty content, section identity, anchor mappings, checksums, and output limits.
8. Write a candidate snapshot, build its index, then publish it atomically only if all checks pass. Keep the previous valid snapshot.

Use recorded HTML and Markdown fixtures for book layouts, command manuals, rustfmt configuration, and the Clippy catalog. The rustfmt options site may render its content dynamically, so an empty HTML shell is not a successful ingestion. Do not assume identical layouts across sources. A changed layout must produce a visible sync failure instead of silently publishing an empty corpus. Set separate bounded download limits for ordinary chapters and large catalogs, then split catalogs into logical entries before serving them.

### Data model

Store at least:

- `SnapshotManifest`: schema version, corpus snapshot ID, creation time, parser version, source inventories, validation summary, and license/attribution references.
- `Source`: source ID, title, entry URL, discovered document URLs, last successful check time, fetched time, content hash, channel, documentation release when identifiable, upstream revision, and parser adapter. Preserve unknown values explicitly.
- `Section`: section ID, source ID, document title, heading path, canonical URL including original anchor, Markdown body, guideline codes/command keys/lint IDs when present, stability/version qualifications, and content hash.
- `ChecklistEntry`: upstream code, title, category, checklist location, and explanatory section IDs.
- `CommandEntry`: canonical command key, purpose, upstream section IDs, documented syntax/example excerpts, known prerequisites and stability/version qualifications, plus separately labeled maintainer-authored effect notes.

Derive logical section IDs from source ID plus normalized chapter path and original anchor; distinguish continuations deterministically. Return IDs to callers rather than asking them to construct IDs or URLs. Snapshot identity disambiguates section content across updates.

A content hash identifies the captured content; it is not a Rust compiler version or proof of an upstream publication date. Expose unavailable upstream version information as unknown.

The MVP captures current stable-distribution tooling documentation and the rolling or repository documentation of independently maintained tools. It does not promise historical documentation for every installed toolchain. Preserve nightly-only qualifications within stable manuals. Distinguish `stable`, `nightly-only`, `rolling`, and `unknown` where supported by evidence; a commit-pinned rustfmt document is not automatically documentation for an installed stable release. Agent instructions require checking local tool versions and local `--help` when compatibility is uncertain.

### Runtime behavior

- On startup, use the latest valid synchronized snapshot, otherwise the bundled snapshot. Rebuild the search index in memory and measure its cost against the complete expanded corpus.
- Keep one snapshot fixed for the process lifetime. After sync, restart the server to activate it; no hot reload or change notifications are needed in the MVP.
- Serve reads without network calls. Queries do not trigger updates.
- Mark a source stale when its last successful upstream check is older than seven days; make the threshold configurable. Staleness is metadata and does not prevent offline use.
- When no valid snapshot exists, exit with an actionable message on stderr directing the operator to run sync. Do not start an apparently healthy empty server.
- Distinguish download time, last check time, and known upstream modification time. A conditional HTTP success may update last-check metadata without changing content identity.

### Licensing gate

Before committing or distributing upstream content, inspect each source repository's applicable licenses and attribution requirements, including code examples. Preserve required notices in the snapshot and distribution. If bundling cannot be completed, document the exception and require a first local sync rather than silently omitting attribution or promising offline first launch.

## 6. MCP interface

All MVP tools are read-only against the active snapshot. Network updates remain a CLI operation. Use typed input/output schemas, descriptive tool help, and appropriate read-only annotations from the selected SDK. Annotations supplement the actual access restrictions.

### Tools

| Tool | Input | Output |
| --- | --- | --- |
| `list_sources` | Empty object | Source catalog, scope, snapshot ID, timestamps, stale flags, release/channel/revision metadata, document/section counts, and command-catalog discovery URI. |
| `search_docs` | `query`; optional `source`, `limit`, and opaque `cursor` | Ranked hits with section IDs, headings, excerpts, applicable guideline codes/command keys/lint IDs, URLs, compatibility metadata, snapshot ID, and next cursor. |
| `get_section` | `section_id`; optional opaque continuation `cursor` | Section Markdown, heading context, citations, source freshness, snapshot ID, and continuation information. |
| `get_api_checklist` | Optional upstream `category`, `limit`, and opaque `cursor` | Checklist entries with original codes, titles, and explanatory section IDs/URLs. Omitted category means all categories, paginated. |
| `get_command_guidance` | Exact `command` catalog key; optional opaque continuation `cursor` | Cited syntax, relevant option sections and examples, prerequisites, compatibility qualifications, separately labeled effect notes, snapshot metadata, and continuation information. Returns documentation and never executes the command. |

Define `source` as one of `api-guidelines`, `style-guide`, `cargo`, `rustc`, `rustup`, `rustfmt`, `clippy`, or `rustdoc`; omission searches all sources. Return valid checklist categories through tool help or checklist metadata. Unknown categories, malformed IDs, and unsupported source values produce clear validation errors.

### Command guidance catalog

Provide source-backed entries for these initial command families; refine their exact keys against the fetched documentation during implementation:

- Project setup and inspection: `cargo new`, `cargo init`, `cargo metadata`, `cargo locate-project`, and `cargo tree`.
- Dependencies and reproducibility: `cargo add`, `cargo remove`, `cargo update`, `cargo generate-lockfile`, `cargo fetch`, and relevant feature, manifest, and lockfile references.
- Implementation and verification: `cargo check`, `cargo build`, `cargo run`, `cargo test`, `cargo bench`, `cargo fmt`, `cargo clippy`, `cargo doc`, and `cargo fix`.
- Packaging and release preparation: `cargo package` and `cargo publish`, including documented dry-run behavior. Documentation lookup does not authorize publishing.
- Toolchains and diagnostics: `rustup show`, documented toolchain/override/component/target command families, `rustc`, `rustfmt`, and `rustdoc` usage, including their documented help/version mechanisms.

The command catalog resource lists supported keys with short purposes and source references, paginated within the response limit. A key such as `cargo test` selects documentation, not a shell command line. Reject unsupported keys with discoverable alternatives; do not accept shell scripts or interpolate input into a process. Exact command and lint queries must also work through `search_docs`.

Document the meaning and applicability of workspace/package selection, target selection, feature configuration, `--locked`, `--offline`, and argument forwarding after `--` only for commands that support them. Distinguish `cargo fmt` options from rustfmt options and Cargo options from Clippy/compiler options. Explain nightly requirements without silently switching the caller's toolchain.

Separate upstream excerpts from maintained notes about source changes, lockfile changes, artifact writes, possible dependency downloads, project-code execution, toolchain changes, or publication. These notes describe possible effects for specific commands/options; they are not permission controls or an assurance that build/test commands have no side effects.

### Retrieval and response rules

- Preserve tokens such as `as_`, `to_`, `into_`, `Send`, `Sync`, `C-CONV`, `cargo test`, `--no-default-features`, and `clippy::unwrap_used` during indexing.
- Rank exact guideline-code, command-name, and lint-ID matches first, then boost headings and identifier matches over body text. Keep a large lint catalog from crowding out command-usage answers when the query names a command. Use deterministic tie-breaking and a documented alias map for common wording differences.
- Return only source-backed excerpts. Empty results are a successful search with zero hits; suggest trying a different term without fabricating an answer.
- Default to five search hits; cap at twenty. Limit queries to 1,000 UTF-8 bytes and reject blank queries.
- Cap complete serialized tool responses at 32 KiB. Account for JSON escaping and metadata; paginate or shorten excerpts with explicit truncation metadata. Never silently clip a code example and present it as complete.
- Bind cursors to operation, filters, and snapshot; reject malformed or mismatched cursors. Returned continuation pages preserve context and source attribution.
- Include `snapshot_id`, `source_id`, canonical URL, and freshness metadata in retrieval responses. Use structured output with a concise text representation compatible with the supported client.
- Map invalid protocol parameters and missing resources according to the negotiated MCP version; use tool execution errors for operational retrieval failures. Avoid exposing internal paths or stack traces.

### Resources

Expose a catalog resource and bounded Markdown section resources, for example:

```text
rust-docs://catalog
rust-docs://snapshots/{snapshot_id}/commands
rust-docs://snapshots/{snapshot_id}/sections/{section_id}
```

Ensure IDs use URI-safe encoding. The catalog and tool results advertise concrete URIs; callers do not need to infer them. Advertise concrete command-catalog page URIs when pagination is needed. Validate exact known identities rather than mapping resource paths onto arbitrary filesystem paths. Paginate resource discovery; split large content during ingestion so each section resource remains bounded.

Advertise only implemented capabilities. Resource subscriptions, prompts, sampling, elicitation, and long-running tasks are unnecessary for this version. The [tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools) and [resources](https://modelcontextprotocol.io/specification/2026-07-28/server/resources) specifications define the protocol contracts; the table above defines this project's proposed application API.

## 7. Agent workflow and client integration

Document this sequence in the README and server instructions:

1. Inspect repository instructions, manifests/workspace membership, feature/target policy, toolchain files, lint/formatter configuration, and CI using the agent's filesystem tools.
2. Call `list_sources` to understand coverage, freshness, and documentation versions. Inspect local tool versions through the agent's terminal when command compatibility matters.
3. Search for relevant design/style topics or use `get_command_guidance` for a known command. Read full relevant sections before relying on short excerpts.
4. Choose commands for the actual task and supported configurations, using project-specific commands and documented options. A review may use formatting check mode; implementation may apply formatting within its authorized scope.
5. Execute through the agent's existing terminal tools, inspect exit status/output, and fix or report issues as the task permits. The MCP does not execute commands or observe their results automatically.
6. Re-run affected checks after changes; report commands actually run, configurations covered, failures, and skipped checks. Cite documentation when explaining command choices or findings.

An implementation/review skill can use this workflow to cover project creation, dependency changes, builds, linting, testing, doctests, documentation generation, MSRV/feature/target checks, and release preparation. There is no universal command list that proves a Rust project correct: the skill and agent determine applicable checks from the project. Treat repository modifications, toolchain installation, execution, and publication according to the calling agent's existing permissions.

For library creation, the agent can additionally request API checklist entries to guide its own design review. The MCP provides evidence, not a guarantee that all guidance has been followed. Client instructions determine when tools are used; installing the server alone cannot ensure every agent consults it.

Provide a generic client configuration example with an absolute executable path and `serve` arguments, then verify an example for the first client selected during implementation. No client-specific integration is required to begin development; the SDK test client establishes the initial contract.

## 8. CLI, configuration, and project layout

Proposed binary name: `rust-docs-mcp`.

```text
rust-docs-mcp serve [--data-dir PATH]
rust-docs-mcp sync [--data-dir PATH]
rust-docs-mcp status [--data-dir PATH]
```

- `serve` launches stdio MCP and uses the selected immutable snapshot.
- `sync` performs bounded network ingestion, reports changes, and publishes only a validated snapshot. Serialize concurrent writers with a lock.
- `status` reports available snapshots, selected corpus, freshness, and validation status without network access.
- Default to an OS-appropriate application data directory; allow an explicit path for tests and portable installations.
- Keep stdout exclusively for MCP traffic during `serve`. Send tracing and diagnostics to stderr, with configurable verbosity. Avoid logging query bodies by default.

Suggested initial layout:

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
src/
  main.rs             # CLI, configuration, startup, shutdown
  lib.rs              # Internal application entry points
  mcp.rs              # Tool/resource handlers and protocol error mapping
  corpus.rs           # Snapshot loading, validation, IDs, bounded section reads
  search.rs           # Tokenization, ranking, pagination
  commands.rs         # Source-backed command catalog and guidance reads
  ingest/
    mod.rs            # Allowed-source fetching, validation, publication
    books.rs          # Book navigation and article extraction
    rustfmt.rs        # Approved upstream Markdown adapter
    clippy.rs         # Lint-catalog entry extraction
  model.rs            # Corpus and tool contract types
data/
  bundled/            # Validated fallback corpus and attribution
tests/
  fixtures/           # Representative upstream HTML and small test corpus
  ingestion.rs
  retrieval.rs
  commands.rs
  stdio.rs
README.md
THIRD_PARTY_NOTICES.md
.github/workflows/ci.yml
```

Use `serde`/`serde_json` for corpus data and tool payloads; use the SDK-compatible schema crate, a bounded HTTP client such as `reqwest`, and an HTML parser such as `scraper`. Select an HTML-to-Markdown conversion approach by testing preservation of actual upstream examples. Add CLI, tracing, hashing, and error-handling dependencies only as needed; record exact versions and compatibility during bootstrap.

## 9. Reliability and trust boundaries

- Documentation is reference data. Strip executable HTML and never interpret fetched text as server instructions or execute embedded examples.
- Restrict network access in sync to the eight registered source collections and their explicit static-source adapters, including every redirect. Tool inputs cannot supply network URLs or filesystem paths.
- Validate schema versions, checksums, IDs, and bounded sizes before loading a snapshot. A checksum detects corruption; it does not authenticate an untrusted corpus.
- Restrict writes to the configured data directory. Use temporary files and atomic publication, preserve the last valid snapshot, and recover cleanly after interrupted sync.
- Bound download concurrency, document size, query size, result size, and indexing work. Respect cancellation and stdio EOF; shut down without orphaned background work.
- Serve concurrent requests against immutable data. Keep blocking parsing/index work outside latency-sensitive async handlers where necessary.
- No project upload, telemetry, external LLM calls, or credentials are required for the MVP.

## 10. Implementation milestones

Complete these in dependency order. Each milestone should leave a demonstrable result.

### M1 — Bootstrap and confirm compatibility

- [x] Select released SDK/dependency versions, Rust toolchain, edition, MSRV, and supported protocol versions. Recorded in [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md): rmcp 3.8.0 / rmcp-macros 3.8.0, edition 2024, MSRV 1.88, stable toolchain; protocol `KNOWN_VERSIONS` (latest 2026-07-28, no `initialize`); first client is the in-process SDK test client, with real-world clients still to test.
- [ ] Create the Cargo package, CLI skeleton, typed errors, and stderr tracing.
- [ ] Establish a stdio lifecycle/discovery smoke test using the official SDK client.
- [ ] Inspect upstream content licenses and define the bundled snapshot attribution policy.

Exit: a client can connect to the server and disconnect cleanly; no diagnostic output contaminates stdout.

### M2 — Build a trustworthy documentation corpus

- [ ] Register all eight sources, allowed paths/aliases, document formats, and bounded fetching policies.
- [ ] Implement book/command discovery, rustfmt Markdown ingestion, and Clippy lint-catalog extraction.
- [ ] Extract semantic Markdown sections with preserved examples and original anchors.
- [ ] Parse API checklist entries and validate their explanatory targets.
- [ ] Implement manifests, hashes, atomic publication, rollback, status, and bundled fallback.
- [ ] Preserve source/tooling release, channel, revision, stability, and unknown-compatibility metadata.
- [ ] Review complete inventories and representative extracted sections for all eight sources.

Exit: all eight collections have validated coverage; command tables, lint explanations, and code examples survive extraction; failed updates leave the previous corpus usable; offline loading works.

### M3 — Implement search and section retrieval

- [ ] Build the in-memory lexical index and exact guideline/command/lint lookup.
- [ ] Build and validate the source-backed command catalog, including citations, compatibility qualifications, and effect notes.
- [ ] Implement source filters, stable ranking, section reads, response bounds, and cursors.
- [ ] Add a small hand-labeled retrieval evaluation set before tuning ranking.

Exit: the retrieval acceptance cases below pass with correct citations and intact examples.

### M4 — Expose the MCP contract

- [ ] Implement all five tools with typed schemas and documented errors.
- [ ] Expose the catalog and section resources from the same corpus operations.
- [ ] Test discovery, calls, pagination, invalid input, resource reads, EOF, and offline operation through a spawned stdio process.
- [ ] Verify the negotiated protocol behavior against the chosen SDK and client.

Exit: an agent-compatible client completes the search-to-section workflow without direct filesystem access.

### M5 — Package and document

- [ ] Add README setup, build/install, client configuration, update, offline, and troubleshooting instructions.
- [ ] Add CI checks, distribution notices, and reproducible bundled-snapshot regeneration instructions.
- [ ] Run end-to-end agent trials for library design, project implementation, and review, using command guidance and the agent's own terminal execution.
- [ ] Record measured search latency, corpus size, and known retrieval limitations.

Exit: a fresh installation can answer design/style and tooling queries offline, and an operator can refresh and inspect its corpus. An agent trial demonstrates retrieving command guidance, running relevant checks through terminal tools, and reporting actual outcomes.

## 11. Validation and acceptance criteria

### Behavioral tests

- Discovery covers all eight source inventories and stays within registered paths, including redirects and rustfmt static-source adapters.
- Fixtures retain nested headings, anchors, lists, tables, command syntax/options, stability callouts, relative links, lint known issues, and fenced examples; navigation and scripts are absent.
- An exact `C-CONV` query returns its upstream explanation first.
- Queries about conversion names, errors in API documentation, naming conventions, indentation, and trailing commas retrieve the appropriate source sections within the first five hits.
- Queries about workspace tests, feature selection, locked/offline builds, dependencies, toolchain pinning, formatter check mode, Clippy usage, compiler options, and doctests retrieve the relevant official sections within the first five hits.
- Exact command names and lint IDs resolve to the intended documentation; `cargo clippy` retrieves usage guidance rather than only individual lint examples.
- Command guidance preserves source links, documented syntax/options, argument-forwarding boundaries, and version/stability qualifications. Unsupported commands/flags do not acquire fabricated guidance.
- Command input containing shell operators is rejected; a test at the handler boundary confirms command lookup never launches a subprocess. Upstream examples are never executed during ingestion.
- Fresh stable documentation is not presented as verified compatibility with an older caller toolchain. Unknown or repository-only release metadata remains explicit.
- Source filters, checklist categories, duplicate headings, empty results, unknown IDs, invalid cursors, and oversized inputs behave predictably.
- Every successful hit resolves to retrievable content and a source URL; checklist entries resolve to explanations.
- Network failure, malformed HTML, missing chapters, corrupt snapshots, and interrupted publication preserve the last valid corpus and report actionable errors.
- With outbound network disabled, a fresh binary containing its bundled corpus completes source listing, search across all eight sources, section reads, checklist retrieval, and command guidance retrieval.
- MCP schemas and serialized outputs agree; tool and resource reads return the same section content and snapshot identity.
- All tool responses respect the complete serialized size cap and make continuation explicit.

Maintain at least forty hand-labeled queries spanning all eight sources, including exact guideline codes, command names, lint IDs, and ordinary agent phrasing. Include at least three queries per source and at least twelve command/workflow queries overall. Require the labeled relevant section in the top five for at least 90% of queries, and first place for every exact guideline-code, canonical command-name, and lint-ID case. Keep the query set distinct from ranking aliases to avoid merely testing hardcoded examples.

Provisional performance targets: after startup, search and section reads have p95 latency below 100 ms on a documented development machine, and startup completes within two seconds for the full corpus. Measure with a fixed corpus and harness; revise targets explicitly if evidence warrants it.

### Rust checks once implementation exists

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test --doc --locked
```

Run the agreed feature configuration and an MSRV check in CI. Keep normal tests offline with local fixtures and temporary directories; upstream ingestion checks are explicit integration jobs rather than mandatory live-network unit tests. Apply the project's own documentation sources to its public APIs and use typed errors without panicking on external input.

## 12. Remaining choices and future scope

No further product clarification blocks this plan. The following implementation choices can be resolved from evidence during M1–M2:

- First target MCP client and its configuration format.
- Exact released dependency versions, MSRV, and protocol compatibility matrix.
- HTML conversion implementation and snapshot attribution details.
- Initial distribution method and supported build targets.

After the MVP, consider historical version-specific documentation, further official Rust sources, measured improvements to search recall, or HTTP deployment only when requested. An external review/implementation skill can use the MVP as its documentation provider immediately. Server-side code review or command execution would be separate capabilities with explicit project-access requirements and their own validation criteria.

## References

- [Rust API Guidelines: About](https://rust-lang.github.io/api-guidelines/about.html)
- [Rust API Guidelines: Checklist](https://rust-lang.github.io/api-guidelines/checklist.html)
- [Rust Style Guide: Introduction](https://doc.rust-lang.org/style-guide/index.html)
- [The Cargo Book](https://doc.rust-lang.org/cargo/)
- [Cargo command reference](https://doc.rust-lang.org/cargo/commands/index.html)
- [The rustc book](https://doc.rust-lang.org/rustc/)
- [The rustup book](https://rust-lang.github.io/rustup/)
- [Official rustfmt usage documentation](https://github.com/rust-lang/rustfmt#readme)
- [rustfmt configuration reference](https://rust-lang.github.io/rustfmt/)
- [Official rustfmt configuration Markdown](https://github.com/rust-lang/rustfmt/blob/main/Configurations.md)
- [Clippy documentation](https://doc.rust-lang.org/clippy/)
- [Clippy usage](https://doc.rust-lang.org/clippy/usage.html)
- [Clippy stable lint catalog](https://clippy.rust-lang.org/stable/index.html)
- [The rustdoc book](https://doc.rust-lang.org/rustdoc/)
- [Official Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [MCP specification, 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28)
- [MCP tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
- [MCP resources](https://modelcontextprotocol.io/specification/2026-07-28/server/resources)

References checked on 2026-10-28. Architecture, limits, commands, and acceptance targets in this document are project proposals unless identified as confirmed scope or attributed to an upstream source.
