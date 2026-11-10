//! Runtime snapshot selection: the *read* side of corpus selection.
//!
//! Task `[029]` (M2), `plan.md` §5 ("Runtime behavior"). Where
//! [`crate::corpus`] is the *write* side (it validates a candidate snapshot and
//! publishes it atomically), this module is the *read* side: on startup it
//! selects the one [`RuntimeSnapshot`] the process will serve for its entire
//! lifetime.
//!
//! The selection rule is the plan's startup behavior:
//!
//! 1. **Prefer the latest valid *local* published snapshot** — the one a prior
//!    `sync` wrote under the configured data directory
//!    ([`crate::corpus::load_published_snapshot`]), gated on the complete-corpus
//!    validation ([`crate::corpus::validate_snapshot`]).
//! 2. **Otherwise fall back to the bundled snapshot** — a validated snapshot
//!    shipped with the binary (`plan.md` §5 "Initial availability: bundle a
//!    validated snapshot … First launch can work without network access"), read
//!    from a fixed path the caller supplies.
//! 3. **Otherwise fail actionably** — when *neither* source yields a valid
//!    corpus, return [`LoadError::NoValidCorpus`] rather than starting an
//!    apparently-healthy, empty server (`plan.md` §5: "When no valid snapshot
//!    exists, exit with an actionable message … Do not start an apparently
//!    healthy empty server").
//!
//! The result, [`RuntimeSnapshot`], holds its [`SnapshotManifest`] **by value**
//! and is **immutable**: it is built once at startup and held for the process
//! lifetime, with no interior mutability. A later `sync` that publishes a newer
//! snapshot changes the file on disk, but an already-returned `RuntimeSnapshot`
//! keeps its original identities — the plan's "Keep one snapshot fixed for the
//! process lifetime. After sync, restart the server to activate it; no hot
//! reload". This is acceptance criterion 2 of task `[029]`.
//!
//! ## Scope and trust boundary
//!
//! Selection is a pure, offline **read**: it performs **no** network call,
//! **no** command execution, and **no** write to the local corpus. A corrupt,
//! truncated, non-UTF-8, oversized, or absent local snapshot is *read*, never
//! *deleted* or *mutated*, so the last valid corpus is always preserved
//! (`plan.md` §5 "… preserve the last valid corpus" and §9 "Restrict writes to
//! the configured data directory … preserve the last valid snapshot"). Because
//! it reads only local files and never touches the network, two independent
//! calls with the same two paths return equal results — the selection is a pure
//! function of `(data_dir, bundle)`.
//!
//! It does **not** rebuild the search index (`[034]`/M3) and does **not** wire
//! into `serve`/`status` (M4); it only *selects* the snapshot.
//
// Like `crate::corpus` and `crate::model`, these public items are not yet
// reached from within the crate: the runtime selection is wired into `serve`
// and `status` by M4. The allow is scoped to this module and lifted the moment
// a consumer is written, mirroring the precedent in `crate::corpus`.
#![allow(dead_code)]

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::corpus::{self, MAX_SNAPSHOT_BYTES, PublishError};
use crate::model::{SchemaError, SnapshotManifest};

/// Where the active [`RuntimeSnapshot`] came from.
///
/// Recording the source (and its on-disk path) is what makes the selection
/// *provenance-carrying* (`plan.md` §5: "snapshot ID, source ID, … provenance"):
/// a consumer can report which corpus it is serving — a freshly synchronized
/// local snapshot or the shipped fallback — without re-deriving it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotSource {
    /// The latest valid snapshot published under the configured data directory
    /// (by a prior `sync`), at `path` (`<data_dir>/corpus/manifest.json`).
    Local {
        /// The on-disk path of the local published snapshot.
        path: PathBuf,
    },
    /// The bundled fallback snapshot shipped with the binary, at `path`.
    Bundle {
        /// The on-disk path of the bundled snapshot.
        path: PathBuf,
    },
}

/// The one snapshot a process serves for its entire lifetime.
///
/// It holds its [`SnapshotManifest`] **by value** and carries **no** interior
/// mutability: it is built once at startup by [`load_snapshot`] and never
/// mutated. A later `sync` that publishes a newer snapshot changes the file on
/// disk, but an already-returned `RuntimeSnapshot` keeps its original
/// identities — the plan's "Keep one snapshot fixed for the process lifetime
/// … no hot reload". This is exactly why acceptance criterion 2 of task
/// `[029]` holds: publishing a newer snapshot cannot change the identities a
/// held snapshot already reports.
#[derive(Clone, Debug)]
pub struct RuntimeSnapshot {
    /// The fixed snapshot's manifest (the active corpus).
    pub manifest: SnapshotManifest,
    /// Where the manifest came from.
    pub source: SnapshotSource,
}

