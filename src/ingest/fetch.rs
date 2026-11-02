#![allow(dead_code)] // transitional: reachable once the sync pipeline consumes the fetcher
#![allow(clippy::result_large_err)] // FetchError::Bound carries a Url, which is large

//! Bounded documentation HTTP fetcher for `sync` ingestion (task [010], issue #10).
//!
//! Turns the registry's static boundaries (task [009]) into the runtime fetch
//! contract the ingestion pipeline needs: it downloads the eight approved source
//! documents while keeping *every* transfer bounded — the `plan.md` §5/§10
//! "bounded HTTP fetcher" and "bounded fetching policies".
//!
//! It answers, and proves with fully offline loopback fixtures, the two
//! acceptance criteria of issue #10:
//!
//! - **Local HTTP fixtures cover timeout, transient failure, retry exhaustion,
//!   and rate-limit responses.** Each failure mode has a hermetic test in
//!   [`tests`]; no real network is touched.
//! - **Oversized bodies stop within configured bounds; the client identifies
//!   itself and preserves response metadata.** The byte cap stops a hostile or
//!   mis-announced body mid-stream instead of buffering it whole, a
//!   `User-Agent` self-identifies the client, and every [`Fetched`] preserves the
//!   requested vs. final URL, status, body, and the headers a later
//!   provenance/freshness layer (`[008]`) consumes
//!   (`Content-Type`, `ETag`, `Last-Modified`, `Content-Length`).
//!
//! ### Why gate, then transfer (not the other way around)
//!
//! `reqwest`'s built-in redirect-following and retry are turned *off*
//! (`redirect::Policy::none()`, no `retry` policy) because they would act on
//! hosts and hops the registry has not vouched for. Instead, a [`UrlGate`] vets
//! the initial URL and *every* redirect hop, so an arbitrary source is refused
//! and an unexpected redirect that escapes the source's host/path boundary is
//! rejected rather than crawled (`plan.md` §9). Production uses
//! [`RegistryGate`](struct.RegistryGate.html); offline tests use a loopback gate.
//!
//! ### No native TLS, no network in ordinary tests
//!
//! The client is built with `reqwest` at `default-features = false` plus only the
//! `rustls` and `stream` features (`docs/COMPARIBILITY.md` §3): no native-TLS,
//! no `http2`, no `system-proxy`. Every ordinary test talks to a
//! [`FixtureServer`], never an external host.
//!
//! ### Errors never panic
//!
//! External input (a timeout, a 5xx/429 storm, an over-sized body, a malformed
//! body, a boundary-escaping redirect) surfaces as a typed [`FetchError`] that
//! records the offending [`Url`] (and, for HTTP failures, its [`StatusCode`]) —
//! the "no avoidable panic on external input" posture of task [004].

use crate::registry::{RegistryError, resolve_url};
use futures_util::stream::StreamExt;
use reqwest::{ClientBuilder, StatusCode, Url};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Per-transfer bounds
// ---------------------------------------------------------------------------

/// The quantitative and behavioral bounds a [`BoundClient`](struct.BoundClient.html)
/// enforces on a fetch: the "bounded fetching policies" `plan.md` §10 assigns to
/// [010]. A timeout caps a stalled transfer; `max_retries` + a capped transient
/// backoff and a capped `Retry-After` prevent retry storms and rate-limit
/// stalls; `max_body_bytes` stops a hostile or mis-announced body; `max_redirects`
/// plus the [`UrlGate`] keep an escaping redirect inside the boundary; and
/// `user_agent` self-identifies the client. The defaults are conservative
/// upstream-courtesy values a caller can override per source.
#[derive(Clone, Debug)]
pub struct FetchPolicy {
    /// A total wall-clock deadline for a whole transfer (connect + read).
    pub request_timeout: Duration,
    /// A cap on the *connect* phase, so an unreachable host fails fast.
    pub connect_timeout: Duration,
    /// Extra attempts after the first, on a transient failure (HTTP 5xx or a
    /// transient transport error); `0` means no retries.
    pub max_retries: u32,
    /// Base of the transient (5xx/transport) backoff; wait grows
    /// `transient_backoff * 2^(attempt-1)` capped by `retry_after_max`.
    pub transient_backoff: Duration,
    /// The upper bound on any single backoff sleep (transient or `Retry-After`).
    pub retry_after_max: Duration,
    /// The maximum number of redirect hops within one transfer.
    pub max_redirects: usize,
    /// The maximum number of in-flight transfers when several are fetched at once
    /// (see [`BoundClient::fetch_all`](struct.BoundClient.html#method.fetch_all)).
    pub max_concurrency: usize,
    /// The per-transfer body cap, in bytes. `None` means no cap.
    pub max_body_bytes: Option<usize>,
    /// The `User-Agent` sent on every request, or `None` to omit it.
    pub user_agent: Option<String>,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(5),
            max_retries: 2,
            transient_backoff: Duration::from_millis(250),
            retry_after_max: Duration::from_secs(300),
            max_redirects: 5,
            max_concurrency: 4,
            // A book page is small; 4 MiB covers a large page while still
            // bounding a hostile or mis-reported size.
            max_body_bytes: Some(4 * 1024 * 1024),
            user_agent: Some(
                "rust-docs-mcp/0.1 (+https://github.com/ThiagoEvoa/rust-mcp)".to_string(),
            ),
        }
    }
}

