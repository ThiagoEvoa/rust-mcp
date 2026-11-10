//! The fixed source registry — task [009] M2 (issue #9).
//!
//! This module is the authoritative, *fixed* registry of the eight official
//! Rust documentation source collections declared in `plan.md` §2 ("Documentation
//! sources and interpretation") and §10/M2 ("Register all eight sources, allowed
//! paths/aliases, document formats, and bounded fetching policies"). It answers
//! three questions the whole ingestion and retrieval pipeline depends on:
//!
//! - **Which collections exist?** — [`SourceCollection`] enumerates exactly the
//!   eight approved ids (`api-guidelines`, `style-guide`, `cargo`, `rustc`,
//!   `rustup`, `rustfmt`, `clippy`, `rustdoc`). Anything else is foreign.
//! - **What may each collection fetch?** — each collection owns one or more
//!   [`Boundary`]s that pin the *permitted hosts*, *path prefixes*, *document
//!   formats*, and *parser adapters*. Fetching is bounded to these; nothing
//!   outside a boundary is a registered source.
//! - **Which explicit aliases are allowed?** — the legacy Clippy catalog
//!   redirect (`rust-lang.github.io/rust-clippy/stable/` →
//!   [`SourceCollection::Clippy`]) is registered as an explicit
//!   [`Boundary::Alias`], and the rustfmt static-markdown files
//!   (`README.md`, `Configurations.md` at a resolved commit under
//!   `raw.githubusercontent.com/rust-lang/rustfmt/`) are the *only* rustfmt
//!   fetch boundary.
//!
//! `plan.md` §9/Discovery boundary is reproduced in code so that, as `plan.md`
//! §10 requires, *arbitrary sources are rejected* and *unexpected redirects and
//! unlisted paths are refused* rather than crawled.
//!
//! ## Scope of task [009]
//!
//! [009] owns the **fixed identity and path/host boundaries** — the registry a
//! later task ([`crate::ingest`]'s `[010]`) consults to decide *what it is
//! allowed to fetch*, and `crate::corpus`'s loader (`[029]`-`[030]`) consults to
//! validate a `Source`'s `id`. Per-collection *byte/entry download limits*
//! (the "bounded" part of "bounded fetching") and *redirect resolution at fetch
//! time* are owned by `[010]`; `[009]` establishes the boundary *shape* those
//! limits run against. Nothing here performs a fetch, network call, or file
//! read — it is pure, dependency-free, and offline.
//!
//! ## Consumers
//!
//! The public items are reached by the tests in this module and by the later
//! M2 consumers (ingestion `[010]`, corpus loading `[029]`-`[030]`, and the MCP
//! catalog surface `[048]`-`[059]`). Until those land, each still-unwired item
//! carries its own `#[allow(dead_code)]` (see the per-item transitional notes
//! below); each is lifted as its consumer is wired in.
//!
//! The allows are deliberately *item-scoped* rather than a single module-level
//! `#![allow(dead_code)]`: that mirrors the precedent in [`crate::model`] and
//! [`crate::provenance`], and — critically — any *future* unwired item added to
//! this module re-appears in a dead-code check the moment it is written, instead
//! of being masked by a blanket. Each transitional `#[allow(dead_code)]` comment
//! names the task that wires it, so the last one to land can be removed cleanly.

use crate::model::{Source, SourceId};

// ---------------------------------------------------------------------------
// Registry error
// ---------------------------------------------------------------------------

/// A failure to classify a URL or source id against the fixed registry.
///
/// Every variant carries enough context to be actionable rather than a bare
/// parse error, matching task [004]'s typed-error posture (`plan.md` §9). The
/// variants are the precise failure modes the acceptance criteria hinge on:
/// `UnregisteredHost` / `OutsidePathBoundary` together *reject arbitrary
/// sources* (a foreign host vs. an in-host-but-off-boundary path), while
/// `UnregisteredSource` rejects a `Source` whose id is not one of the eight.
#[allow(dead_code)] // transitional: unwired until [010]/[029]-[030]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// The input is not a parseable `http(s)` URL with a host and path
    /// (wrong scheme, empty/malformed host, or an unsafe host shape).
    MalformedUrl(String),
    /// The host is not one the registry knows. Arbitrary hosts are rejected.
    UnregisteredHost {
        /// The host that was not registered.
        host: String,
    },
    /// The host is registered, but the path is outside *every* path prefix /
    /// path rule that host's boundaries declare. This is how non-book paths
    /// (e.g. the Rust book at `doc.rust-lang.org/book/`) and the un-fetched
    /// rendered rustfmt site are refused.
    OutsidePathBoundary {
        /// The registered host the path was matched against.
        host: String,
        /// The path that fell outside every boundary prefix for that host.
        path: String,
    },
    /// A source id is not one of the eight approved collections.
    UnregisteredSource {
        /// The source id that was not registered.
        id: String,
    },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedUrl(msg) => write!(f, "malformed URL: {msg}"),
            Self::UnregisteredHost { host } => {
                write!(f, "host `{host}` is not a registered source host")
            }
            Self::OutsidePathBoundary { host, path } => write!(
                f,
                "`{path}` on host `{host}` is outside every registered path boundary \
                 (arbitrary source rejected)"
            ),
            Self::UnregisteredSource { id } => write!(
                f,
                "source id `{id}` is not one of the eight approved collections"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

// ---------------------------------------------------------------------------
// The eight collections
// ---------------------------------------------------------------------------

/// One of the eight approved Rust documentation source collections.
///
/// This is the *only* permitted set of source ids. `plan.md` §2 declares the
/// eight; the source-id strings on each variant are the canonical ids that
/// `list_sources` advertises and that a `Source::id` must resolve to. The
/// `id()` of every variant is URI-safe, so it always constructs a
/// [`SourceId`]; the tests assert that invariant so a future id cannot drift
/// into an invalid form.
#[allow(dead_code)] // transitional: unwired until [010]/[029]-[030]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceCollection {
    /// `api-guidelines` — the Rust API Guidelines (`rust-lang.github.io/api-guidelines/`).
    ApiGuidelines,
    /// `style-guide` — the Rust Style Guide (`doc.rust-lang.org/style-guide/`).
    StyleGuide,
    /// `cargo` — The Cargo Book (`doc.rust-lang.org/cargo/`).
    Cargo,
    /// `rustc` — The rustc Book (`doc.rust-lang.org/rustc/`).
    Rustc,
    /// `rustup` — The rustup Book (`rust-lang.github.io/rustup/`).
    Rustup,
    /// `rustfmt` — the official rustfmt Markdown sources (see below).
    Rustfmt,
    /// `clippy` — the Clippy book *and* the per-lint catalog.
    Clippy,
    /// `rustdoc` — The rustdoc Book (`doc.rust-lang.org/rustdoc/`).
    Rustdoc,
}

