//! Ingestion of the Clippy *book* source (task `[023]`, issue #23).
//!
//! Wires the `clippy` collection's **book** half — the Clippy Book on
//! `doc.rust-lang.org/clippy/` — to the shared pipeline. The Clippy source
//! spans two boundaries (see [`crate::registry`]): the *book* (HTML navigation
//! on `doc.rust-lang.org`, parsed like the other books) and the *catalog*
//! (per-lint extraction on `clippy.rust-lang.org/stable/`, task `[024]`). This
//! module owns the **book**; the catalog stays with [`crate::ingest::clippy`]
//! as an explicit, non-conflicting seam so `[024]` can fill in the per-lint
//! extraction without touching this book path.
//!
//! - **Discovery** reuses [`crate::ingest::books::discover_inventory`]: the
//!   Clippy Book is book-structured, so the same structural, no-filename
//!   chapter discovery books the introduction (`index.html`) and every chapter —
//!   the installation guidance, the Cargo integration and usage
//!   (`usage.html`), the configuration (`configuration.html`), and the
//!   lint-group topics. Its static inventory is the book's navigation; the live
//!   sidebar is JS-populated, so discovery stays offline and structural —
//!   `discover_inventory` reads the link structure, it executes **no** upstream
//!   JavaScript.
//! - **Chapters** go through the shared [`split_article`](crate::ingest::section)
//!   splitter, so each section retains its headings, its examples (the
//!   `cargo clippy …` invocations and the option-forwarding `cargo clippy -- …`
//!   lines), its documented **configuration** (the `Cargo.toml` options and the
//!   command-line options a table the shared HTML converter already renders to
//!   GFM — see the design note below), its stability/version qualifications
//!   (rendered as callouts), and its canonical anchor — like the `rustdoc`,
//!   `rustc`, and `rustup` books, with no checklist or `C-…` codes to resolve.
//!
//! The Clippy Book documents **how to install and run Clippy and how to
//! configure it** — installing the linter through the toolchain, running it
//! through `cargo clippy`, forwarding lints options after `--`, and tuning it
//! with `Cargo.toml` / `clippy.toml` / command-line options. This server
//! **retrieves that documentation**; it never *runs* `cargo clippy`. The
//! calling agent executes those commands through its own terminal tools, so the
//! adapter adds no command execution, project access, or code-review execution —
//! per `plan.md`'s product boundary ("The MCP retrieves documentation. The
//! calling agent executes Cargo and other commands through its own terminal
//! tools").
//!
//! The chapters carry **only upstream content** — the adapter adds no
//! server-authored retrieval hints — so the source's wording, examples, options,
//! and qualifications survive verbatim and stay separate from any retrieval hint
//! ("keep source text separate from server-authored retrieval hints"). The
//! individual lint explanations and their lint IDs are the catalog's concern
//! (`[024]`); this book path does not extract them.
//!
//! ## Design: shared splitter, no dedicated command extractor
//!
//! `plan.md` §2/clippy requires the book to "include installation, usage,
//! configuration, Cargo integration, lint groups, and individual lint
//! explanations/examples." A first instinct is to build a dedicated extractor
//! like `cargo`'s [`extract_command_manual`](crate::ingest::cargo::extract_command_manual).
//! That extractor exists because the Cargo command-manual pages feed a later,
//! curated, version-aware `CommandEntry` catalog (tasks `[039]`-`[044]`) and so
//! need a structured synopsis / option-group / canonical `CommandKey` per
//! *subcommand*. The Clippy Book carries **no such catalog dependency**: its
//! `usage.html` / `configuration.html` are book chapters documenting one
//! program's options and its configuration files, and the shared HTML converter
//! already renders a `<table>` to a GFM Markdown table (option name +
//! description survive) and a `<pre>` to a fenced block. The rustdoc, rustc, and
//! rustup adapters confirm the precedent — book-structured options/examples/
//! qualifications are retained by the shared splitter with no dedicated
//! extractor. This adapter therefore follows the `rustc`/`rustup`/`rustdoc`
//! precedent: discovery + [`ingest_chapter`] only. The offline acceptance tests
//! below assert that the option-forwarding example and the configuration
//! *table*, plus a stability qualification, all survive the shared splitter
//! verbatim — proving the options are retained without a second adapter.
//!
//! No network in these tests: the chapter and navigation bodies are served from
//! the loopback [`FixtureServer`](crate::ingest::http_fixture::FixtureServer)
//! and `discover_inventory` runs over loopback navigation, so the whole stage is
//! exercisable offline.

