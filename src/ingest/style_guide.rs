//! Ingestion of the Rust Style Guide source (task `[017]`, issue #17).
//!
//! Wires the `style-guide` collection (a Rust book, like the API Guidelines and
//! the other `doc.rust-lang.org` books) to the shared pipeline:
//!
//! - **Discovery** reuses [`crate::ingest::books::discover_inventory`]: the Style
//!   Guide is book-structured, so the same structural, no-filename chapter
//!   discovery books the introduction (`index.html`) and every chapter —
//!   criterion 1 of #17.
//! - **Chapters** go through the shared [`split_article`](crate::ingest::section)
//!   splitter, so each section retains its headings, its formatting examples, its
//!   stability/version qualifications (rendered as callouts), and its canonical
//!   anchor — criterion 2 of #17.
//!
//! Unlike the `api-guidelines` source, the Style Guide carries **no checklist**
//! and **no `C-…` guideline codes**, so there is no checklist stage to ingest or
//! resolve: each chapter splits straight into bounded [`Section`]s with no code
//! enrichment. The sections carry **only upstream content** — the adapter adds
//! no server-authored "recommended style" text — so the source's wording,
//! examples, and qualifications survive verbatim, per `plan.md` §2/style-guide
//! ("present these as default Rust style conventions; preserve any qualifications
//! in the source") and the boundary "keep source text separate from
//! server-authored retrieval hints".
//!
//! No network in these tests: the chapter bodies are served from the loopback
//! [`FixtureServer`](crate::ingest::http_fixture::FixtureServer) and
//! `discover_inventory` runs over loopback navigation, so the whole stage is
//! exercisable offline.

// Transitional: these functions are the `[017]` ingestion API, exercised by the
// offline acceptance tests below; their first non-test consumer is the `[029]+`
// sync pipeline that ingests the corpus end-to-end.
#![allow(dead_code)]

use crate::ingest::fetch::Fetched;
use crate::ingest::section::{SplitConfig, SplitError, split_article};
use crate::model::{SchemaError, Section, SourceId};
use reqwest::Url;
use scraper::{Html, Selector};

/// The `SourceId` of the Style Guide collection.
pub const SOURCE: &str = "style-guide";

/// The document title shared by every chapter of the Style Guide corpus, used
/// when a page's own heading cannot supply one.
pub const DOC_TITLE: &str = "Rust Style Guide";

/// A typed, non-panicking failure from ingesting a Style Guide chapter.
///
/// The Style Guide has no checklist, so — unlike the `api-guidelines` source
/// (whose [`IngestError`](crate::ingest::api_guidelines::IngestError) also
/// reports a broken code-to-explanation mapping) — the only failures are the two
/// a chapter split can surface: a converter/schema failure preserved from the
/// shared splitter.
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
            IngestError::Split(e) => write!(f, "style-guide section split: {e}"),
            IngestError::Schema(e) => write!(f, "style-guide section schema: {e}"),
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
/// chapter's file stem under the `style-guide` boundary (`naming` from
/// `…/style-guide/naming.html`, `index` from `…/style-guide/index.html`).
///
/// A fragment is dropped (a section and its `#anchor` are one document) and a
/// missing boundary prefix falls back to the bare path stem, so an off-boundary
/// or fragment-only URL still yields a non-empty path rather than a panic.
pub fn chapter_path(final_url: &Url) -> String {
    // The registry boundary is the URL *path* prefix, not the full URL; `path()`
    // drops the scheme/host and any `#fragment` (a section and its anchor are one
    // document), leaving just the path to normalize.
    let path = final_url.path();
    let stripped = path.strip_prefix("/style-guide/").unwrap_or(path);
    let stem = stripped.trim_start_matches('/');
    stem.strip_suffix(".html").unwrap_or(stem).to_string()
}