#[allow(dead_code)] // transitional: these items are wired up by [010]/[029]-[030]
impl SourceCollection {
    /// The eight approved collections, in `plan.md` §2 table order.
    ///
    /// This is the fixed registry: a complete and exhaustive set. No other
    /// `SourceCollection` variant exists.
    pub const ALL: [SourceCollection; 8] = [
        SourceCollection::ApiGuidelines,
        SourceCollection::StyleGuide,
        SourceCollection::Cargo,
        SourceCollection::Rustc,
        SourceCollection::Rustup,
        SourceCollection::Rustfmt,
        SourceCollection::Clippy,
        SourceCollection::Rustdoc,
    ];

    /// The canonical source id for this collection (e.g. `cargo`).
    ///
    /// Always URI-safe, so it always builds a [`SourceId`]; see
    /// [`source_ids_are_uri_safe`](tests::source_ids_are_uri_safe) for the
    /// invariant this property upholds.
    pub fn id(&self) -> &'static str {
        match self {
            SourceCollection::ApiGuidelines => "api-guidelines",
            SourceCollection::StyleGuide => "style-guide",
            SourceCollection::Cargo => "cargo",
            SourceCollection::Rustc => "rustc",
            SourceCollection::Rustup => "rustup",
            SourceCollection::Rustfmt => "rustfmt",
            SourceCollection::Clippy => "clippy",
            SourceCollection::Rustdoc => "rustdoc",
        }
    }

    /// The human-readable title a client would show for this collection.
    pub fn title(&self) -> &'static str {
        match self {
            SourceCollection::ApiGuidelines => "Rust API Guidelines",
            SourceCollection::StyleGuide => "Rust Style Guide",
            SourceCollection::Cargo => "The Cargo Book",
            SourceCollection::Rustc => "The rustc Book",
            SourceCollection::Rustup => "The rustup Book",
            SourceCollection::Rustfmt => "rustfmt reference",
            SourceCollection::Clippy => "Clippy",
            SourceCollection::Rustdoc => "The rustdoc Book",
        }
    }

    /// The primary human entry point advertised for this collection
    /// (`plan.md` §2 "Entry point" column). It is a reference URL a human or
    /// client can open; it is *not* necessarily the fetch boundary
    /// ([`boundaries`](Self::boundaries)) — e.g. rustfmt's rendered config
    /// site requires JavaScript, so its advertised entry point is the GitHub
    /// README while its fetch boundary is the raw Markdown fallback.
    pub fn entry_url(&self) -> &'static str {
        match self {
            SourceCollection::ApiGuidelines => {
                "https://rust-lang.github.io/api-guidelines/about.html"
            }
            SourceCollection::StyleGuide => "https://doc.rust-lang.org/style-guide/index.html",
            SourceCollection::Cargo => "https://doc.rust-lang.org/cargo/",
            SourceCollection::Rustc => "https://doc.rust-lang.org/rustc/",
            SourceCollection::Rustup => "https://rust-lang.github.io/rustup/",
            SourceCollection::Rustfmt => "https://github.com/rust-lang/rustfmt#readme",
            SourceCollection::Clippy => "https://doc.rust-lang.org/clippy/",
            SourceCollection::Rustdoc => "https://doc.rust-lang.org/rustdoc/",
        }
    }

    /// The upstream source repository that *owns* this collection's content
    /// (`docs/LICENSING.md` §2 "Per-source evidence" → "Upstream repo"), as a
    /// `https://github.com/rust-lang/<repo>` URL — the same `repository` form
    /// the [`AttributionRef`](crate::model::AttributionRef) field documents and
    /// the model's own example uses (`https://github.com/rust-lang/rust`).
    ///
    /// This is distinct from [`entry_url`], which is the *rendered* entry point
    /// a human or client opens. Three collections are not separate repositories:
    /// `style-guide`, `rustc`, and `rustdoc` live inside `rust-lang/rust` under
    /// `src/doc/{style-guide,rustc,rustdoc}` on the `master` branch; `clippy`
    /// owns `rust-lang/rust-clippy`; `cargo` owns `rust-lang/cargo`. Branch and
    /// sub-path detail (`master`/`main`, `src/doc/…`) is documented here rather
    /// than in the bare repo URL, matching the model's repo-only convention.
    pub fn source_repo(&self) -> &'static str {
        match self {
            // `rust-lang/api-guidelines` (master)
            SourceCollection::ApiGuidelines => "https://github.com/rust-lang/api-guidelines",
            // `rust-lang/rust`, `src/doc/style-guide/` (master)
            SourceCollection::StyleGuide => "https://github.com/rust-lang/rust",
            // `rust-lang/cargo` (master; MDBook `book/`)
            SourceCollection::Cargo => "https://github.com/rust-lang/cargo",
            // `rust-lang/rust`, `src/doc/rustc` (master)
            SourceCollection::Rustc => "https://github.com/rust-lang/rust",
            // `rust-lang/rustup` (main)
            SourceCollection::Rustup => "https://github.com/rust-lang/rustup",
            // `rust-lang/rustfmt` (main; README.md, Configurations.md)
            SourceCollection::Rustfmt => "https://github.com/rust-lang/rustfmt",
            // `rust-lang/rust-clippy` (main; `clippy_lints/src`)
            SourceCollection::Clippy => "https://github.com/rust-lang/rust-clippy",
            // `rust-lang/rust`, `src/doc/rustdoc` (master)
            SourceCollection::Rustdoc => "https://github.com/rust-lang/rust",
        }
    }

    /// The fixed fetch boundaries for this collection: the permitted hosts,
    /// path prefixes (or, for rustfmt, the approved file set), document
    /// formats, and parser adapters.
    ///
    /// A collection may legitimately span several boundaries. `clippy` has
    /// three: its HTML book on `doc.rust-lang.org`, its per-lint catalog on
    /// `clippy.rust-lang.org`, and the *legacy* catalog alias on
    /// `rust-lang.github.io/rust-clippy/stable/`. `rustfmt` spans
    /// `raw.githubusercontent.com` for the approved static Markdown files.
    ///
    /// Everything a collector may fetch for this source is one of these
    /// boundaries; everything else is refused by [`resolve_url`].
    pub fn boundaries(&self) -> &'static [Boundary] {
        use BoundaryKind::*;
        match self {
            SourceCollection::ApiGuidelines => {
                &const {
                    [Boundary {
                        host: "rust-lang.github.io",
                        path: PathBoundary::Prefix("/api-guidelines/"),
                        format: DocFormat::Html,
                        parser: Parser::Book,
                        kind: Primary,
                        aliased_to: None,
                    }]
                }
            }
            SourceCollection::StyleGuide => {
                &const {
                    [Boundary {
                        host: "doc.rust-lang.org",
                        path: PathBoundary::Prefix("/style-guide/"),
                        format: DocFormat::Html,
                        parser: Parser::Book,
                        kind: Primary,
                        aliased_to: None,
                    }]
                }
            }
            SourceCollection::Cargo => {
                &const {
                    [Boundary {
                        host: "doc.rust-lang.org",
                        path: PathBoundary::Prefix("/cargo/"),
                        format: DocFormat::Html,
                        parser: Parser::Book,
                        kind: Primary,
                        aliased_to: None,
                    }]
                }
            }
            SourceCollection::Rustc => {
                &const {
                    [Boundary {
                        host: "doc.rust-lang.org",
                        path: PathBoundary::Prefix("/rustc/"),
                        format: DocFormat::Html,
                        parser: Parser::Book,
                        kind: Primary,
                        aliased_to: None,
                    }]
                }
            }
            SourceCollection::Rustup => {
                &const {
                    [Boundary {
                        host: "rust-lang.github.io",
                        path: PathBoundary::Prefix("/rustup/"),
                        format: DocFormat::Html,
                        parser: Parser::Book,
                        kind: Primary,
                        aliased_to: None,
                    }]
                }
            }
            SourceCollection::Rustfmt => {
                &const {
                    // `rustfmt`'s only fetch boundary is the *approved static
                    // Markdown files* at a resolved commit, per `plan.md`§2's
                    // "for rustfmt, allow *only* the official repository's README.md
                    // and Configurations.md Markdown files at a resolved commit".
                    // The rendered `rust-lang.github.io/rustfmt/` site requires
                    // JavaScript ("an empty HTML shell is not a successful
                    // ingestion") and is therefore *not* a fetch boundary — it only
                    // survives as the human `entry_url` above.
                    [Boundary {
                        host: "raw.githubusercontent.com",
                        path: PathBoundary::CommitFiles {
                            prefix: "/rust-lang/rustfmt/",
                            files: &["README.md", "Configurations.md"],
                        },
                        format: DocFormat::Markdown,
                        parser: Parser::Rustfmt,
                        kind: StaticSource,
                        aliased_to: None,
                    }]
                }
            }
            SourceCollection::Clippy => {
                &const {
                    [
                        // (1) The Clippy *book*: HTML navigation on the canonical
                        // doc host, parsed like the other books.
                        Boundary {
                            host: "doc.rust-lang.org",
                            path: PathBoundary::Prefix("/clippy/"),
                            format: DocFormat::Html,
                            parser: Parser::Book,
                            kind: Primary,
                            aliased_to: None,
                        },
                        // (2) The per-lint *catalog*: the canonical host
                        // `clippy.rust-lang.org/stable/`, parsed per-lint.
                        Boundary {
                            host: "clippy.rust-lang.org",
                            path: PathBoundary::Prefix("/stable/"),
                            format: DocFormat::LintCatalog,
                            parser: Parser::Clippy,
                            kind: Primary,
                            aliased_to: None,
                        },
                        // (3) The *legacy redirect*: an explicit, source-specific
                        // alias, NOT permission to crawl unrelated paths.
                        Boundary {
                            host: "rust-lang.github.io",
                            path: PathBoundary::Prefix("/rust-clippy/stable/"),
                            format: DocFormat::LintCatalog,
                            parser: Parser::Clippy,
                            kind: Alias,
                            aliased_to: Some(SourceCollection::Clippy),
                        },
                    ]
                }
            }
            SourceCollection::Rustdoc => {
                &const {
                    [Boundary {
                        host: "doc.rust-lang.org",
                        path: PathBoundary::Prefix("/rustdoc/"),
                        format: DocFormat::Html,
                        parser: Parser::Book,
                        kind: Primary,
                        aliased_to: None,
                    }]
                }
            }
        }
    }

    /// The default parser adapter for this collection's primary boundary.
    ///
    /// A collection whose boundaries disagree (e.g. `clippy`'s book vs. its
    /// catalog) exposes the disagreement through [`boundaries`]; this returns a
    /// single sensible default for callers that want one parser without
    /// iterating. It is the parser of the first boundary.
    pub fn primary_parser(&self) -> Parser {
        self.boundaries()
            .first()
            .map(|b| b.parser)
            .unwrap_or(Parser::Book)
    }

    /// Resolve a source id string to its collection, rejecting any id that is
    /// not one of the eight (`plan.md` §10: "arbitrary sources are rejected").
    ///
    /// The eight `id()` strings are themselves URI-safe, so any accepted input
    /// also builds a [`SourceId`]; this maps by exact, case-sensitive match.
    pub fn by_id(id: &str) -> Option<SourceCollection> {
        Self::ALL.iter().copied().find(|c| c.id() == id)
    }

    /// Classify a [`SourceId`] as a registered collection or reject it.
    pub fn resolve_source_id(id: &SourceId) -> Result<SourceCollection, RegistryError> {
        Self::by_id(id.as_str()).ok_or_else(|| RegistryError::UnregisteredSource {
            id: id.as_str().to_string(),
        })
    }

    /// Classify a [`Source`]'s id as a registered collection, or reject the
    /// source. This is the corpus-loader seam (`[029]`-`[030]`): a `Source`
    /// with an unregistered id can never enter the corpus.
    pub fn resolve_source(source: &Source) -> Result<SourceCollection, RegistryError> {
        Self::resolve_source_id(&source.id)
    }
}

