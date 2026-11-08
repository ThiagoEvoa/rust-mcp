//! Ingestion of the Cargo source (task `[018]`, issue #18).
//!
//! The Cargo docs at `doc.rust-lang.org/cargo/` are one book (rustbook-
//! structured, like the API Guidelines and the Style Guide), so this adapter
//! wires the `cargo` collection to the shared pipeline in two ways:
//!
//! - **Guide / reference / appendix chapters** reuse the shared
//!   [`crate::ingest::books::discover_inventory`] (book navigation) and the
//!   shared [`split_article`](crate::ingest::section) splitter, so each h2
//!   becomes a bounded, provenance-stamped section. That is how the reference
//!   topics — workspaces, dependencies, features, targets, configuration, and the
//!   lockfile — land in the corpus as sections (issue #18, criterion 2).
//! - **Command manual pages** (`/cargo/commands/cargo-<subcommand>.html`) carry
//!   structure a generic splitter would not guarantee: a synopsis, grouped
//!   option/argument tables, fenced examples, and the `--` argument-forwarding
//!   boundary. Those are preserved by a dedicated, pure extractor
//!   ([`extract_command_manual`]) that returns a [`CommandManual`];
//!   [`ingest_command_page`] additionally materializes the full-fidelity sections
//!   and stamps each with the canonical command key, so a command is indexable
//!   alongside its synopsis, options, examples, and forwarding boundary
//!   (issue #18, criterion 1).
//!
//! The upstream [`CommandManual`] is the structured excerpt this task captures;
//! the curated, version-aware [`crate::model::CommandEntry`] catalog is built
//! later (tasks `[039]`-`[044]`). This task does *not* execute Cargo or any
//! project command — the calling agent runs `cargo …` through its own terminal
//! (`plan.md` §3, §6/Command guidance catalog). There is no network in these
//! tests: chapter and command-page bodies are served from the loopback
//! [`FixtureServer`](crate::ingest::http_fixture::FixtureServer) and
//! `discover_inventory` runs over loopback navigation, so the whole stage is
//! exercisable offline.

// Transitional: these functions are the `[018]` ingestion API, exercised by the
// offline acceptance tests below; their first non-test consumer is the `[029]+`
// sync pipeline that ingests the corpus end-to-end.
#![allow(dead_code)]

use crate::ingest::fetch::Fetched;
use crate::ingest::section::{SplitConfig, SplitError, split_article};
use crate::model::{CommandKey, SchemaError, Section, SourceId};
use reqwest::Url;
use scraper::{CaseSensitivity, ElementRef, Html, Selector};

/// The `SourceId` of the Cargo collection.
pub const SOURCE: &str = "cargo";

/// The document title shared by every Cargo Book page, used when a page's own
/// heading cannot supply one.
pub const DOC_TITLE: &str = "The Cargo Book";

/// The Cargo Book's entry point. The caller discovers the chapter inventory from
/// this entry; the extractor never hard-codes a chapter list.
pub const BOOK_URL: &str = "https://doc.rust-lang.org/cargo/";

/// A typed, non-panicking failure from ingesting a Cargo page.
///
/// Mirrors the other book adapters: a chapter split can surface a converter or
/// schema failure preserved from the shared [`split_article`] splitter. The
/// structured command-manual extraction ([`extract_command_manual`]) is a
/// best-effort parse that degrades gracefully (empty fields, not an error) on
/// hostile HTML, so it contributes no error variant of its own.
#[derive(Debug)]
pub enum IngestError {
    /// The chapter body could not be split into sections — a converter or schema
    /// failure preserved from the shared splitter.
    Split(SplitError),
    /// A section's identity could not be derived (e.g. an empty chapter path).
    Schema(SchemaError),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IngestError::Split(e) => write!(f, "cargo section split: {e}"),
            IngestError::Schema(e) => write!(f, "cargo section schema: {e}"),
        }
    }
}

impl std::error::Error for IngestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IngestError::Split(e) => Some(e),
            IngestError::Schema(e) => Some(e),
        }
    }
}

/// One option or argument in a command's option/argument group, preserving the
/// option name (aliases included, e.g. `-r, --release`) and its upstream
/// description verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionEntry {
    /// The option name and aliases, e.g. `--bin <NAME>` or `-r, --release`.
    pub name: String,
    /// The option's description, verbatim from the table's description cell.
    pub description: String,
}

/// A group of options or arguments under one heading (e.g. the "Options" table,
/// the "Arguments" table). Captured together so the option grouping is
/// preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionGroup {
    /// The heading this group came under (e.g. "Options", "Arguments").
    pub heading: String,
    /// The options/arguments in this group, in document order.
    pub options: Vec<OptionEntry>,
}