/// A failure to select a valid runtime snapshot, each variant actionable.
///
/// No variant is a bare `io::Error`: each carries enough context to tell an
/// operator *what* failed and *what to do next* (`plan.md` §8 "typed errors"
/// and §9 "external input … must not cause an avoidable panic").
#[derive(Debug)]
pub enum LoadError {
    /// Neither the local published snapshot nor (when supplied) the bundled
    /// snapshot yielded a valid corpus. `local` / `bundle` carry a short,
    /// human-readable reason each, or `None` when that source was not attempted
    /// (no data directory, or no bundled snapshot path was given). The
    /// [`Display`](std::fmt::Display) directs the operator to run `sync` — the
    /// "no valid corpus" startup failure `plan.md` §5 calls for, surfaced
    /// instead of starting an apparently-healthy empty server.
    NoValidCorpus {
        /// The local-source reason, when the local source was attempted.
        local: Option<String>,
        /// The bundled-source reason, when a bundled snapshot path was supplied.
        bundle: Option<String>,
    },
    /// A single source failed to yield a valid corpus — an I/O, schema, or
    /// complete-corpus validation failure — with a short `context` naming where
    /// it happened and the underlying [`PublishError`] preserved as the cause,
    /// so the full error chain survives [`Error::source`].
    Source {
        /// A short, human description of the operation that failed.
        context: String,
        /// The underlying failure, preserved for the error chain.
        cause: PublishError,
    },
}

impl LoadError {
    /// Build a [`LoadError::Source`] with a short context and a
    /// [`PublishError`] cause, preserving the full error chain.
    fn source(context: impl Into<String>, cause: PublishError) -> Self {
        Self::Source {
            context: context.into(),
            cause,
        }
    }

