//! Ingestion of the Rust API Guidelines source (task `[015]`, issue #15).
//!
//! Wires the `api-guidelines` collection (a mdBook site, like the Rust books) to
//! the shared pipeline and extracts the upstream guideline codes:
//!
//! - **Discovery** reuses [`crate::ingest::books::discover_inventory`]: the API
//!   Guidelines site is mdBook-structured, so the same structural, no-filename
//!   chapter discovery books the introduction (`about.html`), the checklist page
//!   (`checklist.html`), and every topical chapter — criterion 1 of #15.
//! - **Chapters** go through the shared [`split_article`](crate::ingest::section)
//!   splitter and are enriched with the guideline code their heading carries
//!   (e.g. `C-CONV`), so each section retains its C-code *identity*, its
//!   examples, its recommendation wording, and its canonical anchor — criterion 2
//!   of #15.
//! - **The checklist page** is parsed into [`ChecklistEntry`]s: the upstream's
//!   code inventory, one entry per `(C-…)` guideline with its title, category,
//!   and `category#code` location.
//!
//! No network in these tests: the chapter/checklist bodies are served from the
//! loopback [`FixtureServer`](crate::ingest::http_fixture::FixtureServer) and
//! `ingest_checklist` is a pure parse, so the whole stage is exercisable offline.

// Transitional: these functions are the `[015]` ingestion API, exercised by the
// offline acceptance tests below; their first non-test consumer is the `[029]+`
// sync pipeline that ingests the corpus end-to-end.
#![allow(dead_code)]

use crate::ingest::fetch::Fetched;
use crate::ingest::section::{SplitConfig, SplitError, split_article};
use crate::model::{ChecklistEntry, SchemaError, Section, SourceId};
use reqwest::Url;
use scraper::{ElementRef, Html, Node, Selector};

/// The `SourceId` of the API Guidelines collection.
pub const SOURCE: &str = "api-guidelines";

/// The document title shared by every chapter of the API Guidelines corpus, used
/// when a page's own heading cannot supply one.
pub const DOC_TITLE: &str = "Rust API Guidelines";

/// A typed, non-panicking failure from ingesting an API Guidelines page.
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
            IngestError::Split(e) => write!(f, "api-guidelines section split: {e}"),
            IngestError::Schema(e) => write!(f, "api-guidelines section schema: {e}"),
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
/// chapter's file stem under the `api-guidelines` boundary (`naming` from
/// `…/api-guidelines/naming.html`, `about` from `…/about.html`).
///
/// A fragment is dropped (a section and its `#anchor` are one document) and a
/// missing boundary prefix falls back to the bare path stem, so an off-boundary
/// or fragment-only URL still yields a non-empty path rather than a panic.
pub fn chapter_path(final_url: &Url) -> String {
    // The registry boundary is the URL *path* prefix, not the full URL; `path()`
    // drops the scheme/host and any `#fragment` (a section and its anchor are one
    // document), leaving just the path to normalize.
    let path = final_url.path();
    let stripped = path.strip_prefix("/api-guidelines/").unwrap_or(path);
    let stem = stripped.trim_start_matches('/');
    stem.strip_suffix(".html").unwrap_or(stem).to_string()
}

/// The document title of an API Guidelines page: its `h1` when present, else the
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

/// Ingest one API Guidelines *chapter* into its bounded [`Section`]s, each
/// enriched with the guideline code its heading carries.
///
/// `fetched` is the chapter body already fetched through the gated fetcher. The
/// body is split by the shared [`split_article`] (which keeps examples with their
/// explanation and records the heading's original anchor), then each section is
/// tagged with the `(C-…)` code read from its heading, so the section's
/// `guideline_codes` carry the upstream identity (e.g. `C-CONV`).
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

    let sections = split_article(
        &html,
        &fetched.final_url,
        &source,
        &chapter,
        &doc_title,
        &SplitConfig::default(),
    )
    .map_err(IngestError::Split)?;

    let mut enriched = Vec::with_capacity(sections.len());
    for section in sections {
        enriched.push(with_guideline_code(section));
    }
    Ok(enriched)
}

