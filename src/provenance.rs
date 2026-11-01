//! Provenance, compatibility, and freshness semantics — task [008] M2 (issue #8).
//!
//! Task [007] defined the *shape* of the corpus records in [`crate::model`]; the
//! fields that carry provenance (`Source::last_checked`, `Source::fetched_at`,
//! `Source::upstream_modified`, `Source::content_hash`, [`crate::model::Release`])
//! and the compatibility-qualification fields (`Section::qualifications`,
//! `CommandEntry::compatibility`) were deliberately modeled there as **explicit,
//! unknown-preserving holders** with their *semantics* deferred to this module:
//! `plan.md` §5 ("Ingestion, provenance, and freshness") is the specification.
//!
//! This module adds that specialization *over* the [007] holders rather than by
//! changing them, so the versioned on-disk `v1` format and its load gate are
//! untouched and every recorded fixture keeps loading. It owns three concerns:
//!
//! - **Freshness & the freshness/identity split** — [`Instant`],
//!   [`StaleThreshold`], and [`SourceFreshness`] distinguish *download time*
//!   (`fetched_at`), *last-check time* (`last_checked`), and *known upstream
//!   modification time* (`upstream_modified`), and mark a source stale when its
//!   last successful check is older than a **configurable** threshold (default
//!   seven days, per §5). Crucially a conditional HTTP success may update
//!   `last_checked` **without changing content identity**: [`Source::mark_checked_at`]
//!   proves that by leaving `content_hash` and every content field intact.
//! - **Content identity** — a captured content hash is *not* a Rust compiler
//!   version and *not* proof of an upstream publication date. It identifies the
//!   captured bytes only; it is kept decoupled from every freshness timestamp.
//! - **Toolchain compatibility** — [`ToolchainCompat`] +
//!   [`classify_compatibility`] interpret a [`crate::model::Release`]
//!   (`channel` / `documentation_release` / `revision`) without guessing. A
//!   commit-pinned revision (e.g. rustfmt at a recorded git commit) is **never**
//!   treated as documentation for an *installed stable release*; unknown values
//!   stay explicit rather than being manufactured.
//!
//! ## Unknowns are explicit, never guessed
//!
//! Every path that would require a value the evidence does not support returns
//! `None` / [`ToolchainCompat::Unknown`] / [`Unknown`] (channel) instead of a
//! plausible-looking default. `plan.md` §5: "Expose unavailable upstream version
//! information as unknown."
//!
//! ## Scope
//!
//! These APIs are the *model layer* the later M2 tasks consume: `list_sources`
//! and `get_section` freshness surfacing (`[048]`-`[059]`), and the sync pipeline's
//! stalence bookkeeping. Nothing here touches ingestion, network, or command
//! execution. The public items are reachable from the tests in this module and
//! from the consumers above; the module-scoped `allow` mirrors the precedent in
//! [`crate::model`] and will be lifted incrementally as those consumers land.
#![allow(dead_code)]

use crate::model::{Channel, CorpusHash, Release, Source, Timestamp};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A failure to interpret a provenance/freshness value.
///
/// Every variant carries enough context to be actionable rather than a raw parse
/// error, matching `[004]`'s typed-error posture (`plan.md` §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenanceError {
    /// A timestamp string is not a well-formed RFC 3339 UTC/offset instant.
    InvalidTimestamp(String),
}

impl std::fmt::Display for ProvenanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTimestamp(msg) => write!(f, "invalid timestamp: {msg}"),
        }
    }
}

impl std::error::Error for ProvenanceError {}

// ---------------------------------------------------------------------------
// Freshness time
// ---------------------------------------------------------------------------

/// A point in time, stored as whole **seconds since the Unix epoch** (UTC).
///
/// Whole seconds are the only resolution staleness needs: freshness is judged on
/// a multi-day window, and the captured timestamps are RFC 3339 UTC. Ordering
/// (`Ord`/`PartialOrd`) is therefore the natural comparison for "is `a` after
/// `b`?", which is all freshness ever asks. The field is deliberately not
/// `pub` so construction goes through a validated parser and no unvalidated
/// epoch can be spliced in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant {
    secs: i64,
}

impl Instant {
    /// Parse a RFC 3339 timestamp (`YYYY-MM-DDThh:mm:ss[.frac]Z` or with an
    /// `±hh:mm` offset) into whole UTC seconds.
    ///
    /// Fractional seconds are truncated to the second — sufficient for a
    /// multi-day staleness window and keeps the parser dependency-free
    /// (`plan.md` §5/§10: add hashing/time deps only as needed; a full-precision
    /// sub-second model is out of scope for [008]). An offset other than `Z` is
    /// normalised to UTC by subtracting it.
    pub fn parse(value: impl AsRef<str>) -> Result<Instant, ProvenanceError> {
        let s = value.as_ref();
        let (date_part, rest) = s.split_once('T').ok_or_else(|| {
            ProvenanceError::InvalidTimestamp(format!("{s:?} has no `T` date/time separator"))
        })?;

        let (year, month, day) = parse_date(date_part)?;
        // The remainder is `h:m:s[.frac]` followed by a timezone (`Z` or `±hh:mm`).
        // The first `Z`/`+`/`-` closes the time stem; nothing before it holds a
        // sign, so this partition is unambiguous.
        let (time_stem, tz_part) = split_time_and_tz(rest)?;
        let (hour, minute, second) = parse_time(time_stem)?;
        let utc_offset_secs = parse_tz_offset(tz_part)?;

        let days = civil_days_from_epoch(year, month, day);
        let secs = days * 86_400 + hour * 3_600 + minute * 60 + second - utc_offset_secs;
        Ok(Instant { secs })
    }

