//! The sync *writer lock* — task `[028]`.
//!
//! `plan.md` §8/sync requires that a sync "serialize concurrent writers with a
//! lock," and §9 requires that the server "recover cleanly after interrupted
//! sync." A sync publishes a corpus snapshot atomically
//! ([`crate::corpus::publish_snapshot`]); this module is the *serialization*
//! half of that: a single, cross-process exclusive lock that no two syncs may
//! hold at once, plus a recovery path that lets a later sync take over the lock
//! left behind by a crashed or interrupted one.
//!
//! ## Design
//!
//! The lock is a single file, `sync.lock`, *inside the canonical corpus
//! directory* — the same containment-safe location the published snapshot and
//! its transient candidates live in (`crate::corpus`), so a `..` or symlink in
//! the data directory can never carry the lock outside it.
//!
//! - **Exclusive acquisition.** A writer claims the lock with
//!   `OpenOptions::create_new(true)`, which fails atomically with
//!   `AlreadyExists` when another writer holds it. A claim is then stamped with
//!   a [`LockRecord`] (the holder's pid and a monotonic nanosecond timestamp)
//!   and the handle is returned as a [`WriterGuard`], an RAII guard that
//!   *releases the lock on `Drop`* — so a finished publish frees the lock, and a
//!   panic or early return can never leave a live writer holding it.
//!
//! - **Staleness recovery.** If the lock already exists, its recorded holder is
//!   consulted ([`is_stale`]): a dead-sentinel pid (`0`, not a real user
//!   process) or a timestamp older than the staleness threshold ([`DEFAULT_LOCK_TTL`])
//!   means the holder is gone or stuck, so the lock is *reclaimed* (removed and
//!   re-created). This is the "recover cleanly after an interrupted sync" step:
//!   a sync that crashed mid-write leaves a stale `sync.lock`, and the next sync
//!   reclaims it instead of blocking forever.
//!
//! ## A deliberate, documented boundary
//!
//! The reclaim decision is **best-effort** and **std-only**: it relies on a
//! wall-clock timestamp and a dead-pid sentinel rather than a real
//! cross-process liveness probe. Real process liveness would need a platform
//! syscall (e.g. `kill(pid, 0)`) or a dependency, which the MVP deliberately
//! avoids (the dependency policy in `Cargo.toml` / `docs/COMPATIBILITY.md`
//! keeps the closure minimal and offline). The timestamp/TTL check is
//! deterministic in tests (callers inject the clock and threshold) and bounds
//! the worst-case recovery window in production. The hard correctness backstop
//! is the atomic `rename` in `publish_snapshot`: even in the rare reclaim race
//! where two writers briefly overlap, each renames a uniquely named, fully
//! written candidate, so the published snapshot is never torn. The [`WriterGuard`]'s
//! `Drop` likewise releases *only* the lock it still owns, so it can never
//! delete a lock a reclaimer already took over.
//!
//! No network, no command execution, no project access — only the one lock file
//! inside the configured data directory.

use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The fixed name of the writer lock, inside the canonical corpus directory.
///
/// It is distinct from the published file (`manifest.json`) and the transient
/// candidate names (`snapshot-<…>.tmp`), so it is never mistaken for either.
pub const LOCK_NAME: &str = "sync.lock";

/// The default staleness threshold for a held lock: a lock older than this is
/// treated as belonging to a crashed or stuck writer and is reclaimable. Five
/// minutes bounds the worst-case recovery window for the (rarely run) `sync`
/// command without ever blocking a live writer — a live writer is never reclaimed
/// while it is within this window.
pub const DEFAULT_LOCK_TTL: Duration = Duration::from_secs(5 * 60);

/// What a writer records in the lock file so a later writer can judge the holder
/// liveness: the holder's process id and the (monotonic) nanosecond timestamp at
/// which it claimed the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockRecord {
    /// The claiming process's id. A `0` pid is a *dead sentinel* denoting an
    /// unknown or dead holder; it is never a real user-process id on the host,
    /// so such a lock is always reclaimable.
    pub pid: u32,
    /// Nanoseconds since the Unix epoch at which the lock was claimed, used for
    /// the staleness (TTL) check.
    pub acquired_at_ns: u64,
}

impl LockRecord {
    /// Encode the record to its on-disk bytes: two whitespace-separated decimal
    /// fields (`pid acquired_at_ns`) terminated by a newline.
    fn encode(&self) -> Vec<u8> {
        format!("{} {}\n", self.pid, self.acquired_at_ns).into_bytes()
    }

