//! Tokenization, in-memory BM25 ranking, exact-match boosts, source filtering,
//! and snapshot-bound pagination.
//!
//! Tasks [033]-[047]: build the lexical index over the corpus, preserve tokens
//! such as `C-CONV`, `cargo test`, `--no-default-features`, and
//! `clippy::unwrap_used`, and enforce the response-size and cursor rules in
//! `plan.md` section 6. Entry point only for now.