// Transitional: these functions are the `[023]` ingestion API, exercised by the
// offline acceptance tests below; their first non-test consumer is the `[029]+`
// sync pipeline that ingests the corpus end-to-end.
#![allow(dead_code)]

use crate::ingest::fetch::Fetched;
use crate::ingest::section::{SplitConfig, SplitError, split_article};
use crate::model::{SchemaError, Section, SourceId};
use reqwest::Url;
use scraper::{Html, Selector};

/// The `SourceId` of the Clippy Book collection.
pub const SOURCE: &str = "clippy";

/// The document title shared by every chapter of the Clippy Book corpus, used
/// when a page's own heading cannot supply one. Distinct from the catalog's
/// per-lint titles — the Clippy Book documents *how to use and configure*
/// Clippy, not individual lints.
pub const DOC_TITLE: &str = "The Clippy Book";

/// The entry URL of the Clippy Book — the collection the discovery stage books
/// from the site's navigation. The Clippy Book lives on `doc.rust-lang.org`.
pub const BOOK_URL: &str = "https://doc.rust-lang.org/clippy/";

/// A typed, non-panicking failure from ingesting a Clippy Book chapter.
///
/// Like the Style Guide, the rustc Book, the rustup Book, and the rustdoc Book,
/// the Clippy Book has no checklist, so — unlike the `api-guidelines` source
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
            IngestError::Split(e) => write!(f, "clippy section split: {e}"),
            IngestError::Schema(e) => write!(f, "clippy section schema: {e}"),
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
/// chapter's file stem under the `clippy` boundary (`usage` from
/// `…/clippy/usage.html`, `index` from `…/clippy/index.html`).
///
/// A fragment is dropped (a section and its `#anchor` are one document) and a
/// missing boundary prefix falls back to the bare path stem, so an off-boundary
/// or fragment-only URL still yields a non-empty path rather than a panic.
pub fn chapter_path(final_url: &Url) -> String {
    // The registry boundary is the URL *path* prefix, not the full URL; `path()`
    // drops the scheme/host and any `#fragment` (a section and its anchor are one
    // document), leaving just the path to normalize.
    let path = final_url.path();
    let stripped = path.strip_prefix("/clippy/").unwrap_or(path);
    let stem = stripped.trim_start_matches('/');
    stem.strip_suffix(".html").unwrap_or(stem).to_string()
}

/// The document title of a Clippy Book page: its `h1` when present, else its
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