/// Tag a split section with the guideline code that scopes it, when any.
///
/// A `(C-…)` code lives in a heading the section descends from, so the
/// breadcrumb is walked **most-specific first**: the section's own heading is the
/// normal carrier (e.g. `"Ad-hoc conversions … (C-CONV)"`), but a code opened by
/// a coarser ancestor heading (a guideline's sub-section) is captured too, so a
/// code that scopes the section is never swallowed. The *nearest* such heading
/// wins — a deeper heading's code is preferred over an ancestor's. A section
/// whose whole breadcrumb carries no well-formed marker (the chapter's `h1` and
/// any guideline-less subtree) keeps its code list empty.
///
/// The code is attached as a public-field mutation rather than a rebuild, so the
/// section's already-derived id / canonical anchor are preserved exactly; the
/// [`content_hash`](crate::model::Section::content_hash) is then recomputed so it
/// describes the tagged content — two sections that differ only by code differ in
/// hash.
fn with_guideline_code(mut section: Section) -> Section {
    // Most-specific first (`rev()`), so the nearest ancestor that carries a code
    // is taken. Computed before the mutation to avoid aliasing `heading_path`
    // and `guideline_codes` at once.
    let code = section
        .heading_path
        .iter()
        .rev()
        .find_map(|h| guideline_code_in(h));
    if let Some(code) = code {
        section.guideline_codes = vec![code];
        section.recompute_content_hash();
    }
    section
}

/// Parse the API Guidelines *checklist* page into its [`ChecklistEntry`]s — one
/// per `(C-…)` guideline, in document order.
///
/// Each entry carries the upstream `code` (from the guideline's link text), its
/// `title` (the recommendation wording), its `category` (the enclosing
/// `<strong>` group heading), and a `category#code` `location`. A guideline whose
/// link text is not a valid code, or whose enclosing group is unknown, is skipped
/// rather than guessed. `explanation_sections` is left empty here; resolving a
/// code to its explanatory section is a later stage (`get_api_checklist`, task
/// `[016]`).
pub fn ingest_checklist(fetched: &Fetched) -> Vec<ChecklistEntry> {
    let html = String::from_utf8_lossy(&fetched.body).into_owned();
    parse_checklist(&html)
}

/// The shared checklist parser, split out so it is testable on a raw HTML string
/// without constructing a [`Fetched`].
fn parse_checklist(html: &str) -> Vec<ChecklistEntry> {
    let document = Html::parse_fragment(html);
    let link_sel = Selector::parse("a").unwrap();
    let strong_sel = Selector::parse("strong").unwrap();

    // The checklist is a nested `<ul>` of category `<li>`s, each holding a
    // `<strong>` group heading and a nested list of guideline `<li>`s. Each
    // guideline links to its code (`<a>…(C-CONV)</a>`); walking up from that
    // link finds the nearest guideline `<li>` (no `<strong>`) and its enclosing
    // category `<li>` (the one carrying the `<strong>`).
    let mut entries: Vec<ChecklistEntry> = Vec::new();
    for link in document.select(&link_sel) {
        let code = guideline_code_in(&link.text().collect::<String>());
        let Some(code) = code else { continue };

        let (category, title) = resolve_entry(&link, &strong_sel);
        // A code with no resolvable category is not a checklist entry the way we
        // render it — skip it rather than fabricate a category.
        if category.is_empty() {
            continue;
        }
        let location = format!("{category}#{code}");
        entries.push(ChecklistEntry {
            code,
            title,
            location,
            category,
            explanation_sections: Vec::new(),
        });
    }
    entries
}

/// Resolve the `(category, title)` for a guideline link by walking up to its
/// enclosing guideline `<li>` (for the title) and category `<li>` (for the
/// group heading).
fn resolve_entry(link: &ElementRef, strong_sel: &Selector) -> (String, String) {
    let mut node = link.parent();
    let mut guideline_li: Option<ElementRef> = None;
    let mut category = String::new();
    while let Some(n) = node {
        if let Some(elem) = ElementRef::wrap(n)
            && elem.value().name() == "li"
        {
            if let Some(strong) = elem.select(strong_sel).next() {
                category = collapse_ws(&strong.text().collect::<String>());
                break;
            }
            // The first `<li>` without a group heading is the guideline's own
            // list item; remember it for the title, keep climbing for the
            // category.
            if guideline_li.is_none() {
                guideline_li = Some(elem);
            }
        }
        node = n.parent();
    }
    let title = guideline_li
        .as_ref()
        .map(|li| guideline_title(li))
        .unwrap_or_default();
    (category, title)
}