    /// The instant expressed in whole seconds since the Unix epoch.
    pub fn as_secs(self) -> i64 {
        self.secs
    }

    /// Whole seconds elapsed from `other` to `self` (positive if `self` is later).
    pub fn duration_secs(self, other: Instant) -> i64 {
        self.secs - other.secs
    }

    /// Whether `self` is strictly later than `other`.
    pub fn after(self, other: Instant) -> bool {
        self.secs > other.secs
    }

    /// Whether `self` is more than `secs` after `other`.
    pub fn exceeds_by(self, other: Instant, secs: i64) -> bool {
        self.duration_secs(other) > secs
    }
}

/// Tighten a [007] [`Timestamp`] into a comparable instant.
///
/// [007] validated a `Timestamp` only as non-empty/whitespace-free ASCII and
/// explicitly deferred grammar-tightening to [008]. This is that specialization:
/// it parses the stored string as RFC 3339. It is *additive* — `Timestamp::new`
/// still accepts any validated string, so no recorded `v1` fixture is disturbed —
/// and a stored value that cannot parse yields [`ProvenanceError::InvalidTimestamp`]
/// rather than a guess.
impl Timestamp {
    /// Parse this timestamp's stored string as a RFC 3339 UTC/offset instant.
    pub fn as_instant(&self) -> Result<Instant, ProvenanceError> {
        Instant::parse(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Stale threshold
// ---------------------------------------------------------------------------

/// How old a source's last successful check may be before it is *stale*.
///
/// `plan.md` §5: "mark a source stale when its last successful upstream check is
/// older than seven days; make the threshold configurable." The default is seven
/// days; the value is a plain count of days so the runtime can reconfigure it
/// without changing the model. Stored as `i64` so a threshold never multiplies
/// into `i32` overflow for a large reconfigure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StaleThreshold {
    days: i64,
}

impl StaleThreshold {
    /// A threshold of `days` days. A non-positive window is accepted as the
    /// model's job is to *compare* (a `0`-day window means "immediately stale");
    /// callers decide whether that is meaningful.
    pub fn days(days: i64) -> Self {
        Self { days }
    }

    /// The configured window in seconds.
    pub fn seconds(self) -> i64 {
        self.days * 86_400
    }

    /// The window in days.
    pub fn as_days(self) -> i64 {
        self.days
    }
}

impl Default for StaleThreshold {
    /// The plan's seven-day default.
    fn default() -> Self {
        Self { days: 7 }
    }
}

// ---------------------------------------------------------------------------
// Freshness of a source
// ---------------------------------------------------------------------------

/// The freshness of a single [`Source`] judged against an `as_of` instant and a
/// [`StaleThreshold`].
///
/// `stale` is `None` when the source's last successful check is unknown: the
/// model **preserves that unknown explicitly** and does not guess a
/// fresh/stale verdict from an absent `last_checked`. When `last_checked` is
/// known, `stale` is `Some(true)` iff the check is older than the threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceFreshness {
    /// The "now" the verdict was judged against.
    pub as_of: Instant,
    /// The source's last successful check, when known.
    pub last_checked: Option<Instant>,
    /// The threshold the verdict used.
    pub threshold: StaleThreshold,
    /// `Some(true)`/`Some(false)` when `last_checked` is known; `None` when it
    /// is unknown. Never guessed.
    pub stale: Option<bool>,
}

impl SourceFreshness {
    /// The stale verdict, or `None` when freshness is unknown.
    pub fn is_stale(self) -> Option<bool> {
        self.stale
    }
}

/// Judge a `source`'s freshness at `as_of` using the configured `threshold`.
///
/// Pass [`StaleThreshold::default`] for the plan's seven-day default, or a
/// different [`StaleThreshold::days`] to reconfigure the window. The threshold
/// is an explicit parameter so the window stays configurable without changing the
/// model (per `plan.md` §5).
///
/// This is the single place the "download time vs last-check time vs upstream
/// modification time" distinction becomes actionable: only the *last-check*
/// timestamp participates in staleness. `fetched_at` (when this content was
/// downloaded) and `upstream_modified` (when the source last changed upstream)
/// are recorded but are **not** freshness inputs — conflating them would turn a
/// conditional no-op check into an apparent stale source, or a re-fetch into an
/// apparent code change.
pub fn evaluate_freshness(
    source: &Source,
    as_of: Instant,
    threshold: StaleThreshold,
) -> SourceFreshness {
    // A present-but-unparseable last-check is *not* "known": we cannot judge
    // staleness from it, so it collapses to `None` just as an absent check does,
    // preserving the "unknown" verdict rather than guessing.
    let last_checked = source
        .last_checked
        .as_ref()
        .and_then(|ts| ts.as_instant().ok());
    let stale = last_checked.map(|last| as_of.exceeds_by(last, threshold.seconds()));
    SourceFreshness {
        as_of,
        last_checked,
        threshold,
        stale,
    }
}

/// A conditional HTTP success (a `304 Not Modified`) updates a source's
/// **last-check time without changing its content identity**.
///
/// Returns a clone of `source` with only `last_checked` set to `new_check`; the
/// `content_hash` and every content field are untouched, so the captured-content
/// identity `Source::content_hash` returns is identical before and after. This
/// is the model-level expression of `plan.md` §5: "A conditional HTTP success may
/// update last-check metadata without changing content identity."
impl Source {
    /// Produce a copy of this source with its `last_checked` advanced to
    /// `new_check`, preserving content identity.
    pub fn mark_checked_at(&self, new_check: Timestamp) -> Source {
        let mut next = self.clone();
        next.last_checked = Some(new_check);
        next
    }
}

// ---------------------------------------------------------------------------
// Content identity
// ---------------------------------------------------------------------------

/// The captured-content identity of a source: its content hash.
///
/// A content hash identifies *only* the captured bytes; per `plan.md` §5 it is
/// NOT a Rust compiler version and NOT proof of an upstream publication date.
/// Keeping it a distinct value (never conflated with any timestamp) is what lets
/// freshness move while identity is held.
pub fn content_identity(source: &Source) -> &CorpusHash {
    &source.content_hash
}

// ---------------------------------------------------------------------------
// Toolchain compatibility
// ---------------------------------------------------------------------------

/// The toolchain-compatibility stance a [`Release`] supports, derived **without
/// guessing**.
///
/// `plan.md` §5: "Distinguish `stable`, `nightly-only`, `rolling`, and `unknown`
/// where supported by evidence; a commit-pinned rustfmt document is not
/// automatically documentation for an installed stable release."
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToolchainCompat {
    /// The release is identifiable stable documentation for a specific toolchain
    /// version — a `Stable` channel *with* a `documentation_release`.
    Stable {
        /// The identified toolchain/release version (e.g. `1.88.0`).
        release: String,
    },
    /// A nightly-only qualification, preserved within the stable manuals.
    Nightly,
    /// A rolling / independently-versioned channel (e.g. rustup).
    Rolling {
        /// The recorded upstream revision, when one is pinned; otherwise
        /// the rolling channel is known but its exact revision is not.
        revision: Option<String>,
    },
    /// A pinned upstream `revision` (e.g. a commit) with **no** identified stable
    /// release. This is explicitly *not* `Stable`: a commit-pinned document does
    /// not establish documentation for an installed stable release.
    CommitPinned {
        /// The pinned upstream revision (e.g. a git commit).
        revision: String,
    },
    /// No usable channel/release/revision evidence. Preserved as unknown rather
    /// than inferred.
    Unknown,
}

/// Classify what toolchain a [`Release`] describes, using only the evidence its
/// fields carry. No default is synthesized where the evidence is missing.
///
/// The key rule, exercised by the acceptance tests, is that a pinned `revision`
/// with no identified `documentation_release` yields [`ToolchainCompat::CommitPinned`]
/// on *any* channel (even `Stable`): a commit pin is upstream source provenance,
/// not a claim that an installed stable toolchain matches it.
pub fn classify_compatibility(release: &Release) -> ToolchainCompat {
    match release.channel {
        Channel::Stable => {
            // A present-but-blank `documentation_release` reads as *absent*, not
            // as an empty version number. Per `plan.md` §5 we "expose
            // unavailable upstream version information as unknown" rather than
            // surface a fabricated `""`/whitespace value in `list_sources`.
            match release
                .documentation_release
                .as_deref()
                .map(str::trim)
                .filter(|release| !release.is_empty())
            {
                Some(release) => ToolchainCompat::Stable {
                    release: release.to_string(),
                },
                None => {
                    // No identified stable release. A pinned revision (even on a
                    // `Stable` channel) is an upstream commit pin, never an
                    // installed stable release -> CommitPinned.
                    if let Some(rev) = &release.revision {
                        ToolchainCompat::CommitPinned {
                            revision: rev.clone(),
                        }
                    } else {
                        ToolchainCompat::Unknown
                    }
                }
            }
        }
        Channel::Nightly => ToolchainCompat::Nightly,
        Channel::Rolling => ToolchainCompat::Rolling {
            revision: release.revision.clone(),
        },
        Channel::Unknown => ToolchainCompat::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Dependency-free RFC 3339 helpers
// ---------------------------------------------------------------------------

/// Whether `year` is a leap year in the proleptic Gregorian calendar.
///
/// Divisible by 4, except centuries, except four-century years. This mirrors the
/// calendar [`civil_days_from_epoch`] already assumes, so a validated date and
/// its epoch mapping stay consistent.
fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The number of days in the given Gregorian `(year, month)`.
///
/// `month` is assumed to already be validated as `1..=12` by the caller, so this
/// only ever returns a real month length (`28..=31`); the `0` arm exists only to
/// make the `match` exhaustive, not to be used in a comparison.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Split a date stem (`YYYY-MM-DD`) into `(year, month, day)`, validating ranges.
fn parse_date(date_part: &str) -> Result<(i64, i64, i64), ProvenanceError> {
    let mut parts = date_part.splitn(3, '-');
    let year = parts
        .next()
        .ok_or_else(|| ProvenanceError::InvalidTimestamp("missing year".into()))?;
    let month = parts
        .next()
        .ok_or_else(|| ProvenanceError::InvalidTimestamp("missing month".into()))?;
    let day = parts
        .next()
        .ok_or_else(|| ProvenanceError::InvalidTimestamp("missing day".into()))?;
    if parts.next().is_some() {
        return Err(ProvenanceError::InvalidTimestamp(
            "date has more than three `-`-separated fields".into(),
        ));
    }
    let year = parse_int_field(year, "year")?;
    let month = parse_int_field(month, "month")?;
    let day = parse_int_field(day, "day")?;
    if !matches!(month, 1..=12) {
        return Err(ProvenanceError::InvalidTimestamp(format!(
            "month out of range: {month}"
        )));
    }
    // Day must be a real day *for that month*, leap-year aware. Without this,
    // an impossible date such as `2024-02-31` would silently roll to an adjacent
    // day via [`civil_days_from_epoch`]; instead it is rejected as malformed.
    let max_day = days_in_month(year, month);
    if day < 1 || day > max_day {
        return Err(ProvenanceError::InvalidTimestamp(format!(
            "day out of range for {year}-{month} month: {day} (max {max_day})"
        )));
    }
    Ok((year, month, day))
}

/// Split a time+tz remainder into `(time_stem, tz_part)`. The first `Z`/`+`/`-`
/// terminates the `h:m:s[.frac]` stem; if none is present the instant is UTC.
fn split_time_and_tz(rest: &str) -> Result<(&str, &str), ProvenanceError> {
    match rest.find(['Z', '+', '-']) {
        Some(idx) => Ok((&rest[..idx], &rest[idx..])),
        None => Ok((rest, "Z")),
    }
}

/// Parse a `h:m:s` stem (a trailing `.frac` is allowed and ignored — seconds
/// granularity is all staleness needs).
fn parse_time(time_stem: &str) -> Result<(i64, i64, i64), ProvenanceError> {
    let stem = match time_stem.find('.') {
        Some(idx) => &time_stem[..idx],
        None => time_stem,
    };
    let mut parts = stem.splitn(3, ':');
    let h = parts
        .next()
        .ok_or_else(|| ProvenanceError::InvalidTimestamp("missing hour".into()))?;
    let m = parts
        .next()
        .ok_or_else(|| ProvenanceError::InvalidTimestamp("missing minute".into()))?;
    let s = parts
        .next()
        .ok_or_else(|| ProvenanceError::InvalidTimestamp("missing second".into()))?;
    if parts.next().is_some() {
        return Err(ProvenanceError::InvalidTimestamp(
            "time has too many fields".into(),
        ));
    }
    let hour = parse_int_field(h, "hour")?;
    let minute = parse_int_field(m, "minute")?;
    let second = parse_int_field(s, "second")?;
    if hour > 23 {
        return Err(ProvenanceError::InvalidTimestamp(format!(
            "hour out of range: {hour}"
        )));
    }
    if minute > 59 {
        return Err(ProvenanceError::InvalidTimestamp(format!(
            "minute out of range: {minute}"
        )));
    }
    if second > 60 {
        return Err(ProvenanceError::InvalidTimestamp(format!(
            "second out of range: {second}"
        )));
    }
    Ok((hour, minute, second))
}

/// Parse a timezone token (`Z`, `+hh:mm`, `-hh:mm`) into the whole-second UTC
/// offset it represents.
fn parse_tz_offset(tz_part: &str) -> Result<i64, ProvenanceError> {
    let tz = match tz_part.strip_prefix('Z') {
        Some("") => return Ok(0),
        Some(rest) => rest,
        None => tz_part,
    };
    if tz.is_empty() {
        return Err(ProvenanceError::InvalidTimestamp(
            "trailing timezone with no value".into(),
        ));
    }
    let sign = match tz.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        other => {
            return Err(ProvenanceError::InvalidTimestamp(format!(
                "timezone sign is not +/- (found {other:?})"
            )));
        }
    };
    let body = &tz[1..];
    let mut bits = body.splitn(3, ':');
    let h = bits
        .next()
        .ok_or_else(|| ProvenanceError::InvalidTimestamp("missing offset hour".into()))?;
    let m = bits
        .next()
        .ok_or_else(|| ProvenanceError::InvalidTimestamp("missing offset minute".into()))?;
    if bits.next().is_some() {
        return Err(ProvenanceError::InvalidTimestamp(
            "offset has too many fields".into(),
        ));
    }
    let oh = parse_int_field(h, "offset hour")?;
    let om = parse_int_field(m, "offset minute")?;
    // Bound the offset to a real RFC 3339 zone: hours `0..=14` and minutes
    // `0..=59`. [`parse_int_field`] already rejects a negative magnitude, so the
    // only remaining failure here is an upper-bound violation such as `+25:99`;
    // without this check that would be silently accepted.
    if oh > 14 || om > 59 {
        return Err(ProvenanceError::InvalidTimestamp(format!(
            "timezone offset out of range: {oh:02}:{om:02}"
        )));
    }
    Ok(sign * (oh * 3_600 + om * 60))
}

/// Parse a run of ASCII digits into a non-negative integer named `field`.
///
/// The magnitude must be non-negative: every caller (date/time components and
/// the offset hour/minute) requires it, and the timezone sign is extracted
/// separately as a literal `+`/`-` in [`parse_tz_offset`]. A magnitude carrying
/// its own sign (e.g. the `-5` in the double-signed `+-5:30`) is therefore
/// rejected as malformed rather than accepted as a meaningless offset.
fn parse_int_field(field: &str, name: &str) -> Result<i64, ProvenanceError> {
    if field.is_empty() {
        return Err(ProvenanceError::InvalidTimestamp(format!(
            "{name} is empty"
        )));
    }
    let value = field.parse::<i64>().map_err(|_| {
        ProvenanceError::InvalidTimestamp(format!("{name} is not an integer: {field:?}"))
    })?;
    if value < 0 {
        return Err(ProvenanceError::InvalidTimestamp(format!(
            "{name} must be non-negative: {field:?}"
        )));
    }
    Ok(value)
}

/// Days since the Unix epoch for a proleptic Gregorian `(year, month, day)`.
///
/// Howard Hinnant's `days_from_civil` (the 400-year era, 146097 days each),
/// dependency-free and correct across the negative/positive year boundary; the
/// recorded documentation timestamps are all non-negative. Verified by the
/// `instant_epoch_is_zero_for_unix_epoch` test against the known epoch `0`.
fn civil_days_from_epoch(year: i64, month: i64, day: i64) -> i64 {
    let y = year - (if month <= 2 { 1 } else { 0 });
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + (day - 1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (doy as u64);
    era * 146_097 + doe as i64 - 719_468
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
//
// The two issue #8 acceptance criteria are demonstrated here with offline,
// fixture-free data:
//   1. "Unknown release/revision values remain explicit" — `unknown_*` tests.
//   2. "Tests distinguish captured-content identity, last check, fetch time,
//      upstream modification, and toolchain compatibility" — `distinguish_*`.
//
// No network, no third-party crates: timestamps are short RFC 3339 literals and
// [`civil_days_from_epoch`] is verified against known epoch values.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Channel, CorpusHash, Release, Source, Timestamp, Url};

    // A minimal, dependency-free `Source` with only the fields the tests set;
    // everything else defaults so a source carries the full required shape while
    // the tests stay focused on the provenance fields.
    fn source(
        last_checked: Option<&str>,
        fetched_at: &str,
        upstream_modified: Option<&str>,
        channel: Channel,
        documentation_release: Option<&str>,
        revision: Option<&str>,
        content: &[u8],
    ) -> Source {
        Source {
            id: crate::model::SourceId::new("cargo").unwrap(),
            title: "The Cargo Book".into(),
            entry_url: Url::new("https://doc.rust-lang.org/cargo/").unwrap(),
            document_urls: vec![],
            last_checked: last_checked.map(|s| Timestamp::new(s).unwrap()),
            fetched_at: Timestamp::new(fetched_at).unwrap(),
            upstream_modified: upstream_modified.map(|s| Timestamp::new(s).unwrap()),
            content_hash: CorpusHash::compute(content),
            release: Release {
                channel,
                documentation_release: documentation_release.map(str::to_string),
                revision: revision.map(str::to_string),
            },
            parser: "books/2024".into(),
        }
    }

    // A known-epoch reference: 1970-01-01T00:00:00Z is epoch 0.
    #[test]
    fn instant_epoch_is_zero_for_unix_epoch() {
        assert_eq!(Instant::parse("1970-01-01T00:00:00Z").unwrap().as_secs(), 0);
    }

    #[test]
    fn instant_parses_offset_into_utc() {
        // "2024-01-01T12:00:00+01:00" == "2024-01-01T11:00:00Z".
        let with_offset = Instant::parse("2024-01-01T12:00:00+01:00")
            .unwrap()
            .as_secs();
        let as_utc = Instant::parse("2024-01-01T11:00:00Z").unwrap().as_secs();
        assert_eq!(with_offset, as_utc);
    }

    #[test]
    fn instant_rejects_malformed_input() {
        assert!(Instant::parse("not-a-timestamp").is_err());
        assert!(Instant::parse("2024-13-01T00:00:00Z").is_err()); // month out of range
        assert!(Instant::parse("2024-01-01").is_err()); // no time/tz
        assert!(Instant::parse("").is_err());
    }

    #[test]
    fn timestamp_as_instant_tightens_grammar_additively() {
        // [007] accepted a non-empty ASCII string; [008] tightens the *stored*
        // value to a real RFC 3339 instant via `as_instant`, without changing
        // `Timestamp::new` (so v1 fixtures that already passed still load).
        assert!(
            Timestamp::new("2026-10-30T00:00:00Z")
                .unwrap()
                .as_instant()
                .is_ok()
        );
        assert!(Timestamp::new("garbage").unwrap().as_instant().is_err());
    }

    // Criterion 1: unknown release/revision remain explicit.
    #[test]
    fn unknown_release_and_revision_remain_explicit() {
        let release = Release::default();
        assert_eq!(release.channel, Channel::Unknown);
        assert!(
            release.documentation_release.is_none(),
            "release unknown stays None"
        );
        assert!(release.revision.is_none(), "revision unknown stays None");

        // The classification of an all-unknown release is Unknown, never a guess.
        assert_eq!(classify_compatibility(&release), ToolchainCompat::Unknown);

        // A "Stable" channel with neither a documentation_release nor a revision
        // is still Unknown: we do not invent a version.
        let stable_no_evidence = Release {
            channel: Channel::Stable,
            documentation_release: None,
            revision: None,
        };
        assert_eq!(
            classify_compatibility(&stable_no_evidence),
            ToolchainCompat::Unknown
        );

        // Round-trip through JSON keeps the unknowns explicit (optionals skipped,
        // so "unknown" is the absence, not a fabricated default).
        let json = serde_json::to_string(&release).unwrap();
        assert!(
            !json.contains("documentation_release"),
            "unknown release omits field: {json}"
        );
        assert!(
            !json.contains("revision"),
            "unknown revision omits field: {json}"
        );
        let back: Release = serde_json::from_str(&json).unwrap();
        assert_eq!(back, release);
    }

    // Criterion 1 also: an unknown last-checked time yields an *unknown* verdict.
    #[test]
    fn freshness_is_unknown_when_last_check_is_unknown() {
        let s = source(
            None,
            "2026-10-30T00:00:00Z",
            None,
            Channel::Stable,
            Some("1.88.0"),
            None,
            b"cargo",
        );
        let f = evaluate_freshness(
            &s,
            Instant::parse("2026-11-01T00:00:00Z").unwrap(),
            StaleThreshold::default(),
        );
        assert!(
            f.last_checked.is_none(),
            "no last_check -> unknown freshness"
        );
        assert_eq!(
            f.stale, None,
            "an unknown check must not produce a guessed verdict"
        );
        assert_eq!(f.is_stale(), None);
    }

    // Criterion 2: captured-content identity, last check, fetch time, and
    // upstream modification are four distinct, independently settable fields.
    #[test]
    fn distinguish_identity_last_check_fetch_upstream_modified() {
        // All four concepts carry distinct, non-equal evidence on one source.
        let s = source(
            Some("2026-10-28T00:00:00Z"), // last checked
            "2026-10-30T00:00:00Z",       // fetched (download time)
            Some("2026-10-27T00:00:00Z"), // upstream modified
            Channel::Stable,
            Some("1.88.0"),
            None,
            b"content-bytes",
        );
        let last = s.last_checked.as_ref().unwrap().as_instant().unwrap();
        let fetched = s.fetched_at.as_instant().unwrap();
        let upstream = s.upstream_modified.as_ref().unwrap().as_instant().unwrap();
        let identity = content_identity(&s);

        // Each pairwise comparison differs: the concepts do not collapse.
        assert_ne!(fetched, last, "fetch time differs from last check");
        assert_ne!(
            upstream, last,
            "upstream modification differs from last check"
        );
        assert_ne!(
            upstream, fetched,
            "upstream modification differs from fetch time"
        );
        assert!(
            fetched.after(last),
            "download happened after the last check"
        );
        assert!(
            last.after(upstream),
            "last check happened after upstream modified"
        );

        // Content identity is decoupled from every timestamp: it is a hash, not a
        // time. Mutating any timestamp must not move it.
        assert_eq!(
            &s.content_hash, identity,
            "content identity is the hash itself"
        );
    }

    // The freshness/identity split: a conditional HTTP success updates the last
    // check WITHOUT changing content identity.
    #[test]
    fn conditional_check_updates_time_but_not_content_identity() {
        let before = source(
            Some("2026-10-28T00:00:00Z"),
            "2026-10-28T00:00:00Z",
            Some("2026-10-28T00:00:00Z"),
            Channel::Stable,
            Some("1.88.0"),
            None,
            b"stable-bytes",
        );
        let after_noop = before.mark_checked_at(Timestamp::new("2026-11-04T00:00:00Z").unwrap());

        // Content identity is held by a conditional (304) check.
        assert_eq!(
            content_identity(&before),
            content_identity(&after_noop),
            "a conditional check must not change content identity"
        );
        assert_eq!(
            after_noop.fetched_at, before.fetched_at,
            "a conditional check must not record a new download time"
        );
        assert_eq!(
            after_noop.upstream_modified, before.upstream_modified,
            "a conditional check must not move the known upstream-modified time"
        );
        // But the last check advanced.
        let last = after_noop
            .last_checked
            .as_ref()
            .unwrap()
            .as_instant()
            .unwrap();
        assert_eq!(last, Instant::parse("2026-11-04T00:00:00Z").unwrap());
        assert!(last.after(before.last_checked.unwrap().as_instant().unwrap()));
    }

    #[test]
    fn stale_verdict_uses_configurable_threshold() {
        let as_of = Instant::parse("2026-11-04T00:00:00Z").unwrap();
        let base = source(
            Some("2026-10-28T00:00:00Z"), // 7 days before as_of
            "2026-10-28T00:00:00Z",
            None,
            Channel::Stable,
            Some("1.88.0"),
            None,
            b"x",
        );

        // Exactly seven days is NOT "older than seven days" -> fresh.
        let fresh = evaluate_freshness(&base, as_of, StaleThreshold::days(7));
        assert_eq!(
            fresh.stale,
            Some(false),
            "exactly 7 days is not *older* than 7"
        );

        // A shorter window makes the same check stale -> the threshold is configurable.
        let short = evaluate_freshness(&base, as_of, StaleThreshold::days(1));
        assert_eq!(
            short.stale,
            Some(true),
            "the same check is stale under a 1-day window"
        );

        // A far larger window makes it fresh.
        let long = evaluate_freshness(&base, as_of, StaleThreshold::days(3650));
        assert_eq!(long.stale, Some(false), "a large window keeps it fresh");
    }

    #[test]
    fn stale_threshold_defaults_to_seven_days() {
        assert_eq!(StaleThreshold::default().as_days(), 7);
        assert_eq!(StaleThreshold::default().seconds(), 7 * 86_400);
    }

    #[test]
    fn a_fresh_check_is_not_stale() {
        let as_of = Instant::parse("2026-10-31T00:00:00Z").unwrap();
        let s = source(
            Some("2026-10-28T00:00:00Z"),
            "2026-10-28T00:00:00Z",
            None,
            Channel::Stable,
            Some("1.88.0"),
            None,
            b"x",
        );
        let f = evaluate_freshness(&s, as_of, StaleThreshold::default());
        assert_eq!(f.stale, Some(false), "a check within 7 days is fresh");
    }

    // Criterion 2 (toolchain compatibility): a commit-pinned rustfmt is NOT an
    // installed stable release; genuine distinct channels classify distinctly.
    #[test]
    fn commit_pin_is_not_an_installed_stable_release() {
        // rustfmt is captured at a recorded git commit, a rolling-channel source
        // whose revision is a commit. That must not be read as a Stable release.
        let rustfmt = Release {
            channel: Channel::Rolling,
            documentation_release: None,
            revision: Some("rustfmt@a1b2c3d".into()),
        };
        assert_eq!(
            classify_compatibility(&rustfmt),
            ToolchainCompat::Rolling {
                revision: Some("rustfmt@a1b2c3d".into())
            },
            "a rolling/commit-pinned source is rolling, not stable"
        );

        // Even on a `Stable` channel, a bare commit revision with no identified
        // release is a CommitPinned source, never Stable — the explicit rule in
        // plan.md §5.
        let stable_commit = Release {
            channel: Channel::Stable,
            documentation_release: None,
            revision: Some("deadbeef".into()),
        };
        assert_eq!(
            classify_compatibility(&stable_commit),
            ToolchainCompat::CommitPinned {
                revision: "deadbeef".into()
            },
            "a commit pin on a Stable channel is not an installed stable release"
        );
    }

    #[test]
    fn genuine_distinct_channels_classify_distinctly() {
        // Stable with an identified release -> Stable.
        let stable = Release {
            channel: Channel::Stable,
            documentation_release: Some("1.88.0".into()),
            revision: None,
        };
        assert_eq!(
            classify_compatibility(&stable),
            ToolchainCompat::Stable {
                release: "1.88.0".into()
            },
        );

        // Nightly-only qualification is preserved within the stable manuals.
        let nightly = Release::channel_only(Channel::Nightly);
        assert_eq!(classify_compatibility(&nightly), ToolchainCompat::Nightly);

        // Rolling (e.g. rustup) with a recorded revision.
        let rolling = Release {
            channel: Channel::Rolling,
            documentation_release: None,
            revision: Some("rustup-<rev>".into()),
        };
        assert_eq!(
            classify_compatibility(&rolling),
            ToolchainCompat::Rolling {
                revision: Some("rustup-<rev>".into())
            },
        );

        // Unknown channel and (separately) a Stable-with-no-evidence both stay
        // explicit-unknown rather than a guess.
        assert_eq!(
            classify_compatibility(&Release::default()),
            ToolchainCompat::Unknown
        );
    }

    #[test]
    fn compatibility_distinguishes_stable_from_nightly_from_rolling_from_unknown() {
        // One test that names all four §5 categories explicitly so the "distinguish
        // toolchain compatibility" criterion is demonstrated in a single place.
        let stable = classify_compatibility(&Release {
            channel: Channel::Stable,
            documentation_release: Some("1.80.0".into()),
            revision: None,
        });
        let nightly = classify_compatibility(&Release::channel_only(Channel::Nightly));
        let rolling = classify_compatibility(&Release {
            channel: Channel::Rolling,
            documentation_release: None,
            revision: Some("r".into()),
        });
        let unknown = classify_compatibility(&Release::default());

        assert_eq!(
            stable,
            ToolchainCompat::Stable {
                release: "1.80.0".into()
            }
        );
        assert_eq!(nightly, ToolchainCompat::Nightly);
        assert_eq!(
            rolling,
            ToolchainCompat::Rolling {
                revision: Some("r".into())
            }
        );
        assert_eq!(unknown, ToolchainCompat::Unknown);
        // All four verdicts are pair-distinct.
        let all = [stable, nightly, rolling, unknown];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "the four compatibility verdicts must differ");
            }
        }
    }