    /// A short, human-readable reason this failure represents, used when
    /// [`load_snapshot`] aggregates a per-source failure into
    /// [`LoadError::NoValidCorpus`].
    fn reason(&self) -> String {
        match self {
            Self::Source { context, cause } => format!("{context}: {cause}"),
            Self::NoValidCorpus { .. } => self.to_string(),
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoValidCorpus { local, bundle } => {
                write!(f, "no valid corpus is available")?;
                if let Some(reason) = local {
                    write!(f, " (local published snapshot: {reason})")?;
                }
                match bundle {
                    Some(reason) => write!(f, " (bundled snapshot: {reason})")?,
                    None => write!(f, " (no bundled snapshot was provided)")?,
                }
                write!(
                    f,
                    ". Run `rust-docs-mcp sync` to download and publish a local snapshot, or restore the bundled fallback."
                )
            }
            Self::Source { context, cause } => write!(f, "{context}: {cause}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source { cause, .. } => Some(cause),
            Self::NoValidCorpus { .. } => None,
        }
    }
}

/// Select the fixed runtime snapshot, preferring a valid local published
/// snapshot and falling back to the bundled snapshot.
///
/// This is the read side of snapshot selection (`plan.md` §5). It runs three
/// steps, in order:
///
/// 1. **Local** — read and validate the snapshot published under `data_dir` by a
///    prior `sync` ([`crate::corpus::load_published_snapshot`] for the read,
///    [`crate::corpus::validate_snapshot`] for the complete-corpus gate). A
///    read failure (absent / corrupt / truncated / non-UTF-8 / oversized) or a
///    validation failure records a short reason and falls through.
/// 2. **Bundle** — when `bundle` is `Some`, read and validate the bundled
///    snapshot at that path with the same gates.
/// 3. **Fail** — when neither source yields a valid corpus, return
///    [`LoadError::NoValidCorpus`] with both reasons. An empty or
///    apparently-healthy snapshot is **never** returned.
///
/// The result is **immutable** and **fixed for the process lifetime**: the
/// returned [`RuntimeSnapshot`] owns its manifest by value, so a later `sync`
/// that publishes a newer snapshot to the same `data_dir` cannot change the
/// identities this snapshot reports. The selection is a pure function of the two
/// paths — it makes **no** network call and performs **no** write, so it never
/// deletes or mutates the local corpus.
pub fn load_snapshot(data_dir: &Path, bundle: Option<&Path>) -> Result<RuntimeSnapshot, LoadError> {
    // Step 1: the latest valid *local* published snapshot, when present, is the
    // preferred source. A read or validation failure records a short reason and
    // falls through to the bundled fallback.
    let local = match load_local(data_dir) {
        Ok(manifest) => {
            return Ok(RuntimeSnapshot {
                manifest,
                source: SnapshotSource::Local {
                    path: corpus::published_snapshot_path(data_dir),
                },
            });
        }
        Err(err) => Some(err.reason()),
    };

    // Step 2: the bundled fallback, consulted only when a bundle path was given.
    let bundle = match bundle {
        Some(path) => match load_bundle(path) {
            Ok(manifest) => {
                return Ok(RuntimeSnapshot {
                    manifest,
                    source: SnapshotSource::Bundle {
                        path: path.to_path_buf(),
                    },
                });
            }
            Err(err) => Some(err.reason()),
        },
        None => None,
    };

    // Step 3: neither source yielded a valid corpus. Never surface an empty or
    // apparently-healthy snapshot — return an actionable, sync-directing error.
    Err(LoadError::NoValidCorpus { local, bundle })
}

/// Read and validate the local published snapshot under `data_dir`.
///
/// Reuses the corpus loader's read + schema/UTF-8/size gate
/// ([`crate::corpus::load_published_snapshot`]), then runs the complete-corpus
/// validation ([`crate::corpus::validate_snapshot`]). The local file is only
/// *read*, so a corrupt or absent snapshot never deletes or mutates it.
fn load_local(data_dir: &Path) -> Result<SnapshotManifest, LoadError> {
    // Read, gated on the schema version, the on-disk size bound, and
    // JSON/UTF-8 integrity. An absent, corrupt, truncated, non-UTF-8, or
    // oversized file fails here — never a panic.
    let manifest = corpus::load_published_snapshot(data_dir)
        .map_err(|cause| LoadError::source("local published snapshot", cause))?;
    // Gate on the complete-corpus validation: a schema-valid but inconsistent
    // corpus (a missing source, a broken cross-reference, …) is rejected here.
    let report = corpus::validate_snapshot(&manifest);
    if !report.is_valid() {
        return Err(LoadError::source(
            "local published snapshot",
            PublishError::Invalid(report),
        ));
    }
    Ok(manifest)
}

/// Read and validate the bundled snapshot at `path`.
///
/// The bundled snapshot is the same compact-JSON format as a published snapshot,
/// so the same gates apply: read it, bound its on-disk size, check UTF-8
/// integrity, parse it through the schema-version gate
/// ([`SnapshotManifest::from_json_str`]), and run the complete-corpus
/// validation. The bundle is only *read*, so a corrupt or absent bundle never
/// mutates the local corpus.
fn load_bundle(path: &Path) -> Result<SnapshotManifest, LoadError> {
    let bytes = fs::read(path).map_err(|cause| {
        LoadError::source(
            "bundled snapshot",
            PublishError::io("reading bundled snapshot", cause),
        )
    })?;
    // Bound the on-disk size *before* parsing, mirroring the corpus loader: an
    // oversized file is rejected rather than read into memory.
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(LoadError::source(
            "bundled snapshot",
            PublishError::too_large(bytes.len()),
        ));
    }
    // UTF-8 integrity: a corrupt (non-UTF-8) bundle must not be misparsed.
    let json = String::from_utf8(bytes).map_err(|err| {
        LoadError::source(
            "bundled snapshot",
            PublishError::Schema(SchemaError::Json(format!(
                "bundled snapshot is not valid UTF-8: {err}"
            ))),
        )
    })?;
    // Schema gate: the version is checked before the body is trusted.
    let manifest = SnapshotManifest::from_json_str(&json)
        .map_err(|err| LoadError::source("bundled snapshot", PublishError::from(err)))?;
    // Complete-corpus validation, mirroring the local path.
    let report = corpus::validate_snapshot(&manifest);
    if !report.is_valid() {
        return Err(LoadError::source(
            "bundled snapshot",
            PublishError::Invalid(report),
        ));
    }
    Ok(manifest)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{self, publish_snapshot};
    use crate::model::{
        Channel, CorpusHash, ManifestBody, Release, SchemaVersion, SectionBuilder, SnapshotId,
        SnapshotManifest, Source, SourceId, Timestamp, Url, ValidationSummary,
    };
    use crate::registry::SourceCollection;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// One `Source` per required collection, with a single recorded document URL.
    ///
    /// Mirrors `crate::corpus::tests::source_for`: the `id` is the collection's
    /// canonical id (always URI-safe, so it always builds a [`SourceId`]) and the
    /// remaining fields carry a valid, non-empty shape.
    fn source_for(collection: SourceCollection) -> Source {
        let id = collection.id();
        Source {
            id: SourceId::new(id).expect("a registry id must build a SourceId"),
            title: collection.title().to_string(),
            entry_url: Url::new(collection.entry_url())
                .expect("a registry entry URL must be valid"),
            document_urls: vec![
                Url::new(collection.entry_url()).expect("a registry entry URL must be valid"),
            ],
            last_checked: None,
            fetched_at: Timestamp::new("2026-10-30T00:00:00Z").unwrap(),
            upstream_modified: None,
            content_hash: CorpusHash::compute(b"fixture"),
            release: Release::channel_only(Channel::Stable),
            parser: "test/1".into(),
        }
    }

