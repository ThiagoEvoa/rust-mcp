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

    use super::{SOURCE, ingest_chapter};
    use crate::ingest::books::discover_inventory;
    use crate::ingest::fetch::{BoundClient, FetchPolicy, Fetched, LoopbackGate};
    use crate::ingest::http_fixture::{Behavior, FixtureServer};
    use crate::model::SourceId;
    use reqwest::{StatusCode, Url};
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

    // -- AC1: compiler-option and argument syntax is preserved verbatim, with its
    //    citation (source tag + canonical anchor + content hash). The
    //    `command-line-arguments` chapter documents options like `--cfg`, `-L`,
    //    and `--check-cfg`; the exact syntax must survive in the section bodies. --

    /// A `command-line-arguments`-style chapter: compiler options and arguments
    /// documented with their exact syntax, in inline `<code>` and fenced blocks.
    const COMMAND_LINE_ARGUMENTS_HTML: &str = r#"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="https://doc.rust-lang.org/rustc/index.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="top">Command-line arguments</h1>
    <h2 id="cfg">--cfg</h2>
    <p>Configure a custom configuration value, e.g. <code>--cfg 'feature="serde"'</code> or <code>--cfg 'verbose'</code>.</p>
    <pre><code class="language-bash">rustc --cfg 'verbose' main.rs</code></pre>
    <h2 id="L">-L</h2>
    <p>Add a directory to the library search path: <code>-L KIND=PATH</code> where KIND is one of <code>dependency</code>, <code>crate</code>, <code>native</code>, <code>framework</code>, or <code>all</code>.</p>
    <pre><code class="language-bash">rustc -L dependency=/path/to/libs foo.rs</code></pre>
    <h2 id="check-cfg">--check-cfg</h2>
    <p>Emit a warning for a malformed <code>--check-cfg 'cfg(feature, values("serde"))'</code> entry.</p>
  </main>
