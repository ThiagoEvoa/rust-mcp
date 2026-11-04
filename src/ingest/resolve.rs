//! Document-identity resolution for ingestion (task `[011]`, issue #11).
//!
//! This is the *normalizing link-resolution seam* between the registry
//! ([`crate::registry`, `[009]`]) and the bounded fetcher ([`crate::ingest::fetch`,
//! `[010]`]). It answers the two acceptance criteria of issue #11:
//!
//! - **Reject off-source redirects, lookalike path prefixes, and unsupported
//!   schemes.** Every reference a page points at — a relative link, an absolute
//!   link, or the target a redirect *lands on* — is resolved to its normalized,
//!   absolute form and re-gated through [`crate::registry::resolve_url`]. A
//!   foreign host, a registered host carrying an off-boundary path (a lookalike
//!   prefix), or a non-`http(s)` scheme is refused rather than crawled, exactly
//!   as `plan.md` §2/§9 require. Re-gating the *normalized final* URL is what
//!   makes "on every request and redirect" hold: a redirect walk ends at a
//!   normalized URL, and that URL is held to the same boundary as the first hop.
//! - **Relative URLs resolve correctly; requested/final URLs and fragments
//!   remain attributable.** A reference is joined against the page that named it
//!   (RFC 3986 `Url::join`), normalized, and returned together with the raw
//!   reference, the normalized URL, the original anchor, and the owning
//!   boundary — so `plan.md` §5's "record the requested and final URLs" and a
//!   section's "canonical URL including original anchor" are both derivable.
//!
//! ## Why this lives under `ingest`, not `registry`
//!
//! `crate::registry` is deliberately *dependency-free and offline* (it classifies
//! a `&str` by a naive scheme/host/path split, and *drops* the query and fragment
//! for boundary matching). URL *resolution and canonicalization* need the
//! standard-compliant [`reqwest::Url`] (the `url` crate, `[010]`), which the
//! fetch layer already owns. Keeping the boundary *shape* in `registry` and the
//! URL-aware *normalization* here preserves the registry's pure, offline
//! character while reusing its single source of boundary truth.
//!
//! ## No network, no panics
//!
//! [`resolve_document`] performs no fetch: it only *resolves a reference into a
//! normalized, gated identity* (the thing a subsequent fetch would download).
//! Every failure mode is a typed [`ResolveError`] that names the offending
//! reference and the registry's reason — the "no avoidable panic on external
//! input" posture of task `[004]`, applied to links.
//!
//! ## Relationship to the fetcher's per-hop redirect gate
//!
//! Network redirects are *already* boundary-enforced at the fetch layer: the
//! `UrlGate`/`RegistryGate` in [`crate::ingest::fetch`] vets the initial URL and
//! **every** redirect hop against the same `resolve_url` authority
//! (`fetch`'s `out_of_boundary_redirect_is_rejected`,
//! `redirect_chain_exceeding_limit_is_terminal`, and
//! `redirect_loop_is_detected_as_redirectloop` tests).
//! This module does **not** duplicate that — it owns a different concern: the
//! **document-identity normalization** a *discovered link* needs before it can be
//! fetched, namely resolving a relative reference against its page, normalizing
//! (`../` collapse) the result, and preserving the original requested reference and
//! anchor for attribution. A future extraction stage (`[012]+`) is expected to
//! call [`resolve_document`] on links it discovers, then hand the resulting
//! `canonical_url` to the gated fetcher; at that point both halves compose
//! end-to-end. Until then this module carries a transitional `#[allow(dead_code)]`
//! on each unwired item, mirroring the precedent in [`crate::ingest::fetch`].

use crate::registry::{RegistryError, ResolvedEntry, resolve_url};
use reqwest::Url;

