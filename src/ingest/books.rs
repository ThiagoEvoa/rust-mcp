#![allow(dead_code)] // transitional: first consumer is the [013]+ extraction stage

//! Book navigation discovery and document-inventory comparison (task `[012]`, issue #12).
//!
//! Answers the two acceptance criteria of issue #12, proven with fully offline
//! loopback fixtures:
//!
//! - **Chapters are discovered structurally, without a hardcoded TOC filename.**
//!   [`discover_inventory`] fetches a collection's navigation page and the
//!   dependency-free [`nav_link_refs`] reads every `<a href>` *from the page's
//!   structure* (rustc book chapters, Cargo's command index, …). Nothing keys on
//!   a literal `TOC.html`/chapter-listing file name; the same extractor returns
//!   the chapter links of pages named *anything*, because it reads the link
//!   structure, not a filename.
//! - **Print pages, assets, and external references are excluded; added,
//!   removed, or unavailable expected documents are reported.** Each discovered
//!   reference is resolved through the registry-gated resolver
//!   ([`crate::ingest::resolve::resolve_document`]) and classified by
//!   [`classify`] over the *resolved URL* (host / path shape / format), driven
//!   by the registry boundary, not a filename allow-list. The result is a
//!   deduplicated [`DocumentInventory`] (identity = fragmentless canonical URL)
//!   diffed against an expected set into an [`InventoryDiff`] that names
//!   `removed` (expected but not found) and `unavailable` (expected but fetched
//!   then 4xx/failed) documents — nothing is silently dropped.
//!
//! A later stage ([`013`]) turns the inventory's document URLs into faithful
//! Markdown sections; discovery itself stops at enumerating those URLs.

use crate::ingest::fetch::BoundClient;
use crate::ingest::resolve::{ResolveError, ResolvedDocument, resolve_document};
use reqwest::Url;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// The exclusion classifier — non-booking material, driven by URL shape
// ---------------------------------------------------------------------------

/// Why a resolved reference is *not* a document the inventory books.
///
/// The categories are the `plan.md` §2/§9 non-document shapes. The decision is a
/// pure classifier over the resolved URL (host / path / format) — it is **not** a
/// filename allow-list: a chapter whose file is named anything but `chapter` is
/// still booked, and a print page / asset / external link is never booked no
/// matter what its file is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exclusion {
    /// A link to a sibling section *within the same document* (a fragment target
    /// with no distinct path): a chapter and its `#anchor` are one document, so
    /// the fragment is not a separate document (`plan.md` §2 "deduplicate
    /// fragments").
    SameDocumentFragment,
    /// A rendered *print* page of a chapter (rustc's `/print/<chapter>.html`,
    /// Cargo's `/print_chapter.html`): a duplicate edition, not a distinct
    /// document. Detected by the `print` path / `-print` suffix shape, not by a
    /// file allow-list.
    PrintEdition,
    /// A non-document asset the source serves (CSS, JS, PNG/SVG/PDF, …). Its
    /// path extension is not an HTML document, so it is excluded by format.
    Asset,
    /// A reference that escaped every registered source boundary — a foreign host,
    /// an off-boundary path, or a non-http(s) scheme. The resolver already refuses
    /// these; this records *why* so an external hyperlink is preserved as "not
    /// crawled" rather than silently dropped (criterion: "external references are
    /// excluded … reported").
    Excluded(String),
}

/// Classify a resolved reference into "book it" or "exclude it" by the URL shape
/// `plan.md` names — print editions, assets, fragment-only siblings, and
/// off-boundary externals — driven by the registry boundary and URL shape, never
/// a hardcoded TOC / chapter filename.
///
/// `resolved` is a reference that the resolver *accepted* (in-boundary, http(s)),
/// so `SameDocumentFragment` is the only "exclude the in-boundary one" branch; an
/// `off_boundary` error (the resolver refused it) is recorded as `Excluded` so the
/// diff can still report it.
fn classify(reference: &str, resolved: &ResolvedDocument) -> Option<Exclusion> {
    let canonical = &resolved.canonical_url;
    let path = canonical.path();
    let path_lc = path.to_ascii_lowercase();

    // A pure anchor jump (`#section`) names a section *within this same page*,
    // not a separate document — so a chapter and its `#anchor` are one document
    // (`plan.md` §2 "deduplicate fragments"). A reference that carries a
    // *distinct path* (e.g. `../concepts/dependencies.html#x`) is a real target
    // document and is booked; only the pure-fragment case is collapsed.
    if reference.starts_with('#') {
        return Some(Exclusion::SameDocumentFragment);
    }

    // Print editions: the rustc book's `/rustc/print/<chapter>.html` and the
    // Cargo book's `print_chapter` page — a duplicate edition of the rendered
    // chapter, not a distinct document. Matched on the print subdirectory
    // segment and the print-edition *leaf shapes* only, so an unrelated path
    // that merely contains "print" (e.g. `rust-print-protocol.html`, or a
    // chapter legitimately about printing) is still booked, not silently
    // dropped (the "nothing silently dropped" risk `plan.md` §2/§4 guard).
    if is_print_edition(&path_lc) {
        return Some(Exclusion::PrintEdition);
    }

    // Non-document assets by path extension: the source serves CSS/JS/PDF/… ,
    // none of which are document bodies. Shape-based, not an allow-list.
    if let Some(name) = path_lc.rsplit('/').next().and_then(|f| f.rsplit_once('.'))
        && matches!(
            name.1,
            "css"
                | "js"
                | "mjs"
                | "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "svg"
                | "ico"
                | "pdf"
                | "webp"
                | "woff"
                | "woff2"
                | "ttf"
        )
    {
        return Some(Exclusion::Asset);
    }

    // Everything else — an HTML document under the boundary — is booked. The
    // off-boundary case never reaches here: the resolver already refused it and
    // `disco…ory` records it as `Exclusion::Excluded`.
    None
}