</body></html>"#;

    #[tokio::test]
    async fn compiler_options_fixture_preserves_documented_syntax() {
        let server = FixtureServer::start(vec![(
            "/rustc/command-line-arguments.html".to_string(),
            Behavior::Text {
                body: COMMAND_LINE_ARGUMENTS_HTML.as_bytes().to_vec(),
                etag: Some("\"clargs-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!(
            "http://{}/rustc/command-line-arguments.html",
            server.addr
        ))
        .expect("a valid loopback chapter URL");

        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        // AC1a: every documented option/argument syntax survives verbatim in the
        // section bodies (inline `<code>` and fenced blocks are preserved).
        let all: String = sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            all.contains(r#"--cfg 'feature="serde""#),
            "the --cfg syntax with its value must survive verbatim:\n{all}"
        );
        assert!(
            all.contains("-L KIND=PATH"),
            "the -L KIND=PATH syntax must survive verbatim:\n{all}"
        );
        assert!(
            all.contains("-L dependency=/path/to/libs"),
            "the -L usage example must survive verbatim:\n{all}"
        );
        assert!(
            all.contains(r#"--check-cfg 'cfg(feature, values("serde"))'"#),
            "the --check-cfg syntax must survive verbatim:\n{all}"
        );

        // The syntax is retained WITH its citation: the `cfg` section is tagged
        // for the `rustc` source, its canonical URL carries the original anchor,
        // and it carries a content hash.
        let cfg = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("cfg"))
            .expect("a section carrying the `cfg` anchor must exist");
        assert_eq!(
            cfg.source_id,
            SourceId::new(SOURCE).expect("a valid source id")
        );
        assert!(
            cfg.canonical_url.as_str().contains("#cfg"),
            "the `cfg` section's canonical URL must carry its anchor, got {}",
            cfg.canonical_url
        );
        assert!(
            !cfg.content_hash.as_str().is_empty(),
            "the section must carry a non-empty content hash"
        );
    }

    // -- AC1: the test-harness documented syntax is preserved verbatim, with its
    //    citation. The "Tests" chapter documents how to write and run tests. --

    /// A "Tests"/test-harness chapter: the documented attributes and the
    /// `rustc --test` harness invocation, in inline `<code>` and fenced blocks.
    const TESTS_HTML: &str = r#"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="https://doc.rust-lang.org/rustc/index.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="top">Tests</h1>
    <h2 id="writing-tests">Writing tests</h2>
    <p>Tests are ordinary functions marked <code>#[test]</code>. A test that is expected to fail is marked <code>#[should_panic]</code>.</p>
    <pre><code class="language-rust">
#[test]
fn it_works() {
    assert_eq!(2 + 2, 4);
}
</code></pre>
    <p>Compile and run a binary's tests through the harness:</p>
    <pre><code class="language-bash">rustc --test foo.rs</code></pre>
  </main>
</body></html>"#;

    #[tokio::test]
    async fn test_harness_fixture_preserves_documented_syntax() {
        let server = FixtureServer::start(vec![(
            "/rustc/tests/index.html".to_string(),
            Behavior::Text {
                body: TESTS_HTML.as_bytes().to_vec(),
                etag: Some("\"tests-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/rustc/tests/index.html", server.addr))
            .expect("a valid loopback chapter URL");

        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        // AC1b: the test-harness documented syntax survives verbatim.
        let all: String = sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            all.contains("#[test]"),
            "the #[test] attribute must survive verbatim:\n{all}"
        );
        assert!(
            all.contains("#[should_panic]"),
            "the #[should_panic] attribute must survive verbatim:\n{all}"
        );
        assert!(
            all.contains("rustc --test"),
            "the `rustc --test` harness invocation must survive verbatim:\n{all}"
        );

        // The harness section carries its citation.
        let writing = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("writing-tests"))
            .expect("a section carrying the `writing-tests` anchor must exist");
        assert_eq!(
            writing.source_id,
            SourceId::new(SOURCE).expect("a valid source id")
        );
        assert!(
            writing.canonical_url.as_str().contains("#writing-tests"),
            "the `writing-tests` section's canonical URL must carry its anchor, got {}",
            writing.canonical_url
        );
        // AC1: the content hash for THIS asserted section too — mirroring the
        // compiler-options test, so every asserted section carries its citation.
        assert!(
            !writing.content_hash.as_str().is_empty(),
            "the `writing-tests` section must carry a non-empty content hash"
        );
    }

    // -- AC2: a nightly-only / unstable stability qualification stays VISIBLE
    //    (it survives verbatim in the section body as a callout) WITH its
    //    citation (source tag + canonical anchor + content hash) — so the
    //    qualification is provably upstream, not server-authored. --

    /// A chapter whose option carries a nightly-only stability qualification
    /// rendered as a `<blockquote>` callout.
    const STABILITY_HTML: &str = r#"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="https://doc.rust-lang.org/rustc/index.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="top">Some option</h1>
    <h2 id="opt-x">--opt-x</h2>
    <p>Do a thing.</p>
    <blockquote><strong>Stability:</strong> This option is <code>nightly</code>-only and may change without notice.</blockquote>
  </main>
</body></html>"#;

    #[tokio::test]
    async fn stability_qualification_remains_visible_with_citation() {
        let server = FixtureServer::start(vec![(
            "/rustc/command-line-arguments.html".to_string(),
            Behavior::Text {
                body: STABILITY_HTML.as_bytes().to_vec(),
                etag: Some("\"stability-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!(
            "http://{}/rustc/command-line-arguments.html",
            server.addr
        ))
        .expect("a valid loopback chapter URL");

        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        // AC2: the nightly-only qualification stays visible — it survives
        // verbatim in the section body (rendered as a callout).
        let opt = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("opt-x"))
            .expect("a section carrying the `opt-x` anchor must exist");
        assert!(
            opt.body.contains("nightly") && opt.body.contains("may change without notice"),
            "the nightly-only qualification must stay visible in the body:\n{}",
            opt.body
        );
        // AC2 also names the literal `Stability:` label the qualification is
        // rendered under; pin it too, so the visible callout label is asserted
        // alongside its substance.
        assert!(
            opt.body.contains("Stability:"),
            "the section body must carry the `Stability:` label AC2 names:\n{}",
            opt.body
        );

        // ... WITH its citation: the section is attributable to the `rustc`
        // source, its canonical URL carries the original anchor, and it carries
        // a content hash — so the qualification is provably upstream.
        assert_eq!(
            opt.source_id,
            SourceId::new(SOURCE).expect("a valid source id")
        );
        assert!(
            opt.canonical_url.as_str().contains("#opt-x"),
            "the qualification's section must carry its canonical anchor, got {}",
            opt.canonical_url
        );
        assert!(
            !opt.content_hash.as_str().is_empty(),
            "the qualification's section must carry a non-empty content hash"
        );
    }

    // -- No-panic posture on external input: a non-200 body or a chrome-only
    //    page degrades to a typed error rather than panicking. --

    #[tokio::test]
    async fn best_effort_ingest_chapter_never_panics_on_hostile_input() {
        // A non-200 response (a plain-text body, no HTML article) must not
        // panic: `ingest_chapter` does not gate on status, so it simply yields
        // no sections for a body-less page. The call returns without panicking.
        let missing = Fetched {
            requested: Url::parse("https://doc.rust-lang.org/rustc/does-not-exist.html").unwrap(),
            final_url: Url::parse("https://doc.rust-lang.org/rustc/does-not-exist.html").unwrap(),
            status: StatusCode::NOT_FOUND,
            body: b"not found".to_vec(),
            content_type: None,
            etag: None,
            last_modified: None,
            content_length: None,
        };
        let _ = ingest_chapter(&missing); // returns Ok(empty) or an error; never panics

        // A chrome-only page (no article body) fails with a typed error rather
        // than panicking — the posture the `cargo` adapter's equivalent test
        // locks in.
        let chrome = Fetched {
            requested: Url::parse("https://doc.rust-lang.org/rustc/index.html").unwrap(),
            final_url: Url::parse("https://doc.rust-lang.org/rustc/index.html").unwrap(),
            status: StatusCode::OK,
            body: r#"<html><body><nav><a>Home</a></nav><script>x</script></body></html>"#
                .as_bytes()
                .to_vec(),
            content_type: Some("text/html".to_string()),
            etag: None,
            last_modified: None,
            content_length: None,
        };
        assert!(ingest_chapter(&chrome).is_err());
    }
}