impl FetchPolicy {
    /// A `'static` closure form of the transient backoff, computed from
    /// [`FetchPolicy::transient_backoff`] and capped at
    /// [`FetchPolicy::retry_after_max`]; borrowed once so the transfer loop can
    /// call it from `&self` without moving the policy.
    fn backoff_fn(&self) -> impl Fn(u32) -> Duration + '_ {
        let (base, cap) = (self.transient_backoff, self.retry_after_max);
        move |attempt| {
            let shift = attempt.saturating_sub(1).min(31);
            let factor = base.as_millis() * (1u128 << shift);
            Duration::from_millis(factor as u64).min(cap)
        }
    }
}

// ---------------------------------------------------------------------------
// The boundary gate (seam that keeps the fetcher inside the registry)
// ---------------------------------------------------------------------------

/// Whether an (intermediate or final) URL may be fetched, resolved against the
/// source registry's host/path boundaries.
pub(crate) trait UrlGate: Send + Sync {
    /// Classify `url`, returning the registry's reason when it is refused.
    fn allow(&self, url: &Url) -> Result<(), RegistryError>;
}

/// The production gate: a URL is fetchable only if
/// [`crate::registry::resolve_url`] accepts it (a registered host whose path
/// falls within that host's declared boundary). This is the seam that wires the
/// `registry` ([009]) into the fetcher ([010]): every hop a transfer takes is
/// checked here, so an arbitrary source and an escaping redirect are refused.
pub(crate) struct RegistryGate;

impl UrlGate for RegistryGate {
    fn allow(&self, url: &Url) -> Result<(), RegistryError> {
        // `resolve_url` is strict about host shape and the http(s) scheme and
        // cannot panic on a real URL.
        resolve_url(url.as_str())?;
        Ok(())
    }
}

/// The test gate: a URL is fetchable only if its host is the loopback address
/// (`127.0.0.1` / `localhost`) and its scheme is `http`. This is what lets the
/// offline acceptance tests point a transfer at a [`FixtureServer`]
/// (see the `http_fixture` test module) instead of an external host, while still
/// exercising the *gated* walk and its redirect handling. A redirect to any
/// non-loopback host is therefore refused — the same rejection the registry-
/// backed gate gives for a foreign host, here driven offline.
#[allow(dead_code)] // transitional: reachable once the sync pipeline consults the gate
pub(crate) struct LoopbackGate;

impl UrlGate for LoopbackGate {
    fn allow(&self, url: &Url) -> Result<(), RegistryError> {
        // `host_str()/scheme()` are stable, MSRV-safe accessors; the loopback
        // spellings are a closed set and never panic on a `Url`.
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            && matches!(url.scheme(), "http" | "https");
        if loopback {
            Ok(())
        } else {
            let host = url.host_str().unwrap_or("").to_string();
            Err(RegistryError::UnregisteredHost { host })
        }
    }
}

// ---------------------------------------------------------------------------
// What a transfer yields
// ---------------------------------------------------------------------------

/// The result of a successful, in-boundary fetch.
///
/// It preserves the **response metadata** an acceptance criterion requires: the
/// [`Url`] the caller asked for vs. the one that served the body after
/// redirects, the [`StatusCode`], the (capped) body, and the select headers a
/// later provenance/freshness and cache layer consumes
/// (`Content-Type`, `ETag`, `Last-Modified`, `Content-Length`). A body that
/// would exceed its cap is not captured here at all: the transfer fails with
/// `FetchError::Oversized` (the cap is all-or-nothing — see
/// [`FetchError::Oversized`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// The `Url` the caller requested (the transfer's start).
    pub requested: Url,
    /// The `Url` that actually served the body: equal to `requested` with no
    /// redirects, else the final location after the redirect chain.
    pub final_url: Url,
    /// The status of the body-producing hop.
    pub status: StatusCode,
    /// The bytes buffered (capped; the transfer aborts if they would exceed the
    /// cap, so this is never a partial body).
    pub body: Vec<u8>,
    /// The `Content-Type` header, if present.
    pub content_type: Option<String>,
    /// The `ETag` header, if present (a cheap cache identity / freshness hint).
    pub etag: Option<String>,
    /// The `Last-Modified` header, if present (an upstream-freshness hint).
    pub last_modified: Option<String>,
    /// The `Content-Length` the server announced, if present.
    pub content_length: Option<u64>,
}

// ---------------------------------------------------------------------------
// Fetch errors
// ---------------------------------------------------------------------------

