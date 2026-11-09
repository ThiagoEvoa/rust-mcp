//! Semantic section splitting with preserved examples and continuation metadata
//! (task `[014]`, issue #14).
//!
//! `plan.md` §5 step 5: "Split on semantic section boundaries. Keep examples
//! with their surrounding explanation. Split oversized sections at paragraph or
//! code-block boundaries with explicit continuation metadata."
//!
//! This is the first consumer of the `[013]` HTML→Markdown converter: it takes
//! the page's heading-delimited [`ArticleSection`]s (from
//! [`split_article_sections`]) and turns each into one or more
//! [`Section`](crate::model::Section)s:
//!
//! - **Boundaries** — each semantic heading opens a section; its content
//!   (paragraphs, lists, tables, code fences, callouts) stays attached, so a
//!   code example never separates from its explanation.
//! - **Original anchors** — a section's [`SectionId`](crate::model::SectionId)
//!   and `canonical_url` carry the opening heading's `id` (the original anchor),
//!   preserving `#anchor` provenance.
//! - **Continuations** — a section whose body would exceed the size bound is
//!   split at paragraph / code-block boundaries into continuations, each tagged
//!   with a deterministic [`Continuation`] `{ part, parts }` so its id differs
//!   and the split is explicit — never a silent clip.
//!
//! No panics on malformed or hostile input: a refused link or an empty article
//! surface as a typed [`SplitError`].

#![allow(dead_code)] // transitional: first consumer is the [015]/[016] ingestion orchestration

use crate::ingest::html::{ArticleSection, ExtractionError, split_article_sections};
use crate::model::{Continuation, SchemaError, Section, SectionBuilder, SectionId, SourceId};
use reqwest::Url;
use std::fmt;

/// Default maximum size of a single section body, in bytes.
///
/// `plan.md` §6 caps a fully serialized tool response at 32 KiB. A section
/// resource carries the body plus heading context, citations, and snapshot
/// metadata, so the body bound sits comfortably below that cap — leaving
/// headroom for the JSON envelope — and is kept here as a named, configurable
/// constant rather than a magic number.
pub const DEFAULT_MAX_SECTION_BYTES: usize = 16 * 1024;

/// Tunable options for [`split_article`].
///
/// A section whose body exceeds [`SplitConfig::max_section_bytes`] is split into
/// continuations; a single content block that alone exceeds the bound is kept
/// whole in its own part (it is never clipped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitConfig {
    /// The maximum section-body size, in bytes, before a section is split into
    /// continuations.
    pub max_section_bytes: usize,
}

impl Default for SplitConfig {
    fn default() -> Self {
        Self {
            max_section_bytes: DEFAULT_MAX_SECTION_BYTES,
        }
    }
}

/// A typed, non-panicking failure from splitting an article into sections.
#[derive(Debug)]
pub enum SplitError {
    /// The article could not be converted to Markdown — a link the registry
    /// refused, or an article with no content after chrome stripping — preserved
    /// from the `[013]` converter.
    Extraction(ExtractionError),
    /// A section's identity could not be derived (e.g. an empty chapter path or
    /// an invalid continuation), preserved from the schema layer.
    Schema(SchemaError),
}

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SplitError::Extraction(e) => write!(f, "article extraction: {e}"),
            SplitError::Schema(e) => write!(f, "section schema: {e}"),
        }
    }
}

impl std::error::Error for SplitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SplitError::Extraction(e) => Some(e),
            SplitError::Schema(e) => Some(e),
        }
    }
}

/// Split one in-boundary page's HTML into a bounded, ordered list of sections.
///
/// `html` is content already fetched through the gated fetcher; `base_url` is
/// the page's canonical URL used for link resolution; `source`, `chapter_path`,
/// and `doc_title` are the section's provenance context; `config` tunes the
/// continuation bound.
///
/// Each returned [`Section`] is in document order. A section opened by a heading
/// records that heading's `id` as its `anchor` and carries it in its
/// `canonical_url`. A section whose body exceeds `config.max_section_bytes` is
/// split into continuations, each tagged with `Continuation { part, parts }` so
/// its [`SectionId`](crate::model::SectionId) differs deterministically and the
/// split is explicit. The page's `reqwest::Url` is bridged to the validated
/// [`model::Url`](crate::model::Url) stored on each `Section`.
///
/// # Errors
///
/// Returns [`SplitError::Extraction`] when the article cannot be converted
/// (refused link / empty article) and [`SplitError::Schema`] when a section's
/// identity cannot be derived. Never panics on external input.
///
/// This is the HTML *entry*: it converts the page to heading-delimited
/// [`ArticleSection`]s ([`split_article_sections`]) and then hands them to the
/// shared [`sections_from_article_sections`] continuation-splitting entry that
/// the rustfmt Markdown path calls in as well.
pub fn split_article(
    html: &str,
    base_url: &Url,
    source: &SourceId,
    chapter_path: &str,
    doc_title: &str,
    config: &SplitConfig,
) -> Result<Vec<Section>, SplitError> {
    let article_sections =
        split_article_sections(html, base_url).map_err(SplitError::Extraction)?;
    sections_from_article_sections(
        article_sections,
        base_url,
        source,
        chapter_path,
        doc_title,
        config.max_section_bytes,
    )
}

