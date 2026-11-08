//! Ingestion of the rustc book source (task `[019]`, issue #19).
//!
//! Wires the `rustc` collection (The rustc Book, an mdBook site like the API
//! Guidelines, the Style Guide, and the other `doc.rust-lang.org` books) to the
//! shared pipeline:
//!
//! - **Discovery** reuses [`crate::ingest::books::discover_inventory`]: the
//!   rustc Book is book-structured, so the same structural, no-filename chapter
//!   discovery books the introduction (`index.html`) and every chapter — the
//!   command-line options, codegen options, the jobserver, lints, JSON output,
//!   the test harness, targets, and the rest. Its static inventory is the
//!   `toc.html` navigation page; the live sidebar is JS-populated, so discovery
//!   stays offline and structural — `discover_inventory` reads the link
//!   structure, it executes **no** upstream JavaScript.
//! - **Chapters** go through the shared [`split_article`](crate::ingest::section)
//!   splitter, so each section retains its headings, its examples (including the
//!   compiler-option and test-harness examples), its stability/version
//!   qualifications (rendered as callouts), and its canonical anchor — like the
//!   Style Guide, with no checklist or `C-…` codes to resolve.
//!
//! The rustc Book documents the *compiler* (`rustc`): its command-line
//! arguments, codegen options, lints, and the test harness. This server
//! **retrieves that documentation**; it never *runs* the compiler. The calling
//! agent executes `rustc` and its flags through its own terminal tools, so the
//! adapter adds no command execution, project access, or code-review execution —
//! per `plan.md`'s product boundary ("The MCP retrieves documentation. The
//! calling agent executes Cargo and other commands through its own terminal
//! tools").
//!
//! The chapters carry **only upstream content** — the adapter adds no
//! server-authored retrieval hints — so the source's wording, examples, and
//! stability/version qualifications survive verbatim and stay separate from any
//! retrieval hint ("keep source text separate from server-authored retrieval
//! hints").
//!
//! No network in these tests: the chapter and navigation bodies are served from
//! the loopback [`FixtureServer`](crate::ingest::http_fixture::FixtureServer)
//! and `discover_inventory` runs over loopback navigation, so the whole stage is
//! exercisable offline.

// Transitional: these functions are the `[019]` ingestion API, exercised by the
// offline acceptance tests below; their first non-test consumer is the `[029]+`
// sync pipeline that ingests the corpus end-to-end.
#![allow(dead_code)]

use crate::ingest::fetch::Fetched;
use crate::ingest::section::{SplitConfig, SplitError, split_article};
use crate::model::{SchemaError, Section, SourceId};
use reqwest::Url;
use scraper::{Html, Selector};

/// The `SourceId` of the rustc Book collection.
pub const SOURCE: &str = "rustc";

/// The document title shared by every chapter of the rustc Book corpus, used
/// when a page's own heading cannot supply one.
pub const DOC_TITLE: &str = "The rustc Book";

/// The entry URL of the rustc Book — the collection the discovery stage books
/// from the static `toc.html` navigation page.
pub const BOOK_URL: &str = "https://doc.rust-lang.org/rustc/";

/// A typed, non-panicking failure from ingesting a rustc Book chapter.
///
/// Like the Style Guide, the rustc Book has no checklist, so — unlike the
/// `api-guidelines` source (whose
/// [`IngestError`](crate::ingest::api_guidelines::IngestError) also reports a
/// broken code-to-explanation mapping) — the only failures are the two a chapter
/// split can surface: a converter/schema failure preserved from the shared
/// splitter.
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
            IngestError::Split(e) => write!(f, "rustc section split: {e}"),
            IngestError::Schema(e) => write!(f, "rustc section schema: {e}"),
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

/// Derive the normalized chapter path the section identity is built from — the
/// chapter's file stem under the `rustc` boundary (`command-line-arguments`
/// from `…/rustc/command-line-arguments.html`, `index` from
/// `…/rustc/index.html`).
///
/// A fragment is dropped (a section and its `#anchor` are one document) and a
/// missing boundary prefix falls back to the bare path stem, so an off-boundary
/// or fragment-only URL still yields a non-empty path rather than a panic.
pub fn chapter_path(final_url: &Url) -> String {
    // The registry boundary is the URL *path* prefix, not the full URL; `path()`
    // drops the scheme/host and any `#fragment` (a section and its anchor are one
    // document), leaving just the path to normalize.
    let path = final_url.path();
    let stripped = path.strip_prefix("/rustc/").unwrap_or(path);
    let stem = stripped.trim_start_matches('/');
    stem.strip_suffix(".html").unwrap_or(stem).to_string()
}

/// The document title of a rustc Book page: its `h1` when present, else its
/// `<title>`, else the chapter path, else the shared corpus title as a last
/// resort — so a heading-less or chrome-stripped page still gets a sensible,
/// non-empty title rather than an empty one.
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
    // Last resort: the chapter path, so a heading-less or chrome-stripped page
    // still gets a sensible, non-empty title rather than a bare corpus constant.
    let path = chapter_path(final_url);
    if !path.is_empty() {
        return path;
    }
    DOC_TITLE.to_string()
}