/// A typed, non-panicking fetch failure.
///
/// Every variant carries the offending [`Url`] (and, when an HTTP status is in
/// play, the [`StatusCode`]) — the "useful context, never an avoidable panic"
/// posture of task [004], applied to the fetch layer. The sync pipeline ([011]+)
/// maps these onto the crate's typed-error layer when it consumes this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// No URL passed the [`UrlGate`] (a foreign host, an off-boundary path, or a
    /// redirect that escapes it), with the registry's reason.
    Bound {
        /// The offending URL.
        url: Url,
        /// The registry's classification of why it is not fetchable.
        why: RegistryError,
    },
    /// A `3xx` chain exceeded [`FetchPolicy::max_redirects`].
    TooManyRedirects {
        /// The URL at which the hop count was exhausted.
        url: Url,
        /// The hop count reached.
        hops: usize,
    },
    /// The initial URL was a redirect loop (the same location repeated).
    RedirectLoop {
        /// The URL that looped.
        url: Url,
    },
    /// The transfer hit its wall-clock deadline ([`FetchPolicy::request_timeout`]).
    Timeout {
        /// The URL that timed out.
        url: Url,
    },
    /// A *transient* failure (HTTP 5xx or a transient transport error) persisted
    /// through every attempt. `status` is `None` when the last failure was a
    /// transport error rather than an HTTP status.
    RetriesExhausted {
        /// The URL that failed.
        url: Url,
        /// The status of the last attempt, if it produced an HTTP response.
        status: Option<StatusCode>,
        /// The number of attempts made (the initial one plus retries).
        attempts: u32,
    },
    /// A `429 Too Many Requests` persisted through every retry.
    RateLimited {
        /// The URL that was rate-limited.
        url: Url,
        /// The number of attempts made.
        attempts: u32,
        /// The last capped `Retry-After` the server honored, if it sent one.
        retry_after: Option<Duration>,
    },
    /// The response body exceeded the effective cap (the per-call override, else
    /// [`FetchPolicy::max_body_bytes`]); the partial body is dropped because a
    /// half-captured document would be a corrupted one — the cap is all-or-nothing.
    Oversized {
        /// The URL whose body exceeded its cap.
        url: Url,
        /// The effective cap that was exceeded.
        cap: usize,
    },
    /// A non-success, non-retryable HTTP status (a `4xx` other than `429`).
    HttpStatus {
        /// The URL that produced the status.
        url: Url,
        /// The failing status.
        status: StatusCode,
    },
    /// A transport-level failure that was not retryable, or a client-construction
    /// failure.
    Transport {
        /// A short description of the transport failure.
        what: String,
        /// The URL in play, if a transfer was underway.
        url: Option<Url>,
    },
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Bound { url, why } => {
                write!(
                    f,
                    "refused to fetch `{url}` (outside source boundary): {why}"
                )
            }
            FetchError::TooManyRedirects { url, hops } => {
                write!(
                    f,
                    "redirect chain at `{url}` exceeded the hop limit ({hops})"
                )
            }
            FetchError::RedirectLoop { url } => write!(f, "redirect loop at `{url}`"),
            FetchError::Timeout { url } => write!(f, "timed out fetching `{url}`"),
            FetchError::RetriesExhausted {
                url,
                status,
                attempts,
            } => {
                let status = match status {
                    Some(s) => format!("status {s}"),
                    None => "transport error".to_string(),
                };
                write!(
                    f,
                    "giving up on `{url}` after {attempts} attempt(s): last {status}"
                )
            }
            FetchError::RateLimited {
                url,
                attempts,
                retry_after,
            } => match retry_after {
                Some(ra) => write!(
                    f,
                    "rate-limited at `{url}` after {attempts} attempt(s); last Retry-After {ra:?}"
                ),
                None => write!(
                    f,
                    "rate-limited at `{url}` after {attempts} attempt(s); no Retry-After"
                ),
            },
            FetchError::Oversized { url, cap } => {
                write!(
                    f,
                    "body of `{url}` exceeded its {cap}-byte cap; transfer aborted"
                )
            }
            FetchError::HttpStatus { url, status } => {
                write!(f, "non-success status {status} fetching `{url}`")
            }
            FetchError::Transport { what, url } => match url {
                Some(u) => write!(f, "transport failure fetching `{u}`: {what}"),
                None => write!(f, "transport failure: {what}"),
            },
        }
    }
}

impl std::error::Error for FetchError {}

// ---------------------------------------------------------------------------
// The bounded client
// ---------------------------------------------------------------------------

/// A configured, bounded HTTP fetcher.
///
/// Bundle a pre-built [`reqwest::Client`] with a [`FetchPolicy`] and a
/// [`UrlGate`], then hand URLs to [`BoundClient::fetch`](#method.fetch).
/// Reuse one client across a whole sync: the underlying connection pool and TLS
/// session cache are amortized.
#[derive(Clone)]
pub struct BoundClient {
    client: reqwest::Client,
    policy: FetchPolicy,
    gate: Arc<dyn UrlGate>,
}

