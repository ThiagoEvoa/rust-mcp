//! Clippy ingest adapter: per-entry extraction from the [Clippy lint
//! catalog](https://clippy.rust-lang.org/stable/) (`[024]`).
//!
//! The Clippy book proper (the `/stable/book/` chapters) is handled by
//! [`crate::ingest::clippy_book`]. This module owns the **other** Clippy surface:
//! the lint catalog — a single, large page that lists every lint. It splits that
//! one page into individual, cited **lint sections**, one per lint, each retaining
//! the lint's canonical lint ID (`clippy::unwrap_used`), its group and default
//! level, its explanation and examples, any "known issues", and its added-in /
//! applicability metadata.
//!
//! The catalog is a single HTML page (`clippy.rust-lang.org/stable/index.html`)
//! that renders every lint as an `<article id="lint_name">`. Per-lint detail
//! pages do not exist (they 404), so the one page **is** the corpus; the
//! extractor splits it into per-lint sections. The server-side-rendered HTML
//! already contains every lint, so **no upstream JavaScript is executed** — the
//! extractor parses the static HTML directly.
//!
//! The catalog is large (well over an ordinary chapter), so it has its own,
//! larger download cap ([`LINT_CATALOG_MAX_BYTES`]) separate from the ordinary
//! chapter cap. Each per-lint section that the extractor produces is small, so
//! the chapter body limit ([`crate::ingest::section`]) still bounds the
//! assembled corpus.
//!
//! Mirrors [`crate::ingest::cargo`]'s pattern: a pure, independently testable
//! extractor ([`extract_lint_catalog`]) produces structured
//! [`LintEntry`] data, and [`ingest_lint_catalog`] materializes full-fidelity
//! [`Section`]s from it.
//!
//! The extraction is best-effort and never panics on malformed HTML: a lint with
//! no usable id is skipped, and an unparseable section degrades to a text-only
//! body rather than aborting the whole catalog.
//!
//! Like the sibling ingest modules, this stage is transitional: its first
//! consumer is the sync pipeline that lands in `[011]`, so the items are
//! dead until then.

#![allow(dead_code)] // transitional: first consumer is the [011] sync pipeline

use crate::ingest::fetch::{BoundClient, FetchError, Fetched};
use crate::ingest::html::convert_article;
use crate::model::SchemaError;
use crate::model::{Section, SectionBuilder, SourceId, Url as ModelUrl};
use reqwest::Url;
use scraper::{ElementRef, Html, Selector};

/// The Clippy source id (catalog lint sections and book sections share it; the
/// book sections live in [`crate::ingest::clippy_book`]).
const SOURCE: &str = "clippy";

/// The canonical document title for catalog lint sections.
const DOC_TITLE: &str = "Clippy Lints";

/// The chapter path shared by every catalog lint section.
const CHAPTER_PATH: &str = "clippy-catalog";

/// The Clippy lint catalog is a single, large HTML page (it lists every lint),
/// so its download cap is larger than an ordinary chapter's cap. Each per-lint
/// section the extractor produces is small; this cap bounds the *page
/// download*, not individual sections.
pub const LINT_CATALOG_MAX_BYTES: usize = 16 * 1024 * 1024;

/// A single Clippy lint, extracted as structured data (before it is
/// materialized into a [`Section`]).
///
/// `lint_id` is the bare lint name (the in-page anchor, e.g. `unwrap_used`);
/// `canonical_lint_id` is the canonical lint key (`clippy::unwrap_used`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintEntry {
    /// The bare lint name; also the in-page anchor.
    pub lint_id: String,
    /// The canonical lint key, e.g. `clippy::unwrap_used`.
    pub canonical_lint_id: String,
    /// The lint's group, when the catalog states one (e.g. `restriction`,
    /// `style`).
    pub group: Option<String>,
    /// The lint's default level, when the catalog states one (`allow`, `warn`,
    /// `deny`).
    pub level: Option<String>,
    /// The lint's documentation as full-fidelity Markdown (explanation,
    /// examples, configuration, past names — whatever the catalog rendered).
    pub body: String,
    /// The "known issues"/"known false positives" content, when present.
    pub known_issues: Vec<String>,
    /// The version the lint was added in, when the catalog states one.
    pub added_in: Option<String>,
    /// The lint's applicability, when the catalog states one.
    pub applicability: Option<String>,
}

