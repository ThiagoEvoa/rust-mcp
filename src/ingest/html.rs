//! Shared HTML→Markdown article converter (task `[013]`, issue #13).
//!
//! Turns one in-boundary documentation page's HTML into faithful Markdown
//! (`plan.md` §5 steps 3–4). Strips navigation, scripts, styles, and the
//! repeated book chrome a Rust book site lays around content; preserves the
//! article's heading hierarchy, tables, lists, stability/version callouts,
//! links, and fenced Rust examples. Relative content links are resolved through
//! the registry-gated resolver (`crate::ingest::resolve::resolve_document`) so
//! they stay valid and in-boundary. Reusable by the book, rustfmt, and clippy
//! ingestion stages; no panics on malformed or hostile HTML.

#![allow(dead_code)] // transitional: first consumer is the [014] section-splitting stage

use crate::ingest::resolve::resolve_document;
use reqwest::Url;
use scraper::{ElementRef, Html, Node, Selector};

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// A faithful Markdown extraction of one in-boundary page.
///
/// `body` holds the converted Markdown — examples, tables, callouts, and
/// resolved links retained. `heading_path` is the heading breadcrumb
/// (most-to-least specific). `anchor` is the first heading `id` if present.
/// `canonical_url` is the page's canonical URL.
///
/// Compatible with the `Section` fields the `[014]` splitting stage needs.
#[allow(dead_code)] // first consumer is the [014] section-splitting stage
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArticleExtraction {
    /// The converted Markdown body.
    pub body: String,
    /// The heading hierarchy, most-to-least specific.
    pub heading_path: Vec<String>,
    /// The first heading `id` attribute, if any.
    pub anchor: Option<String>,
    /// The page's canonical URL as passed to [`convert_article`].
    pub canonical_url: Url,
}

/// A no-panic, typed extraction failure naming the offending input.
///
/// Mirrors the "no avoidable panic on external input" posture of task `[004]`.
/// HTML *parsing* itself is not a failure mode — scraper's `parse_fragment` is
/// lenient and never fails — so the failure paths are a link the registry
/// refuses and an article that yields no content after chrome stripping.
#[allow(dead_code)] // first consumer is the [014] section-splitting stage
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtractionError {
    /// A content link could not be resolved to a valid in-boundary URL.
    UnresolvableLink {
        /// The raw `href` reference that failed.
        link: String,
        /// The resolver's reason.
        reason: String,
    },
    /// No article content remained after stripping chrome.
    EmptyArticle,
}

impl std::fmt::Display for ExtractionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractionError::UnresolvableLink { link, reason } => {
                write!(f, "content link {link:?} could not be resolved: {reason}")
            }
            ExtractionError::EmptyArticle => {
                write!(f, "no article content after stripping chrome")
            }
        }
    }
}
impl std::error::Error for ExtractionError {}

// ---------------------------------------------------------------------------
// The converter
// ---------------------------------------------------------------------------

