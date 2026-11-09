//! Ingestion of the approved upstream rustfmt Markdown sources (task `[022]`,
//! issue #22).
//!
//! rustfmt's *only* fetch boundary (per [`crate::registry`]) is the two approved
//! static Markdown files at a resolved commit under
//! `raw.githubusercontent.com/rust-lang/rustfmt/` (`README.md`,
//! `Configurations.md`). The rendered `rust-lang.github.io/rustfmt/` site needs
//! JavaScript — `plan.md` §2 calls it "an empty HTML shell is not a successful
//! ingestion" — so it is *not* a fetch boundary and this adapter never parses it.
//!
//! ## How it reaches sections
//!
//! The two paths that feed the shared
//! [`sections_from_article_sections`](crate::ingest::section) splitter reach a
//! page's heading-delimited [`ArticleSection`]s *differently*: the HTML book
//! path walks the DOM ([`crate::ingest::html::split_article_sections`]); this
//! rustfmt path walks the Markdown text. Everything downstream — attaching each
//! section's original anchor (uniquified within the document so a repeated anchor
//! never collides) and splitting an oversized section into tagged continuations —
//! is the shared logic, kept in one place.
//!
//! - **Headings open sections.** A `#{1,6}` heading line (outside a fence) opens
//!   a new [`ArticleSection`]; the content between two headings stays with the
//!   section it appears in, so a config example never separates from its
//!   explanation. Content before the first heading is a heading-less preamble.
//! - **Fences are not headings.** A `#` inside a ```` ``` ````/`~~~` fence is
//!   code, not a section boundary, so a commented config example is preserved
//!   whole and is never mistaken for a new section.
//! - **Anchors are valid, commit-pinned citations.** Each heading's
//!   [`ArticleSection::anchor`] is a deterministic, URI-safe GitHub-style slug of
//!   the heading text. The shared splitter builds each
//!   [`Section`](crate::model::Section)'s `canonical_url` as
//!   `base_url#anchor` — so the section cites the *commit-pinned raw file* at its
//!   *heading anchor* (criterion 1), never the JS-only rendered site.
//! - **Content survives verbatim.** Stable/unstable option entries and edition
//!   entries are ordinary content blocks; the splitter keeps them with their
//!   surrounding explanation, so they "survive extraction" (criterion 2) without
//!   the adapter re-modeling any option.
//! - **An empty shell is a failure, not empty success.** An input that yields no
//!   content (the JS shell, a blank file, a chrome-stripped page) surfaces as
//!   [`ExtractionError::EmptyArticle`] — never an empty `Vec<Section>` that a
//!   caller could mistake for "ingested successfully."
//!
//! The adapter adds **no server-authored retrieval hint** and **no command
//! execution**: it retrieves rustfmt's documentation. The calling agent runs
//! `cargo fmt` / `rustfmt` through its own terminal tools.
//!
//! No network in these tests: the Markdown bodies are inline fixtures, so the
//! whole stage is exercisable offline.

// Transitional: these functions are the `[022]` ingestion API, exercised by the
// offline acceptance tests below; their first non-test consumer is the `[029]+`
// sync pipeline that ingests the corpus end-to-end.
#![allow(dead_code)]

use crate::ingest::fetch::Fetched;
use crate::ingest::html::{ArticleSection, ExtractionError};
use crate::ingest::section::{SplitConfig, SplitError, sections_from_article_sections};
use crate::model::{Section, SourceId};
use reqwest::Url;
use std::fmt;

/// The `SourceId` of the rustfmt collection.
pub const SOURCE: &str = "rustfmt";

/// The document title shared by every rustfmt Markdown section, used as the
/// section's `doc_title` provenance.
pub const DOC_TITLE: &str = "rustfmt reference";