    /// A complete, valid snapshot carrying the given `snapshot_id`: all eight
    /// sources and one well-formed section. It passes
    /// [`crate::corpus::validate_snapshot`] so it is selectable by
    /// [`load_snapshot`].
    fn valid_snapshot(snapshot_id: &str) -> SnapshotManifest {
        // One well-formed section, built through `SectionBuilder` so its id and
        // content hash are the consistent, recomputed values the validator
        // requires.
        let section = SectionBuilder::new(
            SourceId::new("cargo").unwrap(),
            "The cargo book",
            "commands/index",
            Url::new("https://doc.rust-lang.org/cargo/commands/index.html#build").unwrap(),
            "# Build\n\nRun `cargo build` to compile without linking.",
        )
        .anchor("build")
        .build()
        .expect("a well-formed section builds");

        SnapshotManifest {
            schema_version: SchemaVersion::CURRENT,
            body: ManifestBody {
                snapshot_id: SnapshotId::new(snapshot_id).expect("a valid snapshot id"),
                created_at: Timestamp::new("2026-10-30T00:00:00Z").unwrap(),
                parser_version: "test/1".into(),
                sources: SourceCollection::ALL
                    .iter()
                    .copied()
                    .map(source_for)
                    .collect(),
                sections: vec![section],
                checklist: vec![],
                commands: vec![],
                validation: ValidationSummary::default(),
                attribution: vec![],
            },
        }
    }

    /// A schema-valid but *invalid* snapshot: the same shape as
    /// [`valid_snapshot`] with its sources cleared, so it misses all eight
    /// required collections and fails complete-corpus validation.
    fn invalid_snapshot() -> SnapshotManifest {
        let mut snap = valid_snapshot("rust-docs-invalid");
        snap.body.sources.clear();
        snap
    }

    /// A RAII guard that removes its directory on drop, so a test's temp files
    /// never leak and no real user data directory is ever touched.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// Create a fresh, unique directory under the OS temp dir.
    fn temp_dir() -> TempDir {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rust-docs-mcp-runtime-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create the temp dir");
        TempDir(path)
    }

    /// Write the compact JSON of `manifest` to `path` (a bundled snapshot file).
    fn write_bundle(path: &Path, manifest: &SnapshotManifest) {
        fs::write(
            path,
            manifest
                .to_json_string()
                .expect("a valid snapshot serializes"),
        )
        .expect("write the bundle file");
    }

    /// Write raw `bytes` to `path` (a corrupt snapshot file).
    fn write_bytes(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write the corrupt file");
    }

    /// Write a corrupt local snapshot directly at the published path.
    fn corrupt_local(data_dir: &Path, bytes: &[u8]) {
        let local_path = corpus::published_snapshot_path(data_dir);
        fs::create_dir_all(local_path.parent().expect("the corpus dir has a parent"))
            .expect("create the corpus dir");
        write_bytes(&local_path, bytes);
    }

    // -- Acceptance criterion 1: a valid corpus, local preferred over bundle. --