impl LintEntry {
    /// The breadcrumb heading path for this lint's section: the group (when
    /// present) with the lint name as the leaf.
    fn heading_path(&self) -> Vec<String> {
        match &self.group {
            Some(group) => vec![group.clone(), self.lint_id.clone()],
            None => vec![self.lint_id.clone()],
        }
    }

    /// The stability/version metadata for this lint's section, when present.
    fn qualifications(&self) -> Vec<String> {
        let mut q = Vec::new();
        if let Some(level) = &self.level {
            q.push(format!("default level: {level}"));
        }
        if let Some(applicability) = &self.applicability {
            q.push(format!("applicability: {applicability}"));
        }
        if let Some(added_in) = &self.added_in {
            q.push(format!("added in {added_in}"));
        }
        q
    }
}

/// Errors producing catalog lint sections.
#[derive(Debug, PartialEq, Eq)]
pub enum IngestError {
    /// The assembled section failed schema validation.
    Schema(SchemaError),
    /// The catalog contained no usable lint entries.
    EmptyCatalog,
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IngestError::Schema(e) => write!(f, "clippy catalog section schema: {e}"),
            IngestError::EmptyCatalog => {
                write!(f, "the clippy lint catalog contained no lint entries")
            }
        }
    }
}

impl std::error::Error for IngestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IngestError::Schema(e) => Some(e),
            IngestError::EmptyCatalog => None,
        }
    }
}

impl From<SchemaError> for IngestError {
    fn from(err: SchemaError) -> Self {
        IngestError::Schema(err)
    }
}

/// Download the Clippy lint catalog under its **catalog** byte cap
/// ([`LINT_CATALOG_MAX_BYTES`]), which is larger than the ordinary chapter cap
/// because the catalog is one large page. The transfer is still bounded, and a
/// body over the cap is refused (`FetchError::Oversized`) rather than truncated.
#[allow(clippy::result_large_err)]
pub async fn fetch_lint_catalog(
    client: &BoundClient,
    catalog_url: &Url,
) -> Result<Fetched, FetchError> {
    client
        .fetch_with_limit(catalog_url, Some(LINT_CATALOG_MAX_BYTES))
        .await
}

/// Materialize full-fidelity [`Section`]s from a fetched Clippy lint catalog.
///
/// Each lint becomes one section: `source_id = clippy`,
/// `chapter_path = "clippy-catalog"`, `anchor = <lint_name>` (the in-page
/// anchor), `canonical_url = <catalog>#<lint_name>`, `lint_ids =
/// [clippy::<lint_name>]`, the group in the `heading_path`, and the level /
/// applicability / added-in metadata in `qualifications`. A catalog with no
/// usable lints is an [`IngestError::EmptyCatalog`] (a corrupt or non-catalog
/// page), never an empty section set.
#[allow(clippy::result_large_err)]
pub fn ingest_lint_catalog(fetched: &Fetched) -> Result<Vec<Section>, IngestError> {
    let html = String::from_utf8_lossy(&fetched.body).into_owned();
    let source = SourceId::new(SOURCE)?;
    let entries = extract_lint_catalog(&html, &fetched.final_url);
    if entries.is_empty() {
        return Err(IngestError::EmptyCatalog);
    }

    let mut sections = Vec::with_capacity(entries.len());
    for entry in entries {
        let heading_path = entry.heading_path();
        let qualifications = entry.qualifications();
        let anchor = entry.lint_id.clone();
        let canonical_lint_id = entry.canonical_lint_id;
        let body = entry.body;

        // The converter works in `reqwest::Url`; a stored `Section` carries the
        // validated `model::Url`, so bridge at the builder with the lint anchor
        // restored as a `#fragment`.
        let mut url = fetched.final_url.clone();
        url.set_fragment(Some(anchor.as_str()));
        let canonical = ModelUrl::new(url.as_str())?;

        let section = SectionBuilder::new(source.clone(), DOC_TITLE, CHAPTER_PATH, canonical, body)
            .anchor(anchor)
            .heading_path(heading_path)
            .lint_ids([canonical_lint_id])
            .qualifications(qualifications)
            .build()?;
        sections.push(section);
    }
    Ok(sections)
}

