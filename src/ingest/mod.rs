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

mod books;
mod clippy;
mod fetch;
mod html;
mod resolve;
mod rustfmt;
mod section;

#[cfg(test)]
mod http_fixture;