/// Ingest one rustc Book *chapter* into its bounded [`Section`]s.
///
/// `fetched` is the chapter body already fetched through the gated fetcher. The
/// body is split by the shared [`split_article`] (which keeps examples with their
/// explanation — including the compiler-option, codegen, and test-harness
/// examples — preserves the heading hierarchy and stability/version
/// qualifications as callouts, records the heading's original anchor, and splits
/// an oversized section into tagged continuations).
///
/// Like the Style Guide, the rustc Book carries no checklist and no `C-…`
/// guideline codes, so — unlike
/// [`ingest_chapter`](crate::ingest::api_guidelines::ingest_chapter) — there is
/// no code enrichment: each section is returned exactly as the splitter produced
/// it. That is deliberate: the adapter must add **no server-authored retrieval
/// hint**, so the source's wording, examples, and qualifications survive verbatim
/// and stay separate from any retrieval hint (`plan.md` §2/rustc and "keep
/// source text separate from server-authored retrieval hints").
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

    // No enrichment: the rustc Book has no checklist or `C-…` codes, so the
    // sections are returned exactly as the splitter produced them, preserving
    // upstream content verbatim.
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

/// Collapse runs of whitespace to single spaces and trim — the same normalization
/// the shared converter applies, so a heading-less page's title matches the
/// converter's output.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Offline acceptance tests — plan.md §11 / task [019] / issue #19
//
// Criterion 1: the captured chapter inventory matches the discovered book
// navigation — proven by reusing the book discovery over a loopback `toc.html`
// nav, exactly as the `style-guide` and `cargo` sources do. The compiler-option
// and test-harness fixtures that exercise `ingest_chapter` arrive in a later
// increment; this one wires the discovery.
//
// The navigation body is served from the loopback `FixtureServer`, so nothing
// touches the network.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! The `loopback_client` helper returns `Result<BoundClient, FetchError>`;
    //! `FetchError::Bound` is large, mirroring `ingest::fetch`'s suppression.
    #![allow(clippy::result_large_err)]

    use crate::ingest::books::discover_inventory;
    use crate::ingest::fetch::{BoundClient, FetchPolicy, LoopbackGate};
    use crate::ingest::http_fixture::{Behavior, FixtureServer};
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

    // -- Criterion 1: the introduction and the topical chapters are booked by the
    //    shared book discovery (the site is book-structured). The rustc Book
    //    carries no checklist, so — like the `style-guide` discovery — only the
    //    introduction and the topical chapters are expected. --

    #[tokio::test]
    async fn discover_inventory_books_intro_and_topical_chapters() {
        // The rustc Book's static chapter inventory is its `toc.html` navigation
        // page. The live sidebar is JS-populated, but discovery stays structural
        // and offline: `discover_inventory` reads the `<a href>` links from the
        // page's *structure* (it executes no upstream JavaScript). The links are
        // absolute, registered-host `doc.rust-lang.org/rustc/…` URLs (the real
        // upstream shape); the nav page itself is served from loopback, so the
        // chapters are booked without touching the network. A *relative* link
        // would resolve to the loopback host and be refused as off-boundary.
        let nav_html = r#"<html><body>
            <nav id="sidebar"><ol class="chapter">
              <li><a href="https://doc.rust-lang.org/rustc/index.html">Introduction</a></li>
              <li><a href="https://doc.rust-lang.org/rustc/command-line-arguments.html">Command-line arguments</a></li>
              <li><a href="https://doc.rust-lang.org/rustc/tests/index.html">Test harness</a></li>
            </ol></nav>
          </body></html>"#;
        let server = FixtureServer::start(vec![(
            "/rustc/toc.html".to_string(),
            Behavior::Text {
                body: nav_html.as_bytes().to_vec(),
                etag: Some("\"rustc-toc-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/rustc/toc.html", server.addr))
            .expect("a valid loopback nav URL");

        let (inv, diff) = discover_inventory(&client, &base_url, "rustc", None)
            .await
            .expect("discovery succeeds offline");

        let keys: Vec<String> = inv.urls.iter().map(|u| u.as_str().to_string()).collect();
        assert!(
            keys.iter().any(|k| k.ends_with("/rustc/index.html")),
            "the introduction (index.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter()
                .any(|k| k.ends_with("/rustc/command-line-arguments.html")),
            "a topical chapter (command-line-arguments.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/rustc/tests/index.html")),
            "a second topical chapter (tests/index.html) must be booked, got {keys:?}"
        );
        // No delta against an omitted expected set, so the discovery is complete.
        assert!(
            diff.is_complete(),
            "with no expected set the diff must be complete, got {diff:?}"
        );
    }
}