/// The shared continuation-splitting entry both ingestion paths call in.
///
/// The HTML article path ([`split_article`]) and the rustfmt Markdown path
/// ([`crate::ingest::rustfmt`]) differ only in *how they reach* a page's
/// heading-delimited [`ArticleSection`]s (a DOM walk vs. a Markdown line walk).
/// Everything downstream of that — attaching each section's original anchor
/// (uniquified within the document so a repeated anchor never collides) and
/// splitting an oversized section into explicitly tagged continuations — is
/// identical, and factoring it here keeps that single, subtle, bug-prone logic in
/// one place both paths share rather than duplicated across two adapters.
///
/// `article_sections` are already heading-grouped; `base_url` is the page's
/// canonical URL each section's `canonical_url` (and `#anchor`) is built from;
/// `source`, `chapter_path`, and `doc_title` are the section's provenance
/// context; `max` is the per-section body bound. The returned sections are in
/// document order, each carrying a distinct
/// [`SectionId`](crate::model::SectionId) and a non-empty content hash.
///
/// # Errors
///
/// Returns [`SplitError::Schema`] when a section's identity cannot be derived
/// (e.g. an empty chapter path or an invalid continuation). Never panics.
pub(crate) fn sections_from_article_sections(
    article_sections: Vec<ArticleSection>,
    base_url: &Url,
    source: &SourceId,
    chapter_path: &str,
    doc_title: &str,
    max: usize,
) -> Result<Vec<Section>, SplitError> {
    let mut out: Vec<Section> = Vec::new();
    // A document can carry duplicate heading text — and, for hostile input, even
    // duplicate `id` anchors. A section id is derived from its original anchor, so
    // a repeated anchor would yield a *colliding* id. Track the base ids already
    // emitted for this document and disambiguate a repeated anchor deterministically,
    // so every section — and each of its continuations — keeps a distinct id.
    let mut used_base: std::collections::HashSet<String> = std::collections::HashSet::new();
    for mut section in article_sections {
        section.anchor = unique_anchor(&section, source, chapter_path, &mut used_base)?;
        out.extend(build_sections(
            &section,
            source,
            chapter_path,
            doc_title,
            base_url,
            max,
        )?);
    }
    Ok(out)
}

/// Pick the anchor that yields a section id not already emitted in this document.
///
/// The original anchor is tried first, so a unique anchor — the normal case, since
/// every real Rust-docs heading has a unique `id` — is preserved exactly. Only when
/// a repeated anchor would collide with an already-emitted section id is a
/// deterministic `~<n>` discriminator appended, keeping the id distinct and the
/// split reproducible. A heading-less preamble (`None`) is returned as-is: a
/// document has at most one.
fn unique_anchor(
    section: &ArticleSection,
    source: &SourceId,
    chapter_path: &str,
    used: &mut std::collections::HashSet<String>,
) -> Result<Option<String>, SplitError> {
    let anchor = match &section.anchor {
        None => return Ok(None),
        Some(anchor) => anchor,
    };
    let mut candidate = anchor.clone();
    let mut attempt: u32 = 0;
    loop {
        let base = SectionId::derive(source, chapter_path, Some(&candidate), None)
            .map_err(SplitError::Schema)?;
        if used.insert(base.as_str().to_string()) {
            return Ok(Some(candidate));
        }
        attempt += 1;
        candidate = format!("{anchor}~{attempt}");
    }
}

