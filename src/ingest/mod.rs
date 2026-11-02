//! Documentation ingestion: bounded fetching, extraction, validation, and atomic
//! publication.
//!
//! Tasks [010]-[028]. Sub-adapters:
//!
//! - [`books`]   — book navigation discovery and article-to-Markdown extraction.
//! - [`rustfmt`] — adapter for the approved upstream rustfmt Markdown files.
//! - [`clippy`]  — per-entry extraction from the Clippy lint catalog.
//! - [`fetch`]   — the bounded HTTP fetcher every ingestion stage downloads through.

mod books;
mod clippy;
mod fetch;
mod rustfmt;

#[cfg(test)]
mod http_fixture;