/// The guideline's recommendation wording: the text of its list item *up to* the
/// code link, so a nested sub-list (e.g. the `C-COMMON-TRAITS` trait names) that
/// follows the link is excluded and the description reads cleanly.
fn guideline_title(li: &ElementRef) -> String {
    let mut buf = String::new();
    for child in li.children() {
        match child.value() {
            Node::Text(t) => buf.push_str(&t.text),
            Node::Element(e) => {
                let name = e.name();
                if name == "a" {
                    // The code link ends the description.
                    break;
                }
                if name == "input" || name == "ul" || name == "ol" {
                    // Structural, not description text.
                    continue;
                }
                if let Some(elem) = ElementRef::wrap(child) {
                    buf.push_str(&elem.text().collect::<String>());
                }
            }
            _ => {}
        }
    }
    // The upstream checklist renders the code as a trailing "(C-…)" group whose
    // opening parenthesis sits in the text node preceding the link (e.g.
    // "… conventions ("). Strip only that sentinel — a space-prefixed open
    // paren — so a legitimate trailing "(" is never over-trimmed, then collapse.
    collapse_ws(buf.trim_end_matches(" ("))
}

/// Extract the upstream guideline code (`C-CONV`, `C-ITER-TY`, …) from a heading
/// or checklist link text.
///
/// Two upstream shapes carry a code: a chapter *heading* renders the code as a
/// trailing `(C-…)` marker (e.g. `"Ad-hoc conversions … (C-CONV)"`), while a
/// checklist *link* renders the bare code as its text (e.g. `"C-CONV"`). Both
/// are accepted. A heading/link with no well-formed marker — a non-code heading,
/// an empty `(C-)`, a lowercased `(c-conv)`, or a marker with no closing
/// parenthesis — yields `None`, so a generic heading never masquerades as a code.
pub fn guideline_code_in(text: &str) -> Option<String> {
    // 1. A parenthesized marker in a heading, e.g. "… (C-CONV)": take the last
    // one and read to its closing parenthesis. A `(C-` with no closing paren is
    // malformed, so it is not a code (the `?` returns `None` for the whole
    // function).
    if let Some(start) = text.rfind("(C-") {
        let rest = &text[start + 1..];
        let end = rest.find(')')?;
        let code = &rest[..end];
        if is_valid_code(code) {
            return Some(code.to_string());
        }
        // A `(C-…)` marker present but malformed (e.g. "(C-)" / "(C-)") is a
        // deliberate negative: do not fall through to the bare-code path.
        return None;
    }
    // 2. A bare code, e.g. the checklist link text "C-CONV".
    let trimmed = text.trim();
    if is_valid_code(trimmed) {
        return Some(trimmed.to_string());
    }
    None
}

/// Whether `code` is a well-formed upstream guideline code: `C-` followed by a
/// non-empty tail of uppercase letters, digits, and interior hyphens that does
/// not start or end on a hyphen.
fn is_valid_code(code: &str) -> bool {
    let tail = match code.strip_prefix("C-") {
        Some(t) => t,
        None => return false,
    };
    !tail.is_empty()
        && !tail.starts_with('-')
        && !tail.ends_with('-')
        && tail
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
}

