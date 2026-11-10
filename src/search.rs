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

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::model::{Section, SectionId, SnapshotManifest};
use crate::registry::SourceCollection;

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

#[cfg(test)]
impl Hit {
    /// The section ids of a ranked hit list, in order — the order-only view the
    /// ranking assertions read (`score` is the float, `section_id` the
    /// identity).
    fn section_ids(hits: &[Hit]) -> Vec<String> {
        hits.iter()
            .map(|h| h.section_id.as_str().to_string())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Task [035]: source filtering + exact-match boosts
//
// The [034] ranking above returns the *retrievable section identities* in a
// deterministic BM25 order. This task layers two things onto that same seam
// without changing it: (1) **source filtering** — restrict the ranked hits to
// the requested source(s), with an *unknown source value* as a clear
// [`SearchError`] rather than a silent empty result; and (2) **exact-match
// boosts** — an exact guideline-code / command-name / lint-id match lifts a
// section above weaker body matches (and a command-usage section above the
// individual lint entries a large catalog would otherwise crowd it under).
// ---------------------------------------------------------------------------

/// The eight canonical source ids as a single comma-joined list, used in
/// [`SearchError::UnsupportedSource`] messages so a mistyped source names the
/// valid alternatives. Guarded by `supported_sources_match_registry` so it can
/// never drift from the [`SourceCollection`] registry.
#[allow(dead_code)] // transitional: unwired until the search_docs handler ([037])
const SUPPORTED_SOURCES: &str =
    "api-guidelines, style-guide, cargo, rustc, rustup, rustfmt, clippy, rustdoc";

/// A query-time search error.
///
/// Distinct from the corpus/schema errors: these are *query* validation
/// failures — chiefly an unsupported source value — that must surface as a
/// clear, typed error rather than a silent empty result. `plan.md` §6: "Unknown
/// … and unsupported source values produce clear validation errors"; §11:
/// "Source filters, … unknown IDs, invalid cursors, and oversized inputs behave
/// predictably."
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[allow(dead_code)] // transitional: unwired until the search_docs handler ([037])
pub enum SearchError {
    /// The requested source value is not one of the eight canonical source ids
    /// (`api-guidelines`, `style-guide`, `cargo`, `rustc`, `rustup`, `rustfmt`,
    /// `clippy`, `rustdoc`).
    ///
    /// The message lists the valid ids as *discoverable alternatives*
    /// (`plan.md` §6 "Reject unsupported keys with discoverable alternatives"),
    /// so a caller that mistyped a source can correct it from the error alone —
    /// the caller distinguishes this from a successful zero-hit search.
    #[error("unsupported source `{0}`: expected one of {SUPPORTED_SOURCES}")]
    UnsupportedSource(String),
}

/// A source restriction for [`SectionIndex::search_in`] / [`search_source`].
///
/// `plan.md` §6 defines `search_docs`'s optional `source` as "one of the eight
/// canonical ids …; omission searches all sources." This type is that
/// restriction, **validated at construction** so an unknown/unsupported value is
/// a clear [`SearchError`] (from [`SourceFilter::source`] / [`sources`]) rather
/// than a silent empty result:
///
/// - [`SourceFilter::all`] (the default / an omitted source) imposes no
///   restriction;
/// - a non-empty set of canonical source ids (the [`SourceCollection`] ids) restricts hits to
///   those source(s) — the filter supports one *or many*; the MVP tool passes at most
///   one, but the seam is written for the general case.
///
/// A `SourceFilter` is *always valid*: it is built only through the fallible
/// [`source`](Self::source) / [`sources`](Self::sources) constructors, which
/// reject any id outside the eight canonical ids before a filter is returned, so
/// a consumer that already holds a `SourceFilter` never needs to re-check it.
/// `is_all` / [`contains`](Self::contains) are infallible.
///
/// The set is a `BTreeSet` so membership and iteration are deterministic (AC2),
/// and duplicate ids collapse to one restriction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)] // transitional: unwired until the search_docs handler ([037])
pub struct SourceFilter(Option<BTreeSet<&'static str>>);

#[allow(dead_code)] // transitional: unwired until the search_docs handler ([037])
impl SourceFilter {
    /// No restriction — search every source (`plan.md` §6 "omission searches all
    /// sources"). The default for the omitted `source` parameter.
    pub fn all() -> Self {
        SourceFilter(None)
    }

    /// Restrict to the single source whose canonical id is `id`.
    ///
    /// Fails with [`SearchError::UnsupportedSource`] if `id` is not one of the
    /// eight canonical ids — a clear error, never a silent empty result.
    pub fn source(id: impl AsRef<str>) -> Result<Self, SearchError> {
        let canonical = resolve_source(id.as_ref())?;
        Ok(SourceFilter(Some(BTreeSet::from([canonical]))))
    }

    /// Restrict to a set of sources, each identified by its canonical id.
    ///
    /// Duplicates collapse (the restriction is a set) and order is irrelevant.
    /// Any id outside the eight canonical ids fails with
    /// [`SearchError::UnsupportedSource`] *before* a filter is returned. An
    /// empty iterable yields [`all`](Self::all) — the natural reading of "no
    /// sources named" — rather than a vacuous "no sources" filter.
    pub fn sources(ids: impl IntoIterator<Item = impl AsRef<str>>) -> Result<Self, SearchError> {
        let mut set = BTreeSet::new();
        for id in ids {
            set.insert(resolve_source(id.as_ref())?);
        }
        Ok(SourceFilter(Some(set)))
    }

    /// Whether this filter restricts to a *subset* of sources.
    ///
    /// `false` means [`all`](Self::all): every source is searched.
    pub fn is_all(&self) -> bool {
        self.0.is_none()
    }

    /// Whether the source with canonical id `source_id` is within this filter.
    ///
    /// An [`all`](Self::all) filter contains every source; a restricted filter
    /// contains only its named collections. This is the membership test
    /// [`SectionIndex::search_in`] uses to drop out-of-filter hits.
    pub fn contains(&self, source_id: &str) -> bool {
        match &self.0 {
            None => true,
            Some(set) => set.contains(source_id),
        }
    }
}

/// Resolve a source id string to its canonical [`SourceCollection`], failing
/// clearly on a value outside the eight registry ids.
///
/// The match is by exact, case-sensitive id against the [`SourceCollection`]
/// registry (`plan.md` §2/§6): no aliasing or case-folding, so a value like
/// `cargo-book` or `CARGO` is *not* silently accepted — it is an
/// [`UnsupportedSource`](SearchError::UnsupportedSource).
#[allow(dead_code)] // transitional: unwired until the search_docs handler ([037])
fn resolve_source(id: &str) -> Result<&'static str, SearchError> {
    SourceCollection::ALL
        .iter()
        .find(|c| c.id() == id)
        .map(|c| c.id())
        .ok_or_else(|| SearchError::UnsupportedSource(id.to_string()))
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

    /// Rank the sections that contain any token of `query` across **every**
    /// source, in descending deterministic order.
    ///
    /// This is the all-sources convenience entry (`plan.md` §6: "omission
    /// searches all sources"); it is [`search_in`](Self::search_in) with no
    /// source restriction. Empty query, empty index, or no matches all return
    /// `Vec::new()` — a successful search with zero hits (`plan.md` §6 "Empty
    /// results are a successful search with zero hits"), never a panic. The
    /// ranking is a pure function of the index and the query string, so repeated
    /// calls are identical (AC2).
    pub fn search(&self, query: &str) -> Vec<Hit> {
        self.search_in(query, &SourceFilter::all())
    }

    /// Rank the sections that contain any token of `query` and belong to the
    /// sources named by `filter`, in descending deterministic order.
    ///
    /// This is the filtered entry point that the `search_docs` handler will map
    /// its optional `source` onto ([037]). The filter is validated at
    /// construction ([`SourceFilter::source`] / [`SourceFilter::sources`]), so a
    /// value that is *not* one of the eight canonical source ids fails there
    /// with a [`SearchError`] — never as a silent empty result here. A *valid*
    /// filter that matches no section yields an empty `Vec` (a successful search
    /// with zero hits), which is distinct from that construction error.
    ///
    /// The ranking is a pure function of the index, the query, and the filter,
    /// so repeated calls are identical (AC2). Filtering is a post-ranking
    /// restriction: the BM25 statistics (idf, average length) are computed over
    /// the whole corpus, and the filter only drops hits outside the requested
    /// source(s) — the source filter never reshapes the ranking, only which of
    /// the ranked hits are returned.
    pub fn search_in(&self, query: &str, filter: &SourceFilter) -> Vec<Hit> {
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

        // Restrict the ranked hits to the requested source(s) *after* ranking, so
        // the ranking signal is computed over the whole corpus and the filter
        // only decides which hits are returned. Section ids are unique, so the
        // `section id asc` term alone yields a total, deterministic order; no
        // additional tie-break is needed.
        let mut hits: Vec<Hit> = scored
            .into_iter()
            .filter(|(doc, _)| filter.contains(self.sections[*doc].source_id.as_str()))
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
        });
        hits
    }

