//! Lexical tokenization and the in-memory BM25 section index — tasks [033] + [034], M3.
//!
//! This module owns two cooperating pieces:
//!
//! 1. **The tokenizer** ([`Token`] / [`tokenize`], task [033]): raw documentation
//!    text and query strings become an ordered, deterministic sequence of
//!    [`Token`]s that the index builds over. It preserves the atomic Rust/tooling
//!    forms `plan.md` §6 requires (`as_`, `to_`, `into_`, `Send`, `Sync`,
//!    `C-CONV`, `cargo test`, `--no-default-features`, `clippy::unwrap_used`).
//! 2. **The in-memory BM25 section index** ([`SectionIndex`], task [034]): a
//!    deterministic lexical ranking over an *immutable* corpus. It tokenizes each
//!    section through the [033] tokenizer, builds a `BTreeMap`-backed inverted
//!    index, and exposes a ranked [`search`](SectionIndex::search) seam that
//!    returns retrievable section identities — **no remote requests**.
//!
//! The index is pure and total: `build` and `search` make no network call, no I/O,
//! no `unsafe`; they never panic on any corpus (including empty). Determinism
//! (AC2) is enforced structurally — postings and all lookup maps are `BTreeMap`s,
//! and identical-score ties are broken by the section id string, never by
//! iteration order.
//!
//! **Out of scope for [034]** (later M3 work, deliberately *not* built here):
//! exact-code/name/lint boosts and the alias map ([035]), source filtering
//! ([035]), response limits / cursors ([037]), section and checklist reads
//! ([036]), and the command catalog ([039]+). The index produces the ranked
//! identities those consumers refine.

use std::collections::BTreeMap;

use crate::model::{Section, SectionId, SnapshotManifest};

/// A single lexical token produced by [`tokenize`].
///
/// The variants are the atomic forms the index must preserve. Each carries the
/// exact source substring as its payload (via [`Token::as_str`]); the variant
/// is the kind, used by later ranking/boost logic. Tokens are `Clone` and fully
/// `Eq`, so a token sequence can be compared by value for determinism tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// An ordinary identifier or word, e.g. `as_`, `into_`, `to_`, `Send`,
    /// `Sync`. Underscores are part of the identifier, so trailing/leading
    /// underscore forms survive whole.
    Word(String),
    /// A guideline code such as `C-CONV` or `E-RESCUE`: alnum segments joined by
    /// a hyphen that is *not* treated as a token separator.
    GuidelineCode(String),
    /// A command/flag option such as `--no-default-features` or `-v`: a run of
    /// leading dashes followed by an alnum/hyphen name.
    Flag(String),
    /// A qualified identifier or lint id such as `clippy::unwrap_used` or
    /// `std::fmt::Debug`: identifier segments joined by `::`.
    LintId(String),
    /// A multi-word command phrase such as `cargo test` or `rustup show`: a
    /// documented command verb followed by its subcommand, preserved as one
    /// token.
    Command(String),
}

impl Token {
    /// The token's source substring, regardless of its kind.
    pub fn as_str(&self) -> &str {
        match self {
            Token::Word(s)
            | Token::GuidelineCode(s)
            | Token::Flag(s)
            | Token::LintId(s)
            | Token::Command(s) => s,
        }
    }
}

/// The documented command *verbs* that may open a [`Token::Command`] phrase.
///
/// These are the tool families from `plan.md` §6 "Command guidance catalog".
/// A verb immediately followed by a subcommand *word* forms one `Command` token
/// (`cargo test`); a verb standing alone, or followed by a flag, stays a
/// `Word`. The set is deliberately small and explicit (deterministic, no
/// dictionary matching): the later command catalog ([039]-[044]) is the seam
/// that canonicalizes exact command keys.
const COMMAND_VERBS: &[&str] = &["cargo", "rustc", "rustup", "rustfmt", "rustdoc", "clippy"];