/// A typed, non-panicking failure from ingesting a rustfmt Markdown source.
///
/// The two failure modes the acceptance criteria hinge on are both preserved
/// from the shared pipeline: a converter/schema failure from the shared
/// [`sections_from_article_sections`] splitter (`Schema`), and the empty-shell
/// refusal that turns a JS-only/blank/chrome-stripped page into a typed error
/// rather than an empty result (`Extraction`).
#[derive(Debug)]
pub enum IngestError {
    /// The Markdown yielded no content after heading grouping — an empty article.
    /// This is the "an empty HTML shell is not a successful ingestion" criterion:
    /// it surfaces as a typed error, never as an empty `Vec<Section>`.
    Extraction(ExtractionError),
    /// A section's identity or the shared split could not be derived — a schema
    /// failure preserved from the shared splitter.
    Schema(SplitError),
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Extraction(e) => write!(f, "rustfmt markdown extraction failed: {e}"),
            Self::Schema(e) => write!(f, "rustfmt markdown schema failed: {e}"),
        }
    }
}

impl std::error::Error for IngestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Extraction(e) => Some(e),
            Self::Schema(e) => Some(e),
        }
    }
}

/// The chapter path of a rustfmt Markdown section: the approved file name at the
/// tail of the commit-pinned URL.
///
/// The registry boundary is a `CommitFiles` rule, so the URL is
/// `raw.githubusercontent.com/rust-lang/rustfmt/<commit>/<file>`; the last path
/// segment is the approved file (`README.md` / `Configurations.md`). It is the
/// distinct, non-empty chapter component the shared splitter feeds to
/// [`SectionId`](crate::model::SectionId), so the two files never collide. A URL
/// with no tail segment is impossible for a registered rustfmt boundary, but is
/// defended against with the corpus title so the chapter path is never empty.
pub fn chapter_path(final_url: &Url) -> String {
    let path = final_url.path();
    match path.rsplit('/').next().filter(|s| !s.is_empty()) {
        Some(file) => file.to_string(),
        None => DOC_TITLE.to_string(),
    }
}

/// Ingest one approved rustfmt Markdown file (already fetched through the gated
/// fetcher) into its bounded [`Section`]s.
///
/// `fetched` carries the commit-pinned raw file's body and its
/// `final_url` (e.g. `…/rust-lang/rustfmt/<commit>/Configurations.md`). The body
/// is grouped into heading-delimited [`ArticleSection`]s by
/// [`split_markdown_sections`] and then handed to the shared
/// [`sections_from_article_sections`] splitter, so each
/// [`Section`](crate::model::Section) carries the commit-pinned `canonical_url`
/// (`base_url#anchor`), its original heading anchor, its heading breadcrumb, and
/// its examples preserved verbatim.
///
/// # Errors
///
/// Returns [`IngestError::Extraction`] when the body yields no content (an empty
/// shell) and [`IngestError::Schema`] when a section's identity cannot be
/// derived. Never panics on external input.
pub fn ingest_markdown(fetched: &Fetched) -> Result<Vec<Section>, IngestError> {
    let source = SourceId::new(SOURCE).map_err(|e| IngestError::Schema(SplitError::Schema(e)))?;
    let chapter = chapter_path(&fetched.final_url);
    let markdown = String::from_utf8_lossy(&fetched.body).into_owned();

    let article_sections = split_markdown_sections(&markdown).map_err(IngestError::Extraction)?;
    sections_from_article_sections(
        article_sections,
        &fetched.final_url,
        &source,
        &chapter,
        DOC_TITLE,
        SplitConfig::default().max_section_bytes,
    )
    .map_err(IngestError::Schema)
}