/// A failure to resolve — or to gate — a document reference against the registry.
///
/// Every variant carries enough context to be actionable (`plan.md` §9):
/// `MalformedReference` for input that cannot even be joined into a URL,
/// `UnsupportedScheme` for a non-`http(s)` reference or redirect target, and
/// `OutOfBoundary` for a well-formed URL the registry refuses (a foreign host, an
/// off-boundary path, or an off-boundary *lookalike* prefix). `OutOfBoundary`
/// wraps the registry's [`RegistryError`] verbatim so the downstream caller gets
/// the registry's precise classification.
#[allow(dead_code)] // transitional: first consumer is the [012]+ extraction pipeline
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// The reference could not be joined into a URL at all (not parseable as an
    /// absolute URL and not a relative reference the base could resolve).
    MalformedReference {
        /// The reference that failed to resolve.
        reference: String,
        /// The human-readable parse/join reason.
        reason: String,
    },
    /// The resolved (normalized) URL is not an `http`/`https` URL. This refuses a
    /// non-`http(s)` scheme outright — `plan.md` §2 permits only the eight
    /// registered `http(s)` host/path boundaries; `ftp`, `file`, and the like are
    /// not fetchable and must never be crawled.
    UnsupportedScheme {
        /// The resolved URL that carried the offending scheme.
        url: String,
        /// The scheme that was rejected (e.g. `ftp`, `file`).
        scheme: String,
    },
    /// The reference resolved to a well-formed, `http(s)` URL the registry still
    /// refuses: a foreign host, a registered host with an off-boundary path, or a
    /// lookalike prefix. The registry's precise reason is preserved.
    OutOfBoundary {
        /// The resolved URL that fell outside every boundary.
        url: String,
        /// The registry's classification of why it is not fetchable.
        why: RegistryError,
    },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedReference { reference, reason } => {
                write!(f, "cannot resolve reference `{reference}`: {reason}")
            }
            Self::UnsupportedScheme { url, scheme } => {
                write!(
                    f,
                    "refused to resolve `{url}`: scheme `{scheme}` is not http(s) \
                     (only the eight registered http(s) boundaries are fetchable)"
                )
            }
            Self::OutOfBoundary { url, why } => {
                write!(
                    f,
                    "refused to resolve `{url}` (outside every source boundary): {why}"
                )
            }
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OutOfBoundary { why, .. } => Some(why),
            _ => None,
        }
    }
}

/// A document reference resolved to its normalized, registry-gated identity.
///
/// This is the "requested + final + fragment" trio `plan.md` §5/§6 want a
/// section to carry: the raw reference the page named, the normalized absolute
/// URL that reference resolves to (fragment intact), the original anchor, and the
/// registry boundary that owns it. A downstream section builder (`[013]`/`[014]`)
/// can derive a section's `canonical_url` (the
/// [`ResolvedDocument::canonical_url`] field) and its anchor from this without
/// re-deriving them.
#[allow(dead_code)] // transitional: first consumer is the [012]+ extraction pipeline
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDocument {
    /// The reference exactly as the source named it (relative or absolute, with
    /// any fragment it carried). Preserved for attribution — it is not the
    /// normalized form.
    pub requested: String,
    /// The normalized, absolute URL the reference resolves to, with its fragment
    /// intact. This is the identity a later fetch would download and that a
    /// section's `canonical_url` is derived from.
    pub canonical_url: Url,
    /// The original anchor (the fragment of `canonical_url`), when present. Kept
    /// separate so a section builder can store `anchor` and `canonical_url` (which
    /// itself carries the same fragment) without re-parsing.
    pub anchor: Option<String>,
    /// The registry boundary that owns `canonical_url`: its collection and matched
    /// [`Boundary`](crate::registry::Boundary). Present because the resolver only
    /// ever yields in-boundary identities; a consumer reads `entry.collection`
    /// and `entry.boundary` directly.
    pub entry: ResolvedEntry<'static>,
}