// ---------------------------------------------------------------------------
// Boundary model: host, path, format, parser, kind
// ---------------------------------------------------------------------------

/// How a [`Boundary`] matches a URL path.
///
/// Two strategies serve the two shapes `plan.md`§2 distinguishes: the
/// *prefix* strategy for book/navigation/catalog hosts (match any path under a
/// prefix), and the *commit-file* strategy for rustfmt's "approved files only"
/// rule (the path must route through a prefix, then a resolved commit segment,
/// then end at one of a fixed set of file names).
#[allow(dead_code)] // transitional: unwired until [010]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathBoundary {
    /// The path must *start with* this exact prefix.
    ///
    /// Every prefix carries its trailing `/`, so `/cargo/` cannot accidentally
    /// match `/carbon/` and `/rustc/` cannot match `/rustdoc/`.
    Prefix(&'static str),
    /// The path must route through `prefix`, then exactly one
    /// resolved-commit/branch segment, then end at one of `files`.
    ///
    /// This encodes rustfmt's "approved files at a resolved commit" rule:
    /// `raw.githubusercontent.com/rust-lang/rustfmt/<commit>/README.md`. A
    /// missing commit segment, a wrong file name, a wrong repository, or a
    /// doubled path segment all fail — nothing outside the approved two files is
    /// fetchable. The commit segment is validated as a *single, non-empty path
    /// segment* (a resolved SHA, branch name, or tag); it is *not* checked as a
    /// 40-character SHA here, because that stricter form plus the per-collection
    /// byte/entry download limits belong to the fetch layer (task [010]). This
    /// module only pins the boundary shape.
    CommitFiles {
        /// The fixed path prefix (e.g. `/rust-lang/rustfmt/`).
        prefix: &'static str,
        /// The only file names allowed at the tail of the commit segment.
        files: &'static [&'static str],
    },
}