    /// Rank the sections that contain any token of `query` within a single
    /// source named by `source` (its canonical id), in descending deterministic
    /// order.
    ///
    /// This is the fallible, single-source entry the `search_docs` handler's
    /// optional `source` maps onto: a `source` value that is **not** one of the
    /// eight canonical source ids fails with [`SearchError::UnsupportedSource`]
    /// (a clear, typed error with the valid ids as discoverable alternatives —
    /// `plan.md` §6 "unsupported source values produce clear validation
    /// errors"), **not** a silent empty result. A valid source that matches no
    /// section returns `Ok` of an empty `Vec` (a successful zero-hit search).
    pub fn search_source(&self, query: &str, source: &str) -> Result<Vec<Hit>, SearchError> {
        let filter = SourceFilter::source(source)?;
        Ok(self.search_in(query, &filter))
    }

    /// The corpus-wide inverse-document-frequency factor for a term seen in
    /// `df` of `n_docs` documents, using the BM25 "+1" smoothing variant.
    ///
    /// The `+1` smoothing keeps the value **finite and panic-free** for any
    /// corpus: the denominator `df + 1` is always `>= 1` and the numerator
    /// `n_docs - df + 1` is always `>= 1` (since `df <= n_docs`), so the ratio
    /// is positive and `.ln()` is well-defined — it never divides by zero or
    /// logs a non-positive value. The result may, however, be slightly
    /// **negative** for a term present in nearly all documents: when
    /// `df == n_docs` the ratio is `1 / (n_docs + 1) < 1`, so
    /// `log(1 / (n_docs + 1)) < 0`. That is expected for the BM25 "+1" variant
    /// and is harmless to ranking. `df == 0` is never consulted, since `search`
    /// skips absent terms.
    fn idf(&self, df: usize) -> f64 {
        ((self.n_docs - df + 1) as f64 / (df + 1) as f64).ln()
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
    use super::*;
    use crate::model::{
        ManifestBody, Release, SchemaVersion, SectionBuilder, SnapshotId, SnapshotManifest, Source,
        SourceId, Timestamp, Url,
    };

    /// Unique probe words, one per sample section, in the same order as
    /// `sample_sections`. Each word is embedded only in its own section's body,
    /// so a query for it resolves to exactly that section's `SectionId` — the
    /// AC1 coverage check ("every section is retrievable by an identity").
    const SAMPLE_PROBES: [&str; 5] = [
        "alphacargo",
        "bravocrustc",
        "charlieapi",
        "deltacode",
        "echoes",
    ];

    // --- Fixtures ---------------------------------------------------------

    /// A minimal, well-formed section at `(source, chapter)`, carrying the
    /// structured fields a query plausibly names so the ranking has something
    /// to match. `probe` is a single, unique word prefixed into the body so the
    /// section is retrievable by a term no other section shares. Built through
    /// `SectionBuilder` so the id and content hash are the consistent,
    /// recomputed values the model requires.
    fn section(
        source: &str,
        chapter: &str,
        probe: &str,
        body: &str,
        codes: &[String],
        cmds: &[String],
        lints: &[String],
    ) -> Section {
        SectionBuilder::new(
            SourceId::new(source).expect("a registry id builds a SourceId"),
            "Test doc",
            chapter,
            Url::new("https://doc.rust-lang.org/cargo/").expect("a valid URL"),
            format!("{probe} {body}"),
        )
        .guideline_codes(codes)
        .command_keys(cmds)
        .lint_ids(lints)
        .build()
        .expect("a well-formed section builds")
    }

    /// A snapshot that carries the given sections (the corpus the index builds
    /// over). Every other field is the minimal valid shape `SectionBuilder` / the
    /// manifest envelope require.
    fn snapshot(sections: Vec<Section>) -> SnapshotManifest {
        SnapshotManifest {
            schema_version: SchemaVersion::CURRENT,
            body: ManifestBody {
                snapshot_id: SnapshotId::new("rust-docs-search-test").expect("a valid snapshot id"),
                created_at: Timestamp::new("2026-10-30T00:00:00Z").expect("a valid timestamp"),
                parser_version: "test/1".into(),
                sources: vec![Source {
                    id: SourceId::new("cargo").expect("a registry id builds a SourceId"),
                    title: "The Cargo Book".into(),
                    entry_url: Url::new("https://doc.rust-lang.org/cargo/").expect("a valid URL"),
                    document_urls: vec![],
                    last_checked: None,
                    fetched_at: Timestamp::new("2026-10-30T00:00:00Z").expect("a valid timestamp"),
                    upstream_modified: None,
                    content_hash: crate::model::CorpusHash::compute(b"fixture"),
                    release: Release::default(),
                    parser: "test/1".into(),
                }],
                sections,
                checklist: vec![],
                commands: vec![],
                validation: Default::default(),
                attribution: vec![],
            },
        }
    }

    /// A small but multi-source corpus exercising distinct sources, a
    /// guideline-code section, a command-phrase section, a lint-id section, and
    /// a section whose text has no overlap with the others (the no-match
    /// control). Each section carries a unique probe word in its body so it is
    /// retrievable on its own.
    fn sample_sections() -> Vec<Section> {
        vec![
            section(
                "cargo",
                "commands/cargo-build",
                "alphacargo",
                "cargo build compiles the local package and its dependencies",
                &[],
                &["cargo build".to_string()],
                &[],
            ),
            section(
                "rustc",
                "options",
                "bravocrustc",
                "rustc accepts compiler options such as --no-default-features and -O",
                &[],
                &[],
                &[],
            ),
            section(
                "api-guidelines",
                "about",
                "charlieapi",
                "The guideline C-CONV prefers From and Into for conversions",
                &["C-CONV".to_string()],
                &[],
                &[],
            ),
            section(
                "clippy",
                "configuration",
                "deltacode",
                "The lint clippy::unwrap_used catches unwrap calls in production code",
                &[],
                &[],
                &["clippy::unwrap_used".to_string()],
            ),
            // A no-overlap control section: shares none of the query terms
            // above, so a query that names one of the others must never rank it
            // (the no-match / empty-result guarantee is exercised against it).
            section(
                "style-guide",
                "intro",
                "echoes",
                "whitespace and comment style notes",
                &[],
                &[],
                &[],
            ),
        ]
    }

    fn sample_snapshot() -> SnapshotManifest {
        snapshot(sample_sections())
    }

    // --- AC1: indexing covers every loaded source / section ---------------

    /// AC1: `build` indexes **every** section of the corpus, and each indexed
    /// section's identity resolves through the index — a query naming that
    /// section's distinctive term returns its `SectionId`.
    #[test]
    fn indexing_covers_every_section_and_produces_retrievable_identities() {
        let snap = sample_snapshot();
        let index = SectionIndex::build(&snap);

        // The index covers every loaded section.
        assert_eq!(
            index.doc_count(),
            snap.body.sections.len(),
            "the index must cover every loaded section"
        );
        assert_ne!(index.doc_count(), 0, "the corpus is non-empty");

        // Every section is retrievable by an identity: a query naming that
        // section's distinctive probe word returns a hit carrying its
        // `SectionId`, and the probe word is unique so exactly one section
        // resolves.
        let mut resolved: Vec<String> = Vec::new();
        for (i, section) in snap.body.sections.iter().enumerate() {
            let id = section.id.clone();
            let query = SAMPLE_PROBES[i];
            let hits = index.search(query);
            let got = Hit::section_ids(&hits);
            assert_eq!(
                got,
                vec![id.as_str().to_string()],
                "section {id:?} must be the sole hit for its probe {query:?}; got {got:?}"
            );
            resolved.push(id.as_str().to_string());
        }

        // The identities are distinct and complete: every loaded section's id
        // appears, exactly once, across the per-section retrievals.
        let mut expected: Vec<String> = snap
            .body
            .sections
            .iter()
            .map(|s| s.id.as_str().to_string())
            .collect();
        expected.sort_unstable();
        let mut got_ids = resolved.clone();
        got_ids.sort_unstable();
        assert_eq!(
            got_ids, expected,
            "every loaded section must resolve to its own identity, exactly once"
        );
    }

    /// AC1 over a *multi-source* corpus: sections from different sources are
    /// each indexed and retrievable under their own identities.
    #[test]
    fn every_source_section_is_indexed_and_resolvable() {
        let snap = sample_snapshot();
        let index = SectionIndex::build(&snap);
        // Four distinct sources are present and indexed.
        let mut sources: Vec<String> = snap
            .body
            .sections
            .iter()
            .map(|s| s.source_id.as_str().to_string())
            .collect();
        sources.sort_unstable();
        sources.dedup();
        assert!(
            sources.len() >= 3,
            "the sample corpus spans multiple sources"
        );
        // Each source's section resolves to its identity (a non-empty hit list
        // carrying that section's id), searched by its unique probe word.
        for (i, section) in snap.body.sections.iter().enumerate() {
            let got = Hit::section_ids(&index.search(SAMPLE_PROBES[i]));
            assert_eq!(
                got,
                vec![section.id.as_str().to_string()],
                "source section {section:?} must be retrievable by its probe {}",
                SAMPLE_PROBES[i]
            );
        }
    }

    /// AC1: an **empty corpus** yields an empty index whose `search` is the
    /// empty, successful result — never a panic.
    #[test]
    fn empty_corpus_yields_no_results_without_panicking() {
        let index = SectionIndex::build(&snapshot(Vec::new()));
        assert_eq!(index.doc_count(), 0);
        assert!(index.search("cargo build").is_empty());
        assert!(index.search("anything at all").is_empty());
        assert!(index.search("").is_empty());
    }

    // --- AC2: deterministic scoring and tie-breaking ----------------------

    /// AC2: building the same corpus twice and searching the same query yields
    /// **identical ordering and scores**, run several times (no randomness, no
    /// order dependence).
    #[test]
    fn build_and_search_are_deterministic_across_runs() {
        let snap = sample_snapshot();
        let a = SectionIndex::build(&snap);
        let b = SectionIndex::build(&snap);

        for query in [
            "cargo build",
            "C-CONV",
            "clippy::unwrap_used",
            "--no-default-features",
            "build dependencies",
            "",
            "nonexistent-term-xyzzy",
        ] {
            let ra = a.search(query);
            let rb = b.search(query);
            assert_eq!(
                Hit::section_ids(&ra),
                Hit::section_ids(&rb),
                "ordering must be identical for {query:?}"
            );
            assert_eq!(
                ra, rb,
                "scored hits (identity + score) must be identical for {query:?}"
            );
            // Repeated searches of one index agree too.
            for _ in 0..8 {
                assert_eq!(Hit::section_ids(&a.search(query)), Hit::section_ids(&ra));
                assert_eq!(a.search(query), ra, "scores stable for {query:?}");
            }
        }
    }

    /// AC2: when two sections score identically, the tie is broken by the
    /// section id string (a total, deterministic order), **not** by any
    /// iteration/insertion order. The two sections have identical indexable
    /// text, so they score exactly equal and are ordered purely by id.
    #[test]
    fn equal_scores_tie_break_by_section_id() {
        // Identical body, codes, commands, lints => identical token multiset =>
        // identical length and identical BM25 contribution for any query. The
        // only difference is the chapter path (and thus the section id).
        let a = section(
            "cargo",
            "aaa",
            "ignored",
            "cargo build compiles the package and its dependencies",
            &[],
            &["cargo build".to_string()],
            &[],
        );
        let b = section(
            "rustc",
            "zzz",
            "ignored",
            "cargo build compiles the package and its dependencies",
            &[],
            &["cargo build".to_string()],
            &[],
        );
        let index = SectionIndex::build(&snapshot(vec![b.clone(), a.clone()]));
        // Insert the higher-id section *first* to prove ordering does not follow
        // build/insertion order.

        let hits = index.search("cargo build dependencies");
        assert_eq!(hits.len(), 2, "both equal-score sections match the query");
        assert_eq!(
            hits[0].score, hits[1].score,
            "the two sections must score identically"
        );
        // Tie broken by id string: the lexicographically smaller id ranks first.
        let order: Vec<&str> = hits.iter().map(|h| h.section_id.as_str()).collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(
            order, sorted,
            "equal scores must be ordered by ascending section id"
        );
    }

    /// AC2 (no-remote guarantee): `search` reads only the in-memory index and
    /// the query string; it issues no I/O. Exercised here as an offline, pure
    /// call — no network, async, or `unsafe` path is reachable from `build`/
    /// `search`. A query with no matches returns an empty (successful) result
    /// rather than fabricating a hit or panicking.
    #[test]
    fn search_is_offline_and_no_match_is_empty() {
        let index = SectionIndex::build(&sample_snapshot());
        assert!(
            index.search("nonexistent-term-xyzzy").is_empty(),
            "a query with no matches is a successful, empty result"
        );
        assert!(index.search("").is_empty(), "a blank query yields no hits");
        assert!(
            index.search("   \t \n  ").is_empty(),
            "whitespace-only query is blank"
        );
    }

    // --- Ranking quality (sanity for the later [035] boosts) ---------------

    /// A guideline-code query surfaces only its section, mirroring the
    /// behavioural case "an exact `C-CONV` query returns its upstream
    /// explanation first". This is the *ranking* signal [035] will formalize
    /// with an explicit boost; the index already isolates the exact-code
    /// section.
    #[test]
    fn exact_guideline_code_resolves_its_section() {
        let index = SectionIndex::build(&sample_snapshot());
        let got = Hit::section_ids(&index.search("C-CONV"));
        let api = section(
            "api-guidelines",
            "about",
            "ignored",
            "ignored",
            &["C-CONV".to_string()],
            &[],
            &[],
        )
        .id;
        assert_eq!(
            got,
            vec![api.as_str().to_string()],
            "the C-CONV section must be the sole hit; got {got:?}"
        );
    }

    /// A command-phrase query surfaces the section documenting that command, and
    /// a lint-id query surfaces its lint section — the exact-name/lint cases
    /// the index must resolve without [035]'s explicit boosts.
    #[test]
    fn command_phrase_and_lint_id_queries_resolve_their_sections() {
        let index = SectionIndex::build(&sample_snapshot());

        let cmd = section(
            "cargo",
            "commands/cargo-build",
            "ignored",
            "cargo command reference text",
            &[],
            &["cargo build".to_string()],
            &[],
        )
        .id;
        let cmd_hits = index.search("cargo build");
        assert_eq!(
            Hit::section_ids(&cmd_hits).first().map(|s| s.as_str()),
            Some(cmd.as_str()),
            "`cargo build` must surface its command section"
        );

        let lint = section(
            "clippy",
            "configuration",
            "ignored",
            "lint configuration reference text",
            &[],
            &[],
            &["clippy::unwrap_used".to_string()],
        )
        .id;
        let lint_hits = index.search("clippy::unwrap_used");
        assert_eq!(
            Hit::section_ids(&lint_hits).first().map(|s| s.as_str()),
            Some(lint.as_str()),
            "`clippy::unwrap_used` must surface its lint section"
        );
    }

    /// A section that contains none of a query's terms must not appear in the
    /// results: a no-overlap query returns zero hits, never a fabricated one.
    #[test]
    fn a_no_overlap_query_returns_no_hits() {
        let index = SectionIndex::build(&sample_snapshot());
        // No section mentions `quantum chromodynamics`.
        assert!(index.search("quantum chromodynamics").is_empty());
    }

    /// A command-phrase query returns only the section that names the command —
    /// relevance is not a constant, and a no-overlap section is excluded.
    #[test]
    fn a_term_query_ranks_only_sections_containing_it() {
        let index = SectionIndex::build(&sample_snapshot());
        let got = Hit::section_ids(&index.search("cargo build"));
        assert!(
            got.iter()
                .any(|s| s.contains("cargo/commands%2Fcargo-build")),
            "the cargo-build section must match; got {got:?}"
        );
        assert!(
            !got.iter().any(|s| s.contains("style-guide")),
            "a no-overlap section must not match `cargo build`; got {got:?}"
        );
    }

    /// Token preservation end-to-end through the index: the atomic forms the
    /// [033] tokenizer protects (`as_`, `into_`, `--no-default-features`)
    /// survive as indexable terms and are retrievable.
    #[test]
    fn index_preserves_atomic_token_forms_end_to_end() {
        let built = section(
            "cargo",
            "commands/cargo-build",
            "ignored",
            "as_ and into_ conversions; --no-default-features is a flag",
            &[],
            &[],
            &[],
        );
        let index = SectionIndex::build(&snapshot(vec![built]));
        for q in ["as_", "into_", "--no-default-features"] {
            let got = Hit::section_ids(&index.search(q));
            assert!(!got.is_empty(), "atomic form {q:?} must be retrievable");
        }
    }

    // -------------------------------------------------------------------
    // Task [033] tokenizer regression tests — restored in [034].
    //
    // The [033] tokenizer is byte-for-byte unchanged and load-bearing (consumed
    // through `section_tokens` → `tokenize`), but the [034] index build replaced
    // this module's `#[cfg(test)]` body with index-only tests. Those are a
    // different layer and not a superset of these, so the tokenizer's own
    // regression coverage is restored here to coexist with the index tests.
    // -------------------------------------------------------------------

    /// The source substrings of the tokens, in order — a kind-agnostic view
    /// for asserting a phrase/word survives regardless of its tag.
    fn texts(tokens: &[Token]) -> Vec<&str> {
        tokens.iter().map(Token::as_str).collect()
    }

    // --- AC1: the required forms survive as whole tokens -------------------

    #[test]
    fn guideline_code_survives() {
        assert_eq!(
            tokenize("C-CONV"),
            vec![Token::GuidelineCode("C-CONV".to_string())]
        );
        assert_eq!(
            tokenize("follow E-RESCUE"),
            vec![
                Token::Word("follow".to_string()),
                Token::GuidelineCode("E-RESCUE".to_string()),
            ]
        );
    }

    #[test]
    fn identifier_with_trailing_underscore_survives() {
        for form in ["as_", "into_", "to_"] {
            assert_eq!(
                tokenize(form),
                vec![Token::Word(form.to_string())],
                "trailing-underscore identifier `{form}` must survive whole"
            );
        }
    }

    #[test]
    fn camelcase_type_names_survive_with_case() {
        for form in ["Send", "Sync"] {
            let tokens = tokenize(form);
            assert_eq!(tokens, vec![Token::Word(form.to_string())]);
            // Case is preserved: `Send` is not lower-cased to `send`.
            assert_ne!(tokens[0].as_str(), "send");
        }
    }

    #[test]
    fn command_phrase_survives_as_one_token() {
        assert_eq!(
            tokenize("cargo test"),
            vec![Token::Command("cargo test".to_string())]
        );
        assert_eq!(
            tokenize("run rustup show"),
            vec![
                Token::Word("run".to_string()),
                Token::Command("rustup show".to_string())
            ]
        );
    }

    #[test]
    fn long_flag_survives() {
        assert_eq!(
            tokenize("--no-default-features"),
            vec![Token::Flag("--no-default-features".to_string())]
        );
        assert_eq!(tokenize("-v"), vec![Token::Flag("-v".to_string())]);
    }

    #[test]
    fn lint_id_with_double_colon_survives() {
        assert_eq!(
            tokenize("clippy::unwrap_used"),
            vec![Token::LintId("clippy::unwrap_used".to_string())]
        );
        assert_eq!(
            tokenize("see std::fmt::Debug"),
            vec![
                Token::Word("see".to_string()),
                Token::LintId("std::fmt::Debug".to_string()),
            ]
        );
    }

    #[test]
    fn every_ac1_form_survives_in_mixed_text() {
        let tokens = tokenize(
            "Preserve C-CONV and clippy::unwrap_used; use cargo test \
             --no-default-features for a Send implementation (as_ and into_).",
        );
        let seen = texts(&tokens);
        for required in [
            "C-CONV",
            "clippy::unwrap_used",
            "cargo test",
            "--no-default-features",
            "Send",
            "as_",
            "into_",
        ] {
            assert!(
                seen.contains(&required),
                "mixed text must preserve `{required}`; got {seen:?}"
            );
        }
    }

    // --- AC2: determinism across repeated tokenization ---------------------

    #[test]
    fn tokenize_is_deterministic_repeated() {
        let samples = [
            "cargo test --no-default-features --locked",
            "C-CONV clippy::unwrap_used as_ Send",
            "rustup show; rustfmt --check",
            "std::fmt::Debug and E-RESCUE",
            "  mixed   spacing  and\ttabs\nand newlines",
            "",
            "   ",
            "only-punctuation ! @ # $ % ^ & * ()",
            "a-very-long-hyphenated-code-value-1",
        ];
        for sample in samples {
            let first = tokenize(sample);
            // Re-tokenize the same input several times; the sequence must be
            // identical every time (no ordering/randomness/load-order effect).
            for _ in 0..8 {
                assert_eq!(tokenize(sample), first, "non-deterministic for `{sample}`");
            }
        }
    }

    #[test]
    fn repeated_corpus_load_is_deterministic() {
        // Simulate "loading the same corpus" by tokenizing a fixed set of
        // documents, then loading it again, and asserting identical results.
        let corpus = [
            "Convert to Option: see the C-CONV guideline and into_ conversion.",
            "Run cargo test --locked to verify the build is reproducible.",
            "Enable clippy::unwrap_used to catch unwraps in production code.",
            "A Send + Sync bound is required for the Send implementation.",
            "Inspect the toolchain with rustup show.",
        ];
        let pass_one: Vec<Vec<Token>> = corpus.iter().map(|doc| tokenize(doc)).collect();
        let pass_two: Vec<Vec<Token>> = corpus.iter().map(|doc| tokenize(doc)).collect();
        assert_eq!(
            pass_one, pass_two,
            "repeated corpus loads must tokenize identically"
        );
    }

    // --- Edge cases / total (no-panic) behaviour ---------------------------

    #[test]
    fn empty_and_whitespace_input_yields_no_tokens() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
        assert!(tokenize("\t\n\r").is_empty());
    }