/// The structured upstream command-manual excerpt for one command page: its
/// synopsis, its grouped options/arguments, its fenced examples, and the
/// argument-forwarding boundaries (examples that forward extra arguments after a
/// `--` separator).
///
/// This is the upstream-excerpt artifact; the curated, version-aware
/// [`crate::model::CommandEntry`] catalog is built later (tasks `[039]`-`[044]`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandManual {
    /// The canonical command key (e.g. `cargo run`), from the page heading when
    /// present, else the page's chapter path. `None` when neither yields a
    /// valid key — a best-effort extraction never guesses or panics.
    pub command_key: Option<CommandKey>,
    /// The synopsis text (the "Synopsis" section, or the leading paragraphs).
    pub synopsis: String,
    /// The option/argument groups, in document order.
    pub option_groups: Vec<OptionGroup>,
    /// The fenced examples, verbatim, in document order.
    pub examples: Vec<String>,
    /// The argument-forwarding boundaries: examples that forward extra arguments
    /// after a `--` separator, preserved verbatim (a subset of [`examples`]).
    pub argument_forwarding: Vec<String>,
}

/// Derive the normalized chapter path the section identity is built from — the
/// chapter's path under the `cargo` boundary (`commands/cargo-run` from
/// `…/cargo/commands/cargo-run.html`, `getting-started` from
/// `…/cargo/guide/getting-started.html`).
///
/// A fragment is dropped (a section and its `#anchor` are one document) and a
/// missing boundary prefix falls back to the bare path stem, so an off-boundary
/// or fragment-only URL still yields a non-empty path rather than a panic.
pub fn chapter_path(final_url: &Url) -> String {
    let path = final_url.path();
    let stripped = path.strip_prefix("/cargo/").unwrap_or(path);
    let stem = stripped.trim_start_matches('/');
    stem.strip_suffix(".html").unwrap_or(stem).to_string()
}

/// The document title of a Cargo page: its `h1` when present, else its
/// `<title>`, else the chapter path, else the shared corpus title — so a
/// heading-less or chrome-stripped page still gets a sensible, non-empty title.
pub fn document_title(html: &str, final_url: &Url) -> String {
    let document = Html::parse_fragment(html);
    if let Some(h1) = document.select(&Selector::parse("h1").unwrap()).next() {
        let text = collapse_ws(&h1.text().collect::<String>());
        if !text.is_empty() {
            return text;
        }
    }
    if let Some(title) = document.select(&Selector::parse("title").unwrap()).next() {
        let text = collapse_ws(&title.text().collect::<String>());
        if !text.is_empty() {
            return text;
        }
    }
    let path = chapter_path(final_url);
    if !path.is_empty() {
        return path;
    }
    DOC_TITLE.to_string()
}

/// Ingest one Cargo Book *chapter* (guide, reference, or appendix) into its
/// bounded, provenance-stamped [`Section`]s.
///
/// `fetched` is the chapter body already fetched through the gated fetcher. The
/// body is split by the shared [`split_article`] (which keeps examples with their
/// explanation, preserves the heading hierarchy and canonical anchor, and splits
/// an oversized section into tagged continuations). No enrichment: the guide and
/// reference chapters carry no `C-…` codes and no command key, so the sections
/// are returned exactly as the splitter produced them, preserving upstream
/// content verbatim.
///
/// # Errors
///
/// Returns [`IngestError::Split`] when the body cannot be converted (a refused
/// link or an empty article) and [`IngestError::Schema`] when a section's
/// identity cannot be derived. Never panics on external input.
pub fn ingest_chapter(fetched: &Fetched) -> Result<Vec<Section>, IngestError> {
    let source = SourceId::new(SOURCE).map_err(IngestError::Schema)?;
    let chapter = chapter_path(&fetched.final_url);
    let html = String::from_utf8_lossy(&fetched.body).into_owned();
    let doc_title = document_title(&html, &fetched.final_url);

    split_article(
        &html,
        &fetched.final_url,
        &source,
        &chapter,
        &doc_title,
        &SplitConfig::default(),
    )
    .map_err(IngestError::Split)
}

/// Classify a chapter URL as a command-manual page: a page under
/// `/cargo/commands/` whose stem is `cargo-<subcommand>` (not the command index).
/// Guide/reference/appendix chapters return `false` and go through
/// [`ingest_chapter`].
pub fn is_command_page(url: &Url) -> bool {
    if !url.path().starts_with("/cargo/commands/") {
        return false;
    }
    let Some(stem) = chapter_stem(url) else {
        return false;
    };
    // `cargo-<subcommand>` with a non-empty subcommand that is not the index.
    let sub = stem.strip_prefix("cargo-").unwrap_or("");
    !sub.is_empty() && sub != "index"
}

