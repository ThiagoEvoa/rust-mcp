//! Corpus and tool-contract types — task [007] M2.
//!
//! This module defines the **versioned, deterministic** data model that every
//! later M2 milestone builds on:
//!
//! - [`SnapshotManifest`] — the versioned catalog (schema version + the corpus
//!   snapshot it describes),
//! - [`Source`], [`Section`], [`ChecklistEntry`], and [`CommandEntry`] — the
//!   record types,
//! - [`SchemaVersion`] + the load-time gate that **fails clearly on an
//!   unsupported schema version**,
//! - deterministic, **URI-safe** section identities ([`SectionId`]) that
//!   distinguish source, chapter path, anchor, and continuation.
//!
//! ## Scope of task [007]
//!
//! This task owns the *shape* of the records and their serialization contract.
//! The provenance / compatibility / freshness **semantics** — how the
//! `channel`, documentation `release`, `revision`, and stability qualifications
//! are interpreted, compared, and surfaced — are the territory of task [008]
//! (XS, depends on this one); `plan.md` §5 lists those fields here because they
//! live on these records. [007] models them as explicit, unknown-preserving
//! holders so [008] can specialize them (in `crate::provenance`) without
//! re-modeling the records.
//!
//! The eight *source ids* themselves are registered by task [009], the
//! validation of a complete corpus by task [025], and the on-disk file *layout*
//! (a manifest plus side-car bodies) by task [026]. Within [007] the corpus is
//! modeled as a single self-contained [`SnapshotManifest`]; the file split is a
//! storage concern that does not change these record types.
//!
//! ## Trust boundary
//!
//! Per `plan.md` §5/§6/§9 the corpus is *reference data*: upstream excerpts are
//! kept separate from server-authored command metadata, unknown values are
//! preserved explicitly (never guessed), and external input is validated rather
//! than unwrapped. No type here executes anything; command *keys* are validated
//! to reject shell operators so a later catalog can never interpolate input into
//! a process.

// This module is the schema *foundation* for M2: it defines the record types
// and their serialization contract that every later M2 task (provenance [008],
// the source registry [009], corpus loading [029]-[030], the command catalog
// [039]-[044], and the MCP surface [048]-[059]) consumes. Until those consumers
// land, the public items are intentionally un-reached from within this crate,
// which would trip `dead_code`. The allow is lifted incrementally as consumers
// are wired in; it is scoped to this module only.
#![allow(dead_code)]

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

// ---------------------------------------------------------------------------
// Schema versioning
// ---------------------------------------------------------------------------

/// A single supported on-disk snapshot schema version.
///
/// A snapshot records the version it was written under, and the loader rejects
/// any version it does not recognize (see [`SnapshotManifest::from_json_str`])
/// instead of misreading a newer or older layout. Bump this when a *breaking*
/// layout change lands; older versions must continue to be rejected rather than
/// silently reinterpreted.
///
/// Serialized **as an integer** (not a variant name) so a raw `u32` on disk
/// compares directly with the versions the loader supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SchemaVersion {
    /// The first schema. Records carry source, section, checklist, and command
    /// metadata with explicit unknown-provenance fields.
    V1 = 1,
}

impl SchemaVersion {
    /// The version current code reads and writes.
    pub const CURRENT: SchemaVersion = SchemaVersion::V1;

    /// Every version this build understands.
    pub const SUPPORTED: &'static [u32] = &[SchemaVersion::V1 as u32];

    /// The integer encoded on disk.
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Parse a raw on-disk version tag, returning `None` for any version this
    /// build does not support. The load-time gate maps `None` to a clear
    /// [`SchemaError::UnsupportedVersion`].
    pub fn parse(tag: u32) -> Option<SchemaVersion> {
        match tag {
            1 => Some(SchemaVersion::V1),
            other => {
                let _ = other;
                None
            }
        }
    }
}

impl FromStr for SchemaVersion {
    type Err = SchemaError;

    /// Parse a version string (`"1"`), failing clearly on an unknown version.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let tag = s
            .parse::<u32>()
            .map_err(|_| SchemaError::UnknownVersion(format!("`{s}` is not a version integer")))?;
        SchemaVersion::parse(tag).ok_or_else(|| SchemaError::UnsupportedVersion {
            found: tag,
            supported: SchemaVersion::SUPPORTED.to_vec(),
        })
    }
}

// `SchemaVersion` reads from / writes to a raw `u32` on disk. Both directions
// fail with a self-describing message that names the found and supported
// versions, satisfying the "unsupported schema versions fail clearly"
// acceptance criterion whether the gate is reached via `from_json_str` or a
// direct field deserialize.
impl Serialize for SchemaVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.as_u32())
    }
}

impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let tag = u32::deserialize(deserializer)?;
        SchemaVersion::parse(tag).ok_or_else(|| {
            Error::custom(format!(
                "unsupported snapshot schema version {tag} (this build supports {:?})",
                SchemaVersion::SUPPORTED,
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// A source identifier (`api-guidelines`, `cargo`, `clippy`, …).
///
/// The *set* of the eight valid ids is fixed by task [009]; the type itself
/// only enforces the properties every id shares — non-empty and URI-safe — so
/// the [009] registry can be layered on without changing the identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SourceId(String);

impl SourceId {
    /// Build a validated source id, rejecting empty or non-URI-safe input.
    pub fn new(id: impl Into<String>) -> Result<Self, SchemaError> {
        let s = id.into();
        ensure_uri_safe(&s, "source id")?;
        Ok(Self(s))
    }

    /// Borrow the underlying id string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SourceId {
    type Err = SchemaError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for SourceId {
    type Error = SchemaError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        SourceId::new(value)
    }
}

impl From<SourceId> for String {
    fn from(value: SourceId) -> Self {
        value.to_string()
    }
}

/// An opaque corpus snapshot identifier, disambiguating section content across
/// updates (`plan.md` §5: "Snapshot identity disambiguates section content
/// across updates"). Validated as non-empty and URI-safe so it can appear in
/// resource URIs such as `rust-docs://snapshots/{snapshot_id}/commands`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SnapshotId(String);

impl SnapshotId {
    /// Build a validated snapshot id.
    pub fn new(id: impl Into<String>) -> Result<Self, SchemaError> {
        let s = id.into();
        ensure_uri_safe(&s, "snapshot id")?;
        Ok(Self(s))
    }

    /// Borrow the underlying id string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SnapshotId {
    type Err = SchemaError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for SnapshotId {
    type Error = SchemaError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        SnapshotId::new(value)
    }
}

impl From<SnapshotId> for String {
    fn from(value: SnapshotId) -> Self {
        value.to_string()
    }
}

/// A maintainer-authored effect note on a [`CommandEntry`], kept *separate* from
/// upstream documentation per `plan.md` §6 ("Separate upstream excerpts from
/// maintained notes about … possible effects … These notes describe possible
/// effects …; they are not permission controls").
///
/// The `kind` enumerates the effect families named in the plan (source/lockfile
/// changes, artifact writes, dependency downloads, project-code execution,
/// toolchain changes, publication). `detail` carries the human note; it never
/// authorizes the effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectNote {
    /// The effect family this note describes.
    pub kind: EffectKind,
    /// The human-readable note. Describes a *possible* effect, not a
    /// permission.
    pub detail: String,
}

/// The effect families a command or option may have, per `plan.md` §6.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    /// The command writes to the project source tree.
    SourceChange,
    /// The command rewrites `Cargo.lock` / dependency resolution.
    LockfileChange,
    /// The command produces a target/distribution artifact.
    ArtifactWrite,
    /// The command may download dependencies from the network.
    DependencyDownload,
    /// The command compiles and runs the project's own code.
    ProjectCodeExecution,
    /// The command changes the active toolchain or components.
    ToolchainChange,
    /// The command publishes upstream (e.g. `cargo publish`).
    Publication,
}

/// An opaque, deterministic, **URI-safe** section identity.
///
/// A section id is derived (never caller-constructed) from its [`Section`]'s
/// source id, normalized chapter path, original anchor, and continuation part,
/// per `plan.md` §5: "Derive logical section IDs from source ID plus normalized
/// chapter path and original anchor; distinguish continuations deterministically.
/// Return IDs to callers rather than asking them to construct IDs or URLs."
///
/// The grammar keeps every field percent-encoded to URI-safe characters and
/// joins them with structural separators that cannot appear inside an encoded
/// field, so the id is unambiguous:
///
/// ```text
/// <source> / <chapter_path> [ # <anchor> ] [ $c <part>.<total> ]
/// ```
///
/// - `/` separates source from chapter path (chapter-path separators stay
///   internal as `%2F`).
/// - `#` marks an optional encoded anchor (it never appears encoded).
/// - `$c<part>.<total>` marks a deterministic 1-based continuation — both the
///   part and the total parts are emitted so `parse` can recover the pair;
///   it is present only when a document was split into multiple sections.
///
/// Because source, chapter path, anchor, and each continuation part all change
/// the encoded string, two sections that differ in any of those differ in id.
///
/// IDs may be parsed back into their [`SectionIdParts`] with
/// [`SectionId::parse`] for callers that need the components.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SectionId(String);

impl SectionId {
    /// The URI-safe encoded form also used in `rust-docs://.../sections/{id}`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build and validate a section id from its components.
    ///
    /// `continuation` is `None` for an unsplit section; `Some(part, parts)`
    /// (both 1-based, `part <= parts`) appends the `$c<part>.<total>`
    /// discriminator.
    pub fn derive(
        source: &SourceId,
        chapter_path: &str,
        anchor: Option<&str>,
        continuation: Option<(usize, usize)>,
    ) -> Result<Self, SchemaError> {
        if chapter_path.is_empty() {
            return Err(SchemaError::InvalidId(
                "chapter path must be non-empty".to_string(),
            ));
        }
        if let Some((part, parts)) = continuation
            && (part == 0 || part > parts || parts == 0)
        {
            return Err(SchemaError::InvalidId(format!(
                "invalid continuation part {part} of {parts}"
            )));
        }
        let mut s = encode_field(source.as_str());
        s.push('/');
        s.push_str(&encode_field(chapter_path));
        if let Some(anchor) = anchor {
            s.push('#');
            s.push_str(&encode_field(anchor));
        }
        if let Some((part, parts)) = continuation {
            // Both the 1-based part and the total are emitted so `parse` can
            // recover the exact `(part, parts)` pair — the id carries no other
            // place to hide the total.
            s.push_str(&format!("$c{part}.{parts}"));
        }
        // The whole id must round-trip through URI-safe validation: every
        // component field is percent-encoded, so the only raw characters that
        // may survive are the structural separators we just inserted.
        ensure_id_uri_safe(&s)?;
        Ok(Self(s))
    }

    /// Parse a previously derived id back into its components.
    ///
    /// The inverse of [`derive`]. Continuation parts are validated as
    /// `part >= 1`. Returns [`SchemaError::InvalidId`] for any string that is
    /// not a well-formed, URI-safe section id.
    pub fn parse(s: &str) -> Result<SectionIdParts, SchemaError> {
        ensure_id_uri_safe(s)?;
        // Encoded fields percent-escape `/`, `#`, and `$`, so those characters
        // survive raw *only* as the structural separators `derive` inserts: the
        // single `/` between source and chapter, the `#` opening an anchor, and
        // the `$c` opening a continuation. The earliest of them closes the
        // chapter, so the partitions are unambiguous.
        let (source_enc, rest) = s.split_once('/').ok_or_else(|| {
            SchemaError::InvalidId(
                "section id is missing the `/` separating source from chapter path".to_string(),
            )
        })?;
        // A raw `/` anywhere in the remainder is illegal: the chapter, anchor,
        // and continuation each carry their own `/` as `%2F`.
        if rest.contains('/') {
            return Err(SchemaError::InvalidId(
                "unexpected `/` after the source separator".to_string(),
            ));
        }

        let (chapter_enc, tail) = match rest.find(['#', '$']) {
            Some(pos) => (&rest[..pos], &rest[pos..]),
            None => (rest, ""),
        };
        let chapter_path = decode_field(chapter_enc)?;
        if chapter_path.is_empty() {
            return Err(SchemaError::InvalidId(
                "chapter path must be non-empty".to_string(),
            ));
        }

        let (anchor, continuation_raw) = match tail {
            "" => (None, None),
            // `#` opens the anchor; a following `$c` then opens the continuation.
            t if t.starts_with('#') => {
                let anchor_tail = &t[1..];
                match anchor_tail.split_once("$c") {
                    Some((anchor_enc, cont)) => (Some(decode_field(anchor_enc)?), Some(cont)),
                    None => (Some(decode_field(anchor_tail)?), None),
                }
            }
            // A split, unanchored document: the continuation follows the chapter
            // directly, with no anchor.
            t if t.starts_with("$c") => (None, Some(&t[2..])),
            _ => {
                return Err(SchemaError::InvalidId(
                    "unrecognized section id suffix after chapter path".to_string(),
                ));
            }
        };

        let continuation = match continuation_raw {
            Some(raw) => parse_continuation(raw)?,
            None => None,
        };

        Ok(SectionIdParts {
            source: decode_field(source_enc)?,
            chapter_path,
            anchor,
            continuation,
        })
    }

    /// Reconstruct an id from its parsed components — the inverse of
    /// [`SectionId::parse`].
    pub fn from_parts(parts: &SectionIdParts) -> Result<Self, SchemaError> {
        let source = SourceId::new(parts.source.clone())?;
        Self::derive(
            &source,
            &parts.chapter_path,
            parts.anchor.as_deref(),
            parts.continuation,
        )
    }
}

impl FromStr for SectionId {
    type Err = SchemaError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Route through the same validating gate as `TryFrom<String>` so a bare
        // `parse::<SectionId>()` cannot bypass the URI-safety + source/chapter/
        // anchor/continuation grammar that the rest of the module enforces.
        SectionId::from_parts(&SectionId::parse(s)?)
    }
}

// Serde re-parses an on-disk section id through its *encoded* form, so the gate
// runs the full grammar (URI-safety + the source/chapter/anchor/continuation
// partition) rather than wrapping an arbitrary string.
impl TryFrom<String> for SectionId {
    type Error = SchemaError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        SectionId::from_parts(&SectionId::parse(&value)?)
    }
}

impl From<SectionId> for String {
    fn from(value: SectionId) -> Self {
        value.to_string()
    }
}

impl fmt::Display for SectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The recovered components of a parsed [`SectionId`], per `plan.md` §5.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionIdParts {
    /// The source id (decoded).
    pub source: String,
    /// The normalized chapter path (decoded; internal `/` restored).
    pub chapter_path: String,
    /// The original anchor, when the section is not the document root.
    pub anchor: Option<String>,
    /// `(part, parts)` for a split section, both 1-based; `None` for an
    /// unsplit section.
    pub continuation: Option<(usize, usize)>,
}

/// A section's 1-based continuation part and the total number of parts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Continuation {
    pub part: usize,
    pub parts: usize,
}
// ---------------------------------------------------------------------------
// Provenance, freshness, and compatibility holders
//
// These model the *fields* `plan.md` §5 places on a Source; their *semantics*
// (how `channel`/`release`/`revision`/stability are interpreted and compared,
// and how "unknown" is surfaced) are task [008]'s. They are deliberately modeled
// as explicit, unknown-preserving holders here so [008] can specialize them with-
// out re-modeling the records that carry them. As of [008] those semantics live
// in `crate::provenance` (freshness/time, content identity, toolchain
// compatibility); this module keeps the holders and their on-disk contract.
// ---------------------------------------------------------------------------