/// Whether a lowercased canonical path names a *print* edition of a chapter:
/// the print subdirectory (`/print/`) or a print-edition leaf shape
/// (`*-print.html`, `*_print.html`, `print_chapter.html`). Scopes the decision
/// to the path's segments and the leaf filename, so an unrelated path that
/// merely *contains* "print" (e.g. `rust-print-protocol.html`) is not matched.
fn is_print_edition(path_lc: &str) -> bool {
    // A `/print/` segment on the way to a chapter (rustc: `/rustc/print/…`).
    if path_lc.contains("/print/") {
        return true;
    }
    // Match the print-edition shapes on the leaf filename only, not anywhere in
    // the path — a leaf is the substring after the last `/` (or the whole path).
    let leaf = match path_lc.rsplit('/').next() {
        // `rsplit('/').next()` is always `Some` for the empty path, but a
        // leading slash yields an empty trailing field; treat that as "no leaf".
        Some(leaf) if !leaf.is_empty() => leaf,
        _ => return false,
    };
    leaf == "print_chapter.html" || leaf.ends_with("-print.html") || leaf.ends_with("_print.html")
}

/// The fragmentless canonical identity of `url`: scheme + host + path + query,
/// with any `#fragment` removed — the document key, so a chapter and its
/// `#anchor` collapse to one document (`plan.md` §2 "deduplicate fragments").
fn strip_fragment(url: &Url) -> &str {
    // A fragment is the only part of the serialized URL after the path+query, so
    // slicing at the first `#` yields the registry-matched view.
    match url.as_str().split_once('#') {
        Some((before, _fragment)) => before,
        None => url.as_str(),
    }
}

// ---------------------------------------------------------------------------
// The deduplicated document manifest
// ---------------------------------------------------------------------------

/// A deduplicated document inventory of one collection: the set of bookable
/// documents the navigation exposed, identified by fragmentless canonical URL.
///
/// The `urls` reuse the [`crate::model::Source::document_urls`] shape the corpus
/// loader ([`029`]-[`030`]) consumes; `excluded` keeps every reference discovery
/// declined (a print page, an asset, or an external hyperlink) so nothing is
/// silently dropped — `plan.md` §2/§4 require the excluded material be *reported*.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentInventory {
    /// The collection the inventory belongs to (the source id), when known.
    pub collection: Option<String>,
    /// The document URLs that survived discovery + exclusion, order-preserving
    /// and deduplicated by [`key_of`].
    pub urls: Vec<Url>,
    /// The excluded references, each tagged with the reason it was not booked.
    pub excluded: Vec<ExcludedReference>,
}

/// A reference that discovery refused to book, with the reason — so an external
/// hyperlink / print page / asset is *reported*, not dropped (criterion 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExcludedReference {
    /// The reference the source named.
    pub requested: String,
    /// Why it was not booked.
    pub reason: Exclusion,
}

impl DocumentInventory {
    /// The fragmentless canonical identity of `url`: its scheme + host + path +
    /// query, with any `#fragment` removed, so a chapter and its `#anchor` collapse
    /// to one document (`plan.md` §2 "deduplicate fragments"). This is the dedup
    /// key discovery uses.
    pub fn key_of(url: &Url) -> String {
        strip_fragment(url).to_string()
    }

    /// The deduplicated set of fragmentless canonical keys.
    pub fn keys(&self) -> HashSet<String> {
        self.urls.iter().map(Self::key_of).collect()
    }
}

// ---------------------------------------------------------------------------
// The dependency-free navigation link extractor
// ---------------------------------------------------------------------------

/// Read every `<a href>` reference out of a page's HTML, by structure — never by a
/// filename. A chapter is discovered because its link *appears in the page's link
/// structure*, so the same extractor discovers chapters of a rustc book, the
/// Cargo command index, a `TOC.html`, or any page named otherwise.
///
/// The scan is dependency-free (no `scraper`; that belongs to [013] extraction):
/// it walks the HTML byte-for-byte, pulling each `href="…"` attribute value. It is
/// intentionally conservative about what it treats as a link target so the
/// classifier can decide — its job is to *surface candidate references*, not to
/// decide which are documents.
pub fn nav_link_refs(html: &str) -> Vec<String> {
    let bytes = html.as_bytes();
    let lb_lower = html.to_ascii_lowercase();
    let lb = lb_lower.as_bytes();
    let mut refs = Vec::new();
    let mut i = 0usize;
    while i < lb.len() {
        // At each `<`, test whether an `<a…>` anchor tag opens here; if so, read
        // just that tag's first `href` attribute value and record it.
        if start_a_tag(lb, i) {
            // Bound the scan to this one tag: from here to the next unquoted `>`.
            let end = next_tag_end(bytes, i).unwrap_or(bytes.len());
            if let Some(href) = first_href(&bytes[i..end], html.get(i..end)) {
                refs.push(href);
            }
        }
        i += 1;
    }
    refs
}

