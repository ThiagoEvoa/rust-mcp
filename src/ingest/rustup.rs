//! Ingestion of the rustup book source (task `[020]`, issue #20).
//!
//! Wires the `rustup` collection (The rustup Book, an mdBook/rustbook site like
//! the API Guidelines, the Style Guide, and the other book sources) to the
//! shared pipeline:
//!
//! - **Discovery** reuses [`crate::ingest::books::discover_inventory`]: the
//!   rustup Book is book-structured, so the same structural, no-filename chapter
//!   discovery books the introduction (`index.html`) and every chapter —
//!   installation, toolchains, overrides, components, targets, and the
//!   toolchain-file behavior. The live sidebar is JS-populated, so discovery
//!   stays offline and structural — `discover_inventory` reads the link
//!   structure, it executes **no** upstream JavaScript.
//! - **Chapters** go through the shared [`split_article`](crate::ingest::section)
//!   splitter, so each section retains its headings, its command examples (the
//!   `rustup install` / `rustup toolchain …` / `rustup override set` /
//!   `rustup target add` invocations and the `rust-toolchain.toml` example), its
//!   qualifications, and its canonical anchor — with no checklist or `C-…` codes
//!   to resolve.
//!
//! The rustup Book documents the **toolchain installer** (`rustup`): how to
//! install Rust, manage toolchains, set overrides, add components and targets,
//! and pin a toolchain through a `rust-toolchain.toml` file. This server
//! **retrieves that documentation**; it never *runs* `rustup`. The calling agent
//! executes `rustup` and its subcommands through its own terminal tools, so the
//! adapter adds no command execution, project access, or code-review execution —
//! per `plan.md`'s product boundary ("The MCP retrieves documentation. The
//! calling agent executes Cargo and other commands through its own terminal
//! tools").
//!
//! The chapters carry **only upstream content** — the adapter adds no
//! server-authored retrieval hints — so the source's wording, examples, and
//! qualifications survive verbatim and stay separate from any retrieval hint
//! ("keep source text separate from server-authored retrieval hints").
//!
//! **Provenance.** rustup has its *own* documentation host
//! (`rust-lang.github.io/rustup/`) and *its own* rolling revision track; it is
//! **not** a `rustc` compiler release. The rustc Book lives on a different host
//! (`doc.rust-lang.org/rustc/`) and tracks the compiler. This adapter stamps
//! every section with `SourceId` [`SOURCE`] (`"rustup"`) and document title
//! [`DOC_TITLE`] (`"The rustup Book"`), so the source's provenance cannot be
//! conflated with the rustc compiler release. [`plan.md`] §2/rustup requires
//! the source's *independent* documentation revision be recorded — a downstream
//! concern for the `[029]+` sync pipeline, which reads the revision off the
//! `rustup` `Source`/`Release` rather than the rustc one. What this adapter
//! fixes now is the **identity**: a section fetched under the `rustup` host is
//! attributable to `rustup`, never to the `rustc` compiler release.
//!
//! No network in these tests: the chapter and navigation bodies are served from
//! the loopback [`FixtureServer`](crate::ingest::http_fixture::FixtureServer)
//! and `discover_inventory` runs over loopback navigation, so the whole stage is
//! exercisable offline.

// Transitional: these functions are the `[020]` ingestion API, exercised by the
// offline acceptance tests below; their first non-test consumer is the `[029]+`
// sync pipeline that ingests the corpus end-to-end.
#![allow(dead_code)]

use crate::ingest::fetch::Fetched;
use crate::ingest::section::{SplitConfig, SplitError, split_article};
use crate::model::{SchemaError, Section, SourceId};
use reqwest::Url;
use scraper::{Html, Selector};

/// The `SourceId` of the rustup Book collection.
pub const SOURCE: &str = "rustup";

/// The document title shared by every chapter of the rustup Book corpus, used
/// when a page's own heading cannot supply one. Distinct from the rustc Book's
/// `"The rustc Book"` — see the module's provenance note.
pub const DOC_TITLE: &str = "The rustup Book";

/// The entry URL of the rustup Book — the collection the discovery stage books
/// from the site's navigation. The rustup Book lives on `rust-lang.github.io`,
/// a *different host* from the rustc Book's `doc.rust-lang.org`.
pub const BOOK_URL: &str = "https://rust-lang.github.io/rustup/";