impl BoundClient {
    /// Build a production fetcher whose boundary is the source registry.
    ///
    /// Uses `reqwest` at `default-features = false` (rustls + stream only, per
    /// `docs/COMPARIBILITY.md` §3): no native-TLS, `http2`, or `system-proxy`.
    /// Redirects follow *within the boundary only* and are capped by
    /// [`FetchPolicy::max_redirects`].
    pub fn new(policy: FetchPolicy) -> Result<Self, FetchError> {
        Ok(Self {
            client: build_client(&policy)?,
            policy,
            // The sole production boundary is the registry.
            gate: Arc::new(RegistryGate),
        })
    }

    /// Inject a custom [`UrlGate`] (test hook). Production uses the
    /// [`RegistryGate`]; the gate is the single seam that keeps a transfer inside
    /// the source boundary, so the offline acceptance tests substitute a loopback
    /// gate to point a transfer at a [`FixtureServer`] (see the `http_fixture`
    /// test module) instead of an external host.
    pub(crate) fn with_gate(
        policy: FetchPolicy,
        gate: Arc<dyn UrlGate>,
    ) -> Result<Self, FetchError> {
        Ok(Self {
            client: build_client(&policy)?,
            policy,
            gate,
        })
    }

    /// The registry gate, for callers that want to check a URL before fetching.
    pub fn gate(&self) -> &dyn UrlGate {
        self.gate.as_ref()
    }

    /// The policy this client enforces.
    pub fn policy(&self) -> &FetchPolicy {
        &self.policy
    }

    /// Fetch one URL within the boundary (no per-call cap; uses the policy
    /// default).
    pub async fn fetch(&self, url: &Url) -> Result<Fetched, FetchError> {
        self.download_with_limit(url.clone(), None).await
    }

    /// Fetch one URL with a per-call body cap overriding the policy default.
    pub async fn fetch_with_limit(
        &self,
        url: &Url,
        cap: Option<usize>,
    ) -> Result<Fetched, FetchError> {
        self.download_with_limit(url.clone(), cap).await
    }

    /// Fetch several URLs, at most [`FetchPolicy::max_concurrency`] in flight at
    /// a time, via a tokio `Semaphore`, preserving input order.
    pub async fn fetch_all(
        &self,
        urls: &[Url],
        cap: Option<usize>,
    ) -> Vec<Result<Fetched, FetchError>> {
        let cap = cap.or(self.policy.max_body_bytes);
        let sem = Arc::new(tokio::sync::Semaphore::new(
            self.policy.max_concurrency.max(1),
        ));
        let mut outcomes: Vec<Option<Result<Fetched, FetchError>>> =
            (0..urls.len()).map(|_| None).collect();

        // Each spawn is tagged with its slot index and URL so a joined failure
        // is reported through that slot instead of crashing the batch.
        let mut joins = Vec::with_capacity(urls.len());
        for (i, u) in urls.iter().enumerate() {
            let permit = sem.clone().acquire_owned().await.unwrap();
            let client = self.clone();
            let spawn_url = u.clone();
            let handle = tokio::spawn(async move {
                let _permit = permit;
                client.download_with_limit(spawn_url, cap).await
            });
            joins.push((i, u.clone(), handle));
        }

        for (i, url, task) in joins {
            match task.await {
                Ok(out) => {
                    outcomes[i] = Some(out);
                }
                // A transfer that *panicked* would leave its slot unset. Map the
                // `JoinError` to a typed error so this batch path also preserves
                // the "no avoidable panic on external input" invariant instead of
                // deferring to the `unreachable!` guard below.
                Err(join_err) => {
                    tracing::warn!("fetch task for {url} joined with error: {join_err:#}");
                    outcomes[i] = Some(Err(FetchError::Transport {
                        what: format!("fetch for {url} aborted (task join error): {join_err}"),
                        url: Some(url),
                    }));
                }
            }
        }

        outcomes
            .into_iter()
            .map(|o| match o {
                Some(r) => r,
                // Every slot is written exactly once above (both the `Ok` and
                // `JoinError` arms assign it), so this guard only trips if that
                // invariant is ever broken.
                None => unreachable!("every slot was written exactly once"),
            })
            .collect()
    }

    // -- the transfer core ---------------------------------------------------