/// Extract the structured [`CommandManual`] from a fetched command page.
///
/// The extraction is **best-effort**: it never panics on hostile HTML and
/// degrades gracefully — a missing section, an empty table, or an unparseable
/// command key yields an empty field, not an error. The result preserves the
/// synopsis, option groups, examples, and the argument-forwarding boundaries.
pub fn extract_command_manual(fetched: &Fetched) -> CommandManual {
    let html = String::from_utf8_lossy(&fetched.body).into_owned();
    parse_command_manual(&html, &fetched.final_url)
}

/// Ingest a Cargo **command manual page** into corpus sections and stamp every
/// section with the page's canonical command key, so the command is indexable
/// alongside its synopsis, options, examples, and forwarding boundary.
///
/// The full-fidelity sections come from [`ingest_chapter`]; the command key is
/// taken from the structured extraction ([`extract_command_manual`]). If the key
/// cannot be resolved, the sections are still returned without a key stamp. Each
/// stamped section has its [`content_hash`](crate::model::Section::content_hash)
/// recomputed so it describes the tagged content.
///
/// # Errors
///
/// Propagates the [`ingest_chapter`] errors; never panics on external input.
pub fn ingest_command_page(fetched: &Fetched) -> Result<Vec<Section>, IngestError> {
    let mut sections = ingest_chapter(fetched)?;
    let key = extract_command_manual(fetched).command_key;
    if let Some(key) = key {
        for section in &mut sections {
            if !section
                .command_keys
                .iter()
                .any(|k| k.as_str() == key.as_str())
            {
                section.command_keys.push(key.to_string());
                section.recompute_content_hash();
            }
        }
    }
    Ok(sections)
}

/// The pure extraction: a command page's HTML -> a [`CommandManual`]. No
/// network, no client, no panic. The `base_url` is the fallback for the command
/// key when the page has no heading.
pub fn parse_command_manual(html: &str, base_url: &Url) -> CommandManual {
    let document = Html::parse_document(html);
    let Some(body) = select_body(&document) else {
        // No article body: best-effort, only a chapter-path fallback key.
        return CommandManual {
            command_key: command_key_from_base_url(base_url),
            ..CommandManual::default()
        };
    };

    let mut manual = CommandManual::default();
    // Leading paragraphs (before the first non-synopsis heading) are synopsis;
    // a later "Synopsis" heading re-opens capture; any other h2 ends it.
    let mut in_synopsis = true;
    let mut current_group: Option<String> = None;

    for node in body.children() {
        let Some(el) = ElementRef::wrap(node) else {
            continue;
        };
        let tag = el.value().name().to_ascii_lowercase();
        match tag.as_ref() {
            "h1" => {
                if let Some(key) = normalize_command_key(&block_text(&el)) {
                    manual.command_key = Some(key);
                }
                // The command's synopsis may follow the h1, so capture stays open
                // until a non-synopsis h2; only a new group context is reset.
                current_group = None;
            }
            "h2" => match classify_section(&block_text(&el)) {
                SectionKind::Synopsis => {
                    in_synopsis = true;
                    current_group = None;
                }
                SectionKind::Options | SectionKind::Arguments => {
                    current_group = Some(block_text(&el));
                    in_synopsis = false;
                }
                SectionKind::Other => {
                    current_group = None;
                    in_synopsis = false;
                }
            },
            "p" | "ul" | "ol" | "blockquote" => {
                if in_synopsis {
                    append_text(&mut manual.synopsis, &block_text(&el));
                }
            }
            "table" => {
                if let Some(heading) = option_group_heading(&el, &current_group) {
                    manual.option_groups.push(OptionGroup {
                        heading,
                        options: parse_option_table(&el),
                    });
                }
            }
            _ if is_code_block(&el) => {
                let code = code_block_text(&el);
                if !code.trim().is_empty() {
                    if is_forwarding_example(&code) {
                        manual.argument_forwarding.push(code.clone());
                    }
                    manual.examples.push(code);
                }
            }
            _ => {}
        }
    }

    // Fall back to the chapter path for the command key when the heading gave
    // none (e.g. a heading-less page still routed from `/cargo/commands/`).
    if manual.command_key.is_none() {
        manual.command_key = command_key_from_base_url(base_url);
    }

    manual
}

/// What an h2 heading governs in a command-manual page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Synopsis,
    Options,
    Arguments,
    Other,
}

/// Classify an h2 heading by its trimmed, lowercased text.
fn classify_section(heading: &str) -> SectionKind {
    match heading.trim().to_ascii_lowercase().as_str() {
        "synopsis" => SectionKind::Synopsis,
        "options" => SectionKind::Options,
        "arguments" => SectionKind::Arguments,
        _ => SectionKind::Other,
    }
}