/// A captured-document timestamp, stored as an RFC 3339 UTC string.
///
/// `plan.md` §5 distinguishes *download time*, *last-check time*, and *known
/// upstream modification time*. This type is a validated, non-empty string at
/// the [007] layer; `None` on a field means *unknown* and is preserved explicitly
/// rather than guessed. The richer distinctions between the three times, and the
/// RFC-3339 grammar used to compare them, are specialized by `Timestamp::as_instant`
/// in `crate::provenance` ([008]).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Timestamp(String);

impl Timestamp {
    /// Build a timestamp from a string, rejecting empty / non-ASCII input.
    ///
    /// A full RFC 3339 grammar is not validated here (no third-party time crate
    /// is in scope for [007]); RFC-3339 comparison is tightened additively in
    /// `crate::provenance` ([008]) via `Timestamp::as_instant`, so this
    /// constructor stays permissive and no recorded `v1` fixture is disturbed.
    pub fn new(value: impl Into<String>) -> Result<Self, SchemaError> {
        let s = value.into();
        validate_timestamp(&s)?;
        Ok(Self(s))
    }

    /// Borrow the underlying timestamp string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Timestamp {
    type Err = SchemaError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for Timestamp {
    type Error = SchemaError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Timestamp::new(value)
    }
}

impl From<Timestamp> for String {
    fn from(value: Timestamp) -> Self {
        value.to_string()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The distribution channel a documentation release is captured from.
///
/// `plan.md` §5 calls to distinguish `stable`, `nightly-only`, `rolling`, and
/// `unknown`; `crate::provenance::classify_compatibility` ([008]) decides when
/// each applies without guessing. `Unknown` is the
/// default-free explicit "we do not know" value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// A stable-distribution toolchain channel.
    Stable,
    /// A nightly-only feature / tool.
    Nightly,
    /// A rolling / independently versioned channel (e.g. rustup's documentation).
    Rolling,
    /// The channel is not determined from the evidence.
    Unknown,
}

/// The documentation release / upstream revision a source was captured from,
/// with unknown values preserved explicitly. A commit-pinned rustfmt document is
/// *not* automatically a `Stable` release (see `plan.md` §5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    /// The distribution channel the documentation was captured from.
    pub channel: Channel,
    /// The identifier of the documentation release when identifiable
    /// (e.g. a toolchain version); `None` means unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_release: Option<String>,
    /// The upstream source revision when identifiable (e.g. a git commit);
    /// `None` means unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

impl Release {
    /// A release with only its channel known; the release identifier and
    /// revision are explicitly unknown.
    pub fn channel_only(channel: Channel) -> Self {
        Self {
            channel,
            documentation_release: None,
            revision: None,
        }
    }
}

impl Default for Release {
    /// An all-unknown release.
    fn default() -> Self {
        Self::channel_only(Channel::Unknown)
    }
}

/// A URI (HTTP/HTTPS document URL) with light, dependency-free validation.
///
/// Task [011] performs the full source-boundary and redirect checks; [007] only
/// rejects obviously-malformed input so a well-formed corpus cannot silently
/// carry a non-HTTP reference into a URL a client might later fetch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Url(String);

impl Url {
    /// Build a validated URL, rejecting non-http(s) or empty-host input.
    pub fn new(value: impl Into<String>) -> Result<Self, SchemaError> {
        let s = value.into();
        if !is_http_url(&s) {
            return Err(SchemaError::InvalidUri(format!(
                "`{s:?}` is not an http(s) URL with a non-empty host"
            )));
        }
        Ok(Self(s))
    }

    /// Borrow the underlying URL string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Url {
    type Err = SchemaError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for Url {
    type Error = SchemaError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Url::new(value)
    }
}

impl From<Url> for String {
    fn from(value: Url) -> Self {
        value.to_string()
    }
}

/// A content hash identifying captured content.
///
/// Per `plan.md` §5 a content hash identifies captured *content*; it is not a
/// compiler version or proof of an upstream publication date. The 128-bit dual
/// accumulator below is a deterministic, dependency-free stand-in whose *role*
/// (identity of the captured bytes) is all [007] requires; task [025] may swap
/// the algorithm when it adds snapshot integrity checks. It is **not** the
/// FNV-1a algorithm — the prefix `c128-` simply denotes a 128-bit content hash;
/// task [025] should adopt a standardized cryptographic hash (e.g. SHA-256).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CorpusHash(String);

impl CorpusHash {
    const PREFIX: &str = "c128-";