/// Whether `lb` at `i..` opens an `<a` anchor tag: a `<`, an `a`, then a
/// boundary that is *not* an ASCII letter — so `<a ` / `<a>` / `<a\t` match but
/// `<abbr` / `<anchor` do not. This is the structural test that keys nothing on a
/// filename.
fn start_a_tag(lb: &[u8], i: usize) -> bool {
    if lb[i] != b'<' {
        return false;
    }
    let mut j = i + 1;
    while j < lb.len() && (lb[j] as char) <= '\x20' {
        j += 1;
    }
    // The first non-whitespace byte must be the `a` of `a`.
    if lb.get(j) != Some(&b'a') {
        return false;
    }
    // No byte after `a`, or a non-letter boundary (so `<a>` / `<a ` match but
    // `<abbr` / `<anchor` do not).
    lb.get(j + 1).is_none_or(|b| !b.is_ascii_alphabetic())
}

/// The index just past the current tag's closing `>` (the next `>` not inside a
/// `"` or `'`), so an `href` cannot be read from a later tag. `None` if the tag
/// is malformed (no closing `>`): the caller then falls back to the whole rest.
fn next_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i + 1),
            _ => {}
        }
        i += 1;
    }
    None
}

/// From the bytes of a single `<a…>` tag (`raw` and its lowercase view `lower`
/// cover the same range), return the `href` attribute value if the tag carries
/// one. The word boundary on `href` prevents matching, say, `data-href`; the
/// value is read from its matching quotes or, unquoted, to the next whitespace.
fn first_href(raw: &[u8], lower: Option<&str>) -> Option<String> {
    let lb = lower?.as_bytes();
    let mut i = 0;
    while i + 4 <= lb.len() {
        // Match `href` only when it appears as its own attribute: the bytes
        // before and after are at attribute boundaries, so `data-href` / `hrefx`
        // are not matched.
        let at_start = i == 0 || matches!(lb[i - 1], b' ' | b'\t' | b'\n' | b'\r');
        if at_start && lb[i..i + 4] == b"href"[..] {
            let after = lb.get(i + 4);
            let boundary = match after {
                Some(b) => matches!(b, b'=' | b' ' | b'\t' | b'\n' | b'\r') || i + 4 == lb.len(),
                None => true,
            };
            if boundary {
                let mut k = i + 4;
                while k < lb.len() && (lb[k] == b' ' || lb[k] == b'\t') {
                    k += 1;
                }
                if k < lb.len() && lb[k] == b'=' {
                    k += 1;
                    while k < lb.len() && (lb[k] == b' ' || lb[k] == b'\t') {
                        k += 1;
                    }
                    if k >= lb.len() {
                        return None;
                    }
                    let quote = lb[k];
                    if quote == b'"' || quote == b'\'' {
                        k += 1;
                        let start = k;
                        while k < lb.len() && lb[k] != quote {
                            k += 1;
                        }
                        return Some(decode_entities(&String::from_utf8_lossy(&raw[start..k])));
                    }
                    // Unquoted value: read up to the next whitespace.
                    let start = k;
                    while k < lb.len()
                        && !(lb[k] == b' ' || lb[k] == b'\t' || lb[k] == b'\r' || lb[k] == b'\n')
                    {
                        k += 1;
                    }
                    return Some(decode_entities(&String::from_utf8_lossy(&raw[start..k])));
                }
            }
        }
        i += 1;
    }
    None
}

/// Decode the small set of HTML entities that appear in Rust book `href`s
/// (`&amp;` `&#65;` `&#x41;`); everything else is left intact. Good enough for
/// chapter links, which never use exotic entities.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        match rest.find('&') {
            None => {
                out.push_str(rest);
                break;
            }
            Some(pos) => {
                out.push_str(&rest[..pos]);
                let after = &rest[pos + 1..];
                // A complete `&name;` / `&#NN;` / `&#xNN;` entity decodes.
                if let Some((entity, remainder)) = after.split_once(';')
                    && let Some(decoded) = decode_one(entity)
                {
                    out.push_str(&decoded);
                    rest = remainder;
                    continue;
                }
                // Not a complete or recognized entity: keep the `&` literal and
                // scan past it.
                out.push('&');
                rest = after;
            }
        }
    }
    out
}