/// The group heading for a table: an explicit `t_options`/`t_arguments` class
/// wins (the real Cargo docs tag their tables), else the enclosing h2 heading
/// ("Options"/"Arguments"), else `None` (the table is not an option group).
fn option_group_heading(table: &ElementRef, current_group: &Option<String>) -> Option<String> {
    let cs = CaseSensitivity::CaseSensitive;
    if table.value().has_class("t_arguments", cs) {
        return Some("Arguments".to_string());
    }
    if table.value().has_class("t_options", cs) {
        return Some("Options".to_string());
    }
    current_group.clone()
}

/// True when an example forwards extra arguments after a `--` separator — it
/// contains a standalone `--` token (not a `--flag`). This is the
/// argument-forwarding boundary the plan requires preserved.
fn is_forwarding_example(code: &str) -> bool {
    code.split_whitespace().any(|token| token == "--")
}

/// Append `text` to `acc`, space-separated, skipping empty text.
fn append_text(acc: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !acc.is_empty() {
        acc.push(' ');
    }
    acc.push_str(text);
}

/// The article body element, preferring the rustbook content containers
/// (`#main-content`, `.chapter-content`) then `main`/`body`, skipping any that
/// are empty. Returns `None` when the page carries no non-empty body.
fn select_body(doc: &Html) -> Option<ElementRef<'_>> {
    for sel in &["#main-content", ".chapter-content", "main", "body"] {
        let selector = Selector::parse(sel).ok()?;
        for el in doc.select(&selector) {
            if !el.text().collect::<String>().trim().is_empty() {
                return Some(el);
            }
        }
    }
    None
}

/// True when `el` is a code block: a `pre`, or a rustbook highlight/example `div`
/// (`div.highlight`, `div.example`, `div.example-wrap`).
fn is_code_block(el: &ElementRef) -> bool {
    let tag = el.value().name().to_ascii_lowercase();
    if tag == "pre" {
        return true;
    }
    if tag == "div" {
        let cs = CaseSensitivity::CaseSensitive;
        return ["highlight", "example", "example-wrap"]
            .into_iter()
            .any(|c| el.value().has_class(c, cs));
    }
    false
}

/// The verbatim code text of a code block, preferring its inner `code` element
/// (so syntax-highlight spans are read in order), trailing whitespace trimmed.
fn code_block_text(el: &ElementRef) -> String {
    let code_sel = Selector::parse("code").unwrap();
    let text = el
        .select(&code_sel)
        .next()
        .map(|code| code.text().collect::<String>())
        .unwrap_or_else(|| el.text().collect());
    text.trim_end().to_string()
}

/// Parse a command-manual options/arguments table into option entries. The
/// header row (`th` cells only) and empty rows are skipped; each body row's
/// first cell is the option name (aliases included) and the remaining cells form
/// the description.
fn parse_option_table(table: &ElementRef) -> Vec<OptionEntry> {
    let tr_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let mut entries = Vec::new();
    for row in table.select(&tr_sel) {
        let mut cells = row.select(&td_sel);
        let Some(first) = cells.next() else {
            continue; // header row (th only) or empty row
        };
        let name = cell_text(&first);
        if name.is_empty() {
            continue;
        }
        let description = cells
            .map(|c| cell_text(&c))
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        entries.push(OptionEntry { name, description });
    }
    entries
}

/// The text of a table cell, preferring its inner `code`/`option` span so the
/// option name (e.g. `--release`) is read cleanly.
fn cell_text(cell: &ElementRef) -> String {
    let name_sel = Selector::parse("code, .option").unwrap();
    cell.select(&name_sel)
        .next()
        .map(|code| code.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cell.text().collect::<String>().trim().to_string())
}

/// The block-level text of an element, whitespace-collapsed.
fn block_text(el: &ElementRef) -> String {
    collapse_ws(&el.text().collect::<String>())
}

/// Collapse runs of whitespace to single spaces and trim — the same
/// normalization the shared converter applies.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The command-key fallback from a page URL's chapter path (e.g.
/// `/cargo/commands/cargo-new.html` -> `cargo new`).
fn command_key_from_base_url(base_url: &Url) -> Option<CommandKey> {
    let stem = chapter_stem(base_url)?;
    normalize_command_key(&stem)
}

/// The last path segment's filename stem without its extension, e.g.
/// `cargo-new` or `getting-started`.
fn chapter_stem(url: &Url) -> Option<String> {
    let name = url.path().rsplit('/').next()?;
    let stem = name
        .rsplit_once('.')
        .map(|(stem, _ext)| stem)
        .unwrap_or(name);
    Some(stem.to_string())
}