/// The document format a boundary produces.
#[allow(dead_code)] // transitional: unwired until [010]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DocFormat {
    /// Rendered book HTML, discovered through book navigation.
    Html,
    /// A static Markdown file (the rustfmt approved sources).
    Markdown,
    /// The per-lint Clippy catalog, split into individual lint sections.
    LintCatalog,
}

/// The parser adapter a [`Boundary`] selects.
///
/// These name the sub-adapters in `crate::ingest`: `Book` → `ingest::books`,
/// `Rustfmt` → `ingest::rustfmt`, `Clippy` → `ingest::clippy`. Selecting a
/// parser *per boundary* lets a single collection (e.g. `clippy`) route its
/// book and its catalog to different adapters.
#[allow(dead_code)] // transitional: unwired until [010]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Parser {
    /// Book navigation discovery + faithful Markdown extraction (`ingest::books`).
    Book,
    /// The approved rustfmt static-Markdown adapter (`ingest::rustfmt`).
    Rustfmt,
    /// Per-entry split of the Clippy lint catalog (`ingest::clippy`).
    Clippy,
}

/// What role a [`Boundary`] plays in its collection.
#[allow(dead_code)] // transitional: unwired until [010]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoundaryKind {
    /// The collection's normal entry host.
    Primary,
    /// A non-canonical but *explicitly registered* redirect target.
    Alias,
    /// An explicit static-source adapter (the rustfmt raw-Markdown fallback).
    StaticSource,
}

/// A single fetch boundary within a [`SourceCollection`].
///
/// A boundary fully describes one "allowed place to fetch": the exact `host`,
/// the matching `path` rule, the `format` the host serves, the `parser` that
/// ingests it, and its `kind` (with `aliased_to` set only for an
/// [`BoundaryKind::Alias`]).
#[allow(dead_code)] // transitional: unwired until [010]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Boundary {
    /// The exact host this boundary is confined to.
    pub host: &'static str,
    /// The path rule the URL path must satisfy.
    pub path: PathBoundary,
    /// The document format this host serves.
    pub format: DocFormat,
    /// The parser adapter selected for this boundary.
    pub parser: Parser,
    /// The boundary's role.
    pub kind: BoundaryKind,
    /// For [`BoundaryKind::Alias`] boundaries, the collection this redirect
    /// canonicalizes to; `None` otherwise.
    pub aliased_to: Option<SourceCollection>,
}

#[allow(dead_code)] // transitional: unwired until [010]
impl Boundary {
    /// Whether this boundary is an explicit source-specific alias.
    pub fn is_alias(&self) -> bool {
        self.kind == BoundaryKind::Alias
    }
}

// ---------------------------------------------------------------------------
// URL classification
// ---------------------------------------------------------------------------

/// A URL resolved against the registry to its owning collection and boundary.
#[allow(dead_code)] // transitional: unwired until [010]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEntry<'a> {
    /// The collection this URL belongs to.
    pub collection: SourceCollection,
    /// The boundary that matched.
    pub boundary: &'a Boundary,
}