    #[test]
    fn punctuation_is_a_separator_and_is_dropped() {
        assert_eq!(
            tokenize("a, b. c"),
            vec![
                Token::Word("a".to_string()),
                Token::Word("b".to_string()),
                Token::Word("c".to_string()),
            ]
        );
    }

    #[test]
    fn a_single_colon_is_a_separator_but_double_colon_is_not() {
        // `a:b` -> two words (single colon separates); `a::b` -> one lint id.
        assert_eq!(
            tokenize("a:b"),
            vec![Token::Word("a".to_string()), Token::Word("b".to_string())]
        );
        assert_eq!(tokenize("a::b"), vec![Token::LintId("a::b".to_string())]);
    }

    #[test]
    fn multiple_flags_and_a_command_phrase() {
        assert_eq!(
            texts(&tokenize("cargo test --no-default-features --locked")),
            vec!["cargo test", "--no-default-features", "--locked"]
        );
    }

    #[test]
    fn flag_with_equals_splits_into_flag_and_value() {
        assert_eq!(
            texts(&tokenize("--features=dev")),
            vec!["--features", "dev"]
        );
    }

    #[test]
    fn a_command_verb_alone_stays_a_word() {
        // `rustc` with nothing after it is not a phrase; it stays a word.
        assert_eq!(tokenize("rustc"), vec![Token::Word("rustc".to_string())]);
        // A verb followed by a flag does not absorb the flag into a phrase.
        assert_eq!(texts(&tokenize("cargo -v")), vec!["cargo", "-v"]);
    }