/// Ingest one Clippy Book *chapter* into its bounded [`Section`]s.
///
/// `fetched` is the chapter body already fetched through the gated fetcher. The
/// body is split by the shared [`split_article`] (which keeps examples with
/// their explanation — including the `cargo clippy …` invocations and the
/// option-forwarding `cargo clippy -- …` lines — renders the configuration and
/// option tables to GFM, preserves the heading hierarchy and stability/version
/// qualifications as callouts, records the heading's original anchor, and splits
/// an oversized section into tagged continuations).
///
/// The Clippy Book carries no checklist and no `C-…` codes, so — unlike
/// [`ingest_chapter`](crate::ingest::api_guidelines::ingest_chapter) — there is
/// no code enrichment: each section is returned exactly as the splitter produced
/// it. That is deliberate: the adapter must add **no server-authored retrieval
/// hint**, so the source's wording, examples, options, and qualifications survive
/// verbatim and stay separate from any retrieval hint (`plan.md` §2/clippy and
/// "keep source text separate from server-authored retrieval hints").
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

    // No enrichment: the Clippy Book has no checklist or `C-…` codes, so the
    // sections are returned exactly as the splitter produced them, preserving
    // upstream content — including the option-forwarding examples, the
    // configuration tables, and any qualification — verbatim.
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
// Offline acceptance tests — plan.md §11 / task [023] / issue #23
//
// Both criteria from issue #23:
//   (1) Installation, Cargo usage, configuration, and lint-group topics are
//       discoverable — proven by reusing the book discovery over a loopback nav
//       (the site is book-structured) and by splitting loopback-served chapters
//       that carry the installation, Cargo-usage, configuration, and lint-group
//       topics.
//   (2) Option forwarding and source qualifications remain intact — proven by
//       splitting a loopback-served `usage.html` chapter and asserting the
//       option-forwarding `cargo clippy -- …` example, the configuration
//       *table*, and a stability/nightly qualification each survive verbatim in
//       the section bodies, WITH their citation (source tag + canonical anchor +
//       content hash).
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
    //    shared book discovery (the site is book-structured). The Clippy Book
    //    carries no checklist, so — like the `style-guide`, `rustc`, `rustup`,
    //    and `rustdoc` discovery — only the introduction and the topical
    //    chapters are expected. --

    #[tokio::test]
    async fn discover_inventory_books_intro_and_topical_chapters() {
        // The Clippy Book's static chapter inventory is its navigation page. The
        // live sidebar is JS-populated, but discovery stays structural and
        // offline: `discover_inventory` reads the `<a href>` links from the
        // page's *structure* (it executes no upstream JavaScript). The links are
        // absolute, registered-host `doc.rust-lang.org/clippy/…` URLs (the real
        // upstream shape); the nav page itself is served from loopback, so the
        // chapters are booked without touching the network. A *relative* link
        // would resolve to the loopback host and be refused as off-boundary.
        let nav_html = r#"<html><body>
            <nav id="sidebar"><ol class="chapter">
              <li><a href="https://doc.rust-lang.org/clippy/index.html">Introduction</a></li>
              <li><a href="https://doc.rust-lang.org/clippy/usage.html">Usage</a></li>
              <li><a href="https://doc.rust-lang.org/clippy/configuration.html">Configuration</a></li>
              <li><a href="https://doc.rust-lang.org/clippy/lint-groups.html">Lint groups</a></li>
            </ol></nav>
          </body></html>"#;
        let server = FixtureServer::start(vec![(
            "/clippy/index.html".to_string(),
            Behavior::Text {
                body: nav_html.as_bytes().to_vec(),
                etag: Some("\"clippy-toc-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/clippy/index.html", server.addr))
            .expect("a valid loopback nav URL");

        let (inv, diff) = discover_inventory(&client, &base_url, "clippy", None)
            .await
            .expect("discovery succeeds offline");

        let keys: Vec<String> = inv.urls.iter().map(|u| u.as_str().to_string()).collect();
        assert!(
            keys.iter().any(|k| k.ends_with("/clippy/index.html")),
            "the introduction (index.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/clippy/usage.html")),
            "the Cargo usage chapter (usage.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter()
                .any(|k| k.ends_with("/clippy/configuration.html")),
            "the configuration chapter (configuration.html) must be booked, got {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("/clippy/lint-groups.html")),
            "the lint-group chapter (lint-groups.html) must be booked, got {keys:?}"
        );
        // No delta against an omitted expected set, so the discovery is complete.
        assert!(
            diff.is_complete(),
            "with no expected set the diff must be complete, got {diff:?}"
        );
    }

    // -- Criterion 1: the installation topic lands as a retained section. The
    //    Clippy Book's guidance on installing the linter through the toolchain
    //    becomes a section carrying its example and citation. --

    /// A chapter carrying the installation topic issue #23 criterion 1 names:
    /// installing Clippy through `rustup` / the toolchain component, with its
    /// representative `rustup component add clippy` example in a fenced block.
    /// The `<nav>` carries a sibling link that must be stripped as chrome.
    const INSTALLATION_CHAPTER_HTML: &str = r#"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="index.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="top">Introduction</h1>
    <h2 id="installation">Installation</h2>
    <p>Clippy ships with the Rust toolchain. On a toolchain installed through <code>rustup</code>, enable the <code>clippy</code> component:</p>
    <pre><code class="language-bash">rustup component add clippy</code></pre>
    <p>With the component installed, Clippy runs through Cargo as a subcommand.</p>
  </main>
</body></html>"#;

    #[tokio::test]
    async fn installation_topic_resolves_to_section() {
        let server = FixtureServer::start(vec![(
            "/clippy/index.html".to_string(),
            Behavior::Text {
                body: INSTALLATION_CHAPTER_HTML.as_bytes().to_vec(),
                etag: Some("\"clippy-install-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/clippy/index.html", server.addr))
            .expect("a valid loopback chapter URL");

        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        // AC1: the installation topic lands as a section, stamped for the
        // `clippy` source, with its canonical anchor and a non-empty content
        // hash.
        let section = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("installation"))
            .expect("a section carrying the `installation` anchor must exist");
        assert_eq!(
            section.source_id,
            SourceId::new(SOURCE).expect("a valid source id")
        );
        assert!(
            section.canonical_url.as_str().contains("#installation"),
            "the installation section's canonical URL must carry its anchor, got {}",
            section.canonical_url
        );
        assert!(
            !section.content_hash.as_str().is_empty(),
            "the installation section must carry a non-empty content hash"
        );
        // The installation example survives verbatim in the section body.
        assert!(
            section.body.contains("rustup component add clippy"),
            "the installation example must survive verbatim in the section body:\n{}",
            section.body
        );
    }

    // -- Criteria 1 + 2: the usage chapter retains the Cargo-usage invocation,
    //    the option-forwarding `cargo clippy -- …` example, the configuration
    //    *table*, and a stability/nightly qualification — all verbatim, with
    //    each section's citation. This is the evidence that the documented
    //    options and qualifications survive the shared splitter (no dedicated
    //    extractor). --

    /// A `usage.html`-style chapter: a Cargo-usage synopsis, an option-forwarding
    /// block showing `cargo clippy -- …` (the lints options forwarded after
    /// `--`), a documented options *table* (the shared converter renders it to
    /// GFM), and a stability/version qualification rendered as a `<blockquote>`
    /// callout (a `nightly`-only option). The `<nav>` carries a sibling link
    /// that must be stripped as chrome.
    const USAGE_HTML: &str = r#"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="index.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="top">Usage</h1>
    <h2 id="cargo-usage">Running Clippy through Cargo</h2>
    <p>Clippy runs as a Cargo subcommand over the current package:</p>
    <pre><code class="language-bash">cargo clippy
cargo clippy --all-targets</code></pre>
    <h2 id="forwarding-options">Forwarding lints options</h2>
    <p>Options are passed to Clippy after a <code>--</code> separator, forwarded to the underlying driver:</p>
    <pre><code class="language-bash">cargo clippy -- -D warnings
cargo clippy -- -W clippy::pedantic
cargo clippy -- --cfg test</code></pre>
    <h2 id="options">Options</h2>
    <table class="options"><thead>
      <tr><th>Option</th><th>Description</th></tr>
    </thead><tbody>
      <tr><td><code>-W &lt;LINT&gt;</code></td><td>Set lint warning level for a single lint.</td></tr>
      <tr><td><code>-D &lt;LINT&gt;</code></td><td>Set lint denied level, turning a warning into a hard error.</td></tr>
      <tr><td><code>--cfg &lt;CFG&gt;</code></td><td>Passthrough a configuration flag to the driver.</td></tr>
    </tbody></table>
    <h2 id="options-stability">Stability of options</h2>
    <p>Some options are versioned with the toolchain:</p>
    <blockquote><strong>Stability:</strong> The <code>-Zunstable-options</code> flag is <code>nightly</code>-only and may change without notice.</blockquote>
  </main>
</body></html>"#;

    #[tokio::test]
    async fn usage_page_retains_forwarding_options_and_qualification() {
        let server = FixtureServer::start(vec![(
            "/clippy/usage.html".to_string(),
            Behavior::Text {
                body: USAGE_HTML.as_bytes().to_vec(),
                etag: Some("\"clippy-usage-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/clippy/usage.html", server.addr))
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

        // AC2: the option-forwarding examples survive verbatim — the `--`
        // separator and the forwarded lints options are preserved exactly.
        for example in [
            "cargo clippy -- -D warnings",
            "cargo clippy -- -W clippy::pedantic",
            "cargo clippy -- --cfg test",
        ] {
            assert!(
                all.contains(example),
                "the option-forwarding example `{example}` must survive verbatim in the section bodies:\n{all}"
            );
        }
        // The plain Cargo-usage invocations survive too.
        for example in ["cargo clippy", "cargo clippy --all-targets"] {
            assert!(
                all.contains(example),
                "the Cargo-usage example `{example}` must survive verbatim in the section bodies:\n{all}"
            );
        }

        // AC1 + AC2: the documented options survive as a retained table — the
        // option names and their descriptions are verbatim in the section
        // bodies. The shared converter rendered the `<table>` to GFM, so the
        // cells are present even though this adapter added no dedicated
        // extractor.
        for option in ["-W <LINT>", "-D <LINT>", "--cfg <CFG>"] {
            assert!(
                all.contains(option),
                "the documented option `{option}` must survive verbatim in the section bodies:\n{all}"
            );
        }
        // A description survives too, keeping the option's context.
        assert!(
            all.contains("turning a warning into a hard error"),
            "an option description must survive with its context:\n{all}"
        );

        // AC2: the source qualification stays visible (verbatim, as a callout)
        // WITH its citation — so it is provably upstream, not server-authored.
        let stability = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("options-stability"))
            .expect("a section carrying the `options-stability` anchor must exist");
        assert!(
            stability.body.contains("nightly")
                && stability.body.contains("may change without notice"),
            "the stability qualification must stay visible in the body:\n{}",
            stability.body
        );
        assert!(
            stability.body.contains("Stability:"),
            "the qualification's `Stability:` label must survive:\n{}",
            stability.body
        );

        // The retained options, examples, and qualification each carry their
        // citation: the section is tagged for the `clippy` source, its
        // canonical URL carries the original anchor, and it carries a content
        // hash.
        for section in &sections {
            assert_eq!(
                section.source_id,
                SourceId::new(SOURCE).expect("a valid source id")
            );
            assert!(!section.content_hash.as_str().is_empty());
        }
        let options = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("options"))
            .expect("a section carrying the `options` anchor must exist");
        assert!(
            options.canonical_url.as_str().contains("#options"),
            "the options section's canonical URL must carry its anchor, got {}",
            options.canonical_url
        );
    }

    // -- Criterion 1: the configuration topics land as retained sections. The
    //    Clippy Book's guidance on the `Cargo.toml` / `clippy.toml`
    //    configuration each becomes a section carrying its example and citation. --

    /// A `configuration.html`-style chapter carrying the two configuration topics
    /// issue #23 criterion 1 names: the `Cargo.toml` `[lints]` table and the
    /// `clippy.toml` file. Each sits under its own `h2` with its representative
    /// config example in a fenced block; the `<nav>` carries a sibling link that
    /// must be stripped as chrome.
    const CONFIGURATION_CHAPTER_HTML: &str = r#"<!DOCTYPE html>
<html><body>
  <nav id="sidebar"><ol class="chapter"><li><a href="index.html">Introduction</a></li></ol></nav>
  <main>
    <h1 id="top">Configuration</h1>
    <h2 id="cargo-toml">Cargo.toml</h2>
    <p>Lint levels can be set per-crate in <code>Cargo.toml</code> under a <code>[lints]</code> table:</p>
    <pre><code class="language-toml">[lints.clippy]
pedantic = "warn"
</code></pre>
    <h2 id="clippy-toml">clippy.toml</h2>
    <p>A project-local <code>clippy.toml</code> file tunes Clippy's configuration options:</p>
    <pre><code class="language-toml"># Maximum length of a literal before a warning is emitted.
too-many-arguments-threshold = 9
</code></pre>
  </main>
</body></html>"#;

    #[tokio::test]
    async fn configuration_topics_resolve_to_sections() {
        let server = FixtureServer::start(vec![(
            "/clippy/configuration.html".to_string(),
            Behavior::Text {
                body: CONFIGURATION_CHAPTER_HTML.as_bytes().to_vec(),
                etag: Some("\"clippy-config-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let base_url = Url::parse(&format!("http://{}/clippy/configuration.html", server.addr))
            .expect("a valid loopback chapter URL");

        let fetched = client
            .fetch(&base_url)
            .await
            .expect("chapter fetch succeeds offline");
        let sections = ingest_chapter(&fetched).expect("ingestion succeeds");

        // AC1: the `Cargo.toml` and `clippy.toml` configuration topics each land
        // as a section, stamped for the `clippy` source, with its canonical
        // anchor and a non-empty content hash.
        for topic in ["cargo-toml", "clippy-toml"] {
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

        // The configuration examples survive verbatim in the section bodies (the
        // fenced text, not a language tag).
        let all: String = sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for example in ["pedantic = \"warn\"", "too-many-arguments-threshold = 9"] {
            assert!(
                all.contains(example),
                "the configuration example `{example}` must survive verbatim in the section bodies:\n{all}"
            );
        }
    }

    // -- Pure helpers, exercised without a server. --

    #[test]
    fn chapter_path_derives_the_file_stem() {
        assert_eq!(
            chapter_path(&Url::parse("https://doc.rust-lang.org/clippy/usage.html").unwrap()),
            "usage"
        );
        assert_eq!(
            chapter_path(&Url::parse("https://doc.rust-lang.org/clippy/index.html#top").unwrap()),
            "index"
        );
        // A nested chapter keeps its sub-path under the `clippy` boundary.
        assert_eq!(
            chapter_path(
                &Url::parse("https://doc.rust-lang.org/clippy/configuration.html").unwrap()
            ),
            "configuration"
        );
    }

    #[test]
    fn document_title_prefers_the_h1_then_the_title_tag() {
        let with_h1 = r#"<html><body><h1>The Clippy Book</h1></body></html>"#;
        let url = Url::parse("https://doc.rust-lang.org/clippy/index.html").unwrap();
        assert_eq!(document_title(with_h1, &url), "The Clippy Book");

        // Without an `h1`, the `<title>` is used.
        let with_title = r#"<html><head><title>Usage</title></head><body><p>x</p></body></html>"#;
        assert_eq!(document_title(with_title, &url), "Usage");

        // With neither, the chapter path is the last resort.
        let headingless = r#"<html><body><p>no heading, no title</p></body></html>"#;
        assert_eq!(document_title(headingless, &url), "index");

        // The shared corpus title is the ultimate fallback for a bare, off-boundary
        // URL with no heading, title, or path.
        let bare = Url::parse("https://doc.rust-lang.org/clippy/").unwrap();
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
            requested: Url::parse("https://doc.rust-lang.org/clippy/does-not-exist.html").unwrap(),
            final_url: Url::parse("https://doc.rust-lang.org/clippy/does-not-exist.html").unwrap(),
            status: StatusCode::NOT_FOUND,
            body: b"not found".to_vec(),
            content_type: None,
            etag: None,
            last_modified: None,
            content_length: None,
        };
        let _ = ingest_chapter(&missing); // returns Ok(empty) or an error; never panics

        // A chrome-only page (no article body) fails with a typed error rather
        // than panicking — the posture the `rustc`/`rustup`/`rustdoc`
        // adapters' equivalent tests lock in.
        let chrome = Fetched {
            requested: Url::parse("https://doc.rust-lang.org/clippy/index.html").unwrap(),
            final_url: Url::parse("https://doc.rust-lang.org/clippy/index.html").unwrap(),
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