/// Resolve a URL string against the fixed registry.
///
/// Returns the owning [`SourceCollection`] and the matching [`Boundary`], or a
/// [`RegistryError`] when the URL is malformed, on an unregistered host, or
/// registered on the host but outside every path boundary. This is the seam the
/// ingestion layer (`[010]`) consults so that *only* registered sources within
/// their boundaries are ever fetched, and explicit aliases resolve to their
/// canonical collection.
#[allow(dead_code)] // transitional: unwired until [010]
pub fn resolve_url(url: &str) -> Result<ResolvedEntry<'static>, RegistryError> {
    let parsed = split_url(url)?;

    for collection in SourceCollection::ALL {
        for boundary in collection.boundaries() {
            if boundary.host == parsed.host && matches_path(&boundary.path, &parsed.path) {
                return Ok(ResolvedEntry {
                    collection,
                    boundary,
                });
            }
        }
    }

    // No boundary matched. Distinguish "this host is foreign" from "this host is
    // known but the path is off-boundary" so the diagnostic names the real
    // problem (`plan.md`§9: preserve useful context).
    if host_is_registered(&parsed.host) {
        Err(RegistryError::OutsidePathBoundary {
            host: parsed.host,
            path: parsed.path,
        })
    } else {
        Err(RegistryError::UnregisteredHost { host: parsed.host })
    }
}

/// Whether any registered boundary uses this host.
#[allow(dead_code)] // transitional: unwired until [010]
pub fn host_is_registered(host: &str) -> bool {
    SourceCollection::ALL
        .iter()
        .flat_map(|c| c.boundaries())
        .any(|b| b.host == host)
}

// ---------------------------------------------------------------------------
// Dependency-free URL + path matching
// ---------------------------------------------------------------------------

/// A URL stripped to the two components the registry matches on: host and the
/// path *before* any query string or fragment.
#[allow(dead_code)] // transitional: reachable only via resolve_url until [010]
struct ParsedUrl {
    host: String,
    path: String,
}