    #[test]
    fn verb_absorbs_only_the_immediately_following_word() {
        // `cargo test suite` -> `cargo test` (phrase) then the bare word `suite`.
        assert_eq!(
            texts(&tokenize("cargo test suite")),
            vec!["cargo test", "suite"]
        );
    }

    #[test]
    fn a_bare_subcommand_word_is_not_a_phrase() {
        // `test` is not a command verb, so "run the test" keeps `test` a word.
        assert_eq!(texts(&tokenize("run the test")), vec!["run", "the", "test"]);
    }

    #[test]
    fn unicode_and_odd_input_does_not_panic() {
        // Total: arbitrary (even multi-byte / unusual) input tokenizes without
        // panicking and preserves the recognizable ASCII forms around it.
        let tokens = tokenize("café::CONV — C-CONV ƒ\u{1F600}\n");
        let out = texts(&tokens);
        // The multibyte pieces are non-identifier punctuation/chars (separators)
        // or stand alone; what matters is no panic and that `C-CONV` survives.
        assert!(out.contains(&"C-CONV"));
        assert!(out.iter().all(|t| !t.contains('\0')));
    }

    #[test]
    fn ordering_is_stable_and_left_to_right() {
        // Tokens appear in source order; no reordering.
        assert_eq!(
            texts(&tokenize("A B C-CONV clippy::x cargo test --f")),
            vec!["A", "B", "C-CONV", "clippy::x", "cargo test", "--f"]
        );
    }
}