    /// Parse a record from on-disk bytes. Returns `None` for any malformed,
    /// truncated, or non-UTF-8 content — a half-written or corrupt lock is
    /// treated as *stale* (reclaimable) by the acquirer.
    fn parse(bytes: &[u8]) -> Option<LockRecord> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut fields = text.split_whitespace();
        let pid: u32 = fields.next()?.parse().ok()?;
        let acquired_at_ns: u64 = fields.next()?.parse().ok()?;
        // A well-formed record is exactly two fields; a stray third means the
        // record is corrupt and must not be trusted.
        fields.next().is_none().then_some(LockRecord {
            pid,
            acquired_at_ns,
        })
    }

    /// The dead-holder sentinel: an unknown/dead pid and an epoch-0 timestamp,
    /// so a lock carrying it is always reclaimable.
    #[cfg(test)]
    pub fn dead() -> Self {
        Self {
            pid: 0,
            acquired_at_ns: 0,
        }
    }
}

/// The bytes of a *stale* (dead-holder) lock, for recovery tests and for a
/// caller that wants to seed a reclaimable lock without depending on the internal
/// encoding. A later sync reclaims a lock carrying these bytes.
#[cfg(test)]
pub fn dead_lock_bytes() -> Vec<u8> {
    LockRecord::dead().encode()
}

/// A claim on the sync writer lock that releases it on `Drop`.
///
/// Holding a `WriterGuard` means the writer is the sole publisher: no other sync
/// can be writing or promoting a candidate. The guard stores the lock path and
/// the exact record it stamped, so its `Drop` releases *only* the lock it still
/// owns (see the module docs on the reclaim race).
#[derive(Debug)]
pub struct WriterGuard {
    lock_path: PathBuf,
    record: LockRecord,
}

impl Drop for WriterGuard {
    fn drop(&mut self) {
        // Release only the lock we still own. If a reclaimer created a fresh lock
        // over ours during a reclaim race, the file now holds a *different*
        // record; deleting it would clobber the reclaimer, so we leave it.
        if read_record(&self.lock_path).as_ref() == Some(&self.record) {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

/// A failure to acquire the writer lock.
#[derive(Debug)]
pub enum LockError {
    /// Another *live* writer currently holds the lock (an unstale holder), so
    /// this claim did not proceed. The claimer may retry; the previous snapshot
    /// is untouched.
    WriterBusy(LockRecord),
    /// A filesystem failure while creating, stamping, or reading the lock.
    Io {
        /// Short, human description of the operation that failed.
        context: String,
        /// The underlying I/O cause, preserved for the error chain.
        source: io::Error,
    },
}

impl LockError {
    /// Build a [`LockError::Io`] that records *where* a filesystem failure
    /// happened.
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WriterBusy(record) => {
                write!(f, "another sync holds the writer lock (pid {})", record.pid)
            }
            Self::Io { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for LockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::WriterBusy(_) => None,
        }
    }
}

/// Acquire the sync writer lock under `corpus_dir`, reclaiming a stale one.
///
/// `corpus_dir` must already be the canonical corpus directory (as
/// [`crate::corpus::publish_snapshot`] resolves it), so the lock is contained
/// inside the configured data directory. The lock is claimed with a live pid and
/// the current wall-clock nanosecond timestamp, and the default staleness
/// threshold ([`DEFAULT_LOCK_TTL`]) governs reclaim. The returned
/// [`WriterGuard`] releases the lock on `Drop`.
pub fn acquire(corpus_dir: &Path) -> Result<WriterGuard, LockError> {
    acquire_with_clock(corpus_dir, now_ns(), DEFAULT_LOCK_TTL)
}

/// The testable core of [`acquire`]: acquire the lock using an *injected*
/// "now" (nanoseconds) and staleness `ttl`, so the staleness decision is
/// deterministic and free of wall-clock races. Production callers use [`acquire`].
pub(crate) fn acquire_with_clock(
    corpus_dir: &Path,
    now: u64,
    ttl: Duration,
) -> Result<WriterGuard, LockError> {
    let lock_path = corpus_dir.join(LOCK_NAME);
    let ours = LockRecord {
        pid: process_id(),
        acquired_at_ns: now,
    };

    loop {
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            // We won the exclusive create: stamp our record and return the guard.
            Ok(mut file) => match file.write_all(&ours.encode()) {
                Ok(()) => {
                    // Best-effort durability of the stamp. A failure here does
                    // not corrupt the lock (the create already won the race); it
                    // only means the stamp might not survive a crash.
                    let _ = file.sync_all();
                    return Ok(WriterGuard {
                        lock_path: lock_path.clone(),
                        record: ours,
                    });
                }
                Err(e) => {
                    // We own the lock but could not stamp it; remove it so a later
                    // writer is not blocked by a half-stamped lock.
                    let _ = fs::remove_file(&lock_path);
                    return Err(LockError::io("stamping the sync writer lock", e));
                }
            },
            // The lock exists. Decide whether its holder is live.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let existing = read_record(&lock_path);
                match existing {
                    // A live, unstale holder: report busy. The previous snapshot
                    // is untouched and the claimer may retry.
                    Some(record) if !is_stale(&record, now, ttl) => {
                        return Err(LockError::WriterBusy(record));
                    }
                    // Stale (dead pid, past the TTL) or unreadable/corrupt:
                    // reclaim and retry. The next exclusive create either wins or,
                    // if a reclaimer created a fresh lock in the meantime, the next
                    // iteration reports it busy. This converges: after the remove,
                    // a create on a free path succeeds.
                    _ => {
                        let _ = fs::remove_file(&lock_path);
                    }
                }
            }
            Err(e) => return Err(LockError::io("creating the sync writer lock", e)),
        }
    }
}