/// Build the one-or-more [`Section`]s that a single [`ArticleSection`] yields,
/// splitting into continuations when its body exceeds `max` bytes.
fn build_sections(
    section: &ArticleSection,
    source: &SourceId,
    chapter_path: &str,
    doc_title: &str,
    base_url: &Url,
    max: usize,
) -> Result<Vec<Section>, SplitError> {
    let parts = split_into_parts(&section.heading, &section.segments, max);
    let total = parts.len();

    let mut out = Vec::with_capacity(total);
    for (i, body) in parts.into_iter().enumerate() {
        // A single part carries no continuation marker; two or more parts are a
        // 1-based split, each tagged with its position and the total.
        let continuation = if total == 1 {
            None
        } else {
            Some(Continuation {
                part: i + 1,
                parts: total,
            })
        };

        // The converter works in `reqwest::Url`; a stored `Section` carries the
        // validated `model::Url`, so bridge at the builder with the anchor
        // restored as a `#fragment`.
        let canonical =
            crate::model::Url::new(section_url(base_url, section.anchor.as_deref()).as_str())
                .map_err(SplitError::Schema)?;
        let mut builder =
            SectionBuilder::new(source.clone(), doc_title, chapter_path, canonical, body);
        if let Some(anchor) = &section.anchor {
            builder = builder.anchor(anchor.clone());
        }
        builder = builder.heading_path(section.heading_path.iter().cloned());
        if let Some(continuation) = continuation {
            builder = builder.continuation(continuation);
        }
        out.push(builder.build().map_err(SplitError::Schema)?);
    }
    Ok(out)
}

/// The canonical URL of a section: the page URL with its original anchor
/// restored as a `#fragment` when the opening heading carried an `id`.
fn section_url(base: &Url, anchor: Option<&str>) -> Url {
    match anchor {
        Some(anchor) if !anchor.is_empty() => {
            let mut url = base.clone();
            url.set_fragment(Some(anchor));
            url
        }
        _ => base.clone(),
    }
}

/// Split one section's content into one or more continuation-part bodies.
///
/// Parts are joined with `"\n\n"`. The section's heading line (when present) is
/// re-attached to *every* part so each continuation is self-describing (its
/// context is preserved). A part is closed when appending the next block would
/// push it over `max` bytes; a block that alone exceeds `max` is kept whole in
/// its own part, so a code example is never clipped mid-fence.
fn split_into_parts(heading: &Option<String>, segments: &[String], max: usize) -> Vec<String> {
    // Fast path: the whole section already fits.
    if part_body(heading, segments).len() <= max {
        return vec![part_body(heading, segments)];
    }

    let mut parts: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for segment in segments {
        let mut candidate = current.clone();
        candidate.push(segment.clone());
        // Append the segment unless it would overflow the part. When `current`
        // is empty the segment — even one too large on its own — starts its own
        // part rather than being dropped or clipped.
        if current.is_empty() || part_body(heading, &candidate).len() <= max {
            current = candidate;
        } else {
            parts.push(part_body(heading, &current));
            current = vec![segment.clone()];
        }
    }
    if !current.is_empty() {
        parts.push(part_body(heading, &current));
    }
    // A heading-only section (no content blocks) still yields one part.
    if parts.is_empty() {
        parts.push(part_body(heading, segments));
    }
    parts
}

/// Render the body of a part: the heading line (when present) followed by the
/// joined content blocks, with no trailing blank line.
fn part_body(heading: &Option<String>, segments: &[String]) -> String {
    let joined = segments.join("\n\n");
    match heading {
        Some(heading) if !heading.is_empty() => {
            if joined.is_empty() {
                heading.clone()
            } else {
                format!("{heading}\n\n{joined}")
            }
        }
        _ => joined,
    }
}