/// Tokenize `text` into an ordered, deterministic sequence of [`Token`]s.
///
/// The result is a pure function of `text`: the same input always produces the
/// same sequence, and the function never panics on any input (including empty
/// or whitespace-only strings, which yield an empty `Vec`).
///
/// See the module-level documentation for the token kinds, the atomic forms
/// that are preserved, and the case-preserving normalization.
pub fn tokenize(text: &str) -> Vec<Token> {
    // A `Vec<char>` cursor keeps the scan total and panic-free: indexing a
    // `&str` by byte could split a multi-byte UTF-8 char, but indexing a
    // `Vec<char>` by position always names a whole char. The corpus is small
    // and offline, so the one-per-call allocation is acceptable.
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();

    // First pass: scan left-to-right into *atomic* tokens. The command-phrase
    // merge (second pass) runs over these, because a phrase is a relationship
    // between two adjacent atomic words, not something one token boundary can
    // express.
    let mut atomic: Vec<Token> = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];

        // Whitespace is a token separator; runs are collapsed (no empty tokens).
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // A run starting with `-` is a flag: consume the leading dashes and the
        // following alnum/hyphen run so `--no-default-features` is one token
        // rather than `no`/`default`/`features`. A bare `-`/`--` is still a
        // flag token.
        if c == '-' {
            let start = i;
            while i < n && (chars[i] == '-' || chars[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            atomic.push(Token::Flag(chars[start..i].iter().collect()));
            continue;
        }

        // A run starting with an identifier character consumes identifier
        // segments, with `::` (lint id / module path) and `-` (guideline code)
        // as *internal* separators that are preserved within the token.
        if is_ident_char(c) {
            let start = i;
            let mut has_double_colon = false;
            let mut has_hyphen = false;

            // Consume the opening identifier segment.
            while i < n && is_ident_char(chars[i]) {
                i += 1;
            }

            // Then extend across `::` (lint id / path) and `-` (code)
            // separators, each requiring a following identifier segment. A
            // dangling separator at the end is left unconsumed and ends the
            // token, so we never finish on a bare separator except a trailing
            // `::` already consumed above.
            loop {
                if i + 1 < n && chars[i] == ':' && chars[i + 1] == ':' {
                    has_double_colon = true;
                    i += 2;
                    // A `::` not followed by an identifier ends the token; the
                    // consumed `::` stays part of it.
                    if i < n && is_ident_char(chars[i]) {
                        while i < n && is_ident_char(chars[i]) {
                            i += 1;
                        }
                        continue;
                    }
                    break;
                }
                if i < n && chars[i] == '-' && i + 1 < n && chars[i + 1].is_ascii_alphanumeric() {
                    has_hyphen = true;
                    i += 1; // consume the separating `-`
                    while i < n && is_ident_char(chars[i]) {
                        i += 1;
                    }
                    continue;
                }
                break;
            }

            let slice: String = chars[start..i].iter().collect();
            let token = if has_double_colon {
                Token::LintId(slice)
            } else if has_hyphen {
                Token::GuidelineCode(slice)
            } else {
                Token::Word(slice)
            };
            atomic.push(token);
            continue;
        }

        // Any other punctuation (`.`, `,`, `:`, `(`, `)`, quotes, …) is a
        // token separator and is dropped.
        i += 1;
    }

    merge_commands(atomic)
}

/// Second pass: fold a command *verb* immediately followed by a subcommand
/// *word* into one [`Token::Command`] phrase. Everything else passes through
/// unchanged, preserving order.
fn merge_commands(tokens: Vec<Token>) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        // Decide, in one place, whether `tokens[i]` opens a command phrase: it
        // must be a command *verb* immediately followed by a subcommand *word*.
        // A verb standing alone, or followed by a flag/other token, stays a
        // `Word`. The two-arm `match` below avoids a collapsible nested `if`
        // and any `unreachable!` on the common path.
        let phrase = match (&tokens[i], tokens.get(i + 1)) {
            (Token::Word(verb), Some(Token::Word(sub))) if is_command_verb(verb.as_str()) => {
                Some(format!("{verb} {sub}"))
            }
            _ => None,
        };
        match phrase {
            Some(p) => {
                out.push(Token::Command(p));
                i += 2;
            }
            None => {
                out.push(tokens[i].clone());
                i += 1;
            }
        }
    }
    out
}

/// A char that may appear *inside* an identifier segment: ASCII alphanumerics
/// and `_`. Underscores are part of the identifier, so `as_`/`to_`/`into_`
/// survive whole.
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether `s` is one of the documented command verbs that can open a
/// [`Token::Command`] phrase.
fn is_command_verb(s: &str) -> bool {
    COMMAND_VERBS.contains(&s)
}

// ---------------------------------------------------------------------------
// Task [034]: the in-memory BM25 section index
// ---------------------------------------------------------------------------

/// The default relevance parameters for [`SectionIndex`].
///
/// `k1` controls term-frequency saturation (a term's marginal contribution
/// grows sub-linearly with its frequency in one document) and `b` controls
/// length normalization (`1.0` = ignore length, `0.0` = full normalization).
/// The standard BM25 defaults from the Robertson/Walker literature are used,
/// per `plan.md` §4 "Search: In-memory lexical index with BM25 ranking".
const DEFAULT_K1: f64 = 1.2;
const DEFAULT_B: f64 = 0.75;

/// A ranked match returned by [`SectionIndex::search`].
///
/// It carries the **retrievable section identity** ([`SectionId`], AC1) plus
/// its deterministic BM25 [`score`](Hit::score). Callers resolve the identity
/// to section content and a source URL through the section reads ([036]); the
/// index itself returns only the identity and the ranking signal.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// The retrievable section identity the hit resolves to.
    pub section_id: SectionId,
    /// The deterministic BM25 score that ranks this hit.
    ///
    /// A floating-point score, so `Hit` is `PartialEq` but not `Eq` (an
    /// `f64` is not `Eq`); callers that need an exact identity compare
    /// [`section_id`](Hit::section_id) instead.
    pub score: f64,
}