/// Collapse runs of whitespace to single spaces and trim — the same normalization
/// the shared converter applies, so checklist titles match section headings.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Offline acceptance tests — plan.md §11 / task [015]
//
// Both criteria from issue #15:
//   (1) The introduction, checklist page, and topical chapters appear in the
//       inventory — proven by reusing the book discovery over a loopback nav.
//   (2) Fixture sections retain C-code identities, examples, recommendation
//       wording, and canonical anchors.
//
// The chapter and checklist bodies are served from the loopback `FixtureServer`
// or parsed from a static HTML string, so nothing touches the network.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! The `loopback_client` helper returns `Result<BoundClient, FetchError>`;
    //! `FetchError::Bound` is large, mirroring `ingest::fetch`'s suppression.
    #![allow(clippy::result_large_err)]

    use super::{
        SOURCE, chapter_path, guideline_code_in, ingest_chapter, ingest_checklist, parse_checklist,
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

    // -- Criterion 1: the introduction, checklist, and topical chapters are
    //    booked by the shared book discovery (the site is mdBook-structured). --

    #[tokio::test]
    async fn discover_inventory_books_intro_checklist_and_topical_chapters() {
        // A minimal mdBook sidebar: the `about.html` nav page links to the
        // checklist and to two topical chapters. The links are absolute
        // registered-host URLs (the real upstream shape); the nav page itself is
        // served from loopback, so `discover_inventory` books the registered
        // documents without touching the network. `discover_inventory` resolves
        // each link through the registry gate, so a relative link would resolve
        // to the loopback host and be refused as off-boundary.
        let nav_html = r#"<html><body>
            <nav id="sidebar"><ol class="chapter">
              <li><a href="https://rust-lang.github.io/api-guidelines/about.html">Introduction</a></li>
              <li><a href="https://rust-lang.github.io/api-guidelines/checklist.html">Checklist</a></li>
              <li><a href="https://rust-lang.github.io/api-guidelines/naming.html">Naming</a></li>
              <li><a href="https://rust-lang.github.io/api-guidelines/predictability.html">Predictability</a></li>
            </ol></nav>
          </body></html>"#;
        let server = FixtureServer::start(vec![(
            "/api-guidelines/about.html".to_string(),
            Behavior::Text {
                body: nav_html.as_bytes().to_vec(),
                etag: Some("\"about-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/api-guidelines/about.html", server.addr))
            .expect("a valid loopback nav URL");

        let (inv, diff) = discover_inventory(&client, &base_url, "api-guidelines", None)
            .await
            .expect("discovery succeeds offline");

        let keys: Vec<String> = inv.urls.iter().map(|u| u.as_str().to_string()).collect();
        assert!(
            keys.iter().any(|k| k.ends_with("/about.html")),
            "the introduction (about.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/checklist.html")),
            "the checklist page (checklist.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/naming.html")),
            "a topical chapter (naming.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/predictability.html")),
            "a second topical chapter (predictability.html) must be booked, got {keys:?}"
        );
        // No delta against an omitted expected set, so the discovery is complete.
        assert!(
            diff.is_complete(),
            "with no expected set the diff must be complete, got {diff:?}"
        );
    }

    // -- Criterion 2: fixture sections retain C-code, examples, wording, anchor.

    /// A compact naming.html chapter: an `h1`, two guideline `h2`s each carrying a
    /// `(C-…)` code, a recommendation paragraph, and a fenced Rust example under
    /// one of them.
    fn naming_html() -> &'static str {
        r##"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="about.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="top">Naming</h1>
    <h2 id="ad-hoc-conversions-c-conv"><a class="header" href="#ad-hoc-conversions-c-conv">Ad-hoc conversions follow <code>as_</code>, <code>to_</code>, <code>into_</code> conventions (C-CONV)</a></h2>
    <p>Ad-hoc conversions between types should follow the <code>as_</code>, <code>to_</code>, and <code>into_</code> naming conventions.</p>
    <pre><code class="language-rust">impl From&lt;Foo&gt; for Bar {
    fn from(f: Foo) -&gt; Bar { Bar }
}</code></pre>
    <h2 id="casing-c-case"><a class="header" href="#casing-c-case">Casing conforms to RFC 430 (C-CASE)</a></h2>
    <p>Module names and crate names use <code>snake_case</code>; types use <code>PascalCase</code>.</p>
  </main>