    /// Compute the content hash of `bytes` deterministically.
    pub fn compute(bytes: impl AsRef<[u8]>) -> Self {
        let mut hi: u64 = 0xcbf2_9ce4_8422_2325;
        let mut lo: u64 = 0xffa5_3c27_52a3_4b71;
        const PRIME_64: u64 = 0x0000_0100_0000_01b1;
        for &b in bytes.as_ref() {
            lo = (lo ^ (b as u64).wrapping_mul(PRIME_64)).wrapping_add(0x85eb_c6d3);
            hi = (hi ^ (b as u64)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self(format!("{}{:016x}{:016x}", Self::PREFIX, lo, hi,))
    }

    /// Borrow the hash string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CorpusHash {
    type Error = SchemaError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(SchemaError::InvalidField {
                field: "content hash".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        // `TODO([025])`: [007] only requires a non-empty hash; the
        // `c128-` prefix that `compute` emits (and a recomputation against the
        // captured bytes) are integrity checks deferred to task [025]. Until
        // then any non-empty string loads, which is why the recorded fixtures
        // carry decorative hashes rather than `compute` output.
        Ok(Self(value))
    }
}

impl From<CorpusHash> for String {
    fn from(h: CorpusHash) -> String {
        h.0
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// A single approved documentation source and where its documents came from.
///
/// `plan.md` §5: "source ID, title, entry URL, discovered document URLs, last
/// successful check time, fetched time, content hash, channel, documentation
/// release when identifiable, upstream revision, and parser adapter. Preserve
/// unknown values explicitly."
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// The source id (one of the eight approved registry ids; task [009]).
    pub id: SourceId,
    /// The human title of the source collection.
    pub title: String,
    /// The document's entry point URL.
    pub entry_url: Url,
    /// The discovered document URLs of this source's collection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_urls: Vec<Url>,
    /// The last successful upstream check time; `None` means unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<Timestamp>,
    /// When this content was downloaded.
    pub fetched_at: Timestamp,
    /// The known upstream modification time, when available; `None` means
    /// unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_modified: Option<Timestamp>,
    /// Hash identifying the captured content of this source.
    pub content_hash: CorpusHash,
    /// The documentation release / channel / revision this was captured from;
    /// unknown values are preserved via [`Release`].
    pub release: Release,
    /// The parser adapter used to ingest this source.
    pub parser: String,
}

/// A logical documentation section with preserved examples and citations.
///
/// `plan.md` §5: "section ID, source ID, document title, heading path, canonical
/// URL including original anchor, Markdown body, guideline codes/command
/// keys/lint IDs when present, stability/version qualifications, and content
/// hash."
// `TODO([025])`: on load, `Section` deserializes `id`, `source_id`,
// `chapter_path`, `anchor`, and `continuation` independently, so a corrupt
// corpus could carry an `id` that does not match
// `SectionId::derive(source_id, chapter_path, anchor, continuation)`. Likewise
// a deserialized `content_hash` is accepted without recomputation. Both
// cross-checks (id-vs-fields and hash-vs-content) are deferred to task [025],
// which validates a complete corpus; until then the recorded fixtures carry
// hand-written, decorative content hashes that only clear the non-empty gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    /// The deterministic, URI-safe section identity.
    pub id: SectionId,
    /// The source this section belongs to.
    pub source_id: SourceId,
    /// The document's title.
    pub doc_title: String,
    /// The normalized chapter path this section was split from (the component
    /// used to build [`id`]).
    pub chapter_path: String,
    /// The original anchor the section was split from, when present (used to
    /// build [`id`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// The heading hierarchy breadcrumb, most-to-least specific.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    /// The canonical URL, including the original anchor.
    pub canonical_url: Url,
    /// The normalized Markdown body, with examples and links preserved.
    pub body: String,
    /// API guideline codes present in this section (e.g. `C-CONV`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guideline_codes: Vec<String>,
    /// Command keys this section documents (e.g. `cargo test`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_keys: Vec<String>,
    /// Clippy lint IDs present in this section (e.g. `clippy::unwrap_used`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lint_ids: Vec<String>,
    /// Stability/version qualifications, preserved as supplied; unknown
    /// qualifications are kept verbatim rather than guessed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifications: Vec<String>,
    /// Hash identifying this section's content.
    pub content_hash: CorpusHash,
    /// The continuation part of a split section; `None` for an unsplit
    /// section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Continuation>,
}

impl Section {
    /// Recompute this section's [`content_hash`](Section::content_hash) over its
    /// current content fields.
    ///
    /// [`SectionBuilder::build`] fixes the hash over the content *as built*. A
    /// field set after `build` — the API-guidelines stage tags each section with
    /// the `(C-…)` code its heading carries, via `guideline_codes` — would
    /// otherwise leave the hash describing the *pre-tag* content, so two
    /// sections that differ only by `guideline_codes` would be indistinguishable
    /// by hash. Call this once after such a mutation so the hash describes the
    /// section's final content (the hash is the identity of the captured bytes).
    pub fn recompute_content_hash(&mut self) {
        self.content_hash = CorpusHash::compute(section_content(self));
    }
}

/// A fluent builder for [`Section`], deriving the section's [`SectionId`] and
/// [`CorpusHash`] from the supplied components.
///
/// [`SectionBuilder::new`] takes the five fields every section must carry
/// (`source_id`, `doc_title`, `chapter_path`, `canonical_url`, `body`); the
/// remaining fields default to empty/`None` (no anchor, an empty heading path,
/// empty `guideline_codes` / `command_keys` / `lint_ids` / `qualifications`
/// lists, and no continuation) and are set fluently.
///
/// Unlike the previous positional `Section::build` — which took twelve
/// positional arguments, four of them identical `Vec<String>` slots a caller
/// could silently swap — every list here is named, so no caller can swap one
/// for another. `build` returns `Result<_, SchemaError>` because it derives the
/// [`SectionId`] (which validates any [`Continuation`]) and the [`CorpusHash`];
/// an invalid continuation surfaces as a typed error rather than a panic.
#[derive(Clone, Debug)]
pub struct SectionBuilder {
    source_id: SourceId,
    doc_title: String,
    chapter_path: String,
    anchor: Option<String>,
    heading_path: Vec<String>,
    canonical_url: Url,
    body: String,
    guideline_codes: Vec<String>,
    command_keys: Vec<String>,
    lint_ids: Vec<String>,
    qualifications: Vec<String>,
    continuation: Option<Continuation>,
}

impl SectionBuilder {
    /// Start a builder from the five fields every section must carry; every
    /// other field defaults to empty/`None` and is set with the fluent methods
    /// below.
    pub fn new(
        source_id: SourceId,
        doc_title: impl Into<String>,
        chapter_path: impl Into<String>,
        canonical_url: Url,
        body: impl Into<String>,
    ) -> Self {
        Self {
            source_id,
            doc_title: doc_title.into(),
            chapter_path: chapter_path.into(),
            anchor: None,
            heading_path: Vec::new(),
            canonical_url,
            body: body.into(),
            guideline_codes: Vec::new(),
            command_keys: Vec::new(),
            lint_ids: Vec::new(),
            qualifications: Vec::new(),
            continuation: None,
        }
    }

    /// Set the original anchor the section was split from.
    pub fn anchor(mut self, anchor: impl Into<String>) -> Self {
        self.anchor = Some(anchor.into());
        self
    }

    /// Set the heading-hierarchy breadcrumb, most-to-least specific.
    pub fn heading_path(
        mut self,
        heading_path: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.heading_path = heading_path.into_iter().map(Into::into).collect();
        self
    }

    /// API guideline codes present in this section (e.g. `C-CONV`).
    pub fn guideline_codes(mut self, codes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.guideline_codes = codes.into_iter().map(Into::into).collect();
        self
    }

    /// Command keys this section documents (e.g. `cargo test`).
    pub fn command_keys(mut self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.command_keys = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Clippy lint IDs present in this section (e.g. `clippy::unwrap_used`).
    pub fn lint_ids(mut self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.lint_ids = ids.into_iter().map(Into::into).collect();
        self
    }

    /// Stability/version qualifications, preserved as supplied.
    pub fn qualifications(
        mut self,
        qualifications: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.qualifications = qualifications.into_iter().map(Into::into).collect();
        self
    }

    /// Mark this section as a continuation of a split section.
    ///
    /// `part` / `parts` are both 1-based and `part <= parts`; an invalid pair
    /// is rejected by [`SectionBuilder::build`].
    pub fn continuation(mut self, continuation: Continuation) -> Self {
        self.continuation = Some(continuation);
        self
    }

    /// Derive and return the [`Section`], validating the continuation and
    /// computing the [`CorpusHash`].
    pub fn build(self) -> Result<Section, SchemaError> {
        // `derive` validates the continuation (rejecting a part that exceeds the
        // total or a 0 total), so an invalid continuation surfaces as a typed
        // error rather than being silently dropped.
        let cont = self.continuation.as_ref().map(|c| (c.part, c.parts));
        let id = SectionId::derive(
            &self.source_id,
            self.chapter_path.as_str(),
            self.anchor.as_deref(),
            cont,
        )?;
        let content_hash = CorpusHash::compute(section_content(&Section {
            id: id.clone(),
            source_id: self.source_id.clone(),
            doc_title: self.doc_title.clone(),
            chapter_path: self.chapter_path.clone(),
            anchor: self.anchor.clone(),
            heading_path: self.heading_path.clone(),
            canonical_url: self.canonical_url.clone(),
            body: self.body.clone(),
            guideline_codes: self.guideline_codes.clone(),
            command_keys: self.command_keys.clone(),
            lint_ids: self.lint_ids.clone(),
            qualifications: self.qualifications.clone(),
            content_hash: CorpusHash::compute(b""),
            continuation: self.continuation.clone(),
        }));
        Ok(Section {
            id,
            source_id: self.source_id,
            doc_title: self.doc_title,
            chapter_path: self.chapter_path,
            anchor: self.anchor,
            heading_path: self.heading_path,
            canonical_url: self.canonical_url,
            body: self.body,
            guideline_codes: self.guideline_codes,
            command_keys: self.command_keys,
            lint_ids: self.lint_ids,
            qualifications: self.qualifications,
            content_hash,
            continuation: self.continuation,
        })
    }
}

/// The canonical content bytes a [`CorpusHash`] is computed over: every content
/// field except the id, the hash itself, and the structural continuation
/// metadata.
fn section_content(s: &Section) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(s.source_id.as_str().as_bytes());
    out.push(0);
    out.extend_from_slice(s.doc_title.as_bytes());
    out.push(0);
    out.extend_from_slice(s.chapter_path.as_bytes());
    out.push(0);
    out.extend_from_slice(s.anchor.as_deref().unwrap_or("").as_bytes());
    out.push(0);
    for h in &s.heading_path {
        out.extend_from_slice(h.as_bytes());
        out.push(0x1f);
    }
    out.push(0);
    out.extend_from_slice(s.canonical_url.as_str().as_bytes());
    out.push(0);
    out.extend_from_slice(s.body.as_bytes());
    out.push(0);
    for field in [
        &s.guideline_codes,
        &s.command_keys,
        &s.lint_ids,
        &s.qualifications,
    ] {
        for v in field {
            out.extend_from_slice(v.as_bytes());
            out.push(0x1f);
        }
        out.push(0);
    }
    out
}

/// A single API-guideline checklist entry.
///
/// `plan.md` §5: "upstream code, title, category, checklist location, and
/// explanatory section IDs."
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecklistEntry {
    /// The upstream guideline code (e.g. `C-CONV`).
    pub code: String,
    /// The entry's title.
    pub title: String,
    /// The upstream checklist category this entry belongs to.
    pub category: String,
    /// The entry's location within the checklist (a breadcrumb / anchor).
    pub location: String,
    /// The section IDs that explain this entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub explanation_sections: Vec<SectionId>,
}

/// A source-backed command guidance entry.
///
/// `plan.md` §6: "cited syntax, relevant option sections and examples, known
/// prerequisites, stability/version qualifications, plus separately labeled
/// maintainer-authored effect notes." Upstream excerpts (syntax/examples) are
/// kept distinct from [`EffectNote`]s, which describe possible side effects and
/// are *not* permission controls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEntry {
    /// The canonical command key (e.g. `cargo test`); validated to reject shell
    /// operators.
    pub key: CommandKey,
    /// The command's documented purpose.
    pub purpose: String,
    /// The upstream sections this guidance cites.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstream_sections: Vec<SectionId>,
    /// Documented syntax / option excerpts (upstream).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub syntax: Vec<String>,
    /// Documented examples (upstream; never executed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// Documented prerequisites.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<String>,
    /// Documented version / stability qualifications.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compatibility: Vec<String>,
    /// Maintainer-authored effect notes, kept separate from upstream content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effect_notes: Vec<EffectNote>,
}

/// A canonical, validated command key such as `cargo test` or `rustup show`.
///
/// Task [007] does *not* enumerate the catalog (that is tasks [039]-[044]); this
/// type enforces the property every key shares per `plan.md` §6: it is a
/// documentation selector, never a shell line. Shell operators
/// (`| & ; $ \` > < ( ) { } * ? ~ !` and newlines) are rejected so no key can
/// later be interpolated into a process.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommandKey(String);

impl CommandKey {
    /// Build a validated command key.
    pub fn new(value: impl Into<String>) -> Result<Self, SchemaError> {
        let s = value.into();
        if s.is_empty() {
            return Err(SchemaError::InvalidField {
                field: "command key".to_string(),
                reason: "must be non-empty".to_string(),
            });
        }
        if s.trim() != s {
            return Err(SchemaError::InvalidField {
                field: "command key".to_string(),
                reason: "must not have leading or trailing whitespace".to_string(),
            });
        }
        for c in s.chars() {
            if !is_command_key_char(c) {
                return Err(SchemaError::ShellOperator(format!(
                    "command key contains a disallowed character {c:?}"
                )));
            }
        }
        Ok(Self(s))
    }

    /// Borrow the key string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CommandKey {
    type Err = SchemaError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for CommandKey {
    type Error = SchemaError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        CommandKey::new(value)
    }
}

impl From<CommandKey> for String {
    fn from(value: CommandKey) -> Self {
        value.to_string()
    }
}

/// Whether a character is permitted inside a [`CommandKey`]: alphanumerics and
/// the limited, argument-like punctuation, plus the space that separates a
/// command from its subcommand.
fn is_command_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == ' ' || c == '.' || c == '/' || c == '-' || c == '_'
}

// ---------------------------------------------------------------------------
// Manifest and on-disk load gate
// ---------------------------------------------------------------------------

/// The version-independent body of a [`SnapshotManifest`]: the corpus it
/// describes plus its validation summary and attribution.
///
/// `deny_unknown_fields` makes a future-schema field that this build does not
/// know about *fail clearly* rather than silently, which complements the
/// version gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestBody {
    /// The snapshot identity this body was validated under.
    pub snapshot_id: SnapshotId,
    /// When the snapshot was created.
    pub created_at: Timestamp,
    /// The parser version that produced this snapshot.
    pub parser_version: String,
    /// The approved sources in this snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    /// The sections in this snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<Section>,
    /// The API checklist entries in this snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<ChecklistEntry>,
    /// The command-guidance entries in this snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandEntry>,
    /// The validation summary of this snapshot.
    #[serde(default)]
    pub validation: ValidationSummary,
    /// The license / attribution references preserved for this snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attribution: Vec<AttributionRef>,
}

// ---------------------------------------------------------------------------
// Validation summary and attribution
// ---------------------------------------------------------------------------

/// The validation summary a snapshot records so a consumer can see what was
/// checked — and which non-fatal warnings were raised — without re-validating.
///
/// `plan.md` §5 lists the checksum/coverage/validation checks performed during
/// ingestion; task [025] populates these when it validates a complete corpus. A
/// `deny_unknown_fields` body and a `Default` derivation let `ManifestBody`
/// omit the summary entirely and still load.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationSummary {
    /// The number of documents captured.
    pub documents: usize,
    /// The number of sections captured.
    pub sections: usize,
    /// The number of checklist entries captured.
    pub checklist_entries: usize,
    /// The number of command entries captured.
    pub command_entries: usize,
    /// Non-fatal warnings (e.g. removed-document reports) recorded during
    /// validation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A license / attribution reference preserved per `plan.md` §5 and the
/// [006] licensing audit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributionRef {
    /// The source this attribution applies to.
    pub source: SourceId,
    /// The applicable license identifiers (e.g. `MIT OR Apache-2.0`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub licenses: Vec<String>,
    /// The upstream source repository.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repository: String,
    /// Any required distribution notice.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub required_notice: String,
}