/// Group one rustfmt Markdown file into its heading-delimited
/// [`ArticleSection`]s.
///
/// A `#{1,6}` heading line *outside a fence* opens a new section; the content
/// between two headings stays with the section it appears in (content blocks are
/// maximal runs of non-blank lines, so a code fence is one block and an example
/// never separates from its explanation). Each heading's `anchor` is a
/// deterministic, URI-safe GitHub-style slug of the heading text; the heading
/// `heading_path` is the level-based breadcrumb, most-to-least specific.
///
/// # Errors
///
/// Returns [`ExtractionError::EmptyArticle`] when the input yields no content
/// (no heading and no non-blank content) — the empty-shell criterion. Never
/// panics on hostile input.
pub(crate) fn split_markdown_sections(
    markdown: &str,
) -> Result<Vec<ArticleSection>, ExtractionError> {
    let mut sections: Vec<ArticleSection> = Vec::new();
    // The section currently being opened: heading-less until the first heading.
    let mut open = Open {
        heading: None,
        anchor: None,
        heading_path: Vec::new(),
        segments: Vec::new(),
    };
    // A level-based breadcrumb of the headings nested around the current one,
    // most-to-least specific (mirrors the HTML path's heading path).
    let mut heading_stack: Vec<(usize, String)> = Vec::new();
    // The current content block: consecutive non-blank, non-heading lines.
    let mut block: Vec<String> = Vec::new();
    // Some(marker) when inside a ```/~~~ fence; the marker char is what closes it.
    let mut fence: Option<char> = None;

    for raw in markdown.lines() {
        let trimmed = raw.trim_start();
        let is_fence_line = trimmed.starts_with("```") || trimmed.starts_with("~~~");

        match fence {
            // Not in a fence: a fence line opens one; a heading opens a section;
            // a blank line closes the block; everything else is content.
            None => {
                if is_fence_line {
                    flush_block(&mut block, &mut open.segments);
                    fence = Some(trimmed.chars().next().unwrap());
                    block.push(raw.to_string());
                    continue;
                }
                if let Some((level, text)) = parse_heading(trimmed) {
                    flush_block(&mut block, &mut open.segments);
                    sections.push(open.finish());
                    // Maintain the level-based breadcrumb.
                    heading_stack.retain(|(l, _)| *l < level);
                    heading_stack.push((level, text.clone()));
                    open = Open {
                        heading: Some(heading_line(level, &text)),
                        anchor: Some(github_slug(&text)),
                        heading_path: heading_stack.iter().map(|(_, t)| t.clone()).collect(),
                        segments: Vec::new(),
                    };
                    continue;
                }
                if trimmed.is_empty() {
                    flush_block(&mut block, &mut open.segments);
                    continue;
                }
                block.push(raw.to_string());
            }
            // Inside a fence: a fence line closes it; the marker that opened it is
            // not needed to close (a matching-fence nuance the rustfmt Markdown
            // never exercises), so the binding is intentionally unused.
            Some(_) if is_fence_line => {
                block.push(raw.to_string());
                flush_block(&mut block, &mut open.segments);
                fence = None;
            }
            // Inside a fence, a non-fence line is content (a `#` is code, not a
            // heading).
            Some(_) => {
                block.push(raw.to_string());
            }
        }
    }
    // Close any trailing block and the final open section.
    flush_block(&mut block, &mut open.segments);
    sections.push(open.finish());

    // An empty shell (no heading and no content in any section) is a failure,
    // never an empty result a caller could read as "ingested successfully".
    let any_content = sections
        .iter()
        .any(|s| s.segments.iter().any(|b| !b.is_empty()) || s.heading.is_some());
    if !any_content {
        return Err(ExtractionError::EmptyArticle);
    }
    Ok(sections)
}

/// A section currently being opened, accumulated before it is finalized.
struct Open {
    heading: Option<String>,
    anchor: Option<String>,
    heading_path: Vec<String>,
    segments: Vec<String>,
}

impl Open {
    fn finish(self) -> ArticleSection {
        ArticleSection {
            heading: self.heading,
            anchor: self.anchor,
            heading_path: self.heading_path,
            segments: self.segments,
        }
    }
}

/// Flush an accumulated content block into the current section's segments as one
/// joined block (a paragraph, list, or whole code fence).
fn flush_block(block: &mut Vec<String>, segments: &mut Vec<String>) {
    if !block.is_empty() {
        let joined = block.join("\n");
        block.clear();
        segments.push(joined);
    }
}