    // P3-1 hardening: a present-but-blank `documentation_release` must read as
    // *unknown*, never an empty/whitespace version. (§5: expose unavailable
    // upstream version information as unknown.)
    #[test]
    fn blank_documentation_release_is_unknown_not_empty_stable() {
        let empty = Release {
            channel: Channel::Stable,
            documentation_release: Some(String::new()),
            revision: None,
        };
        assert_eq!(
            classify_compatibility(&empty),
            ToolchainCompat::Unknown,
            "an empty documentation_release is unknown, not an empty version"
        );

        for whitespace in ["   ", "\t", "\n", " \t\n "] {
            let padded = Release {
                channel: Channel::Stable,
                documentation_release: Some(whitespace.to_string()),
                revision: None,
            };
            assert_eq!(
                classify_compatibility(&padded),
                ToolchainCompat::Unknown,
                "a whitespace documentation_release {whitespace:?} is unknown"
            );
        }

        // A blank release plus a pinned revision is still a commit pin, never
        // `Stable { release: "" }`.
        let blank_pinned = Release {
            channel: Channel::Stable,
            documentation_release: Some("   ".into()),
            revision: Some("deadbeef".into()),
        };
        assert_eq!(
            classify_compatibility(&blank_pinned),
            ToolchainCompat::CommitPinned {
                revision: "deadbeef".into()
            },
            "a blank release with a revision is a commit pin, not an empty stable"
        );

        // A genuinely identified release (surrounded by incidental whitespace)
        // still resolves to Stable using its trimmed value.
        let padded_real = Release {
            channel: Channel::Stable,
            documentation_release: Some("  1.88.0 ".into()),
            revision: None,
        };
        assert_eq!(
            classify_compatibility(&padded_real),
            ToolchainCompat::Stable {
                release: "1.88.0".into()
            },
            "a real release with surrounding whitespace trims to its value"
        );
    }