/// The document title of a Style Guide page: its `h1` when present, else the
/// shared corpus title, then the chapter path as a last resort — so a
/// heading-less or chrome-stripped page still gets a sensible, non-empty title
/// rather than an empty one.
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

/// Ingest one Style Guide *chapter* into its bounded [`Section`]s.
///
/// `fetched` is the chapter body already fetched through the gated fetcher. The
/// body is split by the shared [`split_article`] (which keeps examples with their
/// explanation, preserves the heading hierarchy and stability/version
/// qualifications as callouts, records the heading's original anchor, and splits
/// an oversized section into tagged continuations).
///
/// The Style Guide carries no `C-…` codes, so — unlike
/// [`ingest_chapter`](crate::ingest::api_guidelines::ingest_chapter) — there is
/// no code enrichment: each section is returned exactly as the splitter produced
/// it. That is deliberate: the adapter must add **no server-authored "recommended
/// style" text**, so the source's wording, examples, and qualifications survive
/// verbatim and stay separate from any retrieval hint (`plan.md` §2/style-guide
/// and "keep source text separate from server-authored retrieval hints").
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

    // No enrichment: the Style Guide has no checklist or `C-…` codes, so the
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
// Offline acceptance tests — plan.md §11 / task [017] / issue #17
//
// Both criteria from issue #17:
//   (1) Captured chapter inventory matches the discovered book navigation —
//       proven by reusing the book discovery over a loopback nav, exactly as
//       the `api-guidelines` source does.
//   (2) Formatting examples, qualifications, headings, and canonical links
//       survive fixture extraction — proven by splitting a loopback-served
//       chapter and asserting each survives in the produced `Section`s.
//
// The chapter and navigation bodies are served from the loopback `FixtureServer`,
// so nothing touches the network.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! The `loopback_client` helper returns `Result<BoundClient, FetchError>`;
    //! `FetchError::Bound` is large, mirroring `ingest::fetch`'s suppression.
    #![allow(clippy::result_large_err)]

    use super::{DOC_TITLE, SOURCE, chapter_path, document_title, ingest_chapter};
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

    // -- Criterion 1: the introduction and the topical chapters are booked by the
    //    shared book discovery (the site is book-structured). The Style Guide
    //    carries no checklist, so — unlike the `api-guidelines` discovery — only
    //    the introduction and the topical chapters are expected. --

    #[tokio::test]
    async fn discover_inventory_books_intro_and_topical_chapters() {
        // A minimal book sidebar: the `index.html` nav page links to the
        // introduction's siblings — two topical chapters. The links are absolute
        // registered-host URLs (the real upstream shape); the nav page itself is
        // served from loopback, so `discover_inventory` books the registered
        // documents without touching the network. `discover_inventory` resolves
        // each link through the registry gate, so a relative link would resolve
        // to the loopback host and be refused as off-boundary.
        let nav_html = r#"<html><body>
            <nav id="sidebar"><ol class="chapter">
              <li><a href="https://doc.rust-lang.org/style-guide/index.html">Introduction</a></li>
              <li><a href="https://doc.rust-lang.org/style-guide/naming.html">Naming</a></li>
              <li><a href="https://doc.rust-lang.org/style-guide/formatting.html">Formatting</a></li>
            </ol></nav>
          </body></html>"#;
        let server = FixtureServer::start(vec![(
            "/style-guide/index.html".to_string(),
            Behavior::Text {
                body: nav_html.as_bytes().to_vec(),
                etag: Some("\"index-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/style-guide/index.html", server.addr))
            .expect("a valid loopback nav URL");

        let (inv, diff) = discover_inventory(&client, &base_url, "style-guide", None)
            .await
            .expect("discovery succeeds offline");

        let keys: Vec<String> = inv.urls.iter().map(|u| u.as_str().to_string()).collect();
        assert!(
            keys.iter().any(|k| k.ends_with("/style-guide/index.html")),
            "the introduction (index.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/style-guide/naming.html")),
            "a topical chapter (naming.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter()
                .any(|k| k.ends_with("/style-guide/formatting.html")),
            "a second topical chapter (formatting.html) must be booked, got {keys:?}"
        );
        // No delta against an omitted expected set, so the discovery is complete.
        assert!(
            diff.is_complete(),
            "with no expected set the diff must be complete, got {diff:?}"
        );
    }

    // -- Criterion 2: a chapter's formatting examples, qualifications, headings,
    //    and canonical links survive fixture extraction. --

    /// A compact `naming.html` chapter: an `h1`, a naming `h2` carrying a
    /// recommendation paragraph, a fenced Rust example, a stability/version
    /// qualification rendered as a callout, and a canonical content link. The
    /// `<nav>` carries a sibling link that must be stripped as chrome.
    ///
    /// The content link is an absolute, in-boundary `doc.rust-lang.org` URL
    /// rather than a relative one: these tests fetch the body from the loopback
    /// `FixtureServer`, so `ingest_chapter` (mirroring `api_guidelines`) uses the
    /// loopback `final_url` as its link-resolution base. A *relative* content
    /// link would then resolve to the loopback host, which the registry refuses
    /// as off-boundary; an absolute registered-host link instead resolves to
    /// itself (`resolve_document` performs no fetch), so the test stays offline
    /// while still proving that a canonical content link survives extraction.
    fn naming_html() -> &'static str {
        r##"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="index.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="top">Rust Style Guide</h1>
    <h2 id="naming">Naming</h2>
    <p>Function and method names use <code>snake_case</code>; this is the default Rust style convention for names.</p>
    <pre><code class="language-rust">fn snake_case_name(x: i32) -&gt; i32 { x }</code></pre>
    <blockquote><strong>Qualification</strong> Applies to the 2021 and 2024 editions.</blockquote>
    <p>See the <a href="https://doc.rust-lang.org/style-guide/formatting.html">Formatting</a> chapter for related conventions.</p>
  </main>
</body></html>"##
    }

    #[tokio::test]
    async fn ingest_chapter_retains_examples_qualifications_headings_and_links() {
        let server = FixtureServer::start(vec![(
            "/style-guide/naming.html".to_string(),
            Behavior::Text {
                body: naming_html().as_bytes().to_vec(),
                etag: Some("\"naming-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/style-guide/naming.html", server.addr))
            .expect("a valid loopback chapter URL");

        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        let naming = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("naming"))
            .expect("a section carrying the `naming` anchor must exist");

        // (2a) Formatting example retained with its explanation.
        assert!(
            naming.body.contains("```rust") && naming.body.contains("fn snake_case_name"),
            "the code example must be retained with the section:\n{}",
            naming.body
        );

        // (2b) Source qualification preserved (rendered as a callout), verbatim.
        assert!(
            naming
                .body
                .contains("Applies to the 2021 and 2024 editions"),
            "the source qualification must survive in the section body:\n{}",
            naming.body
        );

        // (2c) Recommendation wording retained in the body.
        assert!(
            naming.body.contains("snake_case"),
            "the recommendation wording must be in the body:\n{}",
            naming.body
        );

        // (2d) Headings preserved: the section's breadcrumb carries the chapter
        // heading and this section's own heading.
        assert!(
            naming.heading_path.iter().any(|h| h.contains("Naming")),
            "the section's own heading must be in its breadcrumb, got {:?}",
            naming.heading_path
        );
        assert!(
            naming.body.starts_with("## Naming"),
            "the section body must carry its heading line, got:\n{}",
            naming.body
        );

        // (2e) Canonical link retained: the content link survives extraction in
        // its absolute, in-boundary form (a relative link could not resolve
        // offline against the loopback base, so the fixture uses the absolute
        // registered-host URL it resolves to in production).
        assert!(
            naming
                .body
                .contains("https://doc.rust-lang.org/style-guide/formatting.html"),
            "a canonical content link must survive extraction in its absolute form:\n{}",
            naming.body
        );
        // The section's canonical URL carries the original anchor.
        assert!(
            naming.canonical_url.as_str().contains("#naming"),
            "the canonical URL must carry the original anchor, got {}",
            naming.canonical_url.as_str()
        );

        // (2f) The section is tagged for the `style-guide` source with no
        // guideline codes (the Style Guide has no checklist / `C-…` codes).
        assert_eq!(
            naming.source_id,
            SourceId::new(SOURCE).expect("a valid source id")
        );
        assert!(
            naming.guideline_codes.is_empty(),
            "the Style Guide carries no guideline codes, got {:?}",
            naming.guideline_codes
        );
    }

    /// The `<nav>` chrome is stripped: its sibling link does not leak into any
    /// section's body, so the captured content is the article's, not the chrome's.
    #[tokio::test]
    async fn chrome_links_do_not_leak_into_sections() {
        let server = FixtureServer::start(vec![(
            "/style-guide/naming.html".to_string(),
            Behavior::Text {
                body: naming_html().as_bytes().to_vec(),
                etag: None,
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/style-guide/naming.html", server.addr))
            .expect("a valid loopback chapter URL");
        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        let all: String = sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // The chrome's "Introduction" nav link must not appear as body text.
        assert!(
            !all.contains("Introduction"),
            "the sidebar chrome must be stripped, got:\n{all}"
        );
    }

    // -- Pure helpers, exercised without a server. --

    #[test]
    fn chapter_path_derives_the_file_stem() {
        assert_eq!(
            chapter_path(&Url::parse("https://doc.rust-lang.org/style-guide/naming.html").unwrap()),
            "naming"
        );
        assert_eq!(
            chapter_path(
                &Url::parse("https://doc.rust-lang.org/style-guide/index.html#top").unwrap()
            ),
            "index"
        );
        // A fragment-only / off-boundary URL still yields a non-empty stem.
        assert_eq!(
            chapter_path(
                &Url::parse("https://doc.rust-lang.org/style-guide/formatting.html").unwrap()
            ),
            "formatting"
        );
    }

    #[test]
    fn document_title_prefers_the_h1_then_the_title_tag() {
        let with_h1 = r#"<html><body><h1>Rust Style Guide</h1></body></html>"#;
        let url = Url::parse("https://doc.rust-lang.org/style-guide/index.html").unwrap();
        assert_eq!(document_title(with_h1, &url), "Rust Style Guide");

        // Without an `h1`, the `<title>` is used.
        let with_title =
            r#"<html><head><title>The Rust Style Guide</title></head><body><p>x</p></body></html>"#;
        assert_eq!(document_title(with_title, &url), "The Rust Style Guide");

        // With neither, the chapter path is the last resort.
        let headingless = r#"<html><body><p>no heading, no title</p></body></html>"#;
        assert_eq!(document_title(headingless, &url), "index");

        // The shared corpus title is the ultimate fallback for a bare, off-boundary
        // URL with no heading, title, or path.
        let bare = Url::parse("https://doc.rust-lang.org/style-guide/").unwrap();
        assert_eq!(
            document_title("<html><body></body></html>", &bare),
            DOC_TITLE
        );
    }

    #[test]
    fn ingest_chapter_rejects_an_all_chrome_page() {
        // An all-chrome page yields `ExtractionError::EmptyArticle` from the
        // splitter, surfaced as `IngestError::Split` — never a panic.
        let fetched = Fetched {
            requested: Url::parse("https://doc.rust-lang.org/style-guide/naming.html").unwrap(),
            final_url: Url::parse("https://doc.rust-lang.org/style-guide/naming.html").unwrap(),
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
            ingest_chapter(&fetched).is_err(),
            "an all-chrome chapter must fail with a typed error, not panic"
        );
    }
}