/// Split a Clippy lint catalog into one [`LintEntry`] per lint. Pure (no I/O):
/// the catalog HTML and the base URL it was fetched from. Best-effort: a lint
/// without a usable id is skipped, and an unparseable section degrades to a
/// text-only body rather than aborting the whole catalog.
pub fn extract_lint_catalog(html: &str, base_url: &Url) -> Vec<LintEntry> {
    let document = Html::parse_document(html);
    let article_sel = Selector::parse("article").expect("`article` is a valid selector");
    let mut entries = Vec::new();
    for article in document.select(&article_sel) {
        if let Some(entry) = extract_entry(&article, base_url) {
            entries.push(entry);
        }
    }
    entries
}

/// Extract one [`LintEntry`] from a lint `<article>`, or `None` if the article
/// carries no usable lint id (so it is not a lint entry).
fn extract_entry(article: &ElementRef, base_url: &Url) -> Option<LintEntry> {
    // The bare lint name: the article id, else the panel title name, else the
    // first heading text.
    let lint_id = article
        .attr("id")
        .map(str::to_string)
        .or_else(|| class_text(article, "panel-title-name"))
        .or_else(|| first_heading_text(article))
        .filter(|s| !s.trim().is_empty())?;

    let canonical_lint_id = canonical_lint_id(&lint_id);
    let group = class_text(article, "lint-group");
    let level = class_text(article, "lint-level");
    let body = lint_body_md(article, base_url);
    let known_issues = collect_known_issues(article);
    let added_in = class_text(article, "label-version");
    let applicability = class_text(article, "applicability");

    Some(LintEntry {
        lint_id,
        canonical_lint_id,
        group,
        level,
        body,
        known_issues,
        added_in,
        applicability,
    })
}

/// Canonicalize a bare lint name to `clippy::<name>`. A name that already
/// carries the `clippy::` prefix is left unchanged.
fn canonical_lint_id(lint_id: &str) -> String {
    if lint_id.starts_with("clippy::") {
        lint_id.to_string()
    } else {
        format!("clippy::{lint_id}")
    }
}

/// The full-fidelity Markdown body of a lint: the `.lint-doc-md` inner HTML
/// (explanation, examples, configuration, past names — everything the catalog
/// rendered for the lint), converted to Markdown. Falls back to the whole
/// article's inner HTML when the docs container is absent, and to a text-only
/// body when conversion fails — never a panic.
fn lint_body_md(article: &ElementRef, base_url: &Url) -> String {
    let doc_md = Selector::parse(".lint-doc-md").ok();
    let inner = match doc_md {
        Some(sel) => article.select(&sel).next().map(|md| inner_html(&md)),
        None => None,
    }
    .unwrap_or_else(|| inner_html(article));

    match convert_article(&inner, base_url) {
        Ok(extraction) => extraction.body,
        Err(_) => collapse_ws(&article.text().collect::<String>()),
    }
}

/// Collect the "known issues"/"known false positives" content: from the docs
/// container, the text of the first heading whose label mentions "known" and the
/// blocks that follow it, up to the next heading.
fn collect_known_issues(article: &ElementRef) -> Vec<String> {
    let Some(doc_md) = Selector::parse(".lint-doc-md")
        .ok()
        .and_then(|sel| article.select(&sel).next())
    else {
        return Vec::new();
    };

    let mut issues = Vec::new();
    let mut capturing = false;
    for child in doc_md.children() {
        let Some(el) = ElementRef::wrap(child) else {
            continue;
        };
        let name = el.value().name();
        if is_heading(name) {
            let text = element_text(&el);
            if text.to_ascii_lowercase().contains("known") {
                capturing = true;
                if !text.trim().is_empty() {
                    issues.push(text);
                }
            } else {
                capturing = false;
            }
        } else if capturing {
            let text = element_text(&el);
            if !text.trim().is_empty() {
                issues.push(text);
            }
        }
    }
    issues
}

/// The text of the first descendant carrying `class`, or `None`.
fn class_text(article: &ElementRef, class: &str) -> Option<String> {
    let selector = Selector::parse(&format!(".{class}")).ok()?;
    let el = article.select(&selector).next()?;
    let text = element_text(&el);
    if text.is_empty() { None } else { Some(text) }
}

/// The text of the first heading (`h1`–`h3`) in the article, or `None`.
fn first_heading_text(article: &ElementRef) -> Option<String> {
    let selector = Selector::parse("h1, h2, h3").ok()?;
    let el = article.select(&selector).next()?;
    let text = element_text(&el);
    if text.is_empty() { None } else { Some(text) }
}