/// A single versioned, self-contained corpus snapshot.
///
/// The `schema_version` field is the gate: loading a snapshot first reads the
/// version and rejects any version this build does not recognize, *before*
/// trusting the body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    /// The on-disk schema version this snapshot was written under.
    pub schema_version: SchemaVersion,
    /// The version-independent body.
    #[serde(flatten)]
    pub body: ManifestBody,
}

impl SnapshotManifest {
    /// Serialize a snapshot to a compact, deterministic JSON string.
    ///
    /// Compact (not pretty) keeps the on-disk corpus file free of insignificant
    /// whitespace — deterministic for snapshots and hash-able byte-for-byte.
    pub fn to_json_string(&self) -> Result<String, SchemaError> {
        serde_json::to_string(self).map_err(SchemaError::from)
    }

    /// Load a snapshot from JSON, gating on [`SchemaVersion`] first.
    ///
    /// The version is read independently of the body so that an *unsupported*
    /// version (or a future schema carrying new fields) fails with
    /// [`SchemaError::UnsupportedVersion`] / a clear JSON error rather than a
    /// mistaken reinterpretation of a different layout.
    pub fn from_json_str(json: &str) -> Result<Self, SchemaError> {
        let raw: RawManifest = serde_json::from_str(json)?;
        let schema_version = SchemaVersion::parse(raw.schema_version).ok_or_else(|| {
            SchemaError::UnsupportedVersion {
                found: raw.schema_version,
                supported: SchemaVersion::SUPPORTED.to_vec(),
            }
        })?;
        // Re-parse the remaining fields as the version-independent body so
        // `deny_unknown_fields` on the body still rejects unknown keys.
        let body: ManifestBody = serde_json::from_value(serde_json::Value::Object(raw.rest))?;
        Ok(Self {
            schema_version,
            body,
        })
    }
}

