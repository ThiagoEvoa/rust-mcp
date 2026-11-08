//! Documentation ingestion: bounded fetching, extraction, validation, and atomic
//! publication.
//!
//! Tasks [010]-[028]. Sub-adapters:
//!
//! - [`resolve`] — document-identity resolution: relative links are resolved
//!   against their page, normalized, and re-gated through the registry so off
//!   sources, lookalike prefixes, and bad schemes are refused (task `[011]`).
//! - [`books`]   — book navigation discovery and article-to-Markdown extraction.
//! - [`rustfmt`] — adapter for the approved upstream rustfmt Markdown files.
//! - [`clippy`]  — per-entry extraction from the Clippy lint catalog.
//! - [`fetch`]   — the bounded HTTP fetcher every ingestion stage downloads through.
//! - [`html`]    — shared HTML→Markdown article converter reused by the book,
//!   rustfmt, and clippy ingestion stages (task `[013]`).
//! - [`section`] — semantic section splitting: turns a page's heading-delimited
//!   articles into bounded `model::Section`s, attaching original anchors and
//!   splitting oversized sections into tagged continuations (task `[014]`).
//! - [`api_guidelines`] — the `api-guidelines` adapter: books the mdBook site via
//!   the shared book discovery and ingests its chapters, retaining each
//!   guideline's `C-…` code, examples, wording, and anchor, plus the checklist
//!   entries (task `[015]`).
//! - [`style_guide`] — the `style-guide` adapter: books the Style Guide via the
//!   shared book discovery and splits its chapters into sections, retaining each
//!   section's examples, qualifications, and anchor — the source carries no
//!   checklist or `C-…` codes, so no code enrichment (task `[017]`).
//! - [`cargo`] — the `cargo` adapter: books The Cargo Book via the shared book
//!   discovery (guide/reference/appendix chapters → sections, so the reference
//!   topics land as sections) and extracts each command-manual page's synopsis,
//!   option/argument groups, examples, and the `--` argument-forwarding boundary
//!   (task `[018]`).
//! - [`rustc`] — the `rustc` adapter: books The rustc Book via the shared book
//!   discovery (the `toc.html` navigation → sections, so the compiler options,
//!   codegen options, the test harness, lints, and stability/version
//!   qualifications land as sections, with no checklist or `C-…` codes) —
//!   task `[019]`.

mod api_guidelines;
mod books;
mod cargo;
mod clippy;
mod fetch;
mod html;
mod resolve;
mod rustc;
mod rustfmt;
mod section;
mod style_guide;

#[cfg(test)]
mod http_fixture;