/// Resolve a document `reference` (a relative link, an absolute link, or a
/// redirect target) against the page that named it, normalize it, and gate the
/// result through the registry.
///
/// `base` is the URL of the page the `reference` appeared in: a relative
/// `reference` is joined against `base`'s origin and path per RFC 3986; an
/// absolute `reference` ignores `base` and is parsed on its own. Either way the
/// joined URL is the *normalized final* identity; it is then held to the registry
/// boundary, so a foreign host, an off-boundary (lookalike) path, or a non-`http(s)`
/// scheme is refused. The registry is the single source of boundary truth, so the
/// same check that guards every fetch hop (see [`crate::ingest::fetch::RegistryGate`])
/// also guards every link a page exposes.
///
/// The original reference (for attribution) and the original anchor are returned
/// alongside the normalized URL, so a "requested vs final" distinction and a
/// section's "canonical URL including original anchor" are both recoverable
/// (`plan.md` §5/§6).
#[allow(dead_code)] // transitional: first consumer is the [012]+ extraction pipeline
pub fn resolve_document(reference: &str, base: &Url) -> Result<ResolvedDocument, ResolveError> {
    // RFC 3986 resolution: a relative reference joins against `base`; an absolute
    // reference is parsed on its own. `Url::join` does exactly this and normalizes
    // (collapsing `.`/`..`, lowercasing the host, percent-encoding as needed),
    // which is the canonicalization `plan.md` §2 ("Resolve relative links,
    // normalize URLs, deduplicate fragments") calls for.
    let canonical_url = match base.join(reference) {
        Ok(joined) => joined,
        Err(e) => {
            return Err(ResolveError::MalformedReference {
                reference: reference.to_string(),
                reason: e.to_string(),
            });
        }
    };

    // Enforce the scheme at the URL level first: the registry's string split rejects
    // a non-http(s) scheme too, but an explicit check here yields a crisper
    // "unsupported scheme" message than the registry's generic parse error, and it
    // guards the `ftp`/`file` case the `plan.md` §2 boundary is built against.
    match canonical_url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(ResolveError::UnsupportedScheme {
                url: canonical_url.as_str().to_string(),
                scheme: scheme.to_string(),
            });
        }
    }

    // Re-gate the normalized URL through the registry: this is the "on every
    // request and redirect" check. `resolve_url` classifies by host + path and
    // ignores the fragment for matching, so a fragment never affects the boundary
    // decision while the resolved identity below keeps it.
    let entry = match resolve_url(canonical_url.as_str()) {
        Ok(entry) => entry,
        Err(why) => {
            return Err(ResolveError::OutOfBoundary {
                url: canonical_url.as_str().to_string(),
                why,
            });
        }
    };

    // Capture the original anchor before `canonical_url` is moved into the
    // returned document; a section builder derives `anchor` from this without
    // re-parsing the normalized URL.
    let anchor = canonical_url.fragment().map(str::to_string);

    Ok(ResolvedDocument {
        requested: reference.to_string(),
        canonical_url,
        anchor,
        entry,
    })
}