/// A typed, non-panicking failure from ingesting a rustup Book chapter.
///
/// Like the Style Guide and the rustc Book, the rustup Book has no checklist, so
/// — unlike the `api-guidelines` source (whose
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
            IngestError::Split(e) => write!(f, "rustup section split: {e}"),
            IngestError::Schema(e) => write!(f, "rustup section schema: {e}"),
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
/// chapter's file stem under the `rustup` boundary (`overrides` from
/// `…/rustup/overrides.html`, `index` from `…/rustup/index.html`).
///
/// A fragment is dropped (a section and its `#anchor` are one document) and a
/// missing boundary prefix falls back to the bare path stem, so an off-boundary
/// or fragment-only URL still yields a non-empty path rather than a panic.
pub fn chapter_path(final_url: &Url) -> String {
    // The registry boundary is the URL *path* prefix, not the full URL; `path()`
    // drops the scheme/host and any `#fragment` (a section and its anchor are one
    // document), leaving just the path to normalize.
    let path = final_url.path();
    let stripped = path.strip_prefix("/rustup/").unwrap_or(path);
    let stem = stripped.trim_start_matches('/');
    stem.strip_suffix(".html").unwrap_or(stem).to_string()
}

/// The document title of a rustup Book page: its `h1` when present, else its
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