/// Convert one in-boundary page's HTML into faithful Markdown.
///
/// `html` is content already fetched through the gated fetcher; `base_url` is
/// the page's canonical URL used for link resolution. Strips book chrome,
/// preserves the article, resolves relative links through the registry, and
/// returns a typed result — never a panic on malformed or hostile HTML.
///
/// **Selector order (first non-empty selection wins):** `#main-content`,
/// `.chapter-content`, `main`, `body`.
///
/// **Chrome stripped:** `nav`, `script`, `style`, `.sidebar`, `aside`,
/// `.toc`, `.page-previous-page`, `.page-next-page`.
///
/// **Preserved → Markdown mapping:**
/// - `h1…h6` → heading lines (`# ` … `###### `)
/// - `table`  → GFM table: `| col |` header, `|---|` separator, body rows
/// - `ul`/`ol` → `- ` / `1. ` list items
/// - `blockquote` → `> ` (stability/version callout)
/// - `pre > code` (class `language-rust`/`lang-rust`) → ```rust fence
/// - `a[href]` → resolve via `resolve_document`; keep as absolute link if valid
///
/// # Errors
///
/// Returns `ExtractionError` for empty content or an unresolvable link.
pub fn convert_article(html: &str, base_url: &Url) -> Result<ArticleExtraction, ExtractionError> {
    let document = Html::parse_fragment(html);
    let main = select_main(&document);

    // Render body and collect heading path simultaneously.
    let mut heading_path: Vec<String> = Vec::new();
    let lines = render_children(&main, base_url, &mut heading_path)?;

    let body = lines.join("\n\n").trim().to_string();
    if body.is_empty() {
        return Err(ExtractionError::EmptyArticle);
    }

    let anchor = first_heading_id(&main);

    Ok(ArticleExtraction {
        body,
        heading_path,
        anchor,
        canonical_url: base_url.clone(),
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Locate the main content container, trying the known Rust-book selectors in
/// order and falling back to the fragment root (last resort).
fn select_main(doc: &Html) -> ElementRef<'_> {
    let selectors = ["#main-content", ".chapter-content", "main", "body"];

    for sel_str in &selectors {
        if let Ok(sel) = Selector::parse(sel_str) {
            let found: Vec<ElementRef> = doc.select(&sel).collect();
            // Take the first match that actually has child elements, so an
            // empty `<nav></nav>`-style stub doesn't shadow a real container.
            for el in &found {
                if !el.children().collect::<Vec<_>>().is_empty() {
                    return *el;
                }
            }
            if !found.is_empty() {
                return found[0];
            }
        }
    }

    // Last resort: the fragment root itself.
    doc.root_element()
}

/// Render the children of `parent` into Markdown lines, skipping chrome.
///
/// Recurses into list, table, heading, and generic elements. Also walks
/// inline `a` elements to resolve their `href`s through the registry.
fn render_children(
    parent: &ElementRef,
    base_url: &Url,
    heading_path: &mut Vec<String>,
) -> Result<Vec<String>, ExtractionError> {
    let mut out: Vec<String> = Vec::new();

    for child in parent.children() {
        let node = child.value();
        match node {
            Node::Text(t) => {
                let s = collapse_ws(&t.text);
                if !s.is_empty() {
                    out.push(s);
                }
            }
            Node::Element(el) => {
                let name = el.name();
                if is_chrome_name(name, el) {
                    continue;
                }

                // `parent.children()` yields `NodeRef`; wrap the element node in
                // an `ElementRef` once so the helpers below can use the
                // `.select`/`.text`/`.attr` methods. This is always `Some`
                // inside a `Node::Element` node.
                let Some(elem) = ElementRef::wrap(child) else {
                    continue;
                };

                match name {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        let level = heading_level(name);
                        let text = element_text(&elem);
                        if !text.is_empty() {
                            heading_path.push(text.clone());
                            out.push(format!("{} {text}", "#".repeat(level)));
                        }
                    }
                    "table" => {
                        let md = render_table(&elem);
                        if !md.is_empty() {
                            out.push(md);
                        }
                    }
                    "ul" | "ol" => {
                        let ordered = name == "ol";
                        let items = render_list(&elem, ordered, base_url)?;
                        out.extend(items);
                    }
                    "blockquote" => {
                        let text = render_inline(&elem, base_url)?;
                        if !text.is_empty() {
                            out.push(format!("> {text}"));
                        }
                    }
                    "pre" => {
                        let code = render_code(&elem);
                        if !code.is_empty() {
                            out.push(code);
                        }
                    }
                    "p" => {
                        let text = render_inline(&elem, base_url)?;
                        if !text.is_empty() {
                            out.push(text);
                        }
                    }
                    "a" => {
                        // Render as Markdown link with resolved href.
                        let text = render_inline(&elem, base_url)?;
                        if text.is_empty() {
                            continue;
                        }
                        match elem.attr("href") {
                            Some(href) => match resolve_document(href, base_url) {
                                Ok(rd) => out.push(format!("[{text}]({})", rd.canonical_url)),
                                Err(e) => {
                                    return Err(ExtractionError::UnresolvableLink {
                                        link: href.to_string(),
                                        reason: e.to_string(),
                                    });
                                }
                            },
                            None => out.push(text),
                        }
                    }
                    // Any other element (section, div, etc.): recurse.
                    _ => {
                        let sub = render_children(&elem, base_url, heading_path)?;
                        out.extend(sub);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(out)
}

/// Whether a node is chrome (nav/script/style/sidebar/footer/toc/prev/next).
fn is_chrome_name(name: &str, el: &scraper::node::Element) -> bool {
    match name {
        "nav" | "script" | "style" | "aside" | "footer" => true,
        _ => {
            let class = el.attr("class").unwrap_or("").to_lowercase();
            class.contains("sidebar")
                || class.contains("toc")
                || class.contains("page-previous-page")
                || class.contains("page-next-page")
        }
    }
}

/// Map HTML heading tag name to a 1–6 level.
fn heading_level(name: &str) -> usize {
    match name {
        "h1" => 1,
        "h2" => 2,
        "h3" => 3,
        "h4" => 4,
        "h5" => 5,
        "h6" => 6,
        _ => 2,
    }
}

/// Get the concatenated whitespace-collapsed text of all descendant nodes,
/// including text inside nested elements like `<a>` and `<code>`.
fn element_text(el: &ElementRef) -> String {
    collapse_ws(&el.text().collect::<String>())
}

/// Render the *inline* content of an element into a single collapsed Markdown
/// line, resolving `a[href]` to `[text](resolved-url)` links and preserving the
/// visible text of inline elements like `code`/`strong`/`em`.
///
/// Fragment parts are collected and joined with single spaces, so nesting
/// like `<p>` + text + `<a>` + text renders as one clean line with resolved
/// links. Block-level descendants (a nested list) are flattened to text.
fn render_inline(el: &ElementRef, base_url: &Url) -> Result<String, ExtractionError> {
    let mut parts: Vec<String> = Vec::new();

    for child in el.children() {
        match child.value() {
            Node::Text(t) => {
                let s = collapse_ws(&t.text);
                if !s.is_empty() {
                    parts.push(s);
                }
            }
            Node::Element(el2) => {
                let name = el2.name();
                if is_chrome_name(name, el2) {
                    continue;
                }
                let Some(elem) = ElementRef::wrap(child) else {
                    continue;
                };

                if name == "a" {
                    let inner = render_inline(&elem, base_url)?;
                    if inner.is_empty() {
                        continue;
                    }
                    match elem.attr("href") {
                        Some(href) => match resolve_document(href, base_url) {
                            Ok(rd) => parts.push(format!("[{inner}]({})", rd.canonical_url)),
                            Err(e) => {
                                return Err(ExtractionError::UnresolvableLink {
                                    link: href.to_string(),
                                    reason: e.to_string(),
                                });
                            }
                        },
                        None => parts.push(inner),
                    }
                } else if name == "ul" || name == "ol" {
                    // A list nested inside inline content: flatten to its text.
                    let flat = element_text(&elem);
                    if !flat.is_empty() {
                        parts.push(flat);
                    }
                } else {
                    // Any other inline element (code/strong/em/span): recurse to
                    // preserve links inside without emitting block output.
                    let inner = render_inline(&elem, base_url)?;
                    if !inner.is_empty() {
                        parts.push(inner);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(parts.join(" "))
}

/// Collapse runs of whitespace to single spaces and trim.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render a `<ul>` or `<ol>` to Markdown list items.
fn render_list(
    list: &ElementRef,
    ordered: bool,
    base_url: &Url,
) -> Result<Vec<String>, ExtractionError> {
    let li_sel = Selector::parse("li").unwrap();
    let mut items: Vec<String> = Vec::new();
    let mut idx = 1usize;

    for li in list.select(&li_sel) {
        if is_chrome_name(li.value().name(), li.value()) {
            continue;
        }
        let text = render_inline(&li, base_url)?;
        if text.is_empty() {
            continue;
        }
        let prefix = if ordered {
            format!("{idx}. ")
        } else {
            "- ".to_string()
        };
        items.push(format!("{prefix}{text}"));
        idx += 1;
    }
    Ok(items)
}

/// Render a `<table>` to a GFM Markdown table.
fn render_table(table: &ElementRef) -> String {
    let tr_sel = Selector::parse("tr").unwrap();
    let all_trs: Vec<ElementRef> = table.select(&tr_sel).collect();
    if all_trs.is_empty() {
        return String::new();
    }
    let th_sel = Selector::parse("th").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let mut rows: Vec<String> = Vec::new();

    for (i, tr) in all_trs.iter().enumerate() {
        let cells: Vec<String> = if i == 0 {
            // Header row: prefer `<th>` cells.
            let th_cells: Vec<ElementRef> = tr.select(&th_sel).collect();
            if !th_cells.is_empty() {
                th_cells
                    .iter()
                    .map(|c| element_text(c))
                    .filter(|s| !s.is_empty())
                    .collect()
            } else {
                tr.select(&td_sel)
                    .map(|c| element_text(&c))
                    .filter(|s| !s.is_empty())
                    .collect()
            }
        } else {
            tr.select(&td_sel)
                .map(|c| element_text(&c))
                .filter(|s| !s.is_empty())
                .collect()
        };

        if cells.is_empty() {
            continue;
        }
        let row = format!("| {} |", cells.join(" | "));
        rows.push(row);
        if i == 0 {
            let sep = format!(
                "|{}|",
                cells.iter().map(|_| "---").collect::<Vec<_>>().join("|")
            );
            rows.push(sep);
        }
    }

    rows.join("\n")
}

/// Extract a fenced code block from a `<pre>` element.
///
/// Checks for `pre > code`; reads the `class` attribute for a language hint
/// (`language-rust` or `lang-rust`) and emits ` ```rust ` or ` ``` `.
fn render_code(pre: &ElementRef) -> String {
    let code_sel = Selector::parse("code").unwrap();
    let code_el = pre.select(&code_sel).next();

    match code_el {
        Some(code) => {
            let class = code.attr("class").unwrap_or("").to_lowercase();
            let lang = if class.contains("language-rust") || class.contains("lang-rust") {
                "rust"
            } else {
                ""
            };
            let text: String = code.text().collect();
            let text = text.trim().to_string();
            if text.is_empty() {
                String::new()
            } else {
                format!("```{lang}\n{text}\n```")
            }
        }
        None => {
            let text: String = pre.text().collect();
            let text = text.trim().to_string();
            if text.is_empty() {
                String::new()
            } else {
                format!("```\n{text}\n```")
            }
        }
    }
}

/// Get the first heading `id` attribute encountered anywhere in the subtree.
fn first_heading_id(root: &ElementRef) -> Option<String> {
    for child in root.children() {
        if let Node::Element(el) = child.value() {
            let name = el.name();
            if matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
                && let Some(id) = el.attr("id")
            {
                return Some(id.to_string());
            }
            if let Some(elem) = ElementRef::wrap(child)
                && let Some(found) = first_heading_id(&elem)
            {
                return Some(found);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Offline acceptance tests — plan.md §11
//
// Both criteria from issue #13:
//   (a) Headings, tables, lists, callouts, links, and fenced examples retained.
//   (b) Navigation/scripts/styles absent; relative content links resolved valid.
// Uses static HTML string fixtures — no network (Converter is pure).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Base URL for the Cargo book — under the `/cargo/` boundary.
    fn base() -> Url {
        Url::parse("https://doc.rust-lang.org/cargo/").unwrap()
    }

    /// Test (a): a document with nested headings, a table, a list, a stability
    /// callout, a relative content link, and a fenced Rust code example is
    /// converted to Markdown that retains all of them.
    #[test]
    fn test_retains_headings_tables_lists_callouts_links_examples() {
        let html = r#"<!DOCTYPE html>
<html>
<body>
  <div id="main-content">
    <h2>Building for different platforms</h2>
    <h3>Cargo and cross-compilation</h3>
    <p>
      The <a href="chapters/getting-started.html">Getting Started</a>
      chapter introduces the <a href="chapters/commands.html">command reference</a>
      where you find the full option table.
    </p>
    <blockquote>
      <strong>Stability</strong>
      Stable since Rust 1.70.
    </blockquote>
    <p>The following options are available:</p>
    <table>
      <thead>
        <tr><th>Option</th><th>Target</th><th>Notes</th></tr>
      </thead>
      <tbody>
        <tr><td>--target</td><td>x86_64-pc-windows-msvc</td><td>MSVC on Windows</td></tr>
        <tr><td>-Z</td><td>crt=msvc</td><td>Runtime linker flag</td></tr>
      </tbody>
    </table>
    <ul>
      <li><a href="chapters/commands.html">See the full command reference</a></li>
      <li>For cross-compilation use a custom linker.</li>
    </ul>
    <pre><code class="language-rust">fn main() {
    println!("cross-compiled");
}</code></pre>
  </div>
</body>
</html>"#;

        let r = convert_article(html, &base()).expect("conversion should succeed");
        let body = &r.body;

        // (a-1) Heading hierarchy retained.
        assert!(
            body.contains("## Building for different platforms"),
            "missing h2 heading line:\n{body}"
        );
        assert!(
            body.contains("### Cargo and cross-compilation"),
            "missing h3 heading line:\n{body}"
        );

        // (a-2) Table structure retained: both headers and data rows.
        assert!(
            body.contains("| Option | Target | Notes |"),
            "missing table header row:\n{body}"
        );
        assert!(
            body.contains("| --target | x86_64-pc-windows-msvc | MSVC on Windows |"),
            "missing table data row 1:\n{body}"
        );
        assert!(
            body.contains("| -Z | crt=msvc | Runtime linker flag |"),
            "missing table data row 2:\n{body}"
        );

        // (a-3) List items retained.
        assert!(
            body.contains("- [See the full command reference]"),
            "missing list item 1:\n{body}"
        );
        assert!(
            body.contains("custom linker"),
            "missing list item 2:\n{body}"
        );

        // (a-4) Stability callout retained as a blockquote.
        assert!(
            body.contains("> Stability") && body.contains("Stable since Rust 1.70"),
            "missing blockquote callout:\n{body}"
        );

        // (a-5) Relative links resolved to valid absolute in-boundary URLs.
        assert!(
            body.contains("https://doc.rust-lang.org/cargo/chapters/commands.html"),
            "relative link not resolved to absolute URL:\n{body}"
        );
        assert!(
            body.contains("https://doc.rust-lang.org/cargo/chapters/getting-started.html"),
            "relative link 2 not resolved:\n{body}"
        );

        // (a-6) Fenced Rust code example retained with language tag.
        assert!(
            body.contains("```rust\nfn main() {"),
            "missing ```rust fence:\n{body}"
        );

        // Provenance fields populated.
        assert!(
            !r.heading_path.is_empty(),
            "heading_path should not be empty"
        );
        assert_eq!(r.canonical_url, base());
    }

    /// Test (b): nav/script/style/sidebar/aside page-prev/next chrome absent;
    /// article text present; relative link resolved to absolute URL.
    #[test]
    fn test_strips_navigation_scripts_styles() {
        let html = r#"<!DOCTYPE html>
<html>
<body>
  <nav id="sidebar" class="sidebar">
    <a href="https://doc.rust-lang.org/cargo/">HOME</a>
    <a href="chapters/commands.html">COMMANDS</a>
  </nav>
  <script>var x = 42; console.log("nav chrome");</script>
  <style>.sidebar { display: none; } .toc { float: right; }</style>
  <nav id="toc" class="toc">
    <ul><li><a href="main.html">TOC Entry</a></li></ul>
  </nav>
  <aside id="page-prev" class="page-previous-page">
    <a>Previous page</a>
  </aside>
  <div id="main-content">
    <article>
      <h1>Cross-compilation</h1>
      <p>This chapter covers cross-compilation with
        <a href="chapters/commands.html">cargo build --target</a>.</p>
      <blockquote>nightly-only feature since 1.72</blockquote>
      <pre><code>let a = 1;</code></pre>
    </article>
  </div>
  <aside id="page-next" class="page-next-page">
    <a>Next page</a>
  </aside>
</body>
</html>"#;

        let r = convert_article(html, &base()).expect("conversion should succeed");
        let body = &r.body;

        // (b-1) Chrome absent.
        assert!(!body.contains("HOME"), "sidebar NAV survived:\n{body}");
        assert!(!body.contains("nav chrome"), "script survived:\n{body}");
        assert!(
            !body.contains("var x = 42"),
            "script body survived:\n{body}"
        );
        assert!(!body.contains("display: none"), "style survived:\n{body}");
        assert!(!body.contains("TOC Entry"), "TOC nav survived:\n{body}");
        assert!(
            !body.contains("Previous page"),
            "page-prev survived:\n{body}"
        );
        assert!(!body.contains("Next page"), "page-next survived:\n{body}");

        // (b-2) Article content present.
        assert!(
            body.contains("# Cross-compilation"),
            "article h1 missing:\n{body}"
        );
        assert!(
            body.contains("nightly-only feature since 1.72"),
            "callout missing:\n{body}"
        );
        assert!(body.contains("```"), "code fence missing:\n{body}");
        assert!(
            body.contains("cargo build --target"),
            "link text inside article lost:\n{body}"
        );
        // (b-3) Relative link inside article resolved to absolute.
        assert!(
            body.contains("https://doc.rust-lang.org/cargo/chapters/commands.html"),
            "relative link inside article not resolved:\n{body}"
        );
    }

    /// An all-chrome page returns `EmptyArticle`.
    #[test]
    fn test_empty_article_returns_error() {
        let html = r#"<!DOCTYPE html>
<html><body>
  <nav><a>Home</a></nav>
  <script>console.log('x');</script>
  <style>.x{color:red;}</style>
  <aside>sidebar</aside>
</body></html>"#;
        let r = convert_article(html, &base());
        assert!(
            r.is_err(),
            "all-chrome page should return error, got: {r:?}"
        );
        match r {
            Err(ExtractionError::EmptyArticle) => {}
            other => panic!("expected EmptyArticle, got: {other:?}"),
        }
    }

    /// No panic on malformed HTML: scraper's `parse_fragment` is lenient, so a
    /// broken fragment yields best-effort Markdown (`Ok`) or an empty article
    /// (`EmptyArticle`) — never a panic.
    #[test]
    fn test_no_panic_on_malformed_html() {
        let malformed = r#"<div><a></p><h2>Heading</h1>unclosed</div>"#;
        let result = convert_article(malformed, &base());
        match result {
            Ok(a) => {
                assert!(
                    a.body.contains("Heading"),
                    "heading not preserved in malformed output: {}",
                    a.body
                );
            }
            Err(ExtractionError::EmptyArticle) => {
                // scraper parsed an empty fragment after chrome-strip: acceptable.
            }
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }

    /// A content link the registry refuses (foreign host / off-boundary path)
    /// surfaces as `ExtractionError::UnresolvableLink` — it is never silently
    /// dropped, never re-rendered as a live link, and never panics.
    #[test]
    fn test_unresolvable_link_fails_the_article() {
        // `evil.example.com` is not a registered source → `UnregisteredHost`.
        let html = r#"<!DOCTYPE html>
<body>
  <main>
    <h1>Page</h1>
    <p>See <a href="https://evil.example.com/x">the off-source link</a>.</p>
  </main>
</body>
</html>"#;
        let r = convert_article(html, &base());
        match r {
            Ok(a) => {
                // The link must NOT have been resolved into a live absolute URL.
                assert!(
                    !a.body.contains("evil.example.com"),
                    "off-source link must not survive as a rendered link:\n{}",
                    a.body
                );
                panic!("expected UnresolvableLink, got Ok:\n{}", a.body);
            }
            Err(ExtractionError::UnresolvableLink { link, .. }) => {
                assert!(
                    link.contains("evil.example.com"),
                    "offending link not named: {link}"
                );
            }
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }

    /// `heading_path` contains heading texts in document order.
    #[test]
    fn test_heading_path_order() {
        let html = r#"<!DOCTYPE html><html><body>
          <div id="main">
            <h1>Document Root</h1>
            <h2>Section One</h2>
            <h3>Subsection 1.1</h3>
            <h2>Section Two</h2>
          </div>
        </body></html>"#;
        let r = convert_article(html, &base()).unwrap();
        assert!(
            !r.heading_path.is_empty(),
            "heading_path should not be empty"
        );
        assert!(
            r.heading_path[0].contains("Document Root"),
            "first heading should be 'Document Root', got: {v:?}",
            v = r.heading_path
        );
        assert!(
            r.heading_path.iter().any(|h| h.contains("Section One")),
            "heading_path missing 'Section One': {v:?}",
            v = r.heading_path
        );
    }
}