/// The on-the-wire form of a [`SnapshotManifest`] used *only* by the load gate:
/// the version is a raw `u32` (so an unknown version is not silently coerced)
/// and the remaining fields are read as an unconstrained map that the gate
/// re-parses against the versioned body.
#[derive(Deserialize)]
struct RawManifest {
    schema_version: u32,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A failure to build or load the corpus schema.
///
/// Each variant carries enough context to be *actionable* rather than a raw
/// `serde` string, per `plan.md` §9 ("Validate schema versions, checksums, IDs,
/// and bounded sizes before loading a snapshot").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    /// The snapshot's `schema_version` is not one this build supports.
    UnsupportedVersion {
        /// The version found on disk.
        found: u32,
        /// The versions this build supports.
        supported: Vec<u32>,
    },
    /// A version string was not a parseable integer.
    UnknownVersion(String),
    /// An identifier (section / source / snapshot / command) is malformed.
    InvalidId(String),
    /// A field value is invalid for the named field.
    InvalidField {
        /// Which field was being validated.
        field: String,
        /// Why the value is invalid.
        reason: String,
    },
    /// A URL reference is not a well-formed http(s) URL.
    InvalidUri(String),
    /// A shell operator was found in a command key.
    ShellOperator(String),
    /// A JSON (de)serialization failure.
    Json(String),
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "unsupported snapshot schema version {found} (this build supports {supported:?})"
            ),
            Self::UnknownVersion(msg) => write!(f, "unknown schema version: {msg}"),
            Self::InvalidId(msg) => write!(f, "invalid section id: {msg}"),
            Self::InvalidField { field, reason } => {
                write!(f, "invalid `{field}`: {reason}")
            }
            Self::InvalidUri(msg) => write!(f, "invalid URL: {msg}"),
            Self::ShellOperator(msg) => write!(f, "rejected shell operator: {msg}"),
            Self::Json(msg) => write!(f, "JSON error: {msg}"),
        }
    }
}

impl std::error::Error for SchemaError {}

impl From<serde_json::Error> for SchemaError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err.to_string())
    }
}

// ---------------------------------------------------------------------------
// Free-standing helpers
// ---------------------------------------------------------------------------

/// Recover a 1-based `(part, parts)` continuation tuple from the digits that
/// follow a `$c` marker in a section id.
fn parse_continuation(s: &str) -> Result<Option<(usize, usize)>, SchemaError> {
    if s.is_empty() {
        return Err(SchemaError::InvalidId(
            "continuation marker has no part".to_string(),
        ));
    }
    let (part_s, parts_s) = s
        .split_once('.')
        .ok_or_else(|| SchemaError::InvalidId("continuation must be `part.parts`".to_string()))?;
    let part = part_s
        .parse::<usize>()
        .map_err(|_| SchemaError::InvalidId("continuation part is not a number".to_string()))?;
    let parts = parts_s
        .parse::<usize>()
        .map_err(|_| SchemaError::InvalidId("continuation total is not a number".to_string()))?;
    if part == 0 || parts == 0 || part > parts {
        return Err(SchemaError::InvalidId(format!(
            "invalid continuation part {part} of {parts}"
        )));
    }
    Ok(Some((part, parts)))
}

/// Whether a byte is a literal character allowed *raw* in a section id: the
/// RFC 3986 unreserved set plus the structural separators this id grammar
/// inserts.
fn is_id_raw_allowed(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~' | b'/' | b'#' | b'$')
}

/// Guard that a section id contains only URI-safe literal characters and, after
/// every `%`, a valid hex escape.
fn ensure_id_uri_safe(s: &str) -> Result<(), SchemaError> {
    if s.is_empty() {
        return Err(SchemaError::InvalidId(
            "section id must be non-empty".to_string(),
        ));
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'%' {
            let h1 = bytes.get(i + 1);
            let h2 = bytes.get(i + 2);
            match (h1, h2) {
                (Some(a), Some(b2)) if a.is_ascii_hexdigit() && b2.is_ascii_hexdigit() => {
                    i += 3;
                    continue;
                }
                _ => {
                    return Err(SchemaError::InvalidId(format!(
                        "section id has an incomplete percent-escape at byte {i}"
                    )));
                }
            }
        }
        if !is_id_raw_allowed(c) {
            return Err(SchemaError::InvalidId(format!(
                "section id has a non-URI-safe byte {c:?}"
            )));
        }
        i += 1;
    }
    Ok(())
}

/// Percent-encode a field to the RFC 3986 unreserved character set, so that
/// structural separators can be inserted without ambiguity.
fn encode_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.bytes() {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            out.push(c as char);
        } else {
            out.push('%');
            out.push_str(&format!("{c:02X}"));
        }
    }
    out
}

/// Reverse [`encode_field`].
fn decode_field(s: &str) -> Result<String, SchemaError> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'%' {
            let h1 = bytes
                .get(i + 1)
                .ok_or_else(|| SchemaError::InvalidId("incomplete percent-escape".to_string()))?;
            let h2 = bytes
                .get(i + 2)
                .ok_or_else(|| SchemaError::InvalidId("incomplete percent-escape".to_string()))?;
            if !h1.is_ascii_hexdigit() || !h2.is_ascii_hexdigit() {
                return Err(SchemaError::InvalidId(
                    "malformed percent-escape".to_string(),
                ));
            }
            let byte = u8::from_str_radix(&s[(i + 1)..(i + 3)], 16)
                .map_err(|_| SchemaError::InvalidId("malformed percent-escape".to_string()))?;
            out.push(byte);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| {
        SchemaError::InvalidId("percent-decoded section id field is not UTF-8".to_string())
    })
}

/// Guard that an opaque id (`SourceId` / `SnapshotId`) is non-empty and URI-safe
/// to the RFC 3986 unreserved set only (no structural characters).
fn ensure_uri_safe(s: &str, what: &str) -> Result<(), SchemaError> {
    if s.is_empty() {
        return Err(SchemaError::InvalidField {
            field: what.to_string(),
            reason: "must be non-empty".to_string(),
        });
    }
    for (i, c) in s.bytes().enumerate() {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            continue;
        }
        return Err(SchemaError::InvalidField {
            field: what.to_string(),
            reason: format!("byte {i} ({c:?}) is not URI-safe"),
        });
    }
    Ok(())
}

/// Guard that a `Timestamp` string is non-empty and ASCII with no whitespace.
fn validate_timestamp(s: &str) -> Result<(), SchemaError> {
    if s.is_empty() {
        return Err(SchemaError::InvalidField {
            field: "timestamp".to_string(),
            reason: "must be non-empty".to_string(),
        });
    }
    for c in s.chars() {
        if c.is_ascii_whitespace() || !c.is_ascii() {
            return Err(SchemaError::InvalidField {
                field: "timestamp".to_string(),
                reason: format!("byte for {c:?} is not valid in a timestamp"),
            });
        }
    }
    Ok(())
}