/// The in-memory, deterministic BM25 index over an immutable corpus
/// ([`SnapshotManifest`]'s sections).
///
/// Built once per loaded snapshot (`plan.md` §5 "Rebuild the search index in
/// memory"), it holds:
///
/// - `sections` — the indexed sections, **in the order the snapshot presents
///   them** (the canonical ordering used only as a last-resort tie-break);
/// - `postings` — a `token -> BTreeMap<doc_index, term_frequency>` inverted
///   index. `BTreeMap` guarantees deterministic iteration, which is what makes
///   the ranking reproducible.
///
/// `n_docs` and `avg_doc_len` are the corpus-wide statistics BM25 needs;
/// `doc_lens` is the per-document (length-normalized) term count.
///
/// All public items are unwired until the section reads / `search_docs` handler
/// land ([036]/[037]); the `#[allow(dead_code)]` mirrors the precedent in
/// [`crate::model`], [`crate::registry`], and [`crate::runtime`], and is
/// lifted the moment a consumer is wired in.
#[allow(dead_code)] // transitional: unwired until [036]/[037]
pub struct SectionIndex {
    sections: Vec<Section>,
    postings: BTreeMap<String, BTreeMap<usize, usize>>,
    n_docs: usize,
    avg_doc_len: f64,
    doc_lens: Vec<usize>,
    k1: f64,
    b: f64,
}

#[allow(dead_code)] // transitional: unwired until [036]/[037]
impl SectionIndex {
    /// Build the index over every section of `manifest`, tokenized through the
    /// [033] [`tokenize`] API.
    ///
    /// The result indexes **every loaded section** (AC1). An empty corpus
    /// yields an empty index whose [`search`](Self::search) returns no hits
    /// (never a panic).
    pub fn build(manifest: &SnapshotManifest) -> SectionIndex {
        // `build_from_sections` owns the indexing; this is the manifest seam.
        SectionIndex::build_from_sections(manifest.body.sections.as_slice())
    }

    /// Build the index over an explicit slice of sections.
    ///
    /// Separated from [`build`] so a caller can index a *subset* (e.g. a
    /// source filter, [035]) through the identical code path; the manifest seam
    /// passes the whole corpus.
    pub fn build_from_sections(sections: &[Section]) -> SectionIndex {
        SectionIndex::with_params(sections, DEFAULT_K1, DEFAULT_B)
    }

    /// Build the index with explicit `k1`/`b` (used by the determinism tests
    /// to pin the constants and to exercise non-default parameters).
    ///
    /// `sections` is kept **in the order supplied** (the snapshot's canonical
    /// order), so an index built twice from the same input is byte-for-byte
    /// identical — the basis of AC2's "identical inputs → identical ordering".
    pub fn with_params(sections: &[Section], k1: f64, b: f64) -> SectionIndex {
        // One pass over the sections: per-document term frequencies feed the
        // inverted `postings` and the per-document token counts feed length
        // normalization. `BTreeMap` throughout keeps every iteration ordered,
        // so the index is independent of `HashMap`/insertion order.
        let n_docs = sections.len();
        let mut postings: BTreeMap<String, BTreeMap<usize, usize>> = BTreeMap::new();
        let mut doc_lens: Vec<usize> = Vec::with_capacity(n_docs);
        let mut total_tokens: u64 = 0;

        for (doc, section) in sections.iter().enumerate() {
            // The section's indexable text: body plus the structured fields a
            // query plausibly names (guideline codes, command keys, lint ids,
            // qualifications) and the heading/title breadcrumb. All are folded
            // through the [033] tokenizer, so `C-CONV`, `cargo test`, and
            // `clippy::unwrap_used` are preserved as the atomic tokens they are.
            let tokens = section_tokens(section);
            let df = tokens.len();
            total_tokens += df as u64;
            doc_lens.push(df);

            // Collapse repeated tokens in this document to a term frequency,
            // accumulating into the already-ordered `postings`.
            let mut freqs: BTreeMap<&str, usize> = BTreeMap::new();
            for t in &tokens {
                *freqs.entry(t.as_str()).or_insert(0) += 1;
            }
            for (term, tf) in freqs {
                postings
                    .entry(term.to_string())
                    .or_default()
                    .insert(doc, tf);
            }
        }

        let avg_doc_len = if n_docs == 0 {
            0.0
        } else {
            total_tokens as f64 / n_docs as f64
        };

        SectionIndex {
            sections: sections.to_vec(),
            postings,
            n_docs,
            avg_doc_len,
            doc_lens,
            k1,
            b,
        }
    }