    // P3-2 hardening: an impossible calendar day is rejected rather than
    // silently rolling to an adjacent day via `civil_days_from_epoch`.
    #[test]
    fn invalid_calendar_days_are_rejected() {
        // Feb 31 does not exist in any year and must error, not map to Mar 2/3.
        assert!(
            Instant::parse("2024-02-31T00:00:00Z").is_err(),
            "Feb 31 is not a valid day"
        );
        // 2023 is not a leap year, so Feb 29 2023 is invalid too.
        assert!(
            Instant::parse("2023-02-29T00:00:00Z").is_err(),
            "Feb 29 2023 is not a leap day"
        );
        assert!(
            Instant::parse("2023-02-31T00:00:00Z").is_err(),
            "Feb 31 2023 is not a leap day"
        );
        // A real leap day still parses.
        assert!(
            Instant::parse("2024-02-29T00:00:00Z").is_ok(),
            "Feb 29 2024 is a valid leap day"
        );
        // A 30-day month rejects the 31st without regressing a valid 30th.
        assert!(Instant::parse("2024-04-31T00:00:00Z").is_err());
        assert!(Instant::parse("2024-04-30T00:00:00Z").is_ok());
    }

    // P3-2 hardening: an out-of-range timezone offset is rejected.
    #[test]
    fn out_of_range_timezone_offsets_are_rejected() {
        // Offset hours beyond ±14:00 are not valid RFC 3339 offsets.
        assert!(
            Instant::parse("2024-01-01T00:00:00+25:00").is_err(),
            "+25:00 has an out-of-range offset hour"
        );
        assert!(
            Instant::parse("2024-01-01T00:00:00-25:00").is_err(),
            "-25:00 has an out-of-range offset hour"
        );
        // Minutes beyond 59 are not valid offsets.
        assert!(
            Instant::parse("2024-01-01T00:00:00+15:99").is_err(),
            "+15:99 has an out-of-range offset minute"
        );
        // Valid offsets (including the ±14:59 extremes) still parse.
        assert!(
            Instant::parse("2024-01-01T00:00:00+14:59").is_ok(),
            "+14:59 is a valid offset"
        );
        assert!(
            Instant::parse("2024-01-01T00:00:00-14:59").is_ok(),
            "-14:59 is a valid offset"
        );
        // A valid non-zero offset still normalises to UTC as before.
        let with_offset = Instant::parse("2024-01-01T12:00:00+02:30")
            .expect("+02:30 is valid")
            .as_secs();
        let as_utc = Instant::parse("2024-01-01T09:30:00Z")
            .expect("UTC equivalent is valid")
            .as_secs();
        assert_eq!(with_offset, as_utc);
    }