/// Parse a `#{1,6} text` ATX heading line (already fence-stripped). Returns the
/// heading level and its text, or `None` for a non-heading line. A heading must
/// be 1-6 `#` followed by a space, matching CommonMark's ATX rule.
///
/// The leading `#` run is counted with `take_while`, which does **not** consume
/// the character after the run — so the following space check sees the real
/// space rather than the first content character.
fn parse_heading(line: &str) -> Option<(usize, String)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    let rest = line.get(hashes..)?;
    if (1..=6).contains(&hashes) && rest.starts_with(' ') {
        // A heading with no text (just `# `) still opens a section; `rest[1..]`
        // is a char boundary because the space it skips is a single ASCII byte.
        let text = rest[1..].trim().to_string();
        Some((hashes, text))
    } else {
        None
    }
}

/// Render a heading back to its Markdown line (e.g. `## Foo`).
fn heading_line(level: usize, text: &str) -> String {
    format!("{} {}", "#".repeat(level), text)
}

/// A deterministic, URI-safe GitHub-style slug of a heading's text — the anchor
/// a `#fragment` on the commit-pinned URL points at.
///
/// GitHub's slug rules, simplified to the part that keeps an anchor valid and
/// reproducible: lowercase; keep ASCII word characters and hyphens; turn
/// whitespace (and any other separator) into a single hyphen; trim leading and
/// trailing hyphens. Repeated anchors within a document are disambiguated later
/// by the shared splitter's `unique_anchor` step, so this stays collision-free
/// by construction and the caller need not dedupe.
fn github_slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_hyphen = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_hyphen = false;
        } else if (c.is_whitespace() || c == '-' || c == '_') && !last_hyphen && !out.is_empty() {
            out.push('-');
            last_hyphen = true;
        }
        // Any other character is dropped (matching GitHub's punctuation removal).
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// Offline acceptance tests — plan.md §11 / task [022] / issue #22
//
// Both criteria from issue #22:
//   (1) Usage/configuration sections cite the commit-pinned official files and
//       valid heading anchors — proven by ingesting a commit-pinned
//       `Configurations.md` and asserting each section's `canonical_url` is the
//       raw `…/rust-lang/rustfmt/<commit>/Configurations.md#<anchor>` form with a
//       non-empty, heading-derived anchor, and that the chapter path is the file.
//   (2) Stable/unstable options and editions survive extraction — proven by a
//       fixture carrying stable/unstable option and edition entries and asserting
//       they appear in the produced section bodies — and an empty dynamic HTML
//       shell cannot count as success, proven by a blank/chrome-stripped input
//       surfacing as `ExtractionError::EmptyArticle`, never an empty result.
//
// The Markdown bodies are inline fixtures, so nothing touches the network.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    /// A minimal `Fetched` carrying `body` as UTF-8, served from `url`.
    fn fetched(body: &str, url: &Url) -> Fetched {
        Fetched {
            requested: url.clone(),
            final_url: url.clone(),
            status: StatusCode::OK,
            body: body.as_bytes().to_vec(),
            content_type: Some("text/markdown; charset=utf-8".to_string()),
            etag: None,
            last_modified: None,
            content_length: None,
        }
    }

    /// The commit-pinned raw URL the registry boundary permits for the two
    /// approved files, at a fixed resolved commit.
    fn pinned_url(file: &str) -> Url {
        Url::parse(&format!(
            "https://raw.githubusercontent.com/rust-lang/rustfmt/0f4c1e2b3d5a6f8b7c9d1e2f4a6b8c0d3e5f7a9b/{file}"
        ))
        .unwrap()
    }

    /// A representative `Configurations.md`-style body: a preamble, a stable
    /// option, an unstable option (marked with a config comment), an editions
    /// subsection, and a fenced config example with a `#` comment inside the fence
    /// (which must NOT open a section).
    const CONFIG_MD: &str = "\
# Configuration

rustfmt reads its configuration from a `rustfmt.toml` file.

## Stable options

### edition

The edition of Rust to format to. Valid values are `2015`, `2018`, and `2021`.

```toml
# a config comment that starts with # but is inside a fence
edition = \"2021\"
```

## Unstable options

### unstable option