    /// The number of sections the index covers (AC1: "every loaded source").
    pub fn doc_count(&self) -> usize {
        self.n_docs
    }

    /// Rank the sections that contain any token of `query`, in descending
    /// deterministic BM25 order.
    ///
    /// Empty query, empty index, or no matches all return `Vec::new()` — a
    /// successful search with zero hits (`plan.md` §6 "Empty results are a
    /// successful search with zero hits"), never a panic. The ranking is a pure
    /// function of the index and the query string, so repeated calls are
    /// identical (AC2).
    pub fn search(&self, query: &str) -> Vec<Hit> {
        // A query with no tokens cannot match; return the empty (successful)
        // result up front. This also covers an empty/whitespace-only query.
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() || self.n_docs == 0 {
            return Vec::new();
        }

        // Sum BM25 over the distinct query terms. Deduplicating on the token
        // string (via the `BTreeMap` accumulator below) keeps scoring
        // deterministic and independent of a repeated query term's order.
        let mut scored: BTreeMap<usize, f64> = BTreeMap::new();
        for token in &query_tokens {
            let term = token.as_str();
            let Some(posting) = self.postings.get(term) else {
                continue; // term absent from the corpus: contributes nothing
            };
            let idf = self.idf(posting.len());
            for (&doc, &tf) in posting {
                let doc_len = self.doc_lens[doc];
                let norm = self.k1 * (1.0 - self.b + self.b * (doc_len as f64 / self.avg_doc_len));
                let contribution = idf * (tf as f64 * (self.k1 + 1.0)) / (tf as f64 + norm);
                *scored.entry(doc).or_insert(0.0) += contribution;
            }
        }

        // Collect and sort by (score desc, section id asc, original order).
        // Section id is unique per section, so the order is fully determined;
        // the original-order key is a belt-and-suspenders final tie-break that
        // never actually fires because ids are distinct.
        let mut hits: Vec<Hit> = scored
            .into_iter()
            .map(|(doc, score)| Hit {
                section_id: self.sections[doc].id.clone(),
                score,
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.section_id.as_str().cmp(b.section_id.as_str()))
                .then_with(|| {
                    // Original-order fallback: never fires (ids are unique),
                    // but keeps the sort a total order and deterministic.
                    self.rank_of(&a.section_id)
                        .cmp(&self.rank_of(&b.section_id))
                })
        });
        hits
    }

    /// The corpus-wide inverse-document-frequency factor for a term seen in
    /// `df` of `n_docs` documents, using the BM25 "+1" smoothing variant.
    ///
    /// `+1` in both terms guarantees a non-negative, finite idf even for a
    /// term present in every document (`log(1 + n/n) = log 2 > 0`) and keeps the
    /// arithmetic panic-free on an empty corpus (`df == 0` is not consulted,
    /// since `search` skips absent terms).
    fn idf(&self, df: usize) -> f64 {
        ((self.n_docs - df + 1) as f64 / (df + 1) as f64).ln()
    }

    /// The original (snapshot) position of a section id, used only as the
    /// final, never-firing tie-break.
    fn rank_of(&self, id: &SectionId) -> usize {
        self.sections
            .iter()
            .position(|s| s.id == *id)
            .unwrap_or(usize::MAX)
    }
}

/// Fold a section's indexable fields into a token sequence.
///
/// The body plus the structured fields a query plausibly names
/// (`guideline_codes`, `command_keys`, `lint_ids`, `qualifications`) and the
/// heading/title breadcrumb are all tokenized through [`tokenize`], so the
/// atomic Rust/tooling forms survive as the tokens they are. This is the text
/// BM25 ranks over; the [035] exact-match boosts and [037] response bounds are
/// layered on the returned identities, not here.
fn section_tokens(section: &Section) -> Vec<Token> {
    use std::fmt::Write;

    let mut buf = String::new();
    // Body first (the bulk of the document).
    let _ = write!(buf, "{}", section.body);
    // The heading breadcrumb and document title: a query that names a section's
    // heading should rank it (plan.md §6 "boost headings … over body text");
    // the [035] boost multiplies this contribution, the fold is here.
    for h in &section.heading_path {
        let _ = write!(buf, "\n{h}");
    }
    let _ = write!(buf, "\n{}", section.doc_title);
    // The structured, query-facing fields, each as its own space-separated run
    // so their tokens index on their own.
    for field in [
        &section.guideline_codes,
        &section.command_keys,
        &section.lint_ids,
        &section.qualifications,
    ] {
        for value in field {
            let _ = write!(buf, "\n{value}");
        }
    }
    tokenize(&buf)
}

#[cfg(test)]
mod tests {
    // AC1/AC2 coverage is added in the next commit; the module compiles empty.
}