/// The inner HTML of `el` (its children's outer HTML, concatenated). Direct text
/// nodes (inter-element whitespace, insignificant in HTML) are dropped.
fn inner_html(el: &ElementRef) -> String {
    el.children()
        .filter_map(ElementRef::wrap)
        .map(|child| child.html())
        .collect::<Vec<_>>()
        .join("")
}

/// The collapsed text of `el` (descendant text, whitespace-normalized).
fn element_text(el: &ElementRef) -> String {
    collapse_ws(&el.text().collect::<String>())
}

/// Whether a tag name is a heading.
fn is_heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

/// Collapse ASCII whitespace runs to single spaces.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Tests: fully offline. Pure extraction is exercised directly; the large-catalog
// byte limit and the end-to-end fetch are exercised against a loopback fixture.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::fetch::{FetchPolicy, LoopbackGate};
    use crate::ingest::http_fixture::{Behavior, FixtureServer};
    use std::sync::Arc;

    /// A two-lint catalog mirroring the real Clippy catalog structure: one lint
    /// (`unwrap_used`) with group/level/added-in/applicability and a "known
    /// issues" section, and one (`expect_used`) with group/level only (no
    /// version, applicability, or known issues) to exercise the "when present"
    /// branches.
    fn catalog_fixture() -> String {
        r#"<!DOCTYPE html>
<html>
<head><title>Clippy Lints</title></head>
<body>
<nav class="sidebar"><a href="/stable/">Home</a></nav>
<script>window.__catalog__ = 1;</script>
<div class="container">
  <h1 class="page-header">Clippy Lints</h1>
  <p class="lint-list-count">2 lints</p>
  <div class="lint-list">
    <article id="unwrap_used">
      <div class="lint-header">
        <h2 class="lint-title"><div id="lint-unwrap_used" class="panel-title-name">unwrap_used</div></h2>
        <span class="label lint-group group-restriction">restriction</span>
        <span class="label lint-level level-allow">allow</span>
      </div>
      <div class="lint-docs">
        <div class="lint-doc-md">
          <h3>What it does</h3>
          <p>Checks for use of <code>.unwrap()</code> on a <code>Result</code> value.</p>
          <h3>Why restrict this?</h3>
          <p>Unwrapping an <code>Err</code> panics.</p>
          <h3>Examples</h3>
          <pre><code class="language-rust">let x = Some(1u32).unwrap();</code></pre>
          <h3>Known false positives</h3>
          <p>None are known at present.</p>
          <div class="lint-additional-info">Added in: <span class="label label-version">1.45.0</span></div>
          <div class="lint-additional-info">Applicability: <span class="label applicability">Unspecified</span></div>
        </div>
      </div>
    </article>
    <article id="expect_used">
      <div class="lint-header">
        <h2 class="lint-title"><div id="lint-expect_used" class="panel-title-name">expect_used</div></h2>
        <span class="label lint-group group-style">style</span>
        <span class="label lint-level level-warn">warn</span>
      </div>
      <div class="lint-docs">
        <div class="lint-doc-md">
          <h3>What it does</h3>
          <p>Checks for use of <code>.expect()</code>.</p>
          <h3>Examples</h3>
          <pre><code class="language-rust">let x = Some(1u32).expect("ok");</code></pre>
        </div>
      </div>
    </article>
  </div>
</div>
</body>
</html>"#
            .to_string()
    }

    fn loopback_url(server: &FixtureServer, path: &str) -> Url {
        Url::parse(&format!("http://{}{}", server.addr, path)).expect("a valid loopback URL")
    }

    #[test]
    fn catalog_extraction_splits_lints_with_full_fidelity() {
        let base = Url::parse("https://clippy.rust-lang.org/stable/index.html").unwrap();
        let entries = extract_lint_catalog(&catalog_fixture(), &base);

        // Both lints are split out, in document order.
        assert_eq!(entries.len(), 2, "two lints split out");

        // The first lint retains every piece of structured metadata.
        let unwrap = &entries[0];
        assert_eq!(unwrap.lint_id, "unwrap_used");
        assert_eq!(unwrap.canonical_lint_id, "clippy::unwrap_used");
        assert_eq!(unwrap.group.as_deref(), Some("restriction"));
        assert_eq!(unwrap.level.as_deref(), Some("allow"));
        assert_eq!(unwrap.added_in.as_deref(), Some("1.45.0"));
        assert_eq!(unwrap.applicability.as_deref(), Some("Unspecified"));
        // The explanation and the example are retained in the body.
        assert!(
            unwrap.body.contains("unwrap()"),
            "body keeps the explanation/example: {body}",
            body = unwrap.body
        );
        // The "known issues" content is retained and structured.
        assert!(
            unwrap
                .known_issues
                .iter()
                .any(|s| s.to_ascii_lowercase().contains("known")),
            "known-issues heading retained"
        );
        assert!(
            unwrap
                .known_issues
                .iter()
                .any(|s| s.to_ascii_lowercase().contains("none")),
            "known-issues body retained"
        );

        // The second lint has group/level but no version/applicability/known
        // issues — the "when present" branches stay absent.
        let expect = &entries[1];
        assert_eq!(expect.lint_id, "expect_used");
        assert_eq!(expect.canonical_lint_id, "clippy::expect_used");
        assert_eq!(expect.group.as_deref(), Some("style"));
        assert_eq!(expect.level.as_deref(), Some("warn"));
        assert!(expect.added_in.is_none(), "no version when absent");
        assert!(
            expect.applicability.is_none(),
            "no applicability when absent"
        );
        assert!(
            expect.known_issues.is_empty(),
            "no known issues when absent"
        );
    }

    #[test]
    fn canonical_id_is_idempotent_for_prefixed_names() {
        // A name already carrying the prefix is not doubled.
        let html = r#"<article id="clippy::already_prefixed"><div class="lint-doc-md"><p>x</p></div></article>"#;
        let entries = extract_lint_catalog(
            html,
            &Url::parse("https://clippy.rust-lang.org/stable/").unwrap(),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].canonical_lint_id, "clippy::already_prefixed");
        assert_eq!(entries[0].lint_id, "clippy::already_prefixed");
    }

    #[test]
    fn catalog_download_limit_exceeds_chapter_limit() {
        // The catalog cap is a separate, larger bound than the ordinary chapter
        // cap (the catalog is one large page).
        let chapter = FetchPolicy::default()
            .max_body_bytes
            .expect("default chapter cap is set");
        assert!(
            LINT_CATALOG_MAX_BYTES > chapter,
            "catalog cap {LINT_CATALOG_MAX_BYTES} must exceed the chapter cap {chapter}"
        );
    }

    #[tokio::test]
    async fn catalog_materializes_sections_with_metadata() {
        // End-to-end through a loopback fixture, so the canonical_url carries the
        // per-lint anchor.
        let server = FixtureServer::start(vec![(
            "/stable/index.html".to_string(),
            Behavior::Text {
                body: catalog_fixture().into_bytes(),
                etag: Some("cat-v1".to_string()),
                last_modified: None,
                content_type: Some("text/html; charset=utf-8".to_string()),
            },
        )]);
        let client = BoundClient::with_gate(FetchPolicy::default(), Arc::new(LoopbackGate))
            .expect("loopback client builds");
        let url = loopback_url(&server, "/stable/index.html");

        let fetched = fetch_lint_catalog(&client, &url).await.unwrap();
        let sections = ingest_lint_catalog(&fetched).unwrap();
        assert_eq!(sections.len(), 2, "one section per lint");

        let unwrap = &sections[0];
        assert_eq!(unwrap.source_id.as_str(), "clippy");
        assert_eq!(unwrap.anchor.as_deref(), Some("unwrap_used"));
        assert_eq!(unwrap.lint_ids, ["clippy::unwrap_used"]);
        assert_eq!(
            unwrap.heading_path,
            ["restriction", "unwrap_used"],
            "the group is the heading breadcrumb parent"
        );
        assert!(
            unwrap
                .qualifications
                .iter()
                .any(|q| q.contains("default level: allow")),
            "level retained in qualifications: {:?}",
            unwrap.qualifications
        );
        assert!(
            unwrap
                .qualifications
                .iter()
                .any(|q| q.contains("applicability: Unspecified")),
            "applicability retained: {:?}",
            unwrap.qualifications
        );
        assert!(
            unwrap
                .qualifications
                .iter()
                .any(|q| q.contains("added in 1.45.0")),
            "added-in retained: {:?}",
            unwrap.qualifications
        );
        assert!(
            unwrap.canonical_url.as_str().ends_with("#unwrap_used"),
            "canonical url anchored at the lint: {}",
            unwrap.canonical_url.as_str()
        );
        assert!(unwrap.body.contains("unwrap()"), "body keeps the example");
        assert!(!unwrap.body.is_empty(), "non-empty body");

        // The second lint has no version/applicability, so its qualifications
        // omit them.
        let expect = &sections[1];
        assert_eq!(expect.lint_ids, ["clippy::expect_used"]);
        assert_eq!(expect.heading_path, ["style", "expect_used"]);
        assert!(
            !expect.qualifications.iter().any(|q| q.contains("added in")),
            "no added-in when absent: {:?}",
            expect.qualifications
        );
    }

    #[tokio::test]
    async fn large_catalog_passes_catalog_limit_but_not_chapter_limit() {
        // The catalog fetch's cap is larger than an ordinary chapter's cap, so a
        // body that exceeds the chapter cap still fits the catalog cap. The
        // loopback `Big` fixture stays under the socket send buffer, so the
        // caps here are small but preserve the relationship the production caps
        // keep (see `catalog_download_limit_exceeds_chapter_limit`).
        let chapter_cap = 1024;
        let size = chapter_cap + 512; // between the chapter cap and the catalog cap
        assert!(size > chapter_cap && size < LINT_CATALOG_MAX_BYTES);

        let server = FixtureServer::start(vec![(
            "/stable/index.html".to_string(),
            Behavior::Big { size },
        )]);
        let client = BoundClient::with_gate(FetchPolicy::default(), Arc::new(LoopbackGate))
            .expect("loopback client builds");
        let url = loopback_url(&server, "/stable/index.html");

        // Within the catalog cap: accepted.
        let fetched = client
            .fetch_with_limit(&url, Some(LINT_CATALOG_MAX_BYTES))
            .await
            .expect("a body within the catalog cap is fetched");
        assert_eq!(fetched.status.as_u16(), 200);

        // The same body is refused by the smaller chapter cap.
        let err = client
            .fetch_with_limit(&url, Some(chapter_cap))
            .await
            .expect_err("a body over the chapter cap is refused");
        assert!(
            matches!(err, FetchError::Oversized { cap, .. } if cap == chapter_cap),
            "chapter cap refuses an oversized body: {err:?}"
        );
    }

    #[tokio::test]
    async fn catalog_body_over_cap_is_refused() {
        // The per-call cap (the same call the catalog fetch uses) is
        // all-or-nothing: a body that would exceed it is refused rather than
        // truncated. It is exercised with a small body because the loopback
        // `Big` fixture stays under the socket send buffer; `fetch_lint_catalog`
        // applies the same mechanism with the larger [`LINT_CATALOG_MAX_BYTES`].
        let cap = 1024;
        let server = FixtureServer::start(vec![(
            "/stable/index.html".to_string(),
            Behavior::Big { size: cap + 512 },
        )]);
        let client = BoundClient::with_gate(FetchPolicy::default(), Arc::new(LoopbackGate))
            .expect("loopback client builds");
        let url = loopback_url(&server, "/stable/index.html");

        let err = client
            .fetch_with_limit(&url, Some(cap))
            .await
            .expect_err("a body over the cap is refused");
        assert!(
            matches!(err, FetchError::Oversized { cap: got, .. } if got == cap),
            "the cap refuses an oversized body: {err:?}"
        );
    }

    #[tokio::test]
    async fn empty_catalog_is_an_error_not_empty_sections() {
        // A page with no lint articles is an EmptyCatalog, never an empty set.
        let server = FixtureServer::start(vec![(
            "/stable/index.html".to_string(),
            Behavior::Text {
                body: b"<html><body><h1>Clippy Lints</h1></body></html>".to_vec(),
                etag: Some("e".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = BoundClient::with_gate(FetchPolicy::default(), Arc::new(LoopbackGate))
            .expect("loopback client builds");
        let url = loopback_url(&server, "/stable/index.html");

        let fetched = fetch_lint_catalog(&client, &url).await.unwrap();
        let err = ingest_lint_catalog(&fetched).unwrap_err();
        assert_eq!(err, IngestError::EmptyCatalog);
    }
}