This option is `unstable` and only available with `--unstable-features`.
";

    /// The chapter path is the approved file name at the URL tail, distinct per
    /// file, so `README.md` and `Configurations.md` never collide.
    #[test]
    fn chapter_path_is_the_approved_file_name() {
        assert_eq!(
            chapter_path(&pinned_url("Configurations.md")),
            "Configurations.md"
        );
        assert_eq!(chapter_path(&pinned_url("README.md")), "README.md");
        // The two files produce distinct chapter paths.
        assert_ne!(
            chapter_path(&pinned_url("Configurations.md")),
            chapter_path(&pinned_url("README.md")),
        );
    }

    /// A `#` comment inside a code fence is not a section boundary: the fenced
    /// config example stays whole inside its section rather than opening a new
    /// (malformed) section.
    #[test]
    fn a_hash_inside_a_fence_is_not_a_heading() {
        let sections = split_markdown_sections(CONFIG_MD).unwrap();
        // The `# a config comment…` line is inside the fence, so it is not a
        // section: there is no section whose heading is that comment.
        assert!(
            sections.iter().all(|s| s.heading.as_deref()
                != Some("# a config comment that starts with # but is inside a fence")),
            "a `#` inside a fence must not open a section"
        );
        // The fenced block (with the `#` comment and the `edition` value) is
        // preserved whole inside the `edition` section's body.
        let edition = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("edition"))
            .expect("the `edition` section must exist");
        assert!(
            edition
                .segments
                .iter()
                .any(|b| b.contains("# a config comment") && b.contains("edition = \"2021\"")),
            "the fenced config example must stay whole: {:?}",
            edition.segments
        );
    }

    /// Criterion 1: usage/configuration sections cite the commit-pinned file at a
    /// valid heading anchor.
    #[test]
    fn usage_and_configuration_sections_cite_the_commit_pinned_file_at_a_valid_anchor() {
        let sections =
            ingest_markdown(&fetched(CONFIG_MD, &pinned_url("Configurations.md"))).unwrap();
        assert!(!sections.is_empty());

        for section in &sections {
            // Every section belongs to the rustfmt source and is stamped with the
            // corpus title.
            assert_eq!(section.source_id.as_str(), SOURCE);
            assert_eq!(section.doc_title, DOC_TITLE);
            // The chapter path is the approved file name.
            assert_eq!(section.chapter_path, "Configurations.md");
            // The canonical URL is the commit-pinned raw file — never the
            // JS-only rendered `rust-lang.github.io/rustfmt/` site.
            assert_eq!(
                section.canonical_url.as_str(),
                format!(
                    "https://raw.githubusercontent.com/rust-lang/rustfmt/0f4c1e2b3d5a6f8b7c9d1e2f4a6b8c0d3e5f7a9b/Configurations.md{}",
                    section
                        .anchor
                        .as_ref()
                        .map(|a| format!("#{a}"))
                        .unwrap_or_default()
                )
            );
            // A heading-delimited section carries a non-empty, URI-safe anchor.
            if let Some(anchor) = &section.anchor {
                assert!(
                    !anchor.is_empty(),
                    "a heading section must carry a non-empty anchor"
                );
                // The anchor is URI-safe (only alphanumerics, hyphens, or the
                // `~` disambiguator the shared splitter may append).
                assert!(
                    anchor
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'~'),
                    "anchor must be URI-safe: {anchor:?}",
                );
            }
        }

        // The `edition` section's anchor is the GitHub-style slug of its heading.
        let edition = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("edition"))
            .expect("the `edition` section must exist");
        assert!(
            edition.canonical_url.as_str().ends_with("#edition"),
            "edition section cites its heading anchor: {}",
            edition.canonical_url
        );
    }

    /// Criterion 2a: stable/unstable options and editions survive extraction —
    /// they appear in the produced section bodies, not lost.
    #[test]
    fn stable_unstable_options_and_editions_survive_extraction() {
        let sections =
            ingest_markdown(&fetched(CONFIG_MD, &pinned_url("Configurations.md"))).unwrap();
        let bodies: String = sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            bodies.contains("Stable options"),
            "the stable-options section survives: {bodies}"
        );
        assert!(
            bodies.contains("Unstable options"),
            "the unstable-options section survives: {bodies}"
        );
        assert!(
            bodies.contains("unstable") && bodies.contains("--unstable-features"),
            "the unstable option entry survives verbatim: {bodies}"
        );
        assert!(
            bodies.contains("edition") && bodies.contains("2015") && bodies.contains("2021"),
            "the edition entries survive verbatim: {bodies}"
        );
    }

    /// Criterion 2b: an empty dynamic HTML shell cannot count as success — a blank
    /// or chrome-stripped body surfaces as `ExtractionError::EmptyArticle`, never
    /// an empty `Vec<Section>` a caller could read as "ingested successfully".
    #[test]
    fn an_empty_shell_is_not_empty_success() {
        // A blank body (the JS shell / a chrome-stripped page).
        let blank = fetched("   \n\n  \n", &pinned_url("Configurations.md"));
        assert!(matches!(
            ingest_markdown(&blank),
            Err(IngestError::Extraction(ExtractionError::EmptyArticle))
        ));

        // A whitespace-only body with no heading and no content.
        let whitespace = fetched("\n\n\n", &pinned_url("README.md"));
        assert!(matches!(
            ingest_markdown(&whitespace),
            Err(IngestError::Extraction(ExtractionError::EmptyArticle))
        ));

        // A single non-blank content line IS content, so it ingests (not empty) —
        // this guards against over-aggressive empty detection.
        let one_line = fetched("just a paragraph of content\n", &pinned_url("README.md"));
        assert!(ingest_markdown(&one_line).is_ok());
    }

    /// Repeated heading text yields distinct section ids (the shared splitter
    /// disambiguates a repeated anchor), so two same-named sections never collide.
    #[test]
    fn repeated_headings_yield_distinct_section_ids() {
        let md = "\
# Options

## Notes

First notes.

## Notes

Second notes.
";
        let sections = split_markdown_sections(md).unwrap();
        // Pre-disambiguation: both raw slugs are the heading's slug, "notes".
        let raw_notes: Vec<_> = sections
            .iter()
            .filter(|s| s.anchor.as_deref() == Some("notes"))
            .collect();
        assert_eq!(raw_notes.len(), 2, "two raw `Notes` sections must survive");
        // After ingestion the shared splitter disambiguates the repeated anchor,
        // so the two sections carry distinct ids — filter by heading text (the
        // last breadcrumb entry), not the now-disambiguated anchor.
        let ingested = ingest_markdown(&fetched(md, &pinned_url("Configurations.md"))).unwrap();
        let notes: Vec<_> = ingested
            .iter()
            .filter(|s| s.heading_path.last().map(|h| h.as_str()) == Some("Notes"))
            .collect();
        assert_eq!(
            notes.len(),
            2,
            "two `Notes` sections must survive ingestion"
        );
        let id0 = notes[0].id.as_str().to_string();
        let id1 = notes[1].id.as_str().to_string();
        assert_ne!(
            id0, id1,
            "repeated anchors must yield distinct section ids: {id0} vs {id1}"
        );
    }

    /// A heading-less preamble (content before the first heading) is preserved as
    /// a section with no anchor but with content.
    #[test]
    fn a_heading_less_preamble_is_preserved() {
        let md = "\
rustfmt reads its configuration from a `rustfmt.toml` file.

## Options

A documented option.
";
        let sections = split_markdown_sections(md).unwrap();
        // The preamble is the first section, heading-less, with the intro content.
        let preamble = &sections[0];
        assert!(preamble.heading.is_none(), "the preamble has no heading");
        assert!(
            preamble.segments.iter().any(|b| b.contains("rustfmt.toml")),
            "the preamble content is preserved: {:?}",
            preamble.segments
        );
    }

    /// A subsection nests under its parent in its breadcrumb.
    #[test]
    fn a_subsection_nests_under_its_parent_in_the_breadcrumb() {
        let sections = split_markdown_sections(CONFIG_MD).unwrap();
        let edition = sections
            .iter()
            .find(|s| s.anchor.as_deref() == Some("edition"))
            .expect("the `edition` section must exist");
        assert!(
            edition.heading_path.iter().any(|h| h == "Stable options"),
            "the `edition` section's breadcrumb includes its parent: {:?}",
            edition.heading_path
        );
    }
}