// ---------------------------------------------------------------------------
// Offline tests — plan.md §11 / task [014]
//
// All fixtures are static HTML: the splitter is pure (link resolution is
// registry-gated, not a network fetch), so no server or fixture server is
// needed.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::{DEFAULT_MAX_SECTION_BYTES, SplitConfig, SplitError, split_article};
    use crate::ingest::html::ExtractionError;
    use crate::model::{SectionId, SourceId};
    use reqwest::Url;

    fn cargo_base(path: &str) -> Url {
        Url::parse(&format!("https://doc.rust-lang.org/cargo/{path}")).unwrap()
    }

    /// Two headings open two sections; each keeps its original `id` anchor, its
    /// code example stays with its explanation, and the ids are distinct.
    #[test]
    fn two_headings_produce_two_sections_with_preserved_examples() {
        let html = r#"<!DOCTYPE html><html><body>
          <div id="main-content">
            <h1 id="cross">Cross-compilation</h1>
            <p>Intro to cross.</p>
            <pre><code class="language-rust">fn main() {}</code></pre>
            <h2 id="targets">Target selection</h2>
            <p>How targets work.</p>
          </div>
        </body></html>"#;
        let source = SourceId::new("cargo").unwrap();

        let sections = split_article(
            html,
            &cargo_base("guide.html"),
            &source,
            "guide",
            "Guide",
            &SplitConfig::default(),
        )
        .expect("splits");
        assert_eq!(
            sections.len(),
            2,
            "two headings -> two sections: {sections:?}"
        );

        let s0 = &sections[0];
        assert_eq!(s0.anchor.as_deref(), Some("cross"));
        assert!(
            s0.continuation.is_none(),
            "a small section is not a continuation"
        );
        assert!(s0.body.contains("Intro to cross."), "explanation lost");
        assert!(
            s0.body.contains("fn main()"),
            "code example should stay with its explanation"
        );
        assert!(
            s0.canonical_url.as_str().contains("#cross"),
            "canonical url must carry the original anchor: {:?}",
            s0.canonical_url.as_str()
        );

        let s1 = &sections[1];
        assert_eq!(s1.anchor.as_deref(), Some("targets"));
        assert!(s1.body.contains("How targets work."));
        assert!(
            !s1.body.contains("fn main()"),
            "the code example must not leak into the next section: {s1:?}"
        );
        assert!(s1.canonical_url.as_str().contains("#targets"));
        assert_ne!(s0.id, s1.id, "distinct sections must have distinct ids");

        // Each id round-trips through the grammar with its anchor and no continuation.
        let p0 = SectionId::parse(s0.id.as_str()).expect("id0 parses");
        assert_eq!(p0.anchor.as_deref(), Some("cross"));
        assert!(p0.continuation.is_none());
    }

    /// An oversized section splits at paragraph boundaries into explicitly tagged
    /// continuations, losing no content and keeping every id distinct.
    #[test]
    fn oversized_section_splits_into_tagged_continuations() {
        let html = r#"<!DOCTYPE html><html><body>
          <div id="main-content">
            <h1 id="big">Big</h1>
            <p>alpha paragraph</p>
            <p>beta paragraph</p>
            <p>gamma paragraph</p>
            <p>delta paragraph</p>
          </div>
        </body></html>"#;
        let source = SourceId::new("cargo").unwrap();
        let config = SplitConfig {
            max_section_bytes: 30,
        };

        let sections = split_article(
            html,
            &cargo_base("big.html"),
            &source,
            "big",
            "Big",
            &config,
        )
        .expect("splits");
        assert!(
            sections.len() >= 2,
            "a small max must force continuations: {sections:?}"
        );

        // Every part is a continuation with a 1-based index and the shared total.
        for (i, s) in sections.iter().enumerate() {
            let c = s
                .continuation
                .as_ref()
                .expect("every part of a split carries a Continuation");
            assert_eq!(c.part, i + 1, "part index must be 1-based");
            assert_eq!(c.parts, sections.len(), "all parts share the total");
        }

        // No content is lost across the split.
        let all = sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for paragraph in [
            "alpha paragraph",
            "beta paragraph",
            "gamma paragraph",
            "delta paragraph",
        ] {
            assert!(
                all.contains(paragraph),
                "paragraph lost across the split: {paragraph}"
            );
        }

        // Distinct ids, each parsing back to its continuation.
        let ids: Vec<&str> = sections.iter().map(|s| s.id.as_str()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len(), "continuation ids must be distinct");
        for s in &sections {
            let parts = SectionId::parse(s.id.as_str()).expect("split id parses");
            assert!(
                parts.continuation.is_some(),
                "a split id must carry a continuation"
            );
        }
    }

    /// A section whose content is a single oversized code block is kept whole in
    /// its own part — the split lands on the code-block boundary, never inside
    /// the fence.
    #[test]
    fn oversized_section_splits_at_code_block_boundary() {
        let html = r#"<!DOCTYPE html><html><body>
          <div id="main-content">
            <h2 id="config">Config</h2>
            <p>before the example</p>
            <pre><code class="language-rust">
let alpha = 1;
let beta = 2;
let gamma = 3;
let delta = 4;
let epsilon = 5;
</code></pre>
            <p>after the example</p>
          </div>
        </body></html>"#;
        let source = SourceId::new("cargo").unwrap();
        let config = SplitConfig {
            max_section_bytes: 100,
        };

        let sections = split_article(
            html,
            &cargo_base("config.html"),
            &source,
            "config",
            "Config",
            &config,
        )
        .expect("splits");

        // The fence lives in exactly one part.
        let fence_parts = sections
            .iter()
            .filter(|s| s.body.contains("let alpha = 1;"))
            .count();
        assert_eq!(
            fence_parts, 1,
            "the fence must live in exactly one part: {sections:?}"
        );

        let part = sections
            .iter()
            .find(|s| s.body.contains("let alpha = 1;"))
            .expect("a part holds the fence");
        for line in [
            "let alpha = 1;",
            "let beta = 2;",
            "let gamma = 3;",
            "let delta = 4;",
            "let epsilon = 5;",
        ] {
            assert!(
                part.body.contains(line),
                "fence line missing: {line} in {part:?}"
            );
        }
        assert!(
            part.body.contains("```rust"),
            "opening fence marker missing"
        );
        assert!(
            part.body.trim_end().ends_with("```"),
            "closing fence must survive intact (no mid-fence clip): {part:?}"
        );

        // Context is preserved: every continuation re-attaches its heading.
        for s in &sections {
            assert!(
                s.body.starts_with("## Config"),
                "each continuation must re-attach its heading: {s:?}"
            );
        }

        // "before" and "after" flank the fence, so they land in different parts.
        let before = sections
            .iter()
            .find(|s| s.body.contains("before the example"))
            .unwrap();
        let after = sections
            .iter()
            .find(|s| s.body.contains("after the example"))
            .unwrap();
        assert_ne!(
            before.id, after.id,
            "before/after must be in different parts"
        );
    }

    /// A heading-less / all-chrome article surfaces as a typed extraction error
    /// rather than a panic.
    #[test]
    fn empty_article_is_an_extraction_error() {
        let html = r#"<!DOCTYPE html><html><body>
          <nav><a>Home</a></nav>
          <script>x</script>
        </body></html>"#;
        let source = SourceId::new("cargo").unwrap();
        let r = split_article(
            html,
            &cargo_base("guide.html"),
            &source,
            "guide",
            "Guide",
            &SplitConfig::default(),
        );
        assert!(
            matches!(r, Err(SplitError::Extraction(_))),
            "an empty article must be a typed Extraction error: {r:?}"
        );
    }

    /// A content link the registry refuses surfaces as a typed extraction error
    /// naming the offending link.
    #[test]
    fn unresolvable_link_surfaces_as_extraction_error() {
        let html = r#"<!DOCTYPE html><html><body>
          <div id="main-content">
            <h1 id="p">Page</h1>
            <p>See <a href="https://evil.example.com/x">the off-source link</a>.</p>
          </div>
        </body></html>"#;
        let source = SourceId::new("cargo").unwrap();
        let r = split_article(
            html,
            &cargo_base("guide.html"),
            &source,
            "guide",
            "Guide",
            &SplitConfig::default(),
        );
        match r {
            Err(SplitError::Extraction(ExtractionError::UnresolvableLink { link, .. })) => {
                assert!(
                    link.contains("evil.example.com"),
                    "offending link must be named: {link}"
                );
            }
            other => panic!("expected UnresolvableLink, got {other:?}"),
        }
    }

    /// An empty chapter path cannot derive a section id and is a typed schema
    /// error, not a panic.
    #[test]
    fn empty_chapter_path_is_a_schema_error() {
        let html = r#"<!DOCTYPE html><html><body>
          <div id="main-content"><h1 id="p">Page</h1><p>body</p></div>
        </body></html>"#;
        let source = SourceId::new("cargo").unwrap();
        let r = split_article(
            html,
            &cargo_base("guide.html"),
            &source,
            "",
            "Guide",
            &SplitConfig::default(),
        );
        assert!(
            matches!(r, Err(SplitError::Schema(_))),
            "an empty chapter path must fail with a typed Schema error: {r:?}"
        );
    }

    /// A section's `heading_path` is the breadcrumb of its ancestors; a deeper
    /// sibling does not leak into a later sibling.
    #[test]
    fn heading_path_is_a_breadcrumb() {
        let html = r#"<!DOCTYPE html><html><body>
          <div id="main-content">
            <h1 id="a">Alpha</h1>
            <h2 id="b">Beta</h2>
            <h3 id="c">Gamma</h3>
            <p>deep content</p>
            <h2 id="d">Delta</h2>
          </div>
        </body></html>"#;
        let source = SourceId::new("rustc").unwrap();

        let sections = split_article(
            html,
            &Url::parse("https://doc.rust-lang.org/rustc/intro.html").unwrap(),
            &source,
            "intro",
            "Intro",
            &SplitConfig::default(),
        )
        .expect("splits");

        let gamma = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("c"))
            .expect("gamma section");
        assert_eq!(
            gamma.heading_path,
            vec!["Alpha", "Beta", "Gamma"],
            "an h3 breadcrumb must include its h1/h2 ancestors"
        );
        assert!(
            gamma.body.contains("deep content"),
            "deep content belongs to Gamma"
        );

        let delta = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("d"))
            .expect("delta section");
        assert_eq!(
            delta.heading_path,
            vec!["Alpha", "Delta"],
            "a sibling h2 must drop the deeper sibling from its breadcrumb"
        );
        assert!(
            !delta.body.contains("deep content"),
            "deep content must stay with Gamma, not leak into Delta"
        );
    }

    /// The default section bound leaves headroom under the 32 KiB response cap.
    #[test]
    fn default_config_sits_under_the_response_cap() {
        const {
            assert!(
                DEFAULT_MAX_SECTION_BYTES < 32 * 1024,
                "the section body bound must leave headroom for the JSON envelope"
            );
        }
        assert_eq!(
            SplitConfig::default().max_section_bytes,
            DEFAULT_MAX_SECTION_BYTES
        );
    }

    /// Duplicate heading *text* keeps distinct, deterministic ids (each heading's
    /// original `id` anchor is preserved), and the split is reproducible.
    #[test]
    fn duplicate_heading_text_keeps_distinct_deterministic_ids() {
        let html = r#"<!DOCTYPE html><html><body>
          <div id="main-content">
            <h2 id="a">Examples</h2>
            <p>first</p>
            <h2 id="b">Examples</h2>
            <p>second</p>
          </div>
        </body></html>"#;
        let source = SourceId::new("cargo").unwrap();

        let first = split_article(
            html,
            &cargo_base("guide.html"),
            &source,
            "guide",
            "Guide",
            &SplitConfig::default(),
        )
        .expect("splits");
        let second = split_article(
            html,
            &cargo_base("guide.html"),
            &source,
            "guide",
            "Guide",
            &SplitConfig::default(),
        )
        .expect("splits again");

        assert_eq!(first.len(), 2, "two headings -> two sections: {first:?}");
        assert_ne!(
            first[0].id, first[1].id,
            "duplicate headings must give distinct ids"
        );
        assert_eq!(
            first[0].anchor.as_deref(),
            Some("a"),
            "original anchor preserved"
        );
        assert_eq!(
            first[1].anchor.as_deref(),
            Some("b"),
            "original anchor preserved"
        );

        // Deterministic: the same input yields the same ids.
        assert_eq!(
            first.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            second.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            "the split must be deterministic"
        );
    }

    /// Hostile input with literally duplicate `id` anchors still yields distinct,
    /// deterministic ids: the first occurrence keeps its anchor, later ones get a
    /// deterministic discriminator, and each id round-trips through the grammar.
    #[test]
    fn duplicate_anchors_are_disambiguated_deterministically() {
        let html = r#"<!DOCTYPE html><html><body>
          <div id="main-content">
            <h2 id="x">Alpha</h2>
            <p>one</p>
            <h2 id="x">Beta</h2>
            <p>two</p>
          </div>
        </body></html>"#;
        let source = SourceId::new("cargo").unwrap();

        let sections = split_article(
            html,
            &cargo_base("guide.html"),
            &source,
            "guide",
            "Guide",
            &SplitConfig::default(),
        )
        .expect("splits");

        assert_eq!(
            sections.len(),
            2,
            "two headings -> two sections: {sections:?}"
        );
        assert_ne!(
            sections[0].id, sections[1].id,
            "duplicate anchors must still produce distinct ids"
        );
        // The first occurrence keeps the original anchor; the second is disambiguated.
        assert_eq!(sections[0].anchor.as_deref(), Some("x"));
        assert!(
            sections[1].anchor.as_ref().is_some_and(|a| a != "x"),
            "the repeated anchor must be disambiguated, got {:?}",
            sections[1].anchor
        );
        // Both ids parse back and carry the same (source, chapter) components.
        for s in &sections {
            let parts = SectionId::parse(s.id.as_str()).expect("disambiguated id parses");
            assert_eq!(parts.source, "cargo");
            assert_eq!(parts.chapter_path, "guide");
            assert!(parts.continuation.is_none());
        }
    }
}