/// Whether a string is a minimal, well-formed http(s) URL with a non-empty host.
fn is_http_url(s: &str) -> bool {
    let rest = match s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
    {
        Some(r) => r,
        None => return false,
    };
    if rest.is_empty() || rest.bytes().any(|c| c.is_ascii_whitespace() || c == b'\0') {
        return false;
    }
    // A host is the run of characters before the first path separator (or `?`,
    // `#`). It must be non-empty.
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches([':']);
    !host.is_empty() && host.is_ascii()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_serializes_as_integer_and_round_trips() {
        assert_eq!(SchemaVersion::parse(1), Some(SchemaVersion::V1));
        assert_eq!(SchemaVersion::parse(2), None);
        assert_eq!(SchemaVersion::V1.as_u32(), 1);
        let json = serde_json::to_string(&SchemaVersion::V1).unwrap();
        assert_eq!(json, "1", "version must serialize as a bare u32");
        let back: SchemaVersion = serde_json::from_str("1").unwrap();
        assert_eq!(back, SchemaVersion::V1);
    }

    #[test]
    fn unsupported_version_fails_clearly() {
        let err = SchemaVersion::from_str("2").unwrap_err();
        assert!(
            matches!(err, SchemaError::UnsupportedVersion { found: 2, .. }),
            "got {err:?}"
        );
        assert!(
            err.to_string().contains("2"),
            "must name the found version: {err}"
        );
        assert!(
            err.to_string().contains("1"),
            "must name the supported version: {err}"
        );
    }

    #[test]
    fn manifest_rejects_unknown_version() {
        let json = r#"{
            "schema_version": 9,
            "snapshot_id": "s1",
            "created_at": "2026-10-30T00:00:00Z",
            "parser_version": "0.1"
        }"#;
        let err = SnapshotManifest::from_json_str(json).unwrap_err();
        match err {
            SchemaError::UnsupportedVersion { found, supported } => {
                assert_eq!(found, 9);
                assert_eq!(supported, vec![1]);
            }
            other => panic!("expected unsupported-version, got {other:?}"),
        }
    }

    #[test]
    fn ids_distinguish_source_path_anchor_continuation() {
        let source = SourceId::new("cargo").unwrap();
        let a = SectionId::derive(&source, "commands/index", Some("build"), None).unwrap();
        let b_path = SectionId::derive(&source, "reference", Some("build"), None).unwrap();
        let b_anchor = SectionId::derive(&source, "commands/index", Some("test"), None).unwrap();
        let b_source = {
            let other = SourceId::new("clippy").unwrap();
            SectionId::derive(&other, "commands/index", Some("build"), None).unwrap()
        };
        let b_cont =
            SectionId::derive(&source, "commands/index", Some("build"), Some((2, 3))).unwrap();
        assert_ne!(a, b_path, "different chapter path must differ");
        assert_ne!(a, b_anchor, "different anchor must differ");
        assert_ne!(a, b_source, "different source must differ");
        assert_ne!(a, b_cont, "different continuation must differ");
        // Continuation parts are distinct.
        let p1 = SectionId::derive(&source, "commands/index", Some("x"), Some((1, 3))).unwrap();
        let p3 = SectionId::derive(&source, "commands/index", Some("x"), Some((3, 3))).unwrap();
        assert_ne!(p1, p3);
        // No continuation == no `$c` marker.
        assert!(!a.as_str().contains('$'));
        assert!(b_cont.as_str().contains("$c2"));
    }

    #[test]
    fn section_id_round_trips_through_parts() {
        let source = SourceId::new("cargo").unwrap();
        let id = SectionId::derive(&source, "commands/index", Some("build"), Some((2, 3))).unwrap();
        let parts = SectionId::parse(id.as_str()).unwrap();
        assert_eq!(parts.source, "cargo");
        assert_eq!(parts.chapter_path, "commands/index");
        assert_eq!(parts.anchor.as_deref(), Some("build"));
        assert_eq!(parts.continuation, Some((2, 3)));
        // The parsed parts reconstruct the exact same id (true inverse).
        assert_eq!(SectionId::from_parts(&parts).unwrap(), id);
    }

    #[test]
    fn section_id_encodes_unsafe_characters() {
        let source = SourceId::new("cli ppy").unwrap_err(); // space is not allowed
        assert!(matches!(source, SchemaError::InvalidField { .. }));
        // A path containing a space / non-ASCII is percent-encoded, then round-
        // trips intact.
        let cargo = SourceId::new("cargo").unwrap();
        let id = SectionId::derive(&cargo, "Commands/Intro & Setup", None, None).unwrap();
        let parts = SectionId::parse(id.as_str()).unwrap();
        assert_eq!(parts.chapter_path, "Commands/Intro & Setup");
    }

    #[test]
    fn section_id_rejects_malformed_input() {
        let err = SectionId::parse("cargo//missinganchor").unwrap_err();
        assert!(matches!(err, SchemaError::InvalidId(_)));
    }

    #[test]
    fn section_id_from_str_routes_through_the_validating_gate() {
        // `FromStr` must not wrap an arbitrary string: a bare `parse::<SectionId>()`
        // runs the same URL-safety + source/chapter/anchor/continuation gate as the
        // serde `TryFrom<String>` path, so malformed input fails rather than slipping
        // through unchecked.
        assert!("garbage-with/#and/$/chars".parse::<SectionId>().is_err());
        assert!("cargo//missinganchor".parse::<SectionId>().is_err());
        // A well-formed id parses and round-trips through its components, matching
        // `section_id_round_trips_through_parts`.
        let source = SourceId::new("cargo").unwrap();
        let id = SectionId::derive(&source, "commands/index", Some("build"), None).unwrap();
        let parsed = id.as_str().parse::<SectionId>().unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn command_key_rejects_shell_operators() {
        CommandKey::new("cargo test").unwrap();
        for bad in [
            "cargo test | rm -rf",
            "cargo test; echo hi",
            "cargo$(whoami)",
            "cargo `id`",
            "cargo test & bg",
            "cargo test > out",
        ] {
            let err = CommandKey::new(bad).unwrap_err();
            assert!(
                matches!(err, SchemaError::ShellOperator(_)),
                "`{bad}` should be rejected as a shell operator: {err:?}"
            );
        }
    }

    #[test]
    fn full_manifest_round_trips_through_json() {
        let section = SectionBuilder::new(
            SourceId::new("cargo").unwrap(),
            "The cargo book",
            "commands/index",
            Url::new("https://doc.rust-lang.org/cargo/commands/index.html#build").unwrap(),
            "# Build\n\nrun `cargo build`",
        )
        .anchor("build")
        .heading_path(["Building"])
        .guideline_codes(["C-CONV"])
        .command_keys(["cargo test"])
        .lint_ids(["clippy::unwrap_used"])
        .qualifications(["nightly-only"])
        .build()
        .unwrap();
        let manifest = SnapshotManifest {
            schema_version: SchemaVersion::CURRENT,
            body: ManifestBody {
                snapshot_id: SnapshotId::new("cargo-2026-10-30").unwrap(),
                created_at: Timestamp::new("2026-10-30T00:00:00Z").unwrap(),
                parser_version: "v1".into(),
                sources: vec![Source {
                    id: SourceId::new("cargo").unwrap(),
                    title: "The Cargo Book".into(),
                    entry_url: Url::new("https://doc.rust-lang.org/cargo/").unwrap(),
                    document_urls: vec![],
                    last_checked: Some(Timestamp::new("2026-10-28T00:00:00Z").unwrap()),
                    fetched_at: Timestamp::new("2026-10-30T00:00:00Z").unwrap(),
                    upstream_modified: None,
                    content_hash: CorpusHash::compute(b"cargo"),
                    release: Release::channel_only(Channel::Stable),
                    parser: "books/2024".into(),
                }],
                sections: vec![section],
                checklist: vec![ChecklistEntry {
                    code: "C-CONV".into(),
                    title: "Convert names".into(),
                    category: "Conversions and From".into(),
                    location: "Conversions and From#C-CONV".into(),
                    explanation_sections: vec![],
                }],
                commands: vec![CommandEntry {
                    key: CommandKey::new("cargo test").unwrap(),
                    purpose: "Run unit and integration tests".into(),
                    upstream_sections: vec![],
                    syntax: vec!["cargo test [OPTIONS]".into()],
                    examples: vec![],
                    prerequisites: vec![],
                    compatibility: vec!["stable".into()],
                    effect_notes: vec![EffectNote {
                        kind: EffectKind::ProjectCodeExecution,
                        detail: "compiles and runs the project's own test code".into(),
                    }],
                }],
                validation: ValidationSummary {
                    documents: 2,
                    sections: 1,
                    checklist_entries: 1,
                    command_entries: 1,
                    warnings: vec![],
                },
                attribution: vec![AttributionRef {
                    source: SourceId::new("cargo").unwrap(),
                    licenses: vec!["MIT OR Apache-2.0".into()],
                    repository: "https://github.com/rust-lang/rust".into(),
                    required_notice: String::new(),
                }],
            },
        };
        let json = manifest.to_json_string().unwrap();
        // The on-disk schema version must be a bare integer.
        assert!(json.contains("\"schema_version\":1"), "json: {json}");
        // Unknown top-level fields must fail clearly (deny_unknown_fields).
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        value.as_object_mut().expect("is object").insert(
            "unknown_top_level_field".to_string(),
            serde_json::json!("oops"),
        );
        let tampered = serde_json::to_string(&value).expect("serialize tampered");
        assert!(
            SnapshotManifest::from_json_str(&tampered).is_err(),
            "unknown top-level field must be rejected: {tampered}"
        );
        // The clean JSON round-trips byte-for-byte-equivalent through the typed API.
        let back = SnapshotManifest::from_json_str(&json).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    fn release_preserves_unknown_explicitly() {
        let rel = Release::default();
        assert_eq!(rel.channel, Channel::Unknown);
        assert!(rel.documentation_release.is_none());
        assert!(rel.revision.is_none());
        // Round-trips: the optionals are skipped, which means "unknown".
        let json = serde_json::to_string(&rel).unwrap();
        assert!(!json.contains("documentation_release"));
        let back: Release = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rel);
    }

    #[test]
    fn channel_variants_round_trip() {
        for channel in [
            Channel::Stable,
            Channel::Nightly,
            Channel::Rolling,
            Channel::Unknown,
        ] {
            let json = serde_json::to_string(&channel).unwrap();
            let back: Channel = serde_json::from_str(&json).unwrap();
            assert_eq!(channel, back, "channel {channel:?} round-trips");
        }
    }

    // The two recorded on-disk fixtures below exercise the full acceptance
    // criterion "fixtures round-trip through the versioned serialization format"
    // against real files (compiled in via `include_str!`, so the check stays
    // offline). They cover a valid v1 snapshot on the way round and an
    // unsupported version failing clearly on the way in.

    #[test]
    fn recorded_v1_fixture_round_trips_through_the_versioned_format() {
        // `include_str!` embeds the recorded fixture at compile time from the
        // repository's `tests/fixtures/` — no runtime or network access.
        let record = include_str!("../tests/fixtures/snapshot_v1_valid.json").to_string();

        // The version gate accepts v1 and the body validates; IDs, URLs,
        // timestamps, content hashes, releases, and effect-note kinds all
        // parse.
        let loaded = SnapshotManifest::from_json_str(&record).expect("valid v1 loads");
        assert_eq!(loaded.schema_version, SchemaVersion::V1);
        assert_eq!(loaded.body.snapshot_id.as_str(), "rust-docs-2026-10-30");
        assert_eq!(loaded.body.sections.len(), 1);
        assert_eq!(
            loaded.body.sections[0].id.as_str(),
            "cargo/commands%2Findex#build",
            "the recorded section id must survive the round-trip",
        );

        // Reserialized then re-parsed: the form round-trips byte-stable.
        let reserialized = loaded.to_json_string().expect("serializes");
        let reloaded = SnapshotManifest::from_json_str(&reserialized).expect("re-parses");
        assert_eq!(loaded, reloaded);
        assert_eq!(reserialized, loaded.to_json_string().unwrap(), "stable");

        // A command key that carries shell operators can never enter through the
        // record: the key field validates on the way in. Substituting `cargo
        // test` keeps the JSON structure valid, so the failure is a key/schema
        // rejection, not a JSON parse failure.
        for bad in ["cargo test | rm", "cargo test; echo", "cargo$(id)"] {
            let tampered = record.replace("cargo test", bad);
            assert!(
                SnapshotManifest::from_json_str(&tampered).is_err(),
                "`{bad}` must be rejected when written as a command key",
            );
        }
    }

    /// An unknown field *inside* a nested record (e.g. `Source`, `Section`,
    /// `CommandEntry`) must also be rejected, verifying that the module-wide
    /// `deny_unknown_fields` invariant holds at every level — not just the
    /// manifest envelope.
    #[test]
    fn nested_unknown_fields_fail_clearly() {
        // Inject an unknown field into a `Source` within a valid manifest.
        let json = r#"{
            "schema_version": 1,
            "snapshot_id": "s1",
            "created_at": "2026-10-30T00:00:00Z",
            "parser_version": "v1",
            "sources": [{
                "id": "cargo",
                "title": "The Cargo Book",
                "entry_url": "https://doc.rust-lang.org/cargo/",
                "fetched_at": "2026-10-30T00:00:00Z",
                "content_hash": "c128-deadbeef",
                "release": { "channel": "stable" },
                "parser": "books/2024",
                "unknown_nest_field": "surprise"
            }]
        }"#;
        let err = SnapshotManifest::from_json_str(json).unwrap_err();
        assert!(
            matches!(err, SchemaError::Json(_)),
            "nested unknown field must produce a JSON error, got {err:?}"
        );
    }

    /// A `Continuation` stored on a `Section` must round-trip through JSON,
    /// exercising the `Serialize`/`Deserialize` path that the other tests
    /// (which pass `continuation: None`) skip.
    #[test]
    fn continuation_round_trips_through_json() {
        let section = SectionBuilder::new(
            SourceId::new("cargo").unwrap(),
            "Continued doc",
            "chapters/long",
            Url::new("https://doc.rust-lang.org/cargo/index.html#intro").unwrap(),
            "part 2 of 5",
        )
        .anchor("intro")
        .heading_path(["Intro"])
        .continuation(Continuation { part: 2, parts: 5 })
        .build()
        .unwrap();
        assert!(
            section.continuation.is_some(),
            "build must keep the continuation"
        );

        let json = serde_json::to_string(&section).expect("serialize section");
        let back: Section = serde_json::from_str(&json).expect("deserialize section");
        assert_eq!(section, back);
        assert_eq!(back.continuation, Some(Continuation { part: 2, parts: 5 }));
    }

    /// The builder defaults every optional list/field to empty/`None`, so a
    /// caller supplies only what a section actually carries — and the named
    /// list setters (four interchangeable `Vec<String>` slots under the old
    /// positional `build`) cannot be swapped for one another.
    #[test]
    fn section_builder_defaults_optional_lists() {
        let section = SectionBuilder::new(
            SourceId::new("cargo").unwrap(),
            "Cargo reference",
            "reference",
            Url::new("https://doc.rust-lang.org/cargo/").unwrap(),
            "# Reference",
        )
        .build()
        .expect("a section with no optional fields must build");

        assert!(section.anchor.is_none());
        assert!(section.heading_path.is_empty());
        assert!(section.guideline_codes.is_empty());
        assert!(section.command_keys.is_empty());
        assert!(section.lint_ids.is_empty());
        assert!(section.qualifications.is_empty());
        assert!(section.continuation.is_none());
    }

    #[test]
    fn recorded_unknown_version_fixture_fails_clearly() {
        let record = include_str!("../tests/fixtures/snapshot_v9_unknown_version.json");
        let err = SnapshotManifest::from_json_str(record).unwrap_err();
        match &err {
            SchemaError::UnsupportedVersion { found, supported } => {
                assert_eq!(*found, 9);
                assert_eq!(supported, &vec![1u32]);
            }
            other => panic!("expected unsupported-version, got {other:?}"),
        }
        // The error is self-describing to an operator.
        assert!(
            err.to_string().contains("9"),
            "must name the found version: {err}",
        );
        assert!(
            err.to_string().contains("1"),
            "must name the supported version: {err}",
        );
    }
}