    /// **AC1a**: a valid local snapshot is preferred; a valid bundle is never
    /// consulted.
    #[test]
    fn a_valid_local_snapshot_is_preferred_over_the_bundle() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");
        let bundle = tmp.0.join("bundle.json");

        // A valid local published snapshot.
        publish_snapshot(&data_dir, &valid_snapshot("rust-docs-local"))
            .expect("the local snapshot must publish");
        // A valid bundle too — it must not be consulted when the local is valid.
        write_bundle(&bundle, &valid_snapshot("rust-docs-bundle"));

        let rt = load_snapshot(&data_dir, Some(&bundle)).expect("a valid local must load");

        // The source is the local published snapshot, at the fixed path, and its
        // identity is the local's — never the bundle's (the bundle was not
        // consulted).
        assert_eq!(
            rt.source,
            SnapshotSource::Local {
                path: corpus::published_snapshot_path(&data_dir)
            },
        );
        assert_eq!(rt.manifest.body.snapshot_id.as_str(), "rust-docs-local");
        assert_ne!(rt.manifest.body.snapshot_id.as_str(), "rust-docs-bundle");
    }

    /// **AC1b**: a corrupt local falls back to a valid bundle.
    #[test]
    fn a_corrupt_local_falls_back_to_the_bundle() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");
        let bundle = tmp.0.join("bundle.json");

        corrupt_local(&data_dir, b"this is not a snapshot");
        write_bundle(&bundle, &valid_snapshot("rust-docs-bundle"));

        let rt = load_snapshot(&data_dir, Some(&bundle)).expect("the bundle must load");

        assert_eq!(rt.source, SnapshotSource::Bundle { path: bundle },);
        assert_eq!(rt.manifest.body.snapshot_id.as_str(), "rust-docs-bundle");
    }

    /// **AC1c**: a corrupt local with no bundle is an actionable
    /// `NoValidCorpus` whose `Display` directs the operator to run `sync`.
    #[test]
    fn a_corrupt_local_with_no_bundle_is_no_valid_corpus() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");

        corrupt_local(&data_dir, b"corrupt, not json");

        let err = load_snapshot(&data_dir, None).expect_err("no valid corpus must fail");
        // Compute the full `Display` first; matching by reference below keeps
        // `err` available for that message.
        let message = err.to_string();
        match &err {
            LoadError::NoValidCorpus { local, bundle } => {
                assert!(local.is_some(), "the local reason must be recorded");
                assert!(
                    bundle.is_none(),
                    "no bundle was supplied, so no bundle reason"
                );
                // Actionable: directs the operator to run `sync`.
                assert!(
                    message.to_lowercase().contains("sync"),
                    "the message must direct the operator to sync: {message}"
                );
            }
            other => panic!("expected NoValidCorpus, got {other:?}"),
        }
    }

    /// **AC1d**: an absent local falls back to a valid bundle.
    #[test]
    fn an_absent_local_falls_back_to_the_bundle() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data"); // no corpus dir, no published file
        let bundle = tmp.0.join("bundle.json");

        write_bundle(&bundle, &valid_snapshot("rust-docs-bundle"));

        let rt = load_snapshot(&data_dir, Some(&bundle))
            .expect("the bundle must load when the local is absent");

        assert_eq!(rt.source, SnapshotSource::Bundle { path: bundle },);
        assert_eq!(rt.manifest.body.snapshot_id.as_str(), "rust-docs-bundle");
    }

    /// **AC1e**: an absent local with no bundle is an actionable
    /// `NoValidCorpus`.
    #[test]
    fn an_absent_local_with_no_bundle_is_no_valid_corpus() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");

        let err = load_snapshot(&data_dir, None).expect_err("absent local + no bundle must fail");
        let message = err.to_string();
        match &err {
            LoadError::NoValidCorpus { local, bundle } => {
                assert!(local.is_some(), "the local reason must be recorded");
                assert!(
                    bundle.is_none(),
                    "no bundle was supplied, so no bundle reason"
                );
                assert!(
                    message.to_lowercase().contains("sync"),
                    "the message must direct the operator to sync: {message}"
                );
            }
            other => panic!("expected NoValidCorpus, got {other:?}"),
        }
    }

    /// **AC1f**: a corrupt bundle does not clobber a valid local — the local is
    /// returned and the bundle is never consulted.
    #[test]
    fn a_corrupt_bundle_does_not_clobber_a_valid_local() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");
        let bundle = tmp.0.join("bundle.json");

        publish_snapshot(&data_dir, &valid_snapshot("rust-docs-local"))
            .expect("the local must publish");
        write_bytes(&bundle, b"not a snapshot at all");

        let rt = load_snapshot(&data_dir, Some(&bundle))
            .expect("the valid local must load despite a corrupt bundle");

        assert_eq!(
            rt.source,
            SnapshotSource::Local {
                path: corpus::published_snapshot_path(&data_dir)
            },
        );
        assert_eq!(rt.manifest.body.snapshot_id.as_str(), "rust-docs-local");
    }

    /// When *both* sources fail validation (schema-valid but inconsistent), the
    /// selection is a `NoValidCorpus` that records *both* reasons. This exercises
    /// the per-source validation-failure path for both the local and the bundle.
    #[test]
    fn both_sources_invalid_is_no_valid_corpus_with_both_reasons() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");
        let bundle = tmp.0.join("bundle.json");

        // A local that is schema-valid JSON but fails validation (no sources).
        corrupt_local(
            &data_dir,
            invalid_snapshot().to_json_string().unwrap().as_bytes(),
        );
        // A bundled snapshot that is schema-valid JSON but fails validation too.
        write_bundle(&bundle, &invalid_snapshot());

        let err = load_snapshot(&data_dir, Some(&bundle)).expect_err("neither source is valid");
        let message = err.to_string();
        match &err {
            LoadError::NoValidCorpus { local, bundle } => {
                assert!(
                    local.is_some(),
                    "the local reason must be recorded: {local:?}"
                );
                assert!(
                    bundle.is_some(),
                    "the bundle reason must be recorded: {bundle:?}"
                );
                assert!(
                    message.to_lowercase().contains("sync"),
                    "the message must direct the operator to sync: {message}"
                );
            }
            other => panic!("expected NoValidCorpus, got {other:?}"),
        }
    }

    /// A non-UTF-8 bundle must not panic: it is rejected and (with no valid
    /// local) surfaces as `NoValidCorpus`.
    #[test]
    fn a_non_utf8_bundle_does_not_panic_and_falls_back() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");
        let bundle = tmp.0.join("bundle.json");

        write_bytes(&bundle, &[0xff_u8, 0xfe, 0x00, 0x01]);

        let err =
            load_snapshot(&data_dir, Some(&bundle)).expect_err("a non-UTF-8 bundle must not load");
        assert!(
            matches!(err, LoadError::NoValidCorpus { .. }),
            "a non-UTF-8 bundle with no valid local must be NoValidCorpus, got {err:?}"
        );
    }

    /// A truncated (partial) bundle must not panic: it is rejected and, with no
    /// valid local, surfaces as `NoValidCorpus`.
    #[test]
    fn a_truncated_bundle_does_not_panic_and_falls_back() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");
        let bundle = tmp.0.join("bundle.json");

        write_bytes(&bundle, br#"{"schema_version":1,"snap"#);

        let err =
            load_snapshot(&data_dir, Some(&bundle)).expect_err("a truncated bundle must not load");
        assert!(
            matches!(err, LoadError::NoValidCorpus { .. }),
            "a truncated bundle with no valid local must be NoValidCorpus, got {err:?}"
        );
    }

    // -- Acceptance criterion 1 (companion): the loader never mutates the local. --

    /// The loader only *reads*: a corrupt local that forces a fallback is never
    /// deleted or mutated, so the last valid corpus (whatever it is) is
    /// preserved.
    #[test]
    fn the_loader_never_deletes_or_mutates_the_local_snapshot() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");
        let bundle = tmp.0.join("bundle.json");
        let local_path = corpus::published_snapshot_path(&data_dir);

        corrupt_local(&data_dir, b"corrupt local");
        write_bundle(&bundle, &valid_snapshot("rust-docs-bundle"));

        load_snapshot(&data_dir, Some(&bundle)).expect("falls back to the bundle");

        // The corrupt local file is still present and byte-for-byte unchanged:
        // the loader read it, it never deleted or rewrote it.
        assert!(
            local_path.exists(),
            "the loader must not delete the local snapshot"
        );
        assert_eq!(
            fs::read(&local_path).expect("the local file is still present"),
            b"corrupt local",
        );
    }

    // -- Acceptance criterion 2: the loaded snapshot is fixed for the lifetime. --

    /// **AC2a**: publishing a newer snapshot to the same data directory does not
    /// change the identities an already-returned `RuntimeSnapshot` reports — the
    /// snapshot is held fixed. A *fresh* load does pick up the newer snapshot,
    /// proving the selection reads the file while the held snapshot does not.
    #[test]
    fn a_later_publish_does_not_change_the_loaded_snapshot_identities() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");

        // Load a snapshot (v1) and hold it.
        publish_snapshot(&data_dir, &valid_snapshot("rust-docs-v1")).expect("publish v1");
        let loaded = load_snapshot(&data_dir, None).expect("load v1");
        let original_id = loaded.manifest.body.snapshot_id.as_str().to_string();
        let original_section_ids: Vec<String> = loaded
            .manifest
            .body
            .sections
            .iter()
            .map(|s| s.id.as_str().to_string())
            .collect();
        assert_eq!(original_id, "rust-docs-v1");

        // Publish a newer snapshot (v2) over the same data directory.
        publish_snapshot(&data_dir, &valid_snapshot("rust-docs-v2")).expect("publish v2 over v1");

        // The already-held snapshot is unchanged by the later publish: its
        // identities still match what was captured before the newer publish.
        assert_eq!(
            loaded.manifest.body.snapshot_id.as_str(),
            original_id.as_str(),
            "the held snapshot must keep its original snapshot id"
        );
        assert_eq!(
            loaded
                .manifest
                .body
                .sections
                .iter()
                .map(|s| s.id.as_str().to_string())
                .collect::<Vec<_>>(),
            original_section_ids,
            "the held snapshot must keep its original section ids"
        );

        // A fresh load *does* pick up the newer snapshot: the selection reads the
        // file, and it is the held snapshot (not the file) that stays fixed.
        let reloaded = load_snapshot(&data_dir, None).expect("reload v2");
        assert_eq!(
            reloaded.manifest.body.snapshot_id.as_str(),
            "rust-docs-v2",
            "a fresh load must read the newest published snapshot"
        );
    }

    /// **AC2b**: the selection is a pure function of its two paths — two
    /// independent loads of the same inputs return equal results, with no network
    /// call and no mutation (the loader is read-only).
    #[test]
    fn load_snapshot_is_deterministic_and_read_only() {
        let tmp = temp_dir();
        let data_dir = tmp.0.join("data");

        publish_snapshot(&data_dir, &valid_snapshot("rust-docs-v1")).expect("publish");

        let first = load_snapshot(&data_dir, None).expect("first load");
        let second = load_snapshot(&data_dir, None).expect("second load");

        assert_eq!(
            first.manifest, second.manifest,
            "the same inputs must yield the same manifest"
        );
        assert_eq!(
            first.source, second.source,
            "the same inputs must yield the same source"
        );
    }

    /// `LoadError::NoValidCorpus` reports the source that was *not* attempted as
    /// `None`, and its `Display` names both the reasons it did gather and the
    /// `sync` remedy.
    #[test]
    fn no_valid_corpus_reports_reasons_and_the_sync_remedy() {
        let err = LoadError::NoValidCorpus {
            local: Some("reading local published snapshot: <reason>".to_string()),
            bundle: None,
        };
        let message = err.to_string();
        assert!(
            message.contains("reading local published snapshot"),
            "the gathered local reason must appear: {message}"
        );
        assert!(
            message.contains("no bundled snapshot was provided"),
            "the unattempted bundle must be named: {message}"
        );
        assert!(
            message.to_lowercase().contains("sync"),
            "the remedy must name `sync`: {message}"
        );
    }

    /// `LoadError::Source` preserves its cause through
    /// [`std::error::Error::source`], so the full error chain survives.
    #[test]
    fn source_error_preserves_the_cause_chain() {
        use std::error::Error;

        let err = LoadError::source(
            "bundled snapshot",
            PublishError::io(
                "reading bundled snapshot",
                std::io::Error::new(std::io::ErrorKind::NotFound, "gone"),
            ),
        );
        assert!(
            err.to_string().contains("bundled snapshot"),
            "the context must appear: {err}"
        );
        let source = err.source().expect("a Source error must carry its cause");
        assert!(
            source.to_string().contains("reading bundled snapshot"),
            "the underlying PublishError must surface through source(): {source}"
        );
    }
}