// ---------------------------------------------------------------------------
// Tests: fully offline. No fetch is performed — only RFC 3986 resolution against
// an in-memory base, re-gated through the pure, offline registry.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// A cargo page under a registered boundary that every test joins against.
    fn cargo_base() -> Url {
        Url::parse("https://doc.rust-lang.org/cargo/getting-started/creating-a-project.html")
            .expect("a valid fixture base URL")
    }

    /// A rustup page under a *different* registered host — proves a reference that
    /// escapes its own host is still accepted only when its target is in-boundary.
    fn rustup_base() -> Url {
        Url::parse("https://rust-lang.github.io/rustup/overrides.html").expect("valid fixture")
    }

    // -- Criterion 2: relative URLs resolve and fragments remain attributable --

    #[test]
    fn relative_reference_resolves_against_its_base() {
        let base = cargo_base();
        // `../concepts/dependencies.html` collapses the `..` against the base path,
        // landing under `/cargo/` and thus inside the `cargo` boundary.
        let doc = resolve_document("../concepts/dependencies.html", &base).unwrap();
        assert_eq!(
            doc.entry.collection,
            crate::registry::SourceCollection::Cargo
        );
        assert_eq!(
            doc.canonical_url.as_str(),
            "https://doc.rust-lang.org/cargo/concepts/dependencies.html"
        );
        // The raw reference is preserved for attribution; the normalized URL is not
        // equal to it.
        assert_eq!(doc.requested, "../concepts/dependencies.html");
        assert_ne!(doc.requested, doc.canonical_url.as_str());
        // No fragment was named, so the anchor is `None`.
        assert_eq!(doc.anchor, None);
    }

    #[test]
    fn fragment_is_preserved_as_the_original_anchor() {
        let base = cargo_base();
        // A fragment-only reference resolves to the base with the new anchor, and
        // the anchor is retained both as `anchor` and inside `canonical_url`
        // (`plan.md` §5: "canonical URL including original anchor").
        let doc = resolve_document("#getting-started", &base).unwrap();
        assert_eq!(doc.anchor.as_deref(), Some("getting-started"));
        assert!(
            doc.canonical_url.as_str().ends_with("#getting-started"),
            "fragment must survive in the canonical URL: {}",
            doc.canonical_url
        );
        assert_eq!(
            doc.entry.collection,
            crate::registry::SourceCollection::Cargo
        );
    }

    #[test]
    fn relative_reference_with_a_fragment_resolves_and_keeps_its_anchor() {
        let base = cargo_base();
        let doc = resolve_document("../build/building.html#cargo-build", &base).unwrap();
        assert_eq!(doc.anchor.as_deref(), Some("cargo-build"));
        assert_eq!(
            doc.canonical_url.as_str(),
            "https://doc.rust-lang.org/cargo/build/building.html#cargo-build"
        );
        assert_eq!(
            doc.entry.collection,
            crate::registry::SourceCollection::Cargo
        );
        assert_eq!(doc.requested, "../build/building.html#cargo-build");
    }

    #[test]
    fn absolute_in_boundary_reference_ignores_the_base_but_stays_attributable() {
        let base = rustup_base();
        // An absolute `https` reference resolves on its own; the base is irrelevant,
        // but `requested` still records what the page named.
        let doc = resolve_document(
            "https://doc.rust-lang.org/cargo/commands/index.html#build",
            &base,
        )
        .unwrap();
        assert_eq!(
            doc.entry.collection,
            crate::registry::SourceCollection::Cargo
        );
        assert_eq!(doc.anchor.as_deref(), Some("build"));
        assert_eq!(
            doc.requested,
            "https://doc.rust-lang.org/cargo/commands/index.html#build"
        );
        assert_eq!(
            doc.canonical_url.as_str(),
            "https://doc.rust-lang.org/cargo/commands/index.html#build"
        );
    }

    #[test]
    fn clippy_legacy_alias_resolves_to_clippy_through_the_registry() {
        // The legacy catalog host is an explicit registry *alias*; a reference onto
        // it must resolve and canonicalize to the `clippy` collection.
        let base = Url::parse("https://rust-lang.github.io/rustup/").unwrap();
        let doc = resolve_document(
            "https://rust-lang.github.io/rust-clippy/stable/index.html",
            &base,
        )
        .unwrap();
        assert_eq!(
            doc.entry.collection,
            crate::registry::SourceCollection::Clippy
        );
        assert!(doc.entry.boundary.is_alias());
    }

    // -- Criterion 1: off-source redirects, lookalike prefixes, bad schemes --

    #[test]
    fn off_source_host_is_refused() {
        let base = cargo_base();
        // A foreign host (the archetypal "off-source redirect" target) is refused
        // with the registry's `UnregisteredHost` reason preserved.
        let err = resolve_document("https://evil.example.com/anything", &base).unwrap_err();
        assert!(
            matches!(
                err,
                ResolveError::OutOfBoundary {
                    why: RegistryError::UnregisteredHost { .. },
                    ..
                }
            ),
            "a foreign host must be refused, got {err:?}"
        );
    }

    #[test]
    fn a_relative_escape_off_the_path_boundary_is_refused() {
        // A relative `../` that climbs out of the base's prefix onto a foreign
        // `path` on the same registered host is an off-boundary target. This is
        // the resolution half of AC1's "off-source redirect": `resolve_document`
        // normalizes the escape and defers the refusal to the registry (via
        // `OutsidePathBoundary`); the *actual* multi-hop redirect walk is owned by
        // the fetcher's `RegistryGate` (see the module note above), not here.
        let base = cargo_base();
        let err = resolve_document("../../book/ch01-00/hello.html", &base).unwrap_err();
        assert!(
            matches!(
                err,
                ResolveError::OutOfBoundary {
                    why: RegistryError::OutsidePathBoundary { .. },
                    ..
                }
            ),
            "an off-boundary redirect target must be refused, got {err:?}"
        );
        assert!(
            matches!(err, ResolveError::OutOfBoundary { ref url, .. } if url.contains("/book/")),
            "the offending URL must be named, got {err:?}"
        );
    }

    #[test]
    fn lookalike_path_prefix_is_refused() {
        // `/carbon/` shares a `cargo`-like look but is not a registered prefix, so
        // it is refused on a registered host (a lookalike-prefix redirect).
        let base = cargo_base();
        let err = resolve_document("https://doc.rust-lang.org/carbon/x", &base).unwrap_err();
        assert!(
            matches!(
                err,
                ResolveError::OutOfBoundary {
                    why: RegistryError::OutsidePathBoundary { .. },
                    ..
                }
            ),
            "a lookalike prefix must be refused, got {err:?}"
        );
    }

    #[test]
    fn lookalike_host_suffix_is_refused() {
        // A host that merely *starts with* a registered host is foreign; exact host
        // matching defeats the suffix trick (`plan.md` §9).
        let base = cargo_base();
        let err = resolve_document("https://doc.rust-lang.org.evil.ru/cargo/x", &base).unwrap_err();
        assert!(
            matches!(
                err,
                ResolveError::OutOfBoundary {
                    why: RegistryError::UnregisteredHost { ref host },
                    ..
                } if host == "doc.rust-lang.org.evil.ru"
            ),
            "a host-suffix lookalike must be refused, got {err:?}"
        );
    }

    #[test]
    fn unsupported_schemes_are_refused() {
        let base = cargo_base();
        // An `ftp`/`file`/`gopher` reference yields a normalized URL whose scheme
        // is not `http(s)`; the resolver refuses it with `UnsupportedScheme`
        // (`plan.md` §2 permits only the eight registered `http(s)` boundaries).
        for scheme_url in [
            "ftp://doc.rust-lang.org/cargo/",
            "file:///usr/local/share/rust",
            "gopher://doc.rust-lang.org/cargo/",
        ] {
            let err = resolve_document(scheme_url, &base).unwrap_err();
            let scheme = &scheme_url[..scheme_url.find(':').unwrap()];
            match &err {
                ResolveError::UnsupportedScheme { scheme: got, .. } => {
                    assert_eq!(got, scheme, "{scheme_url} must name its rejected scheme");
                }
                other => panic!("unsupported scheme {scheme_url} -> {other:?}"),
            }
        }
    }

    #[test]
    fn an_http_url_with_an_explicit_port_is_accepted_on_its_scheme_and_path() {
        // `http` is a supported *scheme* so a non-standard port does not trip the
        // scheme check: the registry deliberately ignores the port and matches on
        // host + path, so a port-carrying `http` URL lands in the `cargo` boundary.
        // This pins the intent that only the *scheme* is refused (by
        // `unsupported_schemes_are_refused`), not a port — by asserting all three
        // of: scheme was accepted, host was accepted, boundary matched.
        let base = cargo_base();
        let doc = resolve_document("http://doc.rust-lang.org:8080/cargo/", &base).unwrap();
        assert_eq!(doc.canonical_url.scheme(), "http");
        assert_eq!(doc.canonical_url.host_str(), Some("doc.rust-lang.org"));
        assert_eq!(doc.canonical_url.port(), Some(8080));
        assert_eq!(
            doc.entry.collection,
            crate::registry::SourceCollection::Cargo
        );
    }

    #[test]
    fn a_non_https_http_base_is_still_gated_by_the_registry() {
        // Even when the *base* is `http` (a supported scheme), the resolved URL is
        // held to the registry's path boundary, so an off-prefix target is refused.
        let base = Url::parse("http://doc.rust-lang.org/cargo/getting-started/x.html").unwrap();
        // Two `..` climb out of `/cargo/getting-started/` *and* `/cargo/`, landing
        // at the host root under `/book/` — off every boundary.
        let err = resolve_document("../../book/ch01-00/y.html", &base).unwrap_err();
        assert!(
            matches!(err, ResolveError::OutOfBoundary { .. }),
            "an http (not https) off-boundary target is still refused, got {err:?}"
        );
    }

    #[test]
    fn malformed_reference_is_a_typed_error() {
        // A reference that cannot be joined yields a `MalformedReference` — never a
        // panic. The "no avoidable panic on external input" invariant of task `[004]`
        // is what we pin: the input must be surfaced as a *typed* error whose
        // `reason` names the join failure, not swallowed into an `Ok` arm.
        //
        // `///absolute-looking-but-not/a/path` has no authority component (no
        // `host:path`), so `Url::join` against even a bare-host base fails with a
        // non-empty-host parse error and cannot silently succeed. We assert that
        // specific, deterministic variant rather than a three-arm `match` that no
        // input could route through its other branches.
        let base = Url::parse("https://doc.rust-lang.org/").unwrap();
        let err = resolve_document("///absolute-looking-but-not/a/path", &base);
        let resolve_err = match err {
            Ok(doc) => panic!("a join-less reference must not silently resolve, got {doc:?}"),
            Err(e) => e,
        };
        assert!(
            matches!(
                resolve_err,
                ResolveError::MalformedReference {
                    ref reference,
                    ref reason
                } if reference == "///absolute-looking-but-not/a/path" && !reason.is_empty()
            ),
            "expected a `MalformedReference` naming the offending reference, got {resolve_err:?}"
        );
    }

    #[test]
    fn malformed_reference_is_a_typed_error_with_actionable_reason() {
        // The human-readable `reason` preserves the `url` crate's parse message —
        // enough for a caller to diagnose why a link refused without re-deriving it.
        let base = Url::parse("https://doc.rust-lang.org/").unwrap();
        let resolve_err = resolve_document("///no-host/x", &base)
            .expect_err("an authority-less path must fail to join");
        let ResolveError::MalformedReference { reason, .. } = resolve_err else {
            panic!(
                "an authority-less path must surface as `MalformedReference`, got {resolve_err:?}"
            );
        };
        assert!(
            !reason.trim().is_empty(),
            "a `MalformedReference` must carry a non-empty parse reason for the caller"
        );
    }

    #[test]
    fn resolved_error_messages_carry_actionable_context() {
        let base = cargo_base();
        let msg = resolve_document("https://evil.example.com/x", &base)
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("evil.example.com"),
            "off-source refusal must name the offending host: {msg}"
        );
        let msg = resolve_document("ftp://doc.rust-lang.org/cargo", &base)
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("ftp"),
            "scheme refusal must name the rejected scheme: {msg}"
        );
    }

    #[test]
    fn fragment_does_not_change_the_boundary_verdict() {
        // A `#anchor` must never turn an in-boundary page into an off-boundary one:
        // the same base page is accepted with and without a fragment.
        let base = cargo_base();
        assert!(resolve_document("../build/building.html", &base).is_ok());
        assert!(resolve_document("../build/building.html#anything", &base).is_ok());
    }
}