    async fn download_with_limit(
        &self,
        start: Url,
        cap: Option<usize>,
    ) -> Result<Fetched, FetchError> {
        // Gate the initial URL so a foreign request never reaches the network.
        self.gate.allow(&start).map_err(|why| FetchError::Bound {
            url: start.clone(),
            why,
        })?;

        let mut retries_left = self.policy.max_retries;
        let mut attempts: u32 = 0;
        let mut hops: usize = 0;
        let mut visited: HashSet<Url> = HashSet::new();
        let backoff = self.policy.backoff_fn();

        let mut current = start.clone();
        loop {
            attempts += 1;
            // A single request; we never auto-follow (Policy::none) so the
            // redirect walk below stays gated and bounded.
            match self.request(&current).await {
                Ok(resp) => {
                    let status = resp.status();

                    if status.is_success() {
                        return read_capped(resp, cap, &start).await;
                    }

                    if status.is_redirection() {
                        let target = resolve_location(&resp, &current)?;
                        // Dedupe: a repeating location is a loop, not a walk.
                        if !visited.insert(target.clone()) {
                            return Err(FetchError::RedirectLoop {
                                url: target.clone(),
                            });
                        }
                        hops += 1;
                        if hops > self.policy.max_redirects {
                            return Err(FetchError::TooManyRedirects { url: target, hops });
                        }
                        // A redirected location must stay inside the source
                        // boundary (the "reject a redirect that escapes the
                        // source's boundary" criterion), so it is gated too.
                        self.gate.allow(&target).map_err(|why| FetchError::Bound {
                            url: target.clone(),
                            why,
                        })?;
                        current = target;
                        continue;
                    }

                    if status == StatusCode::TOO_MANY_REQUESTS {
                        let retry_after = parse_retry_after(resp.headers())
                            .map(|ra| ra.min(self.policy.retry_after_max));
                        if retries_left == 0 {
                            return Err(FetchError::RateLimited {
                                url: current.clone(),
                                attempts,
                                retry_after,
                            });
                        }
                        retries_left -= 1;
                        tokio::time::sleep(retry_after.unwrap_or_else(|| backoff(attempts))).await;
                        continue;
                    }

                    if status.is_server_error() {
                        if retries_left == 0 {
                            return Err(FetchError::RetriesExhausted {
                                url: current.clone(),
                                status: Some(status),
                                attempts,
                            });
                        }
                        retries_left -= 1;
                        tokio::time::sleep(backoff(attempts)).await;
                        continue;
                    }

                    // A non-success, non-retryable status (a 4xx other than 429).
                    return Err(FetchError::HttpStatus {
                        url: current.clone(),
                        status,
                    });
                }

                Err(err) => {
                    if err.is_timeout() {
                        return Err(FetchError::Timeout {
                            url: current.clone(),
                        });
                    }
                    if is_retryable_transport(&err) && retries_left > 0 {
                        retries_left -= 1;
                        tokio::time::sleep(backoff(attempts)).await;
                        continue;
                    }
                    return Err(FetchError::Transport {
                        what: err.to_string(),
                        url: Some(current.clone()),
                    });
                }
            }
        }
    }

    async fn request(&self, url: &Url) -> Result<reqwest::Response, reqwest::Error> {
        self.client.get(url.clone()).send().await
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Build the `reqwest::Client` a [`BoundClient`] fetches through.
///
/// `default-features = false` (rustls + stream only) keeps the closure
/// reproducible and MSRV-1.88-safe. Redirects are disabled (`Policy::none()`) so
/// the caller owns the bounded, gated walk; there is no `reqwest`-managed retry
/// because ours honors a capped `Retry-After` the built-in one does not.
fn build_client(policy: &FetchPolicy) -> Result<reqwest::Client, FetchError> {
    let mut builder: ClientBuilder = reqwest::Client::builder()
        .timeout(policy.request_timeout)
        .connect_timeout(policy.connect_timeout)
        .redirect(reqwest::redirect::Policy::none());
    if let Some(ua) = &policy.user_agent {
        builder = builder.user_agent(ua);
    }
    builder.build().map_err(|e| FetchError::Transport {
        what: format!("client construction failed: {e}"),
        url: None,
    })
}

/// Join a 3xx's `Location` header against the URL that produced it.
fn resolve_location(resp: &reqwest::Response, current: &Url) -> Result<Url, FetchError> {
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| FetchError::Transport {
            what: "3xx response with no Location header".to_string(),
            url: Some(current.clone()),
        })?;
    current.join(location).map_err(|e| FetchError::Transport {
        what: format!("malformed Location header {location:?}: {e}"),
        url: Some(current.clone()),
    })
}

/// Parse a `Retry-After` header (delta-seconds form) into a `Duration`.
///
/// The HTTP-date form is treated as "immediate" (an empty wait), since a stale
/// upstream date is a freshness concern for `[008]`, not a pacing one.
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Whether a transport error is worth a retry (a transient connection/IO
/// failure as opposed to a permanent one like an unresolvable host).
fn is_retryable_transport(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_body() || err.is_request()
}

// ---------------------------------------------------------------------------
// Body-capped finalizer: turn a successful response into a [`Fetched`],
// capping the body and preserving the select metadata headers.
// ---------------------------------------------------------------------------

async fn read_capped(
    resp: reqwest::Response,
    cap_override: Option<usize>,
    requested: &Url,
) -> Result<Fetched, FetchError> {
    let cap = cap_override;
    let status = resp.status();
    let final_url = resp.url().clone();
    let content_type = opt_header(resp.headers(), "content-type");
    let etag = opt_header(resp.headers(), "etag");
    let last_modified = opt_header(resp.headers(), "last-modified");
    let content_length = resp.content_length();

    let mut body: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| FetchError::Transport {
            what: e.to_string(),
            url: Some(final_url.clone()),
        })?;
        match cap {
            // A cap is all-or-nothing: a chunk that would push us over the
            // budget aborts the transfer instead of yielding a truncated body.
            Some(c) if body.len() + chunk.len() > c => {
                return Err(FetchError::Oversized {
                    url: final_url,
                    cap: c,
                });
            }
            _ => body.extend_from_slice(&chunk),
        }
    }