/// Normalize a raw command name into a validated [`CommandKey`]: `cargo-new` and
/// `cargo new` both become `cargo new`. An empty or operator-bearing name yields
/// `None` (never a shell-operator key, never a panic).
fn normalize_command_key(raw: &str) -> Option<CommandKey> {
    let normalized = raw
        .replace('-', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if normalized.is_empty() {
        return None;
    }
    CommandKey::new(normalized).ok()
}

// ---------------------------------------------------------------------------
// Offline acceptance tests — plan.md §11 / task [018] / issue #18
//
// Both criteria from issue #18:
//   (1) Command pages retain synopsis, option groups, examples, and the
//       argument-forwarding boundary — proven by parsing a loopback-served
//       command page into a `CommandManual` and stamping the split sections.
//   (2) The reference topics (workspaces, dependencies, features, targets,
//       configuration, lockfile) are included as sections — proven by splitting
//       a loopback-served reference chapter and asserting each topic is a section.
//
// Bodies are served from the loopback `FixtureServer` (or parsed from static
// HTML), so nothing touches the network.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! The `loopback_client` helper returns `Result<BoundClient, FetchError>`;
    //! `FetchError::Bound` is large, mirroring `ingest::fetch`'s suppression.
    #![allow(clippy::result_large_err)]

    use super::is_forwarding_example;
    use super::{
        SOURCE, chapter_path, document_title, extract_command_manual, ingest_chapter,
        ingest_command_page, is_command_page, parse_command_manual,
    };
    use crate::ingest::books::discover_inventory;
    use crate::ingest::fetch::{BoundClient, FetchPolicy, Fetched, LoopbackGate};
    use crate::ingest::http_fixture::{Behavior, FixtureServer};
    use crate::model::SourceId;
    use reqwest::Url;
    use std::sync::Arc;

    /// A loopback fetch policy with no retries so an unreachable probe fast-fails.
    fn fast_policy() -> FetchPolicy {
        FetchPolicy {
            request_timeout: std::time::Duration::from_secs(3),
            connect_timeout: std::time::Duration::from_secs(2),
            max_retries: 0,
            transient_backoff: std::time::Duration::from_millis(0),
            retry_after_max: std::time::Duration::from_secs(60),
            max_redirects: 5,
            max_concurrency: 4,
            max_body_bytes: Some(4 * 1024 * 1024),
            user_agent: Some(
                "rust-docs-mcp/0.1 (+https://github.com/ThiagoEvoa/rust-mcp)".to_string(),
            ),
        }
    }

    #[allow(clippy::result_large_err)]
    fn loopback_client() -> Result<BoundClient, crate::ingest::fetch::FetchError> {
        BoundClient::with_gate(fast_policy(), Arc::new(LoopbackGate))
    }

    /// A `cargo run` command-manual page (rustbook-shaped): a synopsis, an
    /// options table, an arguments table (the `--` forwarding row), and fenced
    /// examples including a `--` forwarding boundary.
    const CARGO_RUN_HTML: &str = r#"<!DOCTYPE html>
<html><body>
<div id="main-content" class="content">
<h1>cargo run</h1>
<p>Run a binary or example of the local package.</p>
<p>Usage: <code>cargo run [options] [-- &lt;args&gt;...]</code></p>
<h2>Options</h2>
<table class="t_options"><thead>
<tr><th>Option</th><th>Description</th></tr>
</thead><tbody>
<tr><td class="option"><code>--bin &lt;NAME&gt;</code></td><td>Run the specified binary.</td></tr>
<tr><td class="option"><code>-r, --release</code></td><td>Build artifacts in release mode, with optimizations.</td></tr>
<tr><td class="option"><code>--manifest-path &lt;PATH&gt;</code></td><td>Path to the Cargo.toml manifest.</td></tr>
</tbody></table>
<h2>Arguments</h2>
<table class="t_arguments"><thead>
<tr><th>Argument</th><th>Description</th></tr>
</thead><tbody>
<tr><td class="option"><code>--</code></td><td>All arguments after this are passed to the program.</td></tr>
</tbody></table>
<h2>Examples</h2>
<div class="example"><pre><code class="language-bash">cargo run</code></pre></div>
<div class="example"><pre><code class="language-bash">cargo run -- --help</code></pre></div>
<div class="example"><pre><code class="language-bash">cargo run --release</code></pre></div>
</div>
</body></html>
"#;

    /// A reference chapter carrying the six topics issue #18 criterion 2 requires:
    /// workspaces, dependencies, features, target, configuration, and lockfile.
    const TOPIC_CHAPTER_HTML: &str = r#"<!DOCTYPE html>
<html><body>
<div class="chapter-content">
<h1>Reference</h1>
<h2 id="workspaces">Workspaces</h2>
<p>Workspaces group packages together to share a single Cargo.lock and configuration.</p>
<h2 id="dependencies">Dependencies</h2>
<p>Dependencies are declared in the [dependencies] table and resolved by Cargo.</p>
<h2 id="features">Features</h2>
<p>Features enable optional dependencies and are unified across the graph.</p>
<h2 id="target">Target</h2>
<p>Target-specific configuration is keyed by the build or host target.</p>
<h2 id="configuration">Configuration</h2>
<p>Configuration is read from config.toml files in the workspace and home.</p>
<h2 id="the-cargo-lockfile">The Cargo Lockfile</h2>
<p>The Cargo.lock file records the exact resolved dependency graph.</p>
</div>
</body></html>
"#;

    fn fetched(url: &str, body: &str) -> Fetched {
        let url = Url::parse(url).unwrap();
        Fetched {
            requested: url.clone(),
            final_url: url,
            status: reqwest::StatusCode::OK,
            body: body.as_bytes().to_vec(),
            content_type: Some("text/html".to_string()),
            etag: None,
            last_modified: None,
            content_length: None,
        }
    }

    // -- Criterion 1: the synopsis, option groups, examples, and the
    //    argument-forwarding boundary are retained. --

    #[test]
    fn command_manual_preserves_synopsis_options_examples_and_forwarding() {
        let base = Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-run.html").unwrap();
        let manual = parse_command_manual(CARGO_RUN_HTML, &base);

        // AC1: synopsis retained.
        assert!(
            manual
                .synopsis
                .contains("Run a binary or example of the local package"),
            "synopsis not retained: {}",
            manual.synopsis
        );

        // AC1: option groups retained (Options + Arguments = two groups).
        assert_eq!(manual.option_groups.len(), 2, "expected two option groups");
        let options = &manual.option_groups[0];
        assert_eq!(options.heading, "Options");
        let names: Vec<&str> = options.options.iter().map(|o| o.name.as_str()).collect();
        assert!(names.contains(&"--bin <NAME>"), "missing --bin: {names:?}");
        assert!(
            names.contains(&"-r, --release"),
            "alias not preserved: {names:?}"
        );
        assert!(
            names.contains(&"--manifest-path <PATH>"),
            "missing --manifest-path: {names:?}"
        );
        // A description is preserved verbatim.
        let release = options
            .options
            .iter()
            .find(|o| o.name == "-r, --release")
            .expect("the --release option is present");
        assert!(release.description.contains("release mode"));

        // AC1: examples retained (three fenced examples).
        assert_eq!(manual.examples.len(), 3, "expected three examples");

        // AC1: the argument-forwarding boundary is preserved — only the example
        // with a standalone `--` token; `cargo run --release` is not forwarding.
        assert_eq!(
            manual.argument_forwarding.len(),
            1,
            "expected one forwarding example"
        );
        assert!(
            manual.argument_forwarding[0].contains("cargo run -- --help"),
            "forwarding boundary not preserved: {:?}",
            manual.argument_forwarding
        );

        // The canonical command key is derived from the page heading.
        assert_eq!(
            manual.command_key.as_ref().map(|k| k.as_str()),
            Some("cargo run")
        );
    }

    #[test]
    fn arguments_table_keeps_the_forwarding_row_as_an_option() {
        // The `--` argument row is preserved as an option in the "Arguments"
        // group (the forwarding boundary at the option-table level), not as a
        // captured example.
        let base = Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-run.html").unwrap();
        let manual = parse_command_manual(CARGO_RUN_HTML, &base);
        let arguments = &manual.option_groups[1];
        assert_eq!(arguments.heading, "Arguments");
        assert_eq!(arguments.options[0].name, "--");
        assert!(
            arguments.options[0]
                .description
                .contains("passed to the program")
        );
    }

    #[test]
    fn non_forwarding_example_is_not_captured_as_forwarding() {
        // `cargo run --release` has no standalone `--` token, so it is an
        // example but not an argument-forwarding boundary.
        assert!(!is_forwarding_example("cargo run --release"));
        assert!(is_forwarding_example("cargo run -- --help"));
        assert!(is_forwarding_example("cargo test -- --test-threads=1"));
    }

    #[test]
    fn extract_command_manual_from_fetched_page() {
        let base = Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-run.html").unwrap();
        let page = fetched(base.as_ref(), CARGO_RUN_HTML);
        let manual = extract_command_manual(&page);
        assert_eq!(
            manual.command_key.as_ref().map(|k| k.as_str()),
            Some("cargo run")
        );
        assert_eq!(manual.option_groups.len(), 2);
        assert_eq!(manual.examples.len(), 3);
        assert_eq!(manual.argument_forwarding.len(), 1);
    }

    #[test]
    fn command_key_from_chapter_path_when_no_heading() {
        // A page with a body but no `cargo …` heading falls back to the chapter
        // path stem: `/cargo/commands/cargo-new.html` -> `cargo new`.
        let html = r#"<div class="chapter-content"><p>Create a new package.</p></div>"#;
        let base = Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-new.html").unwrap();
        let manual = parse_command_manual(html, &base);
        assert_eq!(
            manual.command_key.as_ref().map(|k| k.as_str()),
            Some("cargo new")
        );
        assert!(manual.synopsis.contains("Create a new package"));
    }

    // -- Criterion 2: the reference topics are included as sections. --

    #[test]
    fn reference_chapter_includes_all_six_topics() {
        // AC2: workspace, dependency, feature, target, configuration, and
        // lockfile topics are produced as sections.
        let base = Url::parse("https://doc.rust-lang.org/cargo/reference/index.html").unwrap();
        let page = fetched(base.as_ref(), TOPIC_CHAPTER_HTML);
        let sections = ingest_chapter(&page).expect("ingests the reference chapter");

        let topics: Vec<String> = sections
            .iter()
            .filter_map(|s| s.heading_path.last().cloned())
            .collect();
        for topic in [
            "Workspaces",
            "Dependencies",
            "Features",
            "Target",
            "Configuration",
            "The Cargo Lockfile",
        ] {
            assert!(
                topics.iter().any(|t| t == topic),
                "topic not included as a section: {topic} (got {topics:?})"
            );
        }
        // Each topic section has non-empty, provenance-stamped content.
        for section in &sections {
            assert_eq!(section.source_id, SourceId::new(SOURCE).unwrap());
            assert!(!section.body.trim().is_empty());
        }
    }

    #[test]
    fn chapter_path_derives_from_cargo_prefix() {
        let path = chapter_path(
            &Url::parse("https://doc.rust-lang.org/cargo/guide/getting-started.html").unwrap(),
        );
        assert_eq!(path, "guide/getting-started");
        let command = chapter_path(
            &Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-run.html").unwrap(),
        );
        assert_eq!(command, "commands/cargo-run");
    }

    #[test]
    fn document_title_prefers_the_h1_then_the_title_tag() {
        let with_h1 = r#"<html><body><h1>cargo run</h1></body></html>"#;
        let url = Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-run.html").unwrap();
        assert_eq!(document_title(with_h1, &url), "cargo run");

        let with_title =
            r#"<html><head><title>cargo new</title></head><body><p>x</p></body></html>"#;
        assert_eq!(document_title(with_title, &url), "cargo new");
    }

    #[test]
    fn is_command_page_classifies_command_and_guide_chapters() {
        assert!(is_command_page(
            &Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-build.html").unwrap()
        ));
        assert!(is_command_page(
            &Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-new.html").unwrap()
        ));
        // The command index and guide/reference chapters are not command pages.
        assert!(!is_command_page(
            &Url::parse("https://doc.rust-lang.org/cargo/commands/index.html").unwrap()
        ));
        assert!(!is_command_page(
            &Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-index.html").unwrap()
        ));
        assert!(!is_command_page(
            &Url::parse("https://doc.rust-lang.org/cargo/guide/getting-started.html").unwrap()
        ));
    }

    #[test]
    fn ingest_command_page_stamps_sections_with_command_key() {
        let base = Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-run.html").unwrap();
        let page = fetched(base.as_ref(), CARGO_RUN_HTML);
        let sections = ingest_command_page(&page).expect("ingests the command page");
        assert!(!sections.is_empty(), "expected at least one section");
        for section in &sections {
            assert_eq!(section.source_id, SourceId::new(SOURCE).unwrap());
            assert!(
                section
                    .command_keys
                    .iter()
                    .any(|k| k.as_str() == "cargo run"),
                "section not stamped with the command key: {:?}",
                section.command_keys
            );
        }
    }

    #[test]
    fn ingest_chapter_rejects_an_all_chrome_page() {
        let base = Url::parse("https://doc.rust-lang.org/cargo/guide/overview.html").unwrap();
        let page = Fetched {
            requested: base.clone(),
            final_url: base,
            status: reqwest::StatusCode::OK,
            body: r#"<html><body><nav><a>Home</a></nav><script>x</script></body></html>"#
                .as_bytes()
                .to_vec(),
            content_type: Some("text/html".to_string()),
            etag: None,
            last_modified: None,
            content_length: None,
        };
        assert!(
            ingest_chapter(&page).is_err(),
            "an all-chrome chapter must fail with a typed error, not panic"
        );
        // The command-page ingest shares the split stage, so it fails identically.
        assert!(ingest_command_page(&page).is_err());
    }

    #[test]
    fn best_effort_extraction_never_panics_on_hostile_html() {
        // A non-200 body or a page with no article body degrades gracefully:
        // empty fields and a best-effort (or absent) key, never a panic.
        let missing = Fetched {
            requested: Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-x.html").unwrap(),
            final_url: Url::parse("https://doc.rust-lang.org/cargo/commands/cargo-x.html").unwrap(),
            status: reqwest::StatusCode::NOT_FOUND,
            body: b"not found".to_vec(),
            content_type: None,
            etag: None,
            last_modified: None,
            content_length: None,
        };
        let manual = extract_command_manual(&missing);
        // No body to parse, so the only surviving signal is the chapter-path key.
        assert_eq!(
            manual.command_key.as_ref().map(|k| k.as_str()),
            Some("cargo x")
        );
        assert!(manual.synopsis.is_empty());
        assert!(manual.option_groups.is_empty());
        assert!(manual.examples.is_empty());
        assert!(manual.argument_forwarding.is_empty());
    }

    /// A real loopback fetch of a command page through the extractor.
    #[tokio::test]
    async fn command_manual_over_loopback_fetch() {
        let server = FixtureServer::start(vec![(
            "/cargo/commands/cargo-build.html".to_string(),
            Behavior::Text {
                body: CARGO_RUN_HTML.as_bytes().to_vec(),
                etag: Some("\"cargo-run-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base = Url::parse(&format!(
            "http://{}/cargo/commands/cargo-build.html",
            server.addr
        ))
        .unwrap();
        let fetched = client
            .fetch(&base)
            .await
            .expect("loopback fetch of the command page");
        assert_eq!(fetched.status, reqwest::StatusCode::OK);
        let manual = extract_command_manual(&fetched);
        // The command key comes from the heading (`cargo run`), not the path.
        assert_eq!(
            manual.command_key.as_ref().map(|k| k.as_str()),
            Some("cargo run")
        );
        assert_eq!(manual.option_groups.len(), 2);
        assert_eq!(manual.examples.len(), 3);
        assert_eq!(manual.argument_forwarding.len(), 1);
    }

    /// A real loopback fetch of a topic reference chapter.
    #[tokio::test]
    async fn topic_chapter_over_loopback_fetch() {
        let server = FixtureServer::start(vec![(
            "/cargo/reference/index.html".to_string(),
            Behavior::Text {
                body: TOPIC_CHAPTER_HTML.as_bytes().to_vec(),
                etag: Some("\"reference-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base = Url::parse(&format!(
            "http://{}/cargo/reference/index.html",
            server.addr
        ))
        .unwrap();
        let fetched = client
            .fetch(&base)
            .await
            .expect("loopback fetch of the reference chapter");
        let sections = ingest_chapter(&fetched).expect("ingests the reference chapter");
        let topics: Vec<String> = sections
            .iter()
            .filter_map(|s| s.heading_path.last().cloned())
            .collect();
        for topic in [
            "Workspaces",
            "Dependencies",
            "Features",
            "Target",
            "Configuration",
            "The Cargo Lockfile",
        ] {
            assert!(
                topics.iter().any(|t| t == topic),
                "missing topic {topic}: {topics:?}"
            );
        }
    }

    /// The Cargo Book is book-structured, so the shared discovery books the
    /// introduction and the topical chapters over loopback navigation — the
    /// same structural, no-filename discovery the other books use.
    #[tokio::test]
    async fn discover_inventory_books_intro_and_topical_chapters() {
        let nav_html = r#"<html><body>
            <nav id="sidebar"><ol class="chapter">
              <li><a href="https://doc.rust-lang.org/cargo/index.html">Introduction</a></li>
              <li><a href="https://doc.rust-lang.org/cargo/guide/getting-started.html">Getting started</a></li>
              <li><a href="https://doc.rust-lang.org/cargo/reference/index.html">Reference</a></li>
            </ol></nav>
          </body></html>"#;
        let server = FixtureServer::start(vec![(
            "/cargo/index.html".to_string(),
            Behavior::Text {
                body: nav_html.as_bytes().to_vec(),
                etag: Some("\"cargo-index-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/cargo/index.html", server.addr)).unwrap();

        let (inv, diff) = discover_inventory(&client, &base_url, "cargo", None)
            .await
            .expect("discovery succeeds offline");

        let keys: Vec<String> = inv.urls.iter().map(|u| u.as_str().to_string()).collect();
        assert!(
            keys.iter().any(|k| k.ends_with("/cargo/index.html")),
            "the introduction (index.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/getting-started.html")),
            "a topical chapter (getting-started.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/reference/index.html")),
            "the reference chapter must be booked, got {keys:?}"
        );
        assert!(
            diff.is_complete(),
            "with no expected set the diff must be complete, got {diff:?}"
        );
    }
}