</body></html>"##
    }

    #[tokio::test]
    async fn ingest_chapter_retains_code_examples_wording_and_anchor() {
        let server = FixtureServer::start(vec![(
            "/api-guidelines/naming.html".to_string(),
            Behavior::Text {
                body: naming_html().as_bytes().to_vec(),
                etag: Some("\"naming-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!(
            "http://{}/api-guidelines/naming.html",
            server.addr
        ))
        .expect("a valid loopback chapter URL");

        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        // Two guideline sections (plus the chapter's h1 section) were produced.
        let c_conv = sections
            .iter()
            .find(|s| s.guideline_codes.as_slice() == ["C-CONV"])
            .expect("a section carrying C-CONV must exist");

        // (2a) C-code identity retained.
        assert_eq!(c_conv.guideline_codes, vec!["C-CONV".to_string()]);
        assert_eq!(
            c_conv.source_id,
            SourceId::new(SOURCE).expect("a valid source id")
        );

        // (2b) Recommendation wording retained in the body.
        assert!(
            c_conv.body.contains("Ad-hoc conversions"),
            "the recommendation wording must be in the body:\n{}",
            c_conv.body
        );
        assert!(
            c_conv.body.contains("into_ naming conventions"),
            "the recommendation sentence must survive, got:\n{}",
            c_conv.body
        );

        // (2c) The fenced Rust example stays with its explanation.
        assert!(
            c_conv.body.contains("```rust") && c_conv.body.contains("impl From"),
            "the code example must be retained with the section:\n{}",
            c_conv.body
        );

        // (2d) The canonical anchor is preserved from the heading's id.
        assert!(
            c_conv.anchor.as_deref() == Some("ad-hoc-conversions-c-conv"),
            "the heading's original anchor must be preserved, got {:?}",
            c_conv.anchor
        );
        assert!(
            c_conv
                .canonical_url
                .as_str()
                .contains("#ad-hoc-conversions-c-conv"),
            "the canonical URL must carry the original anchor, got {}",
            c_conv.canonical_url.as_str()
        );

        // A second code is independently retained, proving the per-heading path.
        assert!(
            sections
                .iter()
                .any(|s| s.guideline_codes.as_slice() == ["C-CASE"])
                && sections.iter().any(|s| s.body.contains("PascalCase")),
            "C-CASE and its wording must also be retained, got {:?}",
            sections
                .iter()
                .map(|s| (&s.guideline_codes, &s.body))
                .collect::<Vec<_>>()
        );
    }

    /// F-01 regression: a section's `content_hash` reflects its *final* content,
    /// including the `guideline_codes` the enrichment tags on. Before the fix the
    /// hash was fixed at `SectionBuilder::build` time over an empty code list, so
    /// a tagged section hashed its pre-tag content and two sections that differ
    /// only by code were indistinguishable by hash.
    #[tokio::test]
    async fn content_hash_reflects_enriched_guideline_code() {
        let server = FixtureServer::start(vec![(
            "/api-guidelines/naming.html".to_string(),
            Behavior::Text {
                body: naming_html().as_bytes().to_vec(),
                etag: Some("\"naming-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!(
            "http://{}/api-guidelines/naming.html",
            server.addr
        ))
        .expect("a valid loopback chapter URL");
        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        let c_conv = sections
            .iter()
            .find(|s| s.guideline_codes == vec!["C-CONV".to_string()])
            .expect("a C-CONV section exists");

        // (a) Stability: recomputing over the section's own fields is idempotent
        // — the stored hash already reflects the final (tagged) content.
        let mut probe = c_conv.clone();
        probe.recompute_content_hash();
        assert_eq!(
            probe.content_hash, c_conv.content_hash,
            "the stored hash must already reflect the final content (idempotent recompute)"
        );

        // (b) Sensitivity: the same section *without* its code hashes
        // differently, so the code is part of the hashed content — not a
        // post-hoc, hash-excluded tag. Two sections that differ only by code
        // now differ in content_hash.
        let mut without_code = c_conv.clone();
        without_code.guideline_codes.clear();
        without_code.recompute_content_hash();
        assert_ne!(
            without_code.content_hash, c_conv.content_hash,
            "dropping the code must change the content hash (the code is hashed)"
        );
    }

    /// F-02 regression: the code is read from the section's *nearest* heading that
    /// carries one, most-specific first — so a code opened by an ancestor heading
    /// is captured for a codeless leaf (never swallowed) and a deeper heading's
    /// code beats an ancestor's. A breadcrumb with no code anywhere keeps an empty
    /// list. The upstream places each `(C-…)` on its own heading, so the normal
    /// case (the leaf carries the code) still holds; this pins the robustness.
    #[tokio::test]
    async fn guideline_code_is_read_from_the_nearest_heading() {
        let html = r##"<!DOCTYPE html>
<html><body>
  <main>
    <h1>Chapter</h1>
    <h2 id="conv">Ad-hoc conversions (C-CONV)</h2>
    <p>leaf heading carries the code</p>
    <h2 id="g">Guideline (C-FOO)</h2>
    <h3 id="sub">Sub-section</h3>
    <p>the code lives on the ancestor, not this leaf</p>
    <h2 id="plain">Plain heading</h2>
    <p>no code anywhere in this breadcrumb</p>
  </main>
</body></html>"##;
        let server = FixtureServer::start(vec![(
            "/api-guidelines/precedence.html".to_string(),
            Behavior::Text {
                body: html.as_bytes().to_vec(),
                etag: Some("\"precedence-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!(
            "http://{}/api-guidelines/precedence.html",
            server.addr
        ))
        .expect("a valid loopback chapter URL");
        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        let by_anchor = |a: &str| sections.iter().find(|s| s.anchor.as_deref() == Some(a));

        // The leaf heading's own code is captured (the normal case).
        assert_eq!(
            by_anchor("conv").expect("conv section").guideline_codes,
            vec!["C-CONV".to_string()],
            "a leaf heading's own code is captured"
        );

        // A code opened by an ANCESTOR heading is captured for a leaf that carries
        // none itself — it is not swallowed.
        let sub = by_anchor("sub").expect("sub-section exists");
        assert_eq!(
            sub.guideline_codes,
            vec!["C-FOO".to_string()],
            "an ancestor heading's code is captured for a codeless leaf"
        );
        // That capture is reflected in the hash (F-01 + F-02 together): the
        // sub-section hashes as if tagged, so it is not a hash-excluded tag.
        let mut untagged = sub.clone();
        untagged.guideline_codes.clear();
        untagged.recompute_content_hash();
        assert_ne!(
            untagged.content_hash, sub.content_hash,
            "the ancestor code must be part of the sub-section's content hash"
        );

        // A breadcrumb with no code anywhere keeps an empty list — nothing is
        // fabricated.
        assert!(
            by_anchor("plain")
                .expect("plain section")
                .guideline_codes
                .is_empty(),
            "a codeless breadcrumb must not fabricate a code"
        );
    }

    // -- The checklist page parses into C-coded entries. --

    /// A compact checklist.html: two category groups, each a `<strong>` heading
    /// over a nested list of guideline `<li>`s. One guideline (`C-COMMON-TRAITS`)
    /// carries a nested sub-list of trait names that must be excluded from its
    /// title.
    fn checklist_html() -> &'static str {
        r#"<!DOCTYPE html>
<html><body>
  <main>
    <h1 id="checklist">Checklist</h1>
    <ul>
      <li><strong>Conversions and From</strong>
        <ul>
          <li><input disabled type="checkbox"/> Ad-hoc conversions follow <code>as_</code>, <code>to_</code>, <code>into_</code> conventions (<a href="naming.html#c-conv">C-CONV</a>).</li>
        </ul>
      </li>
      <li><strong>Interoperability</strong>
        <ul>
          <li><input disabled type="checkbox"/> Types eagerly implement common traits (<a href="naming.html#c-common-traits">C-COMMON-TRAITS</a>) <ul><li>Copy, Clone, Eq</li><li>Display, Default</li></ul></li>
          <li><input disabled type="checkbox"/> Getters are named <code>get_*</code> (<a href="naming.html#c-getter">C-GETTER</a>).</li>
        </ul>
      </li>
    </ul>
  </main>
</body></html>"#
    }

    #[test]
    fn ingest_checklist_parses_entries_with_category_and_location() {
        let fetched = fetched_checklist();
        let entries = ingest_checklist(&fetched);

        let c_conv = entries
            .iter()
            .find(|e| e.code == "C-CONV")
            .expect("C-CONV must be parsed");
        assert_eq!(c_conv.category, "Conversions and From");
        assert_eq!(c_conv.location, "Conversions and From#C-CONV");
        assert_eq!(
            c_conv.title,
            "Ad-hoc conversions follow as_, to_, into_ conventions"
        );
        // explanation_sections is unresolved in this stage.
        assert!(c_conv.explanation_sections.is_empty());

        let c_common = entries
            .iter()
            .find(|e| e.code == "C-COMMON-TRAITS")
            .expect("C-COMMON-TRAITS must be parsed");
        assert_eq!(c_common.category, "Interoperability");
        // The nested sub-list of trait names must NOT leak into the title.
        assert_eq!(c_common.title, "Types eagerly implement common traits");
        assert!(
            !c_common.title.contains("Copy"),
            "sub-list must be excluded:\n{}",
            c_common.title
        );

        // A third entry confirms the per-group resolution.
        assert!(
            entries
                .iter()
                .any(|e| e.code == "C-GETTER" && e.category == "Interoperability")
        );
        assert_eq!(entries.len(), 3, "all three guideline links must be parsed");
    }

    /// A `Fetched` for the checklist page, built directly so the pure parse needs
    /// no server (the body is the loopback-served `checklist.html`).
    fn fetched_checklist() -> Fetched {
        Fetched {
            requested: Url::parse("https://rust-lang.github.io/api-guidelines/checklist.html")
                .unwrap(),
            final_url: Url::parse("https://rust-lang.github.io/api-guidelines/checklist.html")
                .unwrap(),
            status: reqwest::StatusCode::OK,
            body: checklist_html().as_bytes().to_vec(),
            content_type: Some("text/html".to_string()),
            etag: Some("\"checklist-1\"".to_string()),
            last_modified: None,
            content_length: Some(checklist_html().len() as u64),
        }
    }

    #[test]
    fn chapter_path_derives_the_file_stem() {
        assert_eq!(
            chapter_path(
                &Url::parse("https://rust-lang.github.io/api-guidelines/naming.html").unwrap()
            ),
            "naming"
        );
        assert_eq!(
            chapter_path(
                &Url::parse("https://rust-lang.github.io/api-guidelines/about.html#top").unwrap()
            ),
            "about"
        );
        // A fragment-only / off-boundary URL still yields a non-empty stem.
        assert_eq!(
            chapter_path(
                &Url::parse("https://rust-lang.github.io/api-guidelines/checklist.html").unwrap()
            ),
            "checklist"
        );
    }

    #[test]
    fn guideline_code_in_reads_the_trailing_marker() {
        assert_eq!(
            guideline_code_in("Ad-hoc conversions follow as_, to_, into_ conventions (C-CONV)"),
            Some("C-CONV".to_string())
        );
        assert_eq!(
            guideline_code_in(
                "Iterator type names match the methods that produce them (C-ITER-TY)"
            ),
            Some("C-ITER-TY".to_string())
        );
        assert_eq!(
            guideline_code_in("Foo (see bar) (C-WORD-ORDER)"),
            Some("C-WORD-ORDER".to_string())
        );
        // Non-code headings and malformed markers yield nothing — no fabrication.
        assert_eq!(guideline_code_in("Naming"), None);
        assert_eq!(guideline_code_in("(C-)"), None);
        assert_eq!(guideline_code_in("(c-conv)"), None);
        assert_eq!(guideline_code_in("(C--)"), None);
        assert_eq!(guideline_code_in("trailing (C-FOO"), None);
    }

    #[test]
    fn parse_checklist_is_empty_for_a_page_without_guidelines() {
        // A page with headings but no `(C-…)` links yields no entries — nothing
        // is fabricated.
        let html = r#"<main><h1>Intro</h1><p>No guidelines here.</p></main>"#;
        assert!(parse_checklist(html).is_empty());
    }
}