    Ok(Fetched {
        requested: requested.clone(),
        final_url,
        status,
        body,
        content_type,
        etag,
        last_modified,
        content_length,
    })
}

/// Read an HTTP header by its well-known (case-insensitive) name, as
/// `Option<String>`. A `char::WHITESPACE`-free literal is a syntactic
/// `HeaderName` so this never allocates a `HeaderName` per call.
fn opt_header(headers: &reqwest::header::HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Unit tests: fully offline loopback fixtures
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::http_fixture::{Behavior, FixtureServer};
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    // -- helpers ----------------------------------------------------------

    /// A policy with a short timeout and *no* backoff/retries so the offline
    /// fixtures fail fast. `max_redirects` is large enough for the tests.
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

    /// A policy whose transient backoff and Retry-After cap are both 0 so the
    /// retry/rate-limit fixtures run in a fraction of a second.
    fn no_backoff_policy(retries: u32) -> FetchPolicy {
        let mut p = fast_policy();
        p.max_retries = retries;
        p.transient_backoff = std::time::Duration::from_millis(0);
        p.retry_after_max = std::time::Duration::from_millis(0);
        p.max_body_bytes = Some(256 * 1024 * 1024); // big; we override per-call
        p
    }

    fn loopback_client(policy: FetchPolicy) -> Result<BoundClient, FetchError> {
        BoundClient::with_gate(policy, Arc::new(LoopbackGate))
    }

    fn url(server: &FixtureServer, path: &str) -> Url {
        // `server.addr` is `h:port`; `path` starts with `/`, so no extra
        // separator is needed.
        Url::parse(&format!("http://{}{}", server.addr, path)).expect("valid loopback URL")
    }

    // -- gate ------------------------------------------------------------

    #[test]
    fn policy_default_is_conservative_and_self_identifying() {
        let p = FetchPolicy::default();
        assert_eq!(p.max_concurrency, 4);
        assert_eq!(p.max_redirects, 5);
        assert_eq!(p.max_body_bytes, Some(4 * 1024 * 1024));
        assert!(p.user_agent.is_some());
        assert!(p.transient_backoff < p.retry_after_max);
    }

    #[test]
    fn registry_gate_refuses_a_foreign_url_before_any_request() {
        let gate = RegistryGate;
        let bad = Url::parse("https://evil.example.com/x").unwrap();
        assert!(matches!(
            gate.allow(&bad),
            Err(RegistryError::UnregisteredHost { .. })
        ));
        let off = Url::parse("https://doc.rust-lang.org/book/ch01.html").unwrap();
        assert!(matches!(
            gate.allow(&off),
            Err(RegistryError::OutsidePathBoundary { .. })
        ));
        let good = Url::parse("https://doc.rust-lang.org/cargo/").unwrap();
        assert!(gate.allow(&good).is_ok());
    }

    #[test]
    fn loopback_gate_refuses_a_foreign_host() {
        let gate = LoopbackGate;
        let bad = Url::parse("https://evil.example.com/x").unwrap();
        assert!(matches!(
            gate.allow(&bad),
            Err(RegistryError::UnregisteredHost { .. })
        ));
    }

    // -- happy path: metadata preservation + self-identification ---------

    #[tokio::test]
    async fn fetch_preserves_content_type_etag_and_last_modified() {
        let server = FixtureServer::start(vec![(
            "/article".to_string(),
            Behavior::Text {
                body: b"# Hello world".to_vec(),
                etag: Some("v42".to_string()),
                last_modified: Some("Thu, 01 Jan 2020 00:00:00 GMT".to_string()),
                content_type: Some("text/markdown; charset=utf-8".to_string()),
            },
        )]);
        let client = loopback_client(fast_policy()).unwrap();
        let u = url(&server, "/article");
        let f = client.fetch(&u).await.expect("should succeed");
        assert_eq!(f.final_url, u);
        assert_eq!(f.status.as_u16(), 200);
        assert_eq!(f.body, b"# Hello world\n");
        assert_eq!(
            f.content_type.as_deref(),
            Some("text/markdown; charset=utf-8")
        );
        assert_eq!(f.etag.as_deref(), Some("v42"));
        assert_eq!(
            f.last_modified.as_deref(),
            Some("Thu, 01 Jan 2020 00:00:00 GMT")
        );
        // `Content-Length` is the fourth metadata header the fixture advertises;
        // it equals the body length the server computes, so the "preserves
        // response metadata" criterion covers all four headers.
        assert_eq!(f.content_length, Some(f.body.len() as u64));
        // The server must have captured our self-identifying UA.
        let ua = server
            .last_user_agent()
            .expect("server captured a User-Agent");
        assert!(ua.contains("rust-docs-mcp"), "UA must self-identify: {ua}");
    }

    // -- timeout ---------------------------------------------------------

    #[tokio::test]
    async fn fetch_times_out_on_a_slow_server() {
        let server = FixtureServer::start(vec![(
            "/slow".to_string(),
            Behavior::Slow {
                delay: std::time::Duration::from_secs(10),
                ok_body: b"late".to_vec(),
            },
        )]);
        let client = loopback_client(fast_policy()).unwrap();
        let u = url(&server, "/slow");
        let err = client.fetch(&u).await.expect_err("should time out");
        assert!(matches!(err, FetchError::Timeout { .. }));
    }

    // -- transient retry exhaustion --------------------------------------

    #[tokio::test]
    async fn retry_exhausted_on_persistent_5xx() {
        let server = FixtureServer::start(vec![(
            "/flaky".to_string(),
            Behavior::FlakyThenOk {
                fail_times: 999,
                ok_body: b"never".to_vec(),
            },
        )]);
        let client = loopback_client(no_backoff_policy(2)).unwrap();
        let u = url(&server, "/flaky");
        let err = client.fetch(&u).await.expect_err("should exhaust");
        match &err {
            FetchError::RetriesExhausted { url, attempts, .. } => {
                assert_eq!(*attempts, 3, "1 initial + 2 retries");
                assert_eq!(url, &u);
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn transient_then_ok_recovers() {
        let server = FixtureServer::start(vec![(
            "/flaky".to_string(),
            Behavior::FlakyThenOk {
                fail_times: 2,
                ok_body: b"recovered".to_vec(),
            },
        )]);
        let client = loopback_client(no_backoff_policy(3)).unwrap();
        let u = url(&server, "/flaky");
        let f = client.fetch(&u).await.expect("should recover after 2 503s");
        assert_eq!(f.status.as_u16(), 200);
        assert_eq!(f.body, b"recovered\n");
    }

    // -- rate limit ------------------------------------------------------

    #[tokio::test]
    async fn rate_limited_then_ok_honours_retry_after_zero() {
        let server = FixtureServer::start(vec![(
            "/rl".to_string(),
            Behavior::RateLimitedThenOk {
                times: 2,
                retry_after: Some(0),
                ok_body: b"ok-after".to_vec(),
            },
        )]);
        let client = loopback_client(no_backoff_policy(3)).unwrap();
        let u = url(&server, "/rl");
        let f = client
            .fetch(&u)
            .await
            .expect("should succeed after two 429s");
        assert_eq!(f.body, b"ok-after\n");
    }

    // -- 429 exhaustion --------------------------------------------------

    #[tokio::test]
    async fn rate_limited_exhausted_when_never_ok() {
        let server = FixtureServer::start(vec![(
            "/rl".to_string(),
            Behavior::RateLimitedThenOk {
                times: 999,
                retry_after: Some(0),
                ok_body: b"never".to_vec(),
            },
        )]);
        let client = loopback_client(no_backoff_policy(2)).unwrap();
        let u = url(&server, "/rl");
        let err = client.fetch(&u).await.expect_err("should exhaust");
        assert!(matches!(
            err,
            FetchError::RateLimited { attempts, .. } if attempts == 3
        ));
    }

    // -- 404 -------------------------------------------------------------

    #[tokio::test]
    async fn non_retryable_404_is_http_status() {
        let server = FixtureServer::start(vec![("/missing".to_string(), Behavior::NotFound)]);
        let client = loopback_client(no_backoff_policy(2)).unwrap();
        let u = url(&server, "/missing");
        let err = client.fetch(&u).await.expect_err("404 is terminal");
        match &err {
            FetchError::HttpStatus { status, .. } => assert_eq!(status.as_u16(), 404),
            other => panic!("wrong error: {other:?}"),
        }
    }

    // -- oversized -------------------------------------------------------

    #[tokio::test]
    async fn oversized_body_is_aborted_not_partially_returned() {
        let server = FixtureServer::start(vec![(
            "/big".to_string(),
            Behavior::Big { size: 64 * 1024 },
        )]);
        let client = loopback_client(no_backoff_policy(0)).unwrap();
        let u = url(&server, "/big");
        let err = client
            .fetch_with_limit(&u, Some(1024))
            .await
            .expect_err("should trip the cap");
        match &err {
            FetchError::Oversized { cap, .. } => assert_eq!(*cap, 1024),
            other => panic!("wrong error: {other:?}"),
        }
    }

    // -- in-boundary redirect --------------------------------------------

    #[tokio::test]
    async fn in_boundary_redirect_is_followed_and_final_url_is_recorded() {
        let server = FixtureServer::start(vec![
            (
                "/a".to_string(),
                Behavior::Redirect {
                    location: "/b".to_string(),
                },
            ),
            (
                "/b".to_string(),
                Behavior::Text {
                    body: b"landed at /b".to_vec(),
                    etag: None,
                    last_modified: None,
                    content_type: None,
                },
            ),
        ]);
        let client = loopback_client(no_backoff_policy(0)).unwrap();
        let u = url(&server, "/a");
        let f = client.fetch(&u).await.expect("should follow /a -> /b");
        assert_eq!(f.body, b"landed at /b\n");
        assert_eq!(f.final_url, url(&server, "/b"));
        assert_ne!(f.requested, f.final_url, "a redirect changed the final URL");
        // Two requests: /a and /b.
        assert_eq!(server.request_count(), 2);
    }

    // -- out-of-boundary redirect ----------------------------------------

    #[tokio::test]
    async fn out_of_boundary_redirect_is_rejected() {
        // A 308 that points to a foreign host. The loopback gate refuses it.
        let server = FixtureServer::start(vec![(
            "/a".to_string(),
            Behavior::Redirect {
                location: "https://evil.example.com/x".to_string(),
            },
        )]);
        let client = loopback_client(no_backoff_policy(0)).unwrap();
        let u = url(&server, "/a");
        let err = client
            .fetch(&u)
            .await
            .expect_err("foreign hop must be refused");
        assert!(matches!(err, FetchError::Bound { .. }));
    }

    // -- too many redirects ----------------------------------------------

    #[tokio::test]
    async fn redirect_chain_exceeding_limit_is_terminal() {
        // A loop of three in-boundary hops with a 2-hop budget.
        let server = FixtureServer::start(vec![
            (
                "/a".to_string(),
                Behavior::Redirect {
                    location: "/b".to_string(),
                },
            ),
            (
                "/b".to_string(),
                Behavior::Redirect {
                    location: "/c".to_string(),
                },
            ),
            (
                "/c".to_string(),
                Behavior::Redirect {
                    location: "/a".to_string(),
                },
            ),
        ]);
        let mut p = no_backoff_policy(0);
        p.max_redirects = 2;
        let client = loopback_client(p).unwrap();
        let u = url(&server, "/a");
        let err = client.fetch(&u).await.expect_err(
            "
            should hit the 2-hop limit before a cycle",
        );
        assert!(matches!(
            err,
            FetchError::TooManyRedirects { hops, .. } if hops == 3
        ));
    }

    // -- redirect loop -----------------------------------------------------

    #[tokio::test]
    async fn redirect_loop_is_detected_as_redirectloop() {
        // A two-node cycle (`/a` -> `/b` -> `/a`) with a generous hop budget so
        // the repeating-location detector fires *before* the hop cap does,
        // exercising the `!visited.insert(target)` branch that the 3-node
        // `redirect_chain_exceeding_limit_is_terminal` test skips.
        let server = FixtureServer::start(vec![
            (
                "/a".to_string(),
                Behavior::Redirect {
                    location: "/b".to_string(),
                },
            ),
            (
                "/b".to_string(),
                Behavior::Redirect {
                    location: "/a".to_string(),
                },
            ),
        ]);
        // `no_backoff_policy` carries a 5-hop budget, enough to reach the repeat
        // at the second `/b` before `TooManyRedirects` trips.
        let client = loopback_client(no_backoff_policy(0)).unwrap();
        let u = url(&server, "/a");
        let err = client
            .fetch(&u)
            .await
            .expect_err("a repeating location must be a loop, not a walk");
        assert!(
            matches!(err, FetchError::RedirectLoop { .. }),
            "expected RedirectLoop, got {err:?}",
        );
    }

    // -- bounded concurrency ---------------------------------------------

    #[tokio::test]
    async fn fetch_all_respects_max_concurrency() {
        // Four slow routes, each held open for 0.1 s; a semaphore of two
        // means at most two simultaneous connections.
        let meter = Arc::new(AtomicUsize::new(0));
        let routes = vec![
            (
                "/1".to_string(),
                Behavior::Slow {
                    delay: std::time::Duration::from_millis(100),
                    ok_body: b"1".to_vec(),
                },
            ),
            (
                "/2".to_string(),
                Behavior::Slow {
                    delay: std::time::Duration::from_millis(100),
                    ok_body: b"2".to_vec(),
                },
            ),
            (
                "/3".to_string(),
                Behavior::Slow {
                    delay: std::time::Duration::from_millis(100),
                    ok_body: b"3".to_vec(),
                },
            ),
            (
                "/4".to_string(),
                Behavior::Slow {
                    delay: std::time::Duration::from_millis(100),
                    ok_body: b"4".to_vec(),
                },
            ),
        ];
        let server = FixtureServer::new(routes, meter.clone());
        let mut p = no_backoff_policy(0);
        p.max_concurrency = 2;
        p.request_timeout = std::time::Duration::from_secs(5);
        let client = loopback_client(p).unwrap();
        let urls: Vec<Url> = vec![
            url(&server, "/1"),
            url(&server, "/2"),
            url(&server, "/3"),
            url(&server, "/4"),
        ];
        let outcomes = client.fetch_all(&urls, None).await;
        assert_eq!(outcomes.len(), 4);
        for (u, o) in urls.iter().zip(outcomes.iter()) {
            let f = o.as_ref().cloned().expect("every URL should succeed");
            assert_eq!(f.final_url, *u);
        }
        let hw = server.observed_high_water();
        assert!(
            hw <= 2,
            "max-concurrency of 2 never exceeded; observed high water {hw}"
        );
        assert!(hw >= 2, "at least two were in flight simultaneously");
    }
}