    #[test]
    fn double_signed_timezone_offsets_are_rejected() {
        // A magnitude that carries its own sign after the leading `+`/`-` is
        // stripped. `parse_tz_offset` now routes every offset component through
        // `parse_int_field`, whose non-negative contract rejects it, so a
        // double-signed body is an InvalidTimestamp rather than a silently
        // accepted meaningless offset.
        assert!(
            Instant::parse("2024-01-01T00:00:00+-5:30").is_err(),
            "+-5:30 has a negative offset-hour magnitude"
        );
        assert!(
            Instant::parse("2024-01-01T00:00:00+15:-1").is_err(),
            "+15:-1 has a negative offset-minute magnitude"
        );
        assert!(
            Instant::parse("2024-01-01T00:00:00--1:00").is_err(),
            "--1:00 has a negative offset-hour magnitude"
        );
        assert!(
            Instant::parse("2024-01-01T00:00:00+5:-30").is_err(),
            "+5:-30 has a negative offset-minute magnitude"
        );
        // Valid extremes and a non-zero offset still parse and normalise as
        // before (guards against regressing the ±14:59 bounds the prior P3 fix
        // added).
        assert!(
            Instant::parse("2024-01-01T00:00:00+14:59").is_ok(),
            "+14:59 is a valid offset"
        );
        assert!(
            Instant::parse("2024-01-01T00:00:00-14:59").is_ok(),
            "-14:59 is a valid offset"
        );
        let with_offset = Instant::parse("2024-01-01T12:00:00+02:30")
            .expect("+02:30 is valid")
            .as_secs();
        let as_utc = Instant::parse("2024-01-01T09:30:00Z")
            .expect("UTC equivalent is valid")
            .as_secs();
        assert_eq!(with_offset, as_utc);
    }
}