/// Split an `http(s)` URL into its host and path (query and fragment dropped,
/// since the registry bounds by path prefix only).
///
/// Dependency-free on purpose (`plan.md` §10 adds deps only as needed). The
/// parser is deliberately *strict*: only the scheme, host, and path the eight
/// boundaries use are accepted. Anything with userinfo, an empty host, a
/// control character, or a host whose charset is not the safe
/// `{alnum, '.', '-'}` set is [`RegistryError::MalformedUrl`] — this is what
/// stops host-suffix tricks like `raw.githubusercontent.com.evil.ru`.
#[allow(dead_code)] // transitional: reachable only via resolve_url until [010]
fn split_url(url: &str) -> Result<ParsedUrl, RegistryError> {
    let rest = match url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        Some(remainder) => remainder,
        None => {
            return Err(RegistryError::MalformedUrl(format!(
                "`{url}` is not an http(s) URL"
            )));
        }
    };

    // The authority ends at the first path separator or query/fragment marker.
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];

    // The host is the authority minus an optional `:port`. Reject userinfo
    // (`@`) up front: none of the eight boundaries ever need it, and an
    // attacker-supplied host+userinfo is exactly the shape we must refuse.
    if authority.is_empty() || authority.contains('@') {
        return Err(RegistryError::MalformedUrl(format!("no host in `{url}`")));
    }
    let host = authority
        .split_once(':')
        .map(|(host, _port)| host)
        .unwrap_or(authority);
    if host.is_empty() {
        return Err(RegistryError::MalformedUrl(format!(
            "empty host in `{url}`"
        )));
    }
    // Host charset: alphanumerics, dots, and hyphens only. This rejects
    // underscored or otherwise exotic hosts and — combined with the exact
    // host comparison in `resolve_url` — defeats host-suffix redirections.
    if host
        .bytes()
        .any(|b| !(b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
    {
        return Err(RegistryError::MalformedUrl(format!(
            "host `{host}` is not a registered host shape"
        )));
    }

    // The path runs from the first separator after the authority to the first
    // query/fragment marker (or end). A missing path is the host root "/".
    let tail = &rest[authority_end..];
    let path = match tail {
        t if t.is_empty() || t.starts_with('?') || t.starts_with('#') => "/",
        t => {
            let end = t.find(['?', '#']).unwrap_or(t.len());
            &t[..end]
        }
    };
    Ok(ParsedUrl {
        host: host.to_string(),
        path: path.to_string(),
    })
}

/// Whether a URL path satisfies a path boundary.
#[allow(dead_code)] // transitional: reachable only via resolve_url until [010]
fn matches_path(boundary: &PathBoundary, path: &str) -> bool {
    match boundary {
        PathBoundary::Prefix(prefix) => path.starts_with(prefix),
        PathBoundary::CommitFiles { prefix, files } => {
            // The path starts with the prefix (which ends in `/`), leaving
            // exactly `<commit>/<file>`. A resolved commit/branch identifier is a
            // single path segment (a SHA, a branch like `main`, or a tag); it is
            // *not* validated as a 40-char SHA here — that check, with the
            // per-collection byte/entry download limits, belongs to the fetch
            // layer ([010]). What this boundary guarantees is strict: the URL
            // routes through the approved repo prefix, carries exactly one
            // non-empty, slash-free commit segment, and ends at an approved file.
            let after = match path.strip_prefix(prefix) {
                Some(remainder) => remainder,
                None => return false,
            };
            match after.rsplit_once('/') {
                // Exactly one commit segment precedes the file: it must be
                // non-empty, must itself contain no `/` (which rejects doubled
                // segments like `//main/README.md` or `main//README.md`), and the
                // tail must be one of the approved names.
                Some((commit, file)) => {
                    !commit.is_empty() && !commit.contains('/') && files.contains(&file)
                }
                // No commit segment (e.g. `.../rustfmt/README.md`, the file
                // directly under the prefix) or an empty tail: not fetchable.
                None => false,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Channel, CorpusHash, Release, SnapshotId, Source, SourceId, Timestamp, Url,
    };

    /// A minimal `Source` with a fixed, valid shape and a variable id. Mirrors
    /// the provenance test helper: only the id varies; everything else defaults
    /// so the registry test stays focused on the `id` mapping.
    fn source_with_id(id: &str) -> Source {
        Source {
            id: SourceId::new(id).expect("a test-provided source id"),
            title: "Test source".into(),
            entry_url: Url::new("https://doc.rust-lang.org/cargo/").unwrap(),
            document_urls: vec![],
            last_checked: None,
            fetched_at: Timestamp::new("2026-10-01T00:00:00Z").unwrap(),
            upstream_modified: None,
            content_hash: CorpusHash::compute(b"fixture"),
            release: Release {
                channel: Channel::Unknown,
                documentation_release: None,
                revision: None,
            },
            parser: "registry-test".into(),
        }
    }

    // -- Criterion 1: the registry is exactly the eight approved sources. --

    #[test]
    fn registry_has_exactly_the_eight_sources() {
        assert_eq!(SourceCollection::ALL.len(), 8);
        // Every id is distinct.
        let mut ids: Vec<&str> = SourceCollection::ALL.iter().map(|c| c.id()).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "the eight ids must be distinct");
        // Every id the plan names is present.
        for expected in [
            "api-guidelines",
            "style-guide",
            "cargo",
            "rustc",
            "rustup",
            "rustfmt",
            "clippy",
            "rustdoc",
        ] {
            assert!(
                SourceCollection::ALL.iter().any(|c| c.id() == expected),
                "`{expected}` must be a registered source",
            );
        }
    }

    #[test]
    fn source_ids_are_uri_safe() {
        // Every registry id must build a `SourceId` — a future id literal that
        // slipped a non-URI-safe character would fail here rather than at load.
        for c in SourceCollection::ALL {
            SourceId::new(c.id()).unwrap_or_else(|e| panic!("id {} is not URI-safe: {e}", c.id()));
        }
    }

    #[test]
    fn entry_urls_are_valid_urls() {
        // Advertised entry points must be real http(s) URLs with a host.
        for c in SourceCollection::ALL {
            Url::new(c.entry_url())
                .unwrap_or_else(|e| panic!("entry_url for {} is not a valid URL: {e}", c.id()));
        }
    }

    #[test]
    fn every_collection_has_at_least_one_boundary() {
        for c in SourceCollection::ALL {
            assert!(
                !c.boundaries().is_empty(),
                "{} has no fetch boundaries",
                c.id()
            );
        }
    }

    // -- Criterion 2, part A: the Clippy legacy redirect is an explicit alias. --

    #[test]
    fn clippy_legacy_alias_is_explicit_and_targets_clippy() {
        let clippy = SourceCollection::Clippy;
        let alias = clippy
            .boundaries()
            .iter()
            .find(|b| b.is_alias())
            .expect("clippy must declare exactly one alias boundary");
        assert_eq!(alias.host, "rust-lang.github.io");
        assert_eq!(alias.path, PathBoundary::Prefix("/rust-clippy/stable/"));
        assert_eq!(alias.aliased_to, Some(SourceCollection::Clippy));
        assert_eq!(alias.parser, Parser::Clippy);
        assert_eq!(alias.format, DocFormat::LintCatalog);
    }

    #[test]
    fn clippy_legacy_alias_url_resolves_to_clippy() {
        let resolved =
            resolve_url("https://rust-lang.github.io/rust-clippy/stable/index.html").unwrap();
        assert_eq!(resolved.collection, SourceCollection::Clippy);
        assert!(resolved.boundary.is_alias());
        assert_eq!(resolved.boundary.aliased_to, Some(SourceCollection::Clippy));
    }

    #[test]
    fn clippy_catalog_and_book_are_separate_boundaries() {
        let clippy = SourceCollection::Clippy;
        assert_eq!(
            clippy.boundaries().len(),
            3,
            "book + catalog + legacy alias"
        );
        let book = resolve_url("https://doc.rust-lang.org/clippy/index.html").unwrap();
        assert_eq!(book.boundary.parser, Parser::Book);
        assert_eq!(book.boundary.format, DocFormat::Html);
        let catalog = resolve_url(
            "https://clippy.rust-lang.org/stable/author/this_argument_could_be_a_slice.html",
        )
        .unwrap();
        assert_eq!(catalog.collection, SourceCollection::Clippy);
        assert_eq!(catalog.boundary.parser, Parser::Clippy);
        assert_eq!(catalog.boundary.format, DocFormat::LintCatalog);
        assert!(!catalog.boundary.is_alias());
    }

    // -- Criterion 2, part B: rustfmt's approved files are explicit. --

    #[test]
    fn rustfmt_static_boundary_restricts_the_two_files_at_a_commit() {
        let rustfmt = SourceCollection::Rustfmt;
        let (only, _) = rustfmt.boundaries().split_first().unwrap();
        assert_eq!(only.host, "raw.githubusercontent.com");
        assert_eq!(only.kind, BoundaryKind::StaticSource);
        assert_eq!(only.parser, Parser::Rustfmt);
        assert_eq!(only.format, DocFormat::Markdown);
        match &only.path {
            PathBoundary::CommitFiles { prefix, files } => {
                assert_eq!(*prefix, "/rust-lang/rustfmt/");
                assert_eq!(*files, &["README.md", "Configurations.md"]);
            }
            PathBoundary::Prefix(_) => panic!("rustfmt boundary must be CommitFiles, not a prefix"),
        }
    }

    #[test]
    fn rustfmt_allows_only_the_two_approved_files() {
        assert!(
            resolve_url("https://raw.githubusercontent.com/rust-lang/rustfmt/main/README.md")
                .is_ok()
        );
        assert!(
            resolve_url(
                "https://raw.githubusercontent.com/rust-lang/rustfmt/3a1b2c/Configurations.md"
            )
            .is_ok()
        );
        // A non-approved file under the same commit is refused.
        assert!(
            resolve_url("https://raw.githubusercontent.com/rust-lang/rustfmt/3a1b2c/Other.md")
                .is_err()
        );
    }

    // The rendered `rust-lang.github.io/rustfmt/` site is listed in `plan.md`
    // §2 as an *entry point*, but it requires JavaScript ("an empty HTML shell
    // is not a successful ingestion"). It is therefore NOT a fetch boundary:
    // rustfmt's only fetch boundary is the approved raw Markdown files. This
    // locks that decision in for the `[010]` consumer so it never tries to
    // crawl the rendered, JS-dependent site.
    #[test]
    fn rustfmt_rendered_js_site_is_not_a_fetch_boundary() {
        for url in [
            "https://rust-lang.github.io/rustfmt/",
            "https://rust-lang.github.io/rustfmt/options.html",
            "https://rust-lang.github.io/rustfmt/index.html",
        ] {
            let err = resolve_url(url).unwrap_err();
            assert!(
                matches!(
                    err,
                    RegistryError::OutsidePathBoundary { .. }
                        | RegistryError::UnregisteredHost { .. }
                ),
                "{url} must be refused as a rustfmt fetch boundary, got {err:?}",
            );
        }
        // The approved Markdown sibling *is* accepted, so the refusal is the
        // rendered host, not rustfmt as a whole.
        assert!(
            resolve_url("https://raw.githubusercontent.com/rust-lang/rustfmt/main/README.md")
                .is_ok()
        );
    }

    #[test]
    fn rustfmt_requires_a_resolved_commit_and_the_right_repo() {
        // A file directly under the prefix, with no commit segment, is refused:
        // "approved files *at a resolved commit*".
        let no_commit =
            resolve_url("https://raw.githubusercontent.com/rust-lang/rustfmt/README.md")
                .unwrap_err();
        assert!(matches!(
            no_commit,
            RegistryError::OutsidePathBoundary { .. }
        ));

        // A wrong repository does not match the `/rust-lang/rustfmt/` prefix.
        let wrong_repo =
            resolve_url("https://raw.githubusercontent.com/rustlang/rustfmt/3a1b2c/README.md")
                .unwrap_err();
        assert!(matches!(
            wrong_repo,
            RegistryError::OutsidePathBoundary { .. }
        ));

        // A commit-pinned 40-character SHA also resolves.
        assert!(
            resolve_url(
                "https://raw.githubusercontent.com/rust-lang/rustfmt/\
             0f4c1e2b3d5a6f8b7c9d1e2f4a6b8c0d3e5f7a9b/README.md"
            )
            .is_ok()
        );

        // A doubled path segment is a malformed commit/branch identifier and is
        // refused: the commit segment must be a single, slash-free segment.
        for url in [
            "https://raw.githubusercontent.com/rust-lang/rustfmt//main/README.md",
            "https://raw.githubusercontent.com/rust-lang/rustfmt/main//README.md",
        ] {
            let err = resolve_url(url).unwrap_err();
            assert!(
                matches!(err, RegistryError::OutsidePathBoundary { .. }),
                "{url} has a malformed (doubled) commit segment and must be refused, got {err:?}",
            );
        }
    }

    // -- Criterion 1 (part): every book resolves through its prefix. --

    #[test]
    fn each_book_source_resolves_through_its_prefix() {
        let cases = [
            (
                "https://rust-lang.github.io/api-guidelines/about.html",
                SourceCollection::ApiGuidelines,
            ),
            (
                "https://doc.rust-lang.org/style-guide/index.html",
                SourceCollection::StyleGuide,
            ),
            (
                "https://doc.rust-lang.org/cargo/getting-started/creating-a-project.html",
                SourceCollection::Cargo,
            ),
            (
                "https://doc.rust-lang.org/rustc/basics.html",
                SourceCollection::Rustc,
            ),
            (
                "https://rust-lang.github.io/rustup/overrides.html",
                SourceCollection::Rustup,
            ),
            (
                "https://doc.rust-lang.org/rustdoc/commands.html",
                SourceCollection::Rustdoc,
            ),
        ];
        for (url, want) in cases {
            let resolved = resolve_url(url).unwrap_or_else(|e| panic!("{url} -> {e}"));
            assert_eq!(resolved.collection, want, "{url}");
            assert_eq!(resolved.boundary.format, DocFormat::Html, "{url}");
            assert_eq!(resolved.boundary.parser, Parser::Book, "{url}");
            assert!(!resolved.boundary.is_alias(), "{url} must not be an alias");
        }
    }

    // -- Arbitrary sources are rejected. --

    #[test]
    fn arbitrary_foreign_hosts_are_rejected() {
        for url in [
            "https://example.com/foo",
            "https://docs.rs/serde/1.0/serde",
            "https://static.rust-lang.org/whatever",
        ] {
            let err = resolve_url(url).unwrap_err();
            assert!(
                matches!(err, RegistryError::UnregisteredHost { .. }),
                "unexpected error for {url}: {err}",
            );
        }
        // A wrong repository *under a registered host* is a more precise
        // off-boundary rejection, not an unregistered host; see
        // `rustfmt_requires_a_resolved_commit_and_the_right_repo`.
        let wrong_repo =
            resolve_url("https://raw.githubusercontent.com/rust-lang/other-crate/main/README.md")
                .unwrap_err();
        assert!(matches!(
            wrong_repo,
            RegistryError::OutsidePathBoundary { .. }
        ));
    }

    #[test]
    fn registered_host_but_off_boundary_path_is_rejected() {
        // The Rust book lives on a registered host but under an unregistered
        // prefix; it is refused rather than crawled.
        let rust_book =
            resolve_url("https://doc.rust-lang.org/book/ch01-00/getting-started.html").unwrap_err();
        assert!(matches!(
            rust_book,
            RegistryError::OutsidePathBoundary { .. }
        ));

        // The old clippy host shape is refused too.
        let old_clippy = resolve_url("https://rust-lang.github.io/clippy/index.html").unwrap_err();
        assert!(matches!(
            old_clippy,
            RegistryError::OutsidePathBoundary { .. }
        ));
    }

    #[test]
    fn host_prefix_boundaries_do_not_collide() {
        // A trailing `/` in each prefix stops adjacent prefixes from bleeding
        // into one another.
        assert!(resolve_url("https://doc.rust-lang.org/cargo/").is_ok());
        assert!(resolve_url("https://doc.rust-lang.org/carbon/").is_err());
        assert!(resolve_url("https://doc.rust-lang.org/rustc/").is_ok());
        assert!(resolve_url("https://doc.rust-lang.org/rustdoc/").is_ok());
    }

    #[test]
    fn query_and_fragment_do_not_affect_boundary_matching() {
        assert!(resolve_url("https://doc.rust-lang.org/cargo/?q=test#frag").is_ok());
        let resolved =
            resolve_url("https://doc.rust-lang.org/cargo/getting-started.html#foo").unwrap();
        assert_eq!(resolved.collection, SourceCollection::Cargo);
        // The stored path drops the fragment for boundary purposes.
        assert!(resolved.boundary.path == PathBoundary::Prefix("/cargo/"));
    }

    #[test]
    fn host_suffix_attacks_are_rejected() {
        // A longer host that merely *starts with* a registered host is not that
        // host: exact comparison defeats the suffix trick.
        let err =
            resolve_url("https://raw.githubusercontent.com.evil.ru/rust-lang/rustfmt/3a/README.md")
                .unwrap_err();
        assert!(
            matches!(
                err,
                RegistryError::UnregisteredHost { ref host } if host == "raw.githubusercontent.com.evil.ru"
            ),
            "got {err}",
        );
        // An underscored host fails the host-shape gate.
        assert!(resolve_url("https://doc.rust-lang.org_evil/cargo/").is_err());
    }

    #[test]
    fn malformed_urls_are_rejected() {
        for url in [
            "not-a-url",
            "",
            "ftp://doc.rust-lang.org/cargo/",
            "https://",
            "https:///cargo/",                       // empty host
            "https://d\u{1}oc.rust-lang.org/cargo/", // control char
            "https://user@doc.rust-lang.org/cargo/", // userinfo
            "https://doc.rust-lang.org/",            // empty-ish root on a book host
        ] {
            // The last case is a host with no boundary prefix, so it is
            // OutsidePathBoundary, not MalformedUrl.
            let err = resolve_url(url);
            if url == "https://doc.rust-lang.org/" {
                assert!(
                    matches!(err, Err(RegistryError::OutsidePathBoundary { .. })),
                    "empty root on a registered host is off-boundary: {err:?}",
                );
            } else {
                assert!(
                    matches!(err, Err(RegistryError::MalformedUrl(_))),
                    "expected MalformedUrl for {url:?}, got {err:?}",
                );
            }
        }
    }

    // -- Source id mapping and the `Source` validation seam. --

    #[test]
    fn every_registered_source_id_maps_to_a_collection() {
        for c in SourceCollection::ALL {
            assert_eq!(SourceCollection::by_id(c.id()), Some(c));
            assert_eq!(
                SourceCollection::resolve_source_id(&SourceId::new(c.id()).unwrap()),
                Ok(c),
            );
        }
    }

    #[test]
    fn unregistered_source_id_is_rejected() {
        assert!(SourceCollection::by_id("rust-book").is_none());
        assert!(matches!(
            SourceCollection::resolve_source_id(&SourceId::new("rust-book").unwrap()),
            Err(RegistryError::UnregisteredSource { id }) if id == "rust-book",
        ));
        // Case is significant: the canonical ids are lowercase.
        assert!(SourceCollection::by_id("CARGO").is_none());
    }

    #[test]
    fn resolve_source_accepts_a_registered_source_and_rejects_foreign() {
        assert_eq!(
            SourceCollection::resolve_source(&source_with_id("cargo")),
            Ok(SourceCollection::Cargo),
        );
        assert!(matches!(
            SourceCollection::resolve_source(&source_with_id("rust-book")),
            Err(RegistryError::UnregisteredSource { .. }),
        ));
    }

    // -- Parser / format selection is explicit per boundary. --

    #[test]
    fn parser_selection_matches_expected_adapters() {
        assert_eq!(SourceCollection::Cargo.primary_parser(), Parser::Book);
        assert_eq!(SourceCollection::Rustfmt.primary_parser(), Parser::Rustfmt);
        // Clippy's primary_parser is the *first* boundary's parser (the book),
        // while the catalog boundary selects the Clippy parser.
        assert_eq!(
            SourceCollection::Clippy.boundaries()[0].parser,
            Parser::Book
        );
        assert_eq!(
            SourceCollection::Clippy.boundaries()[1].parser,
            Parser::Clippy
        );
    }

    // -- Error / host registration. --

    #[test]
    fn registered_hosts_are_exactly_the_four() {
        assert!(host_is_registered("doc.rust-lang.org"));
        assert!(host_is_registered("rust-lang.github.io"));
        assert!(host_is_registered("clippy.rust-lang.org"));
        assert!(host_is_registered("raw.githubusercontent.com"));
        assert!(!host_is_registered("example.com"));
        assert!(!host_is_registered(""));
    }

    #[test]
    fn registry_errors_carry_actionable_context() {
        let e = resolve_url("https://example.com/x").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("example.com"), "host context: {msg}");
        let e = resolve_url("https://doc.rust-lang.org/book/x").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("/book/x"), "path context: {msg}");
        assert!(
            msg.contains("arbitrary source rejected"),
            "diagnosis: {msg}"
        );
    }

    // A corpus snapshot id is a URI-safe identity the future corpora loader
    // (`crate::corpus`, `[029]-[030]`) will carry. Assert a real round trip:
    // every URI-safe registry id also builds a `SnapshotId` whose string form
    // is unchanged, and non-URI-safe input is rejected rather than coerced.
    #[test]
    fn snapshot_identity_round_trips_as_uri_safe() {
        for c in SourceCollection::ALL {
            let id = c.id();
            let snap = SnapshotId::new(id).expect("{id} must build a SnapshotId");
            assert_eq!(snap.as_str(), id, "as_str must round-trip {id}");
            // Re-reads through the string back into an id without loss.
            assert_eq!(SourceId::new(snap.as_str()).unwrap().as_str(), id);
        }
        // A non-URI-safe snapshot id is rejected, not silently coerced.
        assert!(SnapshotId::new("snap with space").is_err());
        assert!(SnapshotId::new("").is_err());
    }
}
