//! Snapshot loading, validation, stable section IDs, and bounded section reads.
//!
//! Tasks [029]-[030]: load the latest valid synchronized snapshot (or the bundled
//! fallback), validate schema/ids/sizes, and implement `status` plus the
//! configurable staleness threshold. Entry point only for now.