/// Ingest one rustup Book *chapter* into its bounded [`Section`]s.
///
/// `fetched` is the chapter body already fetched through the gated fetcher. The
/// body is split by the shared [`split_article`] (which keeps examples with their
/// explanation — including the `rustup …` command examples and the
/// `rust-toolchain.toml` example — preserves the heading hierarchy and
/// qualifications as callouts, records the heading's original anchor, and splits
/// an oversized section into tagged continuations).
///
/// The rustup Book carries no checklist and no `C-…` codes, so — unlike
/// [`ingest_chapter`](crate::ingest::api_guidelines::ingest_chapter) — there is
/// no code enrichment: each section is returned exactly as the splitter produced
/// it, tagged for the `rustup` source. That is deliberate: the adapter must add
/// **no server-authored retrieval hint**, so the source's wording, examples, and
/// qualifications survive verbatim and stay separate from any retrieval hint
/// (`plan.md` §2/rustup and "keep source text separate from server-authored
/// retrieval hints").
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

    // No enrichment: the rustup Book has no checklist or `C-…` codes, so the
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
// Offline acceptance tests — plan.md §11 / task [020] / issue #20
//
// Both criteria from issue #20:
//   (1) The book inventory and representative command examples are captured —
//       proven by reusing the book discovery over a loopback nav (the site is
//       book-structured) and by splitting a loopback-served chapter that carries
//       the installation / toolchain / override / component / target /
//       toolchain-file examples.
//   (2) Independent rustup documentation provenance is not mislabeled as a Rust
//       compiler release — proven by stamping every section `rustup` / "The
//       rustup Book" (never `rustc` / "The rustc Book") and by the fixed
//       registry resolving the rustup host to `SourceCollection::Rustup`,
//       distinct from the rustc compiler's `SourceCollection::Rustc`.
//
// The chapter and navigation bodies are served from the loopback `FixtureServer`
// (or parsed from static HTML), so nothing touches the network.
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
    use crate::registry::{SourceCollection, resolve_url};
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
    //    shared book discovery (the site is book-structured). The rustup Book
    //    carries no checklist, so — like the `style-guide` and `rustc`
    //    discovery — only the introduction and the topical chapters are expected. --

    #[tokio::test]
    async fn discover_inventory_books_intro_and_topical_chapters() {
        // The rustup Book's static chapter inventory is its navigation page. The
        // live sidebar is JS-populated, but discovery stays structural and
        // offline: `discover_inventory` reads the `<a href>` links from the
        // page's *structure* (it executes no upstream JavaScript). The links are
        // absolute, registered-host `rust-lang.github.io/rustup/…` URLs (the real
        // upstream shape); the nav page itself is served from loopback, so the
        // chapters are booked without touching the network. A *relative* link
        // would resolve to the loopback host and be refused as off-boundary.
        let nav_html = r#"<html><body>
            <nav id="sidebar"><ol class="chapter">
              <li><a href="https://rust-lang.github.io/rustup/index.html">Introduction</a></li>
              <li><a href="https://rust-lang.github.io/rustup/installation.html">Installation</a></li>
              <li><a href="https://rust-lang.github.io/rustup/overrides.html">Overrides</a></li>
            </ol></nav>
          </body></html>"#;
        let server = FixtureServer::start(vec![(
            "/rustup/index.html".to_string(),
            Behavior::Text {
                body: nav_html.as_bytes().to_vec(),
                etag: Some("\"rustup-toc-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/rustup/index.html", server.addr))
            .expect("a valid loopback nav URL");

        let (inv, diff) = discover_inventory(&client, &base_url, "rustup", None)
            .await
            .expect("discovery succeeds offline");

        let keys: Vec<String> = inv.urls.iter().map(|u| u.as_str().to_string()).collect();
        assert!(
            keys.iter().any(|k| k.ends_with("/rustup/index.html")),
            "the introduction (index.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter()
                .any(|k| k.ends_with("/rustup/installation.html")),
            "a topical chapter (installation.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/rustup/overrides.html")),
            "a second topical chapter (overrides.html) must be booked, got {keys:?}"
        );
        // No delta against an omitted expected set, so the discovery is complete.
        assert!(
            diff.is_complete(),
            "with no expected set the diff must be complete, got {diff:?}"
        );
    }

    // -- Criterion 1: the representative `rustup …` command examples and the
    //    `rust-toolchain.toml` toolchain-file example survive chapter extraction
    //    verbatim, with each section's citation (source tag + canonical anchor +
    //    content hash). The topics the issue/plan call out — installation,
    //    toolchains, overrides, components, targets, toolchain files — each land
    //    as a section carrying its documented invocation. --

    /// A rustup Book chapter carrying the six reference topics, each with its
    /// representative command example: installation (`rustup install`),
    /// toolchains (`rustup toolchain install`/`link`), overrides
    /// (`rustup override set`), components (`rustup component add`), targets
    /// (`rustup target add`), and the toolchain file (`rust-toolchain.toml`).
    ///
    /// The examples are in fenced blocks; the shared converter renders a
    /// non-`rust` fence as a bare ` ``` ` block, so the example *text* — not a
    /// language tag — is what must survive verbatim. The `<nav>` carries a
    /// sibling link that must be stripped as chrome.
    const TOOLCHAIN_CHAPTER_HTML: &str = r#"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="index.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="installation">Installation</h1>
    <p>Rustup is the Rust toolchain installer. On the machine where you want Rust, run the following to install it:</p>
    <pre><code class="language-bash">rustup install</code></pre>
    <h2 id="toolchains">Toolchains</h2>
    <p>A toolchain is a named Rust release. Install a named toolchain and link a local one:</p>
    <pre><code class="language-bash">rustup toolchain install stable
rustup toolchain install nightly
rustup toolchain link my-toolchain /path/to/rustc</code></pre>
    <h2 id="overrides">Overrides</h2>
    <p>An override is a toolchain selection that applies to a directory. Set it with:</p>
    <pre><code class="language-bash">rustup override set stable
rustup override set my-toolchain</code></pre>
    <h2 id="components">Components</h2>
    <p>Components are add-on packages such as <code>rustfmt</code> and <code>clippy</code>:</p>
    <pre><code class="language-bash">rustup component add rustfmt
rustup component add clippy</code></pre>
    <h2 id="targets">Targets</h2>
    <p>Targets are add-on packages that enable cross-compilation:</p>
    <pre><code class="language-bash">rustup target add x86_64-unknown-linux-gnu
rustup target add aarch64-apple-darwin</code></pre>
    <h2 id="toolchain-file">The toolchain file</h2>
    <p>A <code>rust-toolchain.toml</code> file pins the toolchain, its overrides, and its components for a directory:</p>
    <pre><code class="language-toml">
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
targets = ["x86_64-unknown-linux-gnu"]
</code></pre>
  </main>
</body></html>"#;

    #[tokio::test]
    async fn representative_command_examples_survive_verbatim() {
        let server = FixtureServer::start(vec![(
            "/rustup/installation.html".to_string(),
            Behavior::Text {
                body: TOOLCHAIN_CHAPTER_HTML.as_bytes().to_vec(),
                etag: Some("\"toolchain-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/rustup/installation.html", server.addr))
            .expect("a valid loopback chapter URL");

        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        // AC1: every representative command example survives verbatim across the
        // section bodies (the bare-fence text, not a language tag).
        let all: String = sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for example in [
            "rustup install",
            "rustup toolchain install stable",
            "rustup toolchain link my-toolchain",
            "rustup override set stable",
            "rustup component add rustfmt",
            "rustup target add x86_64-unknown-linux-gnu",
            "rustup target add aarch64-apple-darwin",
        ] {
            assert!(
                all.contains(example),
                "the example `{example}` must survive verbatim in the section bodies:\n{all}"
            );
        }
        // The toolchain-file example survives with its documented contents.
        assert!(
            all.contains("rust-toolchain.toml"),
            "the toolchain-file name must survive verbatim:\n{all}"
        );
        assert!(
            all.contains("channel = \"stable\""),
            "the toolchain-file contents must survive verbatim:\n{all}"
        );
        assert!(
            all.contains("targets = [\"x86_64-unknown-linux-gnu\"]"),
            "the toolchain-file target list must survive verbatim:\n{all}"
        );

        // Each reference topic lands as a section, stamped for the `rustup`
        // source, with its canonical anchor and a non-empty content hash.
        for topic in [
            "installation",
            "toolchains",
            "overrides",
            "components",
            "targets",
            "toolchain-file",
        ] {
            let section = sections
                .iter()
                .find(|s| s.anchor.as_deref() == Some(topic))
                .unwrap_or_else(|| panic!("a section carrying the `{topic}` anchor must exist"));
            assert_eq!(
                section.source_id,
                SourceId::new(SOURCE).expect("a valid source id")
            );
            assert!(
                section
                    .canonical_url
                    .as_str()
                    .contains(&format!("#{topic}")),
                "the `{topic}` section's canonical URL must carry its anchor, got {}",
                section.canonical_url
            );
            assert!(
                !section.content_hash.as_str().is_empty(),
                "the `{topic}` section must carry a non-empty content hash"
            );
        }
    }

    // -- Criterion 2: a rustup chapter's provenance is the `rustup` source, NOT
    //    the rustc compiler release. Every section is tagged `rustup` / "The
    //    rustup Book" — never `rustc` / "The rustc Book" — and the fixed
    //    registry resolves the rustup host to `SourceCollection::Rustup`,
    //    distinct from the rustc compiler's `SourceCollection::Rustc`. --

    #[tokio::test]
    async fn rustup_sections_are_never_mislabeled_as_the_rustc_compiler() {
        // A chapter fetched under the rustup host (`rust-lang.github.io/rustup/`)
        // must be attributable to the `rustup` source, not the rustc compiler.
        let server = FixtureServer::start(vec![(
            "/rustup/overrides.html".to_string(),
            Behavior::Text {
                body: TOOLCHAIN_CHAPTER_HTML.as_bytes().to_vec(),
                etag: Some("\"overrides-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        // A page fetched under the *rustup* host — the body is the same fixture,
        // but the provenance comes from where it was fetched, not its contents.
        let base_url = Url::parse(&format!("http://{}/rustup/overrides.html", server.addr))
            .expect("a valid loopback chapter URL");
        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        let rustc_id = SourceId::new("rustc").expect("the rustc source id is valid");
        for section in &sections {
            // (2a) The section is stamped for the independent `rustup` source…
            assert_eq!(
                section.source_id,
                SourceId::new(SOURCE).expect("a valid source id")
            );
            // …and is provably NOT the rustc compiler's source id.
            assert_ne!(
                section.source_id, rustc_id,
                "a rustup section must never carry the rustc compiler's source id, got {}",
                section.source_id
            );
            // (2b) The document title is a rustup chapter's own heading (or the
            // "The rustup Book" fallback when a page has no heading) — never the
            // rustc Book's title.
            assert_ne!(
                section.doc_title.as_str(),
                "The rustc Book",
                "a rustup section must never carry the rustc Book's title, got {}",
                section.doc_title
            );
            // (2c) The section's own provenance carries no compiler release: the
            // rustup revision is tracked independently, so no `rustc` token leaks
            // into the section's source or title.
            assert!(
                section.source_id.as_str() != "rustc" && !section.doc_title.contains("rustc"),
                "rustup provenance must not name the rustc compiler release, got source {:?} / title {:?}",
                section.source_id.as_str(),
                section.doc_title
            );
        }
    }

    /// The fixed registry keeps the two sources distinct at the *boundary* level
    /// too: a rustup URL resolves to `SourceCollection::Rustup` and a rustc URL
    /// to `SourceCollection::Rustc`, so the provenance cannot collapse across the
    /// two. This is the registry-side half of "not mislabeled as a compiler
    /// release".
    #[test]
    fn registry_keeps_rustup_distinct_from_the_rustc_compiler() {
        // A rustup URL resolves to the `Rustup` collection on its own host.
        let rustup = resolve_url("https://rust-lang.github.io/rustup/overrides.html")
            .expect("rustup URL resolves");
        assert_eq!(rustup.collection, SourceCollection::Rustup);
        assert_eq!(rustup.collection.id(), "rustup");

        // A rustc URL resolves to the *different* `Rustc` collection on its own
        // host — the two are not the same source.
        let rustc =
            resolve_url("https://doc.rust-lang.org/rustc/basics.html").expect("rustc URL resolves");
        assert_eq!(rustc.collection, SourceCollection::Rustc);
        assert_eq!(rustc.collection.id(), "rustc");

        // The two collections are distinct, and the rustup source id matches the
        // `Rustup` collection — never the `Rustc` one — so a section ingested
        // under the rustup host cannot be mislabeled as a compiler release.
        assert_ne!(SourceCollection::Rustup, SourceCollection::Rustc);
        assert_ne!(SourceCollection::Rustup.id(), SourceCollection::Rustc.id());
        assert_eq!(
            SourceId::new(SOURCE).unwrap(),
            SourceId::new(SourceCollection::Rustup.id()).unwrap()
        );
        assert_ne!(
            SourceId::new(SOURCE).unwrap(),
            SourceId::new(SourceCollection::Rustc.id()).unwrap()
        );
    }

    // -- Pure helpers, exercised without a server. --

    #[test]
    fn chapter_path_derives_the_file_stem() {
        assert_eq!(
            chapter_path(&Url::parse("https://rust-lang.github.io/rustup/overrides.html").unwrap()),
            "overrides"
        );
        assert_eq!(
            chapter_path(&Url::parse("https://rust-lang.github.io/rustup/index.html#top").unwrap()),
            "index"
        );
        // A nested chapter keeps its sub-path under the `rustup` boundary.
        assert_eq!(
            chapter_path(
                &Url::parse("https://rust-lang.github.io/rustup/guide/toolchains.html").unwrap()
            ),
            "guide/toolchains"
        );
    }

    #[test]
    fn document_title_prefers_the_h1_then_the_title_tag() {
        let with_h1 = r#"<html><body><h1>The rustup Book</h1></body></html>"#;
        let url = Url::parse("https://rust-lang.github.io/rustup/index.html").unwrap();
        assert_eq!(document_title(with_h1, &url), "The rustup Book");

        // Without an `h1`, the `<title>` is used.
        let with_title =
            r#"<html><head><title>Overrides</title></head><body><p>x</p></body></html>"#;
        assert_eq!(document_title(with_title, &url), "Overrides");

        // With neither, the chapter path is the last resort.
        let headingless = r#"<html><body><p>no heading, no title</p></body></html>"#;
        assert_eq!(document_title(headingless, &url), "index");

        // The shared corpus title is the ultimate fallback for a bare, off-boundary
        // URL with no heading, title, or path.
        let bare = Url::parse("https://rust-lang.github.io/rustup/").unwrap();
        assert_eq!(
            document_title("<html><body></body></html>", &bare),
            DOC_TITLE
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
            requested: Url::parse("https://rust-lang.github.io/rustup/does-not-exist.html")
                .unwrap(),
            final_url: Url::parse("https://rust-lang.github.io/rustup/does-not-exist.html")
                .unwrap(),
            status: StatusCode::NOT_FOUND,
            body: b"not found".to_vec(),
            content_type: None,
            etag: None,
            last_modified: None,
            content_length: None,
        };
        let _ = ingest_chapter(&missing); // returns Ok(empty) or an error; never panics

        // A chrome-only page (no article body) fails with a typed error rather
        // than panicking — the posture the `rustc` adapter's equivalent test
        // locks in.
        let chrome = Fetched {
            requested: Url::parse("https://rust-lang.github.io/rustup/index.html").unwrap(),
            final_url: Url::parse("https://rust-lang.github.io/rustup/index.html").unwrap(),
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