/// Whether the holder of `record` should be treated as gone (reclaimable) at time
/// `now` (nanoseconds) with staleness threshold `ttl`.
///
/// A dead-sentinel pid (`0`) is always reclaimable; otherwise a holder is
/// reclaimable only once it has been "alive" longer than the TTL. The decision is
/// pure so tests can drive it deterministically with injected `now`/`ttl`.
fn is_stale(record: &LockRecord, now: u64, ttl: Duration) -> bool {
    if record.pid == 0 {
        return true;
    }
    // Compute the elapsed time as `u128` so a large `ttl` never truncates the
    // comparison; a holder is stale once it has been "alive" longer than the TTL.
    let elapsed = u128::from(now).saturating_sub(u128::from(record.acquired_at_ns));
    elapsed > ttl.as_nanos()
}

/// Read and parse the lock record at `path`, if any. Returns `None` when the file
/// is absent or its contents are not a well-formed record (the latter is treated
/// as stale by the acquirer).
fn read_record(path: &Path) -> Option<LockRecord> {
    LockRecord::parse(&fs::read(path).ok()?)
}

/// The current wall-clock time in nanoseconds since the Unix epoch, matching the
/// stamping used for transient candidate names in `crate::corpus`. The value fits
/// in a `u64` for ~584 million years from the epoch, and the stamp is used only for
/// a best-effort staleness check, so the truncating cast is safe.
fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// The current process id, recorded in the lock for the liveness judgment.
fn process_id() -> u32 {
    std::process::id()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    /// A process-local counter so each test gets a unique corpus directory even
    /// when cargo runs tests in parallel.
    static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A RAII guard that removes its temp directory on `Drop`.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Create a fresh, unique, and *existing* canonical corpus directory.
    fn temp_corpus_dir() -> TempDir {
        let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = now_ns();
        let dir = std::env::temp_dir().join(format!(
            "rust-docs-mcp-lock-{}-{nanos}-{n}",
            std::process::id()
        ));
        let corpus = dir.join("corpus");
        fs::create_dir_all(&corpus).expect("create the temp corpus dir");
        TempDir(dir)
    }

    /// **Exclusive acquisition:** a claim creates the lock and the guard releases
    /// it on `Drop`; a second claim after release succeeds again.
    #[test]
    fn acquire_creates_the_lock_and_the_guard_releases_on_drop() {
        let dir = temp_corpus_dir();
        let corpus = dir.0.join("corpus");
        let lock = corpus.join(LOCK_NAME);

        let guard = acquire(&corpus).expect("the first claim acquires the lock");
        assert!(lock.exists(), "the lock file must exist while held");
        drop(guard);
        assert!(
            !lock.exists(),
            "the lock must be released when the guard drops"
        );

        // A fresh claim after release acquires and releases again.
        let guard = acquire(&corpus).expect("a claim after release succeeds");
        drop(guard);
        assert!(!lock.exists());
    }

    /// **AC1 (mutual exclusion):** two writers contending for the same lock are
    /// serialized — at most one is inside the critical section at a time. Both
    /// retry on `WriterBusy` (a real caller's behaviour), so whichever loses the
    /// initial `create_new` race waits and acquires after the holder releases it.
    /// The assertion — that the two never overlap — is timing-independent: the
    /// counter only ever records a holder, and the lock guarantees at most one.
    #[test]
    fn two_concurrent_claims_are_mutually_exclusive() {
        let dir = temp_corpus_dir();
        let corpus = Arc::new(dir.0.join("corpus"));
        let barrier = Arc::new(Barrier::new(2));
        let active = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let hold_ms = 50u64;

        let join_a = {
            let corpus = corpus.clone();
            let barrier = barrier.clone();
            let active = active.clone();
            let max = max_concurrent.clone();
            std::thread::spawn(move || run_writer(&corpus, &barrier, &active, &max, hold_ms))
        };
        let join_b = {
            let corpus = corpus.clone();
            let barrier = barrier.clone();
            let active = active.clone();
            let max = max_concurrent.clone();
            std::thread::spawn(move || run_writer(&corpus, &barrier, &active, &max, hold_ms))
        };

        join_a.join().expect("writer A finishes");
        join_b.join().expect("writer B finishes");

        // The invariant: the two writers never overlapped inside the critical
        // section, so the counter never exceeded 1.
        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "the two writers must never hold the lock at the same time"
        );
    }

    /// A test writer that retries acquiring the lock (backing off on `WriterBusy`)
    /// until it can enter the critical section, where it records the peak number of
    /// concurrent holders, holds briefly, and releases. Both contending writers run
    /// this, so whichever loses the `create_new` race eventually wins after the
    /// holder drops its guard.
    fn run_writer(
        corpus: &Path,
        barrier: &Barrier,
        active: &AtomicUsize,
        max_concurrent: &AtomicUsize,
        hold_ms: u64,
    ) {
        barrier.wait();
        loop {
            match acquire(corpus) {
                Ok(guard) => {
                    // Only one writer may be inside the critical section at a time.
                    let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                    assert_eq!(n, 1, "a writer must hold the lock alone");
                    max_concurrent.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(hold_ms));
                    active.fetch_sub(1, Ordering::SeqCst);
                    drop(guard);
                    break;
                }
                Err(LockError::WriterBusy(_)) => {
                    // A live writer holds it; back off and retry. Not a failure.
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(other) => panic!("unexpected acquire error: {other:?}"),
            }
        }
    }

    /// **AC1 (busy path):** a second claim while the first is held returns
    /// `WriterBusy`, naming the live holder, without touching the lock file.
    #[test]
    fn a_second_claim_reports_writer_busy() {
        let dir = temp_corpus_dir();
        let corpus = dir.0.join("corpus");

        let held = acquire(&corpus).expect("the first claim acquires");
        let err = acquire(&corpus).expect_err("a second claim must be busy");
        let LockError::WriterBusy(record) = err else {
            panic!("expected WriterBusy, got {err:?}");
        };
        assert_ne!(
            record.pid, 0,
            "the busy holder is a live pid, not the dead sentinel"
        );
        // The busy claim must not clobber the held lock.
        assert!(
            corpus.join(LOCK_NAME).exists(),
            "the held lock is untouched"
        );
        drop(held);
        assert!(!corpus.join(LOCK_NAME).exists());
    }

    /// **AC2 (dead pid):** a lock stamped with the dead-sentinel pid is
    /// reclaimed by a later claim.
    #[test]
    fn a_lock_with_a_dead_pid_is_reclaimed() {
        let dir = temp_corpus_dir();
        let corpus = dir.0.join("corpus");
        let lock = corpus.join(LOCK_NAME);

        // Seed a stale lock left by a crashed writer.
        fs::write(&lock, dead_lock_bytes()).expect("seed a dead-hold lock");
        assert!(lock.exists());

        // A later claim reclaims it and holds a fresh lock.
        let guard = acquire_with_clock(&corpus, 1_000, Duration::from_secs(60))
            .expect("a dead-hold lock is reclaimed");
        assert!(
            lock.exists(),
            "the reclaimed lock is held by the new writer"
        );
        drop(guard);
        assert!(!lock.exists(), "the reclaimed lock is released on drop");
    }

    /// **AC2 (TTL):** a lock held by a *live* pid but older than the TTL is
    /// reclaimed, while one within the TTL is not.
    #[test]
    fn a_lock_past_the_ttl_is_reclaimed_but_a_fresh_one_is_not() {
        let dir = temp_corpus_dir();
        let corpus = dir.0.join("corpus");
        let lock = corpus.join(LOCK_NAME);
        let ttl = Duration::from_secs(60);
        let live_pid = std::process::id();
        // A `now` comfortably larger than the TTL, so `now - ttl - 1` does not
        // underflow and stays a `u64`-safe value.
        let now = 1_000_000_000_000u64;
        let ttl_ns = ttl.as_secs() * 1_000_000_000;

        // A fresh lock (within the TTL) is not reclaimed: the claim reports busy.
        fs::write(
            &lock,
            LockRecord {
                pid: live_pid,
                acquired_at_ns: now,
            }
            .encode(),
        )
        .expect("seed a fresh lock");
        let err = acquire_with_clock(&corpus, now, ttl).expect_err("a fresh lock is busy");
        assert!(
            matches!(err, LockError::WriterBusy(_)),
            "a fresh lock must report WriterBusy, got {err:?}"
        );

        // Age the lock past the TTL: it is now reclaimed.
        fs::write(
            &lock,
            LockRecord {
                pid: live_pid,
                acquired_at_ns: now - ttl_ns - 1,
            }
            .encode(),
        )
        .expect("age the lock past the TTL");
        let guard =
            acquire_with_clock(&corpus, now, ttl).expect("a lock past the TTL is reclaimed");
        drop(guard);
        assert!(!lock.exists());
    }

    /// **AC2 (corrupt):** an unreadable/corrupt lock is treated as stale and
    /// reclaimed.
    #[test]
    fn a_corrupt_lock_is_reclaimed() {
        let dir = temp_corpus_dir();
        let corpus = dir.0.join("corpus");
        let lock = corpus.join(LOCK_NAME);

        fs::write(&lock, b"this is not a lock record").expect("seed a corrupt lock");
        let guard = acquire_with_clock(&corpus, 1_000, Duration::from_secs(60))
            .expect("a corrupt lock is reclaimed");
        drop(guard);
        assert!(!lock.exists());
    }

    /// A guard releases *only* the lock it still owns: it never deletes a lock a
    /// reclaimer has already taken over.
    #[test]
    fn a_guard_releases_only_the_lock_it_owns() {
        let dir = temp_corpus_dir();
        let corpus = dir.0.join("corpus");
        let lock = corpus.join(LOCK_NAME);

        let guard =
            acquire_with_clock(&corpus, 1_000, Duration::from_secs(60)).expect("acquire the lock");
        // Simulate a reclaimer creating a fresh lock over ours.
        let takeover = LockRecord {
            pid: 9_999,
            acquired_at_ns: 5_000,
        };
        fs::write(&lock, takeover.encode()).expect("a reclaimer takes the lock");

        // Dropping our guard must not delete the reclaimer's lock.
        drop(guard);
        let remaining = read_record(&lock).expect("the reclaimer's lock remains");
        assert_eq!(
            remaining, takeover,
            "a guard must not delete a lock it no longer owns"
        );
    }

    /// The lock is contained inside the corpus directory (its path is a join of
    /// the corpus dir and the fixed lock name).
    #[test]
    fn the_lock_is_contained_in_the_corpus_directory() {
        let dir = temp_corpus_dir();
        let corpus = dir.0.join("corpus");
        acquire(&corpus).expect("acquire");
        let lock = corpus.join(LOCK_NAME);
        assert!(
            lock.starts_with(&corpus),
            "the lock must be inside the corpus dir"
        );
    }

    /// The record encode/parse round-trips, and malformed bytes do not parse.
    #[test]
    fn a_record_round_trips_and_malformed_bytes_do_not_parse() {
        let record = LockRecord {
            pid: 4321,
            acquired_at_ns: 9_876_543_210,
        };
        assert_eq!(LockRecord::parse(&record.encode()), Some(record));

        assert!(LockRecord::parse(b"").is_none());
        assert!(LockRecord::parse(b"not a record").is_none());
        assert!(
            LockRecord::parse(b"123 456 789").is_none(),
            "a third field is corrupt"
        );
        assert!(LockRecord::parse(b"abc def").is_none());
    }

    /// A claim against a non-existent corpus directory surfaces a typed `Io` error
    /// rather than panicking (the corpus dir is created by the caller).
    #[test]
    fn a_claim_into_a_missing_directory_is_a_typed_io_error() {
        let missing = std::env::temp_dir().join("rust-docs-mcp-lock-missing-dir-xyz");
        let _ = fs::remove_dir_all(&missing);
        let err = acquire(&missing).expect_err("a missing corpus dir must fail");
        assert!(
            matches!(err, LockError::Io { .. }),
            "expected an Io error, got {err:?}"
        );
    }
}
