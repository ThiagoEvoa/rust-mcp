//! Lexical tokenization for the in-memory search index — task [033] M3.
//!
//! This module owns the **tokenizer** that feeds the later retrieval work
//! ([034]-[047]): it turns raw documentation text and query strings into an
//! ordered, deterministic sequence of [`Token`]s that the index (and the
//! ranking/boost logic of plan.md §6) can build over.
//!
//! ## What it owns — and what it deliberately does not
//!
//! - **It preserves the meaningful Rust/tooling forms** that a naive
//!   whitespace-and-punctuation split would destroy. In particular,
//!   `plan.md` §6 "Retrieval and response rules" requires that tokens such as
//!   `as_`, `to_`, `into_`, `Send`, `Sync`, `C-CONV`, `cargo test`,
//!   `--no-default-features`, and `clippy::unwrap_used` survive indexing.
//!   [`tokenize`] recognizes each of these as an *atomic* token rather than
//!   fragmenting it.
//! - **It is deterministic** (AC2): the output is a pure function of the input
//!   string. There is no randomness, no `HashMap`-order dependence, and no
//!   dependence on load order — the same input always yields the same ordered
//!   token sequence. [`tokenize`] is total: it never panics on any `&str`,
//!   including empty, whitespace-only, or otherwise unusual input.
//! - **It is pure and offline**: `tokenize` allocates only its result and its
//!   per-call `Vec<char>` cursor; it makes no network call, performs no I/O, and
//!   uses no `unsafe`. It is `std`-only.
//!
//! **Out of scope for this task** (later [034]-[047] work, deliberately *not*
//! built here): the BM25 index, ranking and boosts, source filtering, response
//! size caps, and cursors. This module produces tokens; it does not index,
//! rank, or serve them.
//!
//! ## Token representation and normalization
//!
//! The output is a `Vec<Token>` where [`Token`] is a small tagged enum. The tag
//! is the token's *kind* — enough information for [034] to apply the
//! "exact guideline-code / command-name / lint-ID matches first" boosts from
//! plan.md §6 without re-parsing:
//!
//! | Variant            | Example                     | Rule                                              |
//! |--------------------|-----------------------------|---------------------------------------------------|
//! | [`Token::Word`]   | `as_`, `into_`, `Send`      | a run of identifier characters `[A-Za-z0-9_]`; underscores (leading/trailing/internal) are part of the identifier, so `as_`/`to_`/`into_` survive whole. |
//! | [`Token::GuidelineCode`] | `C-CONV`, `E-RESCUE` | an identifier run with alnum segments joined by `-`; the hyphen is *not* a separator. |
//! | [`Token::Flag`]   | `--no-default-features`, `-v` | a run of one or more `-` followed by alnum/`-` characters; the hyphens are not separators. |
//! | [`Token::LintId`] | `clippy::unwrap_used`       | identifier segments joined by `::`; the `::` path is not a separator. |
//! | [`Token::Command`]| `cargo test`, `rustup show`| a documented command *verb* (`cargo`, `rustc`, `rustup`, `rustfmt`, `rustdoc`, `clippy`) immediately followed by a subcommand word; the two words are preserved as one phrase token. |
//!
//! **Case is preserved.** `Send` stays `Send` (not lower-cased), because the
//! forms the plan must preserve are case-sensitive Rust identifiers and type
//! names. Case folding and aliasing are ranking concerns owned by [034], not
//! tokenization.
//!
//! **Command phrases** are recognized with a small, explicit, *deterministic*
//! command-verb vocabulary ([`COMMAND_VERBS`]) sourced from the plan's command
//! families (§6). A verb immediately followed by a subcommand *word* becomes one
//! `Command` phrase token; a verb standing alone, or a verb followed by a flag,
//! stays a plain `Word`. The vocabulary is intentionally tiny and is the seam
//! the later command catalog ([039]-[044]) can canonicalize; it is not a
//! free-form dictionary.

// `search` is a private module not yet reached by non-test code ([034]-[047]
// consume `tokenize`/`Token`). Until a caller is wired in, its public items are
// crate-private and unused in the library/binary build, which would trip
// `dead_code`; the `--all-targets` test build uses them, so the allow is scoped
// here and lifted when [034] consumes the tokenizer. Mirrors `model.rs`.
#![allow(dead_code)]

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

#[cfg(test)]
mod tests {
    use super::*;

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