/// Decode one named / numeric entity body (the chars between `&` and `;`), or
/// `None` to leave it literal.
fn decode_one(entity: &str) -> Option<String> {
    match entity {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        "nbsp" => Some("\u{00A0}".to_string()),
        other => {
            if let Some(dec) = other
                .strip_prefix("#x")
                .or_else(|| other.strip_prefix("#X"))
            {
                u32::from_str_radix(dec, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
            } else if let Some(dec) = other.strip_prefix('#') {
                dec.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
            } else {
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The inventory builder — the pure core that `discover_inventory` drives
// ---------------------------------------------------------------------------

/// Build a [`DocumentInventory`] from a page's HTML.
///
/// This is the pure, network-free core that [`discover_inventory`] calls after
/// fetching the navigation page. It reads every `<a href>` structurally
/// ([`nav_link_refs`]), resolves each reference against `base_url` through the
/// registry-gated [`resolve_document`], classifies the survivors into documents
/// (kept) vs. non-document material (excluded but *reported*), and deduplicates
/// by fragmentless canonical URL so a chapter and its `#anchor` collapse to one
/// document (`plan.md` §2).
///
/// `collection` names the source (e.g. `"rustc"`, `"cargo"`) for the inventory
/// record.
///
/// Returns the inventory: the booked [`DocumentInventory::urls`] plus the
/// [`DocumentInventory::excluded`] references that were *checked but not booked*
/// so their exclusion is reported, not silently dropped (`plan.md` §4).
pub fn build_inventory(html: &str, base_url: &Url, collection: &str) -> DocumentInventory {
    let mut inventory = DocumentInventory {
        collection: Some(collection.to_string()),
        urls: Vec::new(),
        excluded: Vec::new(),
    };
    let mut seen: HashSet<String> = HashSet::new();

    for reference in nav_link_refs(html) {
        match resolve_document(&reference, base_url) {
            Ok(doc) => {
                // Classify the resolved URL: an exclusion means "report it but
                // don't book it"; `None` means it is a document to (dedup-)book.
                if let Some(reason) = classify(&reference, &doc) {
                    inventory.excluded.push(ExcludedReference {
                        requested: reference,
                        reason,
                    });
                } else {
                    let key = DocumentInventory::key_of(&doc.canonical_url);
                    if seen.insert(key) {
                        inventory.urls.push(doc.canonical_url);
                    }
                }
            }
            Err(err) => {
                // The resolver refused this reference: an off-boundary host, a
                // malformed reference, or an unsupported scheme. Report it as
                // `Excluded` so the caller can see what was *checked*, not just
                // what was booked (`plan.md` §4 — excluded material is reported,
                // not silently dropped).
                let reason = match err {
                    ResolveError::OutOfBoundary { why, .. } => why.to_string(),
                    ResolveError::MalformedReference { reason, .. } => reason,
                    ResolveError::UnsupportedScheme { scheme, .. } => scheme,
                };
                inventory.excluded.push(ExcludedReference {
                    requested: reference,
                    reason: Exclusion::Excluded(reason),
                });
            }
        }
    }

    inventory
}

// ---------------------------------------------------------------------------
// The discovery pipeline
// ---------------------------------------------------------------------------

/// Discover the document inventory of one collection: fetch its navigation page
/// through the gated fetcher, read its `<a href>`s structurally, resolve each
/// through the registry-gated [`resolve_document`], classify out non-document
/// material, and deduplicate by fragmentless canonical URL.
///
/// `base_url` is the collection's navigation page (a
/// [`crate::registry::SourceCollection::entry_url`] in production; a loopback URL
/// in the offline tests). `collection` names the inventory (the source id).
/// `expected` is the set of canonical document URLs the caller knows the
/// collection should expose (its known chapter/command index); it drives the
/// [`InventoryDiff`] — an expected document that was not discovered is `removed`,
/// one that no longer fetches is `unavailable`, and a discovered document the
/// caller did not expect is `added`. Pass `None` to skip the diff.
///
/// Returns the deduplicated [`DocumentInventory`] plus its [`InventoryDiff`].
pub async fn discover_inventory(
    client: &BoundClient,
    base_url: &Url,
    collection: &str,
    expected: Option<&[String]>,
) -> Result<(DocumentInventory, InventoryDiff), DiscoveryError> {
    // Fetch the navigation page through the gated fetcher. A fetch failure is a
    // typed error, never a panic on external input (task `[004]`).
    let page = client
        .fetch(base_url)
        .await
        .map_err(|err| DiscoveryError::Fetch(err.to_string()))?;
    let html = String::from_utf8_lossy(&page.body).into_owned();

    // 1–4: read the links from the page's *structure* (criterion 1 — no TOC
    // filename is ever named), then resolve, re-gate through the registry,
    // classify, and deduplicate.
    let inventory = build_inventory(&html, base_url, collection);

    // 5. Diff against the expected set (when given): name removed / added /
    //    unavailable documents so nothing missing or broken is silent.
    let mut diff = match expected {
        Some(expected) => build_diff(&inventory, expected),
        None => InventoryDiff::default(),
    };

    // An expected document the navigation no longer exposes is either `removed`
    // (the source moved it out of the index, but it still fetches) or
    // `unavailable` (the source no longer serves it — 4xx/5xx/transport). The
    // fetch distinguishes the two.
    let pending = std::mem::take(&mut diff.removed);
    for e in pending {
        let url = match Url::parse(&e) {
            Ok(u) => u,
            Err(err) => {
                diff.unavailable.push(format!("{e} (unparseable: {err})"));
                continue;
            }
        };
        match client.fetch(&url).await {
            Ok(_) => diff.removed.push(e),
            Err(err) => diff.unavailable.push(format!("{e} ({err})")),
        }
    }
    Ok((inventory, diff))
}

/// Build the `added` / `removed` delta of `inventory` against `expected` without
/// any fetch — `added` is a discovered document the caller did not list, and
/// `removed` is an expected document the navigation no longer exposes. The
/// fetch-probing pass in [`discover_inventory`] later splits those un-fetched
/// `removed` documents into `removed` (still fetches) and `unavailable` (does
/// not), so a missing body is reported rather than silently dropped.
fn build_diff(inventory: &DocumentInventory, expected: &[String]) -> InventoryDiff {
    let found = inventory.keys();
    let expected_keys: HashSet<String> = expected
        .iter()
        .filter_map(|e| Url::parse(e).ok())
        .map(|u| DocumentInventory::key_of(&u))
        .collect();

    let mut diff = InventoryDiff::default();

    // `removed`: expected document the navigation did not expose.
    for e in expected {
        let key = match Url::parse(e) {
            Ok(u) => DocumentInventory::key_of(&u),
            Err(_) => continue,
        };
        if !found.contains(&key) {
            diff.removed.push(e.clone());
        }
    }

    // `added`: discovered document the caller did not list.
    for url in &inventory.urls {
        let key = DocumentInventory::key_of(url);
        if !expected_keys.contains(&key) {
            diff.added.push(key);
        }
    }
    diff
}

/// A report of how a discovered inventory differs from its expected set.
///
/// The [`warnings`] fold every `removed` / `unavailable` document into an
/// actionable string, the form [`crate::model::ValidationSummary::warnings`]
/// consumes so a consumer "sees what was checked … without re-validating". An
/// expected document that is missing is never silently dropped — it is named in
/// `removed` (no longer in the navigation) or `unavailable` (no longer serving).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InventoryDiff {
    /// Discovered documents not in the expected set (new chapters/commands).
    pub added: Vec<String>,
    /// Expected documents the navigation no longer exposes, that still fetch.
    pub removed: Vec<String>,
    /// Expected documents that no longer fetch (4xx/5xx/transport or
    /// unparseable), surfaced so a missing body is reported, not dropped.
    pub unavailable: Vec<String>,
}

impl InventoryDiff {
    /// The warnings this diff folds into [`crate::model::ValidationSummary`], one
    /// per removed / unavailable document, each naming the document.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::with_capacity(self.removed.len() + self.unavailable.len());
        for r in &self.removed {
            w.push(format!("expected document not discovered: {r}"));
        }
        for u in &self.unavailable {
            w.push(format!("expected document unavailable (fetch failed): {u}"));
        }
        w
    }

    /// Whether discovery matched its expected set exactly (nothing added, removed,
    /// or unavailable).
    pub fn is_complete(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.unavailable.is_empty()
    }
}

/// A typed failure to discover an inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryError {
    /// The navigation page could not be fetched (the gated fetcher refused or
    /// failed it), with the fetcher's reason.
    Fetch(String),
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fetch(what) => write!(f, "discovery fetch failed: {what}"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

// ---------------------------------------------------------------------------
// Offline tests — pure logic and one loopback integration test
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! The `loopback_client` helper returns `Result<BoundClient, FetchError>`;
    //! `FetchError::Bound` carries a `Url` and is large, mirroring
    //! `ingest::fetch`'s module-level suppression.
    #![allow(clippy::result_large_err)]

    use super::*;
    use crate::ingest::fetch::{BoundClient, LoopbackGate};
    use crate::ingest::http_fixture::{Behavior, FixtureServer};
    use std::sync::Arc;

    use crate::ingest::fetch::FetchPolicy;

    /// A loopback fetch policy with no retries so an unreachable expected URL
    /// fast-fails rather than sleeping.
    fn fast_policy() -> FetchPolicy {
        FetchPolicy {
            request_timeout: std::time::Duration::from_secs(3),
            connect_timeout: std::time::Duration::from_secs(2),
            max_retries: 0,
            transient_backoff: std::time::Duration::from_millis(0),
            retry_after_max: std::time::Duration::from_secs(60),
            max_redirects: 5,
            max_concurrency: 4,
            max_body_bytes: Some(4 * 1024 * 1024),
            user_agent: Some(
                "rust-docs-mcp/0.1 (+https://github.com/ThiagoEvoa/rust-mcp)".to_string(),
            ),
        }
    }

    // `FetchError` is large (136 bytes); this module is transitional and the
    // error type is shared with `ingest::fetch`, which will carry the same
    // suppression until a stable public contract is chosen.
    #[allow(clippy::result_large_err)]
    fn loopback_client() -> Result<BoundClient, crate::ingest::fetch::FetchError> {
        BoundClient::with_gate(fast_policy(), Arc::new(LoopbackGate))
    }

    // -- Criterion 1: chapters discovered without a hardcoded TOC filename ------

    #[test]
    fn inventory_books_relative_chapters_against_registered_base() {
        // The registered `rustc` collection boundary is `doc.rust-lang.org/rustc/`.
        let base = Url::parse("https://doc.rust-lang.org/rustc/index.html").unwrap();
        let html = r#"<html><body>
            <a href="intro.html">intro</a>
            <a href="traits.html">traits</a>
            <abbr>not a link</abbr>
            </body></html>"#;
        let inv = build_inventory(html, &base, "rustc");
        // Both chapters are discovered structurally — no hardcoded TOC filename
        // was named. `intro.html` and `traits.html` resolve to the rustc boundary.
        let keys = inv.keys();
        assert!(
            keys.contains("https://doc.rust-lang.org/rustc/intro.html"),
            "intro.html must be booked, got {keys:?}"
        );
        assert!(
            keys.contains("https://doc.rust-lang.org/rustc/traits.html"),
            "traits.html must be booked, got {keys:?}"
        );
    }

    #[test]
    fn fragment_collapse_deduplicates_chapter_and_anchor() {
        // `intro.html` and `intro.html#section` must collapse to one document.
        let base = Url::parse("https://doc.rust-lang.org/rustc/index.html").unwrap();
        let html = r#"<html><body>
            <a href="intro.html">intro</a>
            <a href="intro.html#section">intro#section</a>
            </body></html>"#;
        let inv = build_inventory(html, &base, "rustc");
        let keys: Vec<String> = inv.keys().into_iter().collect();
        // Only one entry — the fragment is stripped by `key_of`.
        assert_eq!(
            keys.len(),
            1,
            "intro and intro#section must collapse to one document, got {keys:?}"
        );
    }

    #[test]
    fn external_references_are_excluded_and_reported() {
        // `www.rust-lang.org` is not a registered host; it is excluded.
        let base = Url::parse("https://doc.rust-lang.org/rustc/index.html").unwrap();
        let html = r#"<html><body>
            <a href="https://www.rust-lang.org/blog/">blog</a>
            <a href="https://www.rust-lang.org/ann.html">ann</a>
            </body></html>"#;
        let inv = build_inventory(html, &base, "rustc");
        // No documents booked: every reference is off-boundary.
        assert!(
            inv.urls.is_empty(),
            "no in-boundary URLs should be booked, got {:?}",
            inv.urls
        );
        assert_eq!(
            inv.excluded.len(),
            2,
            "both external links must appear in excluded, got {:?}",
            inv.excluded
        );
        // The excluded references are tagged with `Exclusion::Excluded`.
        assert!(
            inv.excluded
                .iter()
                .all(|e| matches!(e.reason, Exclusion::Excluded(_))),
            "all excluded references must be Exclusion::Excluded, got {:?}",
            inv.excluded
        );
    }

    #[test]
    fn print_pages_and_assets_are_excluded_not_booked() {
        let base = Url::parse("https://doc.rust-lang.org/rustc/index.html").unwrap();
        let html = r#"<html><body>
            <a href="intro.html">intro</a>
            <a href="print_chapter.html">print</a>
            <a href="style.css">css</a>
            </body></html>"#;
        let inv = build_inventory(html, &base, "rustc");
        // The print_chapter and css references must be excluded.
        let exclude_reasons: Vec<String> = inv
            .excluded
            .iter()
            .map(|e| match e.reason {
                Exclusion::PrintEdition => "PrintEdition".into(),
                Exclusion::Asset => "Asset".into(),
                Exclusion::Excluded(_) => "Excluded".into(),
                Exclusion::SameDocumentFragment => "SameDocumentFragment".into(),
            })
            .collect();
        assert!(
            exclude_reasons.iter().any(|r| r == "PrintEdition"),
            "print_chapter must be PrintEdition, got {exclude_reasons:?}"
        );
        assert!(
            exclude_reasons.iter().any(|r| r == "Asset"),
            "css must be Asset, got {exclude_reasons:?}"
        );
        // The intro must still be booked.
        assert!(
            inv.urls.iter().any(|u| u.path().contains("intro.html")),
            "intro must be booked, got {:?}",
            inv.urls
        );
    }

    #[test]
    fn print_edition_match_is_scoped_not_substring() {
        // The `PrintEdition` decision must key the leaf / segment, not a bare
        // `contains`, so an unrelated page that merely names "print" is booked,
        // not silently dropped — the "nothing silently dropped" guard.
        let base = Url::parse("https://doc.rust-lang.org/rustc/index.html").unwrap();
        let html = r#"<html><body>
            <a href="rust-print-protocol.html">protocol</a>
            <a href="printing.html">printing</a>
            <a href="chapter-1-print.html">leaf-print-suffix</a>
            <a href="print/intro.html">print-subdir</a>
            <a href="intro.html">intro</a>
            </body></html>"#;
        let inv = build_inventory(html, &base, "rustc");
        let booked: HashSet<&str> = inv.urls.iter().map(|u| u.as_str()).collect();
        // Unrelated "print"-containing pages must still be booked.
        assert!(
            booked.contains("https://doc.rust-lang.org/rustc/rust-print-protocol.html"),
            "an unrelated print-named page must be booked, got {booked:?}"
        );
        assert!(
            booked.contains("https://doc.rust-lang.org/rustc/printing.html"),
            "`printing.html` (about printing) must be booked, got {booked:?}"
        );
        // The genuine print-edition shapes must still be excluded.
        assert!(
            booked
                .iter()
                .all(|u| !u.contains("chapter-1-print.html") && !u.contains("/print/intro.html")),
            "genuine print-edition shapes must be excluded, not booked, got {booked:?}"
        );
        // And a print-edition exclusion reason is actually present.
        assert!(
            inv.excluded
                .iter()
                .any(|e| matches!(e.reason, Exclusion::PrintEdition)),
            "a print-edition exclusion must be recorded, got {:?}",
            inv.excluded
        );
    }

    #[test]
    fn diff_reports_added_removed_and_unavailable() {
        // A navigation that lists only `intro`; the caller expects `chapter2`
        // — which is `removed`. The caller also provides a `chapter1` that was
        // *not* in the navigation — it becomes `added`.
        let base = Url::parse("https://doc.rust-lang.org/rustc/index.html").unwrap();
        // Navigation HTML exposes `intro.html` relative to the base;
        // `build_inventory` resolves it to the canonical rustc URL.
        let html = r#"<html><body><a href="intro.html">intro</a></body></html>"#;
        let inv = build_inventory(html, &base, "rustc");
        // Expected: `intro.html` (discovered) and `chapter2.html` (not discovered).
        let expected = vec![
            "https://doc.rust-lang.org/rustc/intro.html".to_string(),
            "https://doc.rust-lang.org/rustc/chapter2.html".to_string(),
        ];
        let diff = build_diff(&inv, &expected);
        // `intro.html` is in both → not added, not removed.
        assert!(
            !diff.added.iter().any(|a| a.contains("intro")),
            "intro is discovered so it must not be added, got added={:?}",
            diff.added
        );
        // `chapter2.html` was expected but not found in the navigation → removed.
        assert!(
            diff.removed.iter().any(|r| r.contains("chapter2")),
            "chapter2 must be removed (expected but not discovered), got removed={:?}",
            diff.removed
        );
    }

    #[test]
    fn diff_warnings_fold_removed_and_unavailable() {
        let mut d = InventoryDiff::default();
        d.removed.push("http://h/rustc/gone.html".into());
        d.unavailable.push("http://h/rustc/gone2.html (404)".into());
        let w = d.warnings();
        assert_eq!(w.len(), 2, "one warning per removed/unavailable");
        assert!(
            w.iter().all(|x| x.starts_with("expected document")),
            "every warning must start with 'expected document', got {w:?}"
        );
    }

    #[test]
    fn is_complete_is_false_with_any_delta() {
        let d = InventoryDiff {
            added: vec!["x".into()],
            removed: vec![],
            unavailable: vec![],
        };
        assert!(!d.is_complete());
    }

    // -- Loopback integration: fetch + build_inventory end-to-end --------------

    #[tokio::test]
    async fn build_inventory_loopback_books_registered_chapters() {
        // The navigation page is served from a loopback fixture via
        // `FixtureServer`; the HTML contains absolute registered-host links
        // so that `resolve_document` (which gates through the *real* registry)
        // accepts them. `LoopbackGate` allows `127.0.0.1`, so the fetch
        // succeeds offline. This tests the pure builder: fetch the nav page,
        // then hand its HTML to `build_inventory` directly.
        let nav_html = r#"<html><body>
            <a href="https://doc.rust-lang.org/rustc/intro.html">intro</a>
            <a href="https://doc.rust-lang.org/rustc/traits.html">traits</a>
            <a href="https://www.rust-lang.org/blog/">blog</a>
            </body></html>"#;
        let server = FixtureServer::start(vec![(
            "/rustc/index.html".to_string(),
            Behavior::Text {
                body: nav_html.as_bytes().to_vec(),
                etag: Some("\"nav-1\"".to_string()),
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        // `server.addr` is `127.0.0.1:<ephemeral>`; the nav route is `/rustc/…`.
        let base_url = Url::parse(&format!("http://{}/rustc/index.html", server.addr))
            .expect("a valid loopback nav URL");
        // Fetch the nav page through the gate, then call build_inventory on the
        // fetched HTML (the pure core, exercised in isolation).
        let fetched = client
            .fetch(&base_url)
            .await
            .expect("nav fetch succeeds offline");
        let html = String::from_utf8_lossy(&fetched.body).into_owned();
        let inv = build_inventory(&html, &base_url, "rustc");
        let keys = inv.keys();
        assert!(
            keys.contains("https://doc.rust-lang.org/rustc/intro.html"),
            "intro must be booked, got {keys:?}"
        );
        assert!(
            keys.contains("https://doc.rust-lang.org/rustc/traits.html"),
            "traits must be booked, got {keys:?}"
        );
        // The external `www.rust-lang.org` link is excluded.
        let excluded: Vec<&String> = inv.excluded.iter().map(|e| &e.requested).collect();
        assert!(
            excluded.iter().any(|r| r.contains("www.rust-lang.org")),
            "external link must be in excluded, got {excluded:?}"
        );
        assert!(
            inv.excluded
                .iter()
                .all(|e| matches!(e.reason, Exclusion::Excluded(_))),
            "all excluded must be Excluded, got {:?}",
            inv.excluded
        );
    }

    // -- discover_inventory: the public entry point, driven end-to-end ---------

    #[tokio::test]
    async fn discover_inventory_reports_removed_and_unavailable() {
        // End-to-end through the *public* entry point — `discover_inventory`
        // itself, not `build_inventory`. The nav page lists the registered
        // `intro`/`traits` chapters (booked), while `expected` names two loopback
        // documents the navigation does *not* expose: one that still fetches (a
        // moved-out chapter → `removed`) and one that no longer serves (`404` →
        // `unavailable`). This exercises the only path that populates
        // `InventoryDiff::unavailable`, proving criterion 2's "…or unavailable
        // expected documents are reported" rather than asserting a hand-built diff.
        let nav_html = r#"<html><body>
            <a href="https://doc.rust-lang.org/rustc/intro.html">intro</a>
            <a href="https://doc.rust-lang.org/rustc/traits.html">traits</a>
            </body></html>"#;
        // One server, one client: the nav route serves the navigation; the two
        // probe routes are the `expected` documents the diff fetches to split
        // `removed` (still serves) from `unavailable` (no longer serves).
        let server = FixtureServer::start(vec![
            (
                "/rustc/index.html".to_string(),
                Behavior::Text {
                    body: nav_html.as_bytes().to_vec(),
                    etag: Some("\"nav-1\"".to_string()),
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            // A moved-out chapter that still fetches → back to `removed`.
            (
                "/intro.html".to_string(),
                Behavior::Text {
                    body: b"<html><body>still here</body></html>".to_vec(),
                    etag: None,
                    last_modified: None,
                    content_type: Some("text/html".to_string()),
                },
            ),
            // A document the source no longer serves → `unavailable`.
            ("/gone.html".to_string(), Behavior::NotFound),
        ]);
        let client = loopback_client().expect("loopback client builds");
        let addr = server.addr.clone();
        let base_url = Url::parse(&format!("http://{addr}/rustc/index.html"))
            .expect("a valid loopback nav URL");
        // `expected` names documents the navigation did not expose. Loopback
        // hosts pass the `LoopbackGate`, so the probe can fetch them offline;
        // they are absent from the nav HTML, so `build_diff` starts them as
        // `removed` and the probe then splits the two.
        let intro_probe = format!("http://{addr}/intro.html");
        let gone_probe = format!("http://{addr}/gone.html");
        let expected = vec![intro_probe.clone(), gone_probe.clone()];

        let (inv, diff) = discover_inventory(&client, &base_url, "rustc", Some(&expected))
            .await
            .expect("discover_inventory succeeds offline");

        // The navigation still books the registered chapters through the public
        // entry point (the happy path of criterion 1).
        let keys = inv.keys();
        assert!(
            keys.contains("https://doc.rust-lang.org/rustc/intro.html"),
            "intro must be booked, got {keys:?}"
        );
        assert!(
            keys.contains("https://doc.rust-lang.org/rustc/traits.html"),
            "traits must be booked, got {keys:?}"
        );
        // The moved-out chapter that still fetches is `removed` (the probe split
        // it off from `unavailable`).
        assert!(
            diff.removed.iter().any(|r| r.contains("/intro.html")),
            "a still-fetching expected doc must be `removed`, got removed={:?}",
            diff.removed
        );
        // The document that no longer serves is `unavailable` — the headline gap
        // this test closes: the fetch-probe path is now exercised end-to-end.
        assert!(
            diff.unavailable.iter().any(|u| u.contains("/gone.html")),
            "a non-served expected doc must be `unavailable`, got unavailable={:?}",
            diff.unavailable
        );
        // A discovered chapter the caller did not list is `added`, so the diff
        // is not "complete" — nothing is silently dropped.
        assert!(
            !diff.is_complete(),
            "with added/removed/unavailable deltas the diff must not be complete, got {diff:?}"
        );
    }

    #[tokio::test]
    async fn discover_inventory_none_expected_skips_the_diff() {
        // `expected = None` is the diff short-circuit: discovery returns the
        // booked inventory with an empty diff — no `removed`/`unavailable` probe
        // fetches happen.
        let nav_html = r#"<html><body>
            <a href="https://doc.rust-lang.org/rustc/intro.html">intro</a>
            <a href="https://doc.rust-lang.org/rustc/traits.html">traits</a>
            </body></html>"#;
        let server = FixtureServer::start(vec![(
            "/rustc/index.html".to_string(),
            Behavior::Text {
                body: nav_html.as_bytes().to_vec(),
                etag: None,
                last_modified: None,
                content_type: Some("text/html".to_string()),
            },
        )]);
        let client = loopback_client().expect("loopback client builds");
        let addr = server.addr.clone();
        let base_url = Url::parse(&format!("http://{addr}/rustc/index.html"))
            .expect("a valid loopback nav URL");

        let (inv, diff) = discover_inventory(&client, &base_url, "rustc", None)
            .await
            .expect("discover_inventory succeeds offline");

        // The navigation books the registered chapters even with no expected set.
        let keys = inv.keys();
        assert!(
            keys.contains("https://doc.rust-lang.org/rustc/intro.html"),
            "intro must be booked, got {keys:?}"
        );
        // No expected set → an empty diff that is "complete" (no deltas).
        assert_eq!(diff, InventoryDiff::default(), "None → empty diff");
        assert!(diff.is_complete(), "an empty diff is complete");
        assert_eq!(server.request_count(), 1, "only the nav page is fetched");
    }
}
// End of tests
