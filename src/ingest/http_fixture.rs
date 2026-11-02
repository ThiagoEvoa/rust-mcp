#![allow(dead_code)] // transitional: reachable once the sync pipeline uses these fixtures

//! A minimal, offline loopback HTTP/1.1 fixture server plus a concurrency meter
//! for the fetcher's acceptance tests.
//!
//! The server speaks a *hand-rolled* slice of HTTP/1.1 over a blocking
//! [`std::net::TcpListener`] — no extra dependency (so it never perturbs the
//! `reqwest` closure `docs/COMPARIBILITY.md` §3 pins). Each accepted connection
//! is served on a *detached* plain thread with `Connection: close`, so the async
//! `reqwest` client on the multi-thread runtime drives it. Routing is by request
//! path to a small [`Behavior`] so each acceptance scenario (timeout, transient
//! 5xx, 429 rate-limit, oversized body, in-/out-boundary redirect, metadata
//! preservation, self-identification) is one route.
//!
//! The accept loop uses a polling shutdown flag, so dropping a [`FixtureServer`]
//! cleanly stops the loop; a slow or hung client (the timeout test) is served on
//! a detached thread and never blocks the accept loop. An optional meter records
//! the concurrent-connection high water so a bounded-concurrency test can assert
//! the client never exceeds `max_concurrency`.

use std::io::BufRead;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// One behavior a route produces. A per-path counter lets a single route behave
/// differently across *sequential* requests (transient failure, rate-limit),
/// which is exactly the retry / rate-limit acceptance criteria.
pub(super) enum Behavior {
    /// `200 OK` with a fixed body and the select metadata headers a later
    /// provenance/freshness and cache layer consumes.
    Text {
        /// The response body.
        body: Vec<u8>,
        /// The `ETag` to advertise.
        etag: Option<String>,
        /// The `Last-Modified` to advertise.
        last_modified: Option<String>,
        /// The `Content-Type` to advertise.
        content_type: Option<String>,
    },
    /// `200 OK` with `size` bytes of `x` — deliberately larger than a realistic
    /// cap — so the cap stops the transfer (the oversized criterion). Sized well
    /// under the loopback send buffer so a client that stops reading (the cap)
    /// never blocks the writer.
    Big {
        /// The body length in bytes.
        size: usize,
    },
    /// `503 Service Unavailable` for the first `fail_times` requests, then `200`
    /// with `ok_body` — a transient-then-healthy stream.
    FlakyThenOk {
        /// Consecutive failing requests before the first `200`.
        fail_times: u32,
        /// The body the eventual `200` returns.
        ok_body: Vec<u8>,
    },
    /// `429 Too Many Requests` (with `Retry-After: <retry_after>` delta-seconds)
    /// for the first `times` requests, then `200` with `ok_body`.
    RateLimitedThenOk {
        /// Consecutive rate-limited requests before the first `200`.
        times: u32,
        /// A `Retry-After` delta-seconds, or `None` to omit it.
        retry_after: Option<u64>,
        /// The body the eventual `200` returns.
        ok_body: Vec<u8>,
    },
    /// Sleep `delay`, then `200` with `ok_body`. With a client deadline shorter
    /// than `delay`, the transfer times out — the timeout criterion.
    Slow {
        /// The post-accept delay before the response is written.
        delay: Duration,
        /// The body the eventual `200` returns.
        ok_body: Vec<u8>,
    },
    /// `308 Permanent Redirect` to `location` (relative or absolute). An
    /// in-boundary redirect uses a loopback path; an out-boundary one uses a
    /// foreign host the gate then refuses.
    Redirect {
        /// The `Location` header value.
        location: String,
    },
    /// `404 Not Found` — a non-retryable, non-success status.
    NotFound,
}

/// A tiny, lock-guarded request log a test asserts on after the fact: each
/// request that arrives captures its `User-Agent` and `path`, so the
/// "self-identifies" and "N requests were made" assertions are provable.
#[derive(Clone, Default)]
pub(super) struct Captured {
    /// Each arriving request, as `(user_agent, path)`, in arrival order.
    pub requests: Vec<(Option<String>, String)>,
}

/// Shared per-path request counters that let a single route fail a bounded number
/// of times (see [`Behavior::FlakyThenOk`] / [`Behavior::RateLimitedThenOk`]).
#[derive(Default)]
struct Counters(Mutex<Vec<(String, u32)>>);

impl Counters {
    /// Read-and-increment the counter for `path` (creating it at `0`).
    fn next(&self, path: &str) -> u32 {
        let mut map = self.0.lock().expect("counter lock is poison-free");
        if let Some(slot) = map.iter_mut().find(|(p, _)| p == path) {
            let n = slot.1;
            slot.1 = n + 1;
            return n;
        }
        map.push((path.to_string(), 0));
        0
    }
}

/// Shared request state for one running server: the request log and the per-path
/// failure counters.
#[derive(Default)]
struct Shared {
    log: Mutex<Captured>,
    counters: Mutex<Vec<(String, u32)>>,
}

impl Shared {
    fn record(&self, user_agent: Option<String>, path: &str) {
        let mut log = self.log.lock().expect("fixture log lock is poison-free");
        log.requests.push((user_agent, path.to_string()));
    }

    fn next(&self, path: &str) -> u32 {
        let mut map = self.counters.lock().expect("counter lock is poison-free");
        if let Some(slot) = map.iter_mut().find(|(p, _)| p == path) {
            let n = slot.1;
            slot.1 = n + 1;
            return n;
        }
        map.push((path.to_string(), 0));
        0
    }
}

/// A running fixture server bound to an ephemeral loopback port.
///
/// Drop it to release the port; the accept loop stops when the shutdown flag is
/// set. A server built with [`FixtureServer::new`] can meter the
/// concurrent-connection high water so a bounded-concurrency test can assert the
/// client never exceeds `max_concurrency`.
pub(super) struct FixtureServer {
    /// The address the client connects to (`127.0.0.1:<ephemeral>`).
    pub addr: String,
    /// The shared request log.
    shared: Arc<Shared>,
    /// The observed concurrent-connection high water, if a meter was requested.
    high_water: Arc<AtomicUsize>,
    _shutdown: Arc<AtomicBool>,
    _worker: Option<thread::JoinHandle<()>>,
}

impl FixtureServer {
    /// Start a server serving one route per given `(path, behavior)`, with no
    /// concurrency meter. The route whose path the request path *matches* and
    /// that is longest wins.
    pub(super) fn start(routes: Vec<(String, Behavior)>) -> FixtureServer {
        Self::new(routes, Arc::new(AtomicUsize::new(0)))
    }

    /// Start a server, metering the concurrent-connection high water.
    #[allow(dead_code)] // used by the bounded-concurrency test
    pub(super) fn new(routes: Vec<(String, Behavior)>, meter: Arc<AtomicUsize>) -> FixtureServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback fixture");
        // Ephemeral port; capture it before moving the listener into the loop.
        let addr: SocketAddr = listener
            .local_addr()
            .expect("local_addr on a bound loopback listener");

        // Set the listener non-blocking so the accept loop can check the
        // shutdown flag even when no connection is pending -- otherwise
        // `Drop` would block forever on `accept()` returning and
        // `worker.join()` on the stuck loop.
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on loopback fixture");

        let shared = Arc::new(Shared::default());
        let routes = Arc::new(routes);
        let shutdown = Arc::new(AtomicBool::new(false));
        // `in_flight` tracks the live connection count; `meter` (high_water) is
        // updated with `fetch_max` whenever a new connection is accepted, so a
        // bounded-concurrency test can assert the high water never exceeds
        // `max_concurrency`.
        let in_flight = Arc::new(AtomicUsize::new(0));

        // The `high_water` field in `FixtureServer` and the one passed to `run`
        // are the same `Arc`, so the test reads the max observed while the
        // handler threads update it.
        let high_water = Arc::clone(&meter);

        let shutdown_c = shutdown.clone();
        let shared_c = Arc::clone(&shared);
        let high_water_c = Arc::clone(&high_water);
        let worker = thread::Builder::new()
            .name("http-fixture".to_string())
            .spawn(move || {
                run(
                    listener,
                    routes,
                    shared_c,
                    in_flight,
                    high_water_c,
                    shutdown_c,
                )
            })
            .expect("spawn fixture accept loop");

        FixtureServer {
            addr: addr.to_string(),
            shared,
            high_water,
            _shutdown: shutdown,
            _worker: Some(worker),
        }
    }

    /// The `User-Agent` of the most recent captured request, if any.
    pub(super) fn last_user_agent(&self) -> Option<String> {
        self.shared
            .log
            .lock()
            .expect("fixture log lock is poison-free")
            .requests
            .last()
            .and_then(|(ua, _)| ua.clone())
    }

    /// How many requests the client made so far.
    pub(super) fn request_count(&self) -> usize {
        self.shared
            .log
            .lock()
            .expect("fixture log lock is poison-free")
            .requests
            .len()
    }

    /// The observed concurrent-connection high water.
    pub(super) fn observed_high_water(&self) -> usize {
        self.high_water.load(Ordering::SeqCst)
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        // Signal the loop; detached handlers finish on their own.
        self._shutdown.store(true, Ordering::SeqCst);
        if let Some(worker) = self._worker.take() {
            let _ = worker.join();
        }
    }
}

/// The accept loop over a **non-blocking** listener so the shutdown flag in
/// `Drop` can stop it even when no connection is arriving.
fn run(
    listener: TcpListener,
    routes: Arc<Vec<(String, Behavior)>>,
    shared: Arc<Shared>,
    in_flight: Arc<AtomicUsize>,
    high_water: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let shared_c = shared.clone();
                let routes_c = routes.clone();
                let in_flight_c = in_flight.clone();
                let high_water_c = high_water.clone();
                // Detach; the handler finishes on its own even if its client
                // times out mid-body (an over-cap transfer).
                thread::spawn(move || {
                    in_flight_c.fetch_add(1, Ordering::SeqCst);
                    let cur = in_flight_c.load(Ordering::SeqCst);
                    high_water_c.fetch_max(cur, Ordering::SeqCst);
                    let _ = stream.set_nodelay(true);
                    handle(stream, &routes_c, &shared_c);
                    in_flight_c.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No connection yet -- check if it's time to quit.
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                // Sleep a small interval so the loop doesn't busy-spin.
                thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(_) => break,
        }
    }
}

/// Read the request (request line + headers up to the blank line), capture it,
/// route to a [`Behavior`], and write the response. A read error or a malformed
/// request ends the connection rather than panicking — a hostile or half-sent
/// request must never trap the server.
fn handle(mut stream: TcpStream, routes: &Arc<Vec<(String, Behavior)>>, shared: &Arc<Shared>) {
    let mut reader = std::io::BufReader::new(&stream);

    // Request line: `GET /path HTTP/1.1\r\n`.
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return; // client hung up before sending anything
    }
    let method = request_line.split(' ').next().unwrap_or("");
    let path = request_line
        .split(' ')
        .nth(1)
        .unwrap_or("/")
        .trim()
        .to_string();

    // Headers, terminated by a blank line. Capture the `User-Agent`.
    let mut user_agent: Option<String> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some((name, val)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("user-agent")
        {
            user_agent = Some(val.trim().to_string());
        }
    }

    // Capture before routing, so even a route mismatch is logged.
    shared.record(user_agent.clone(), &path);

    // Route to the longest matching prefix.
    let mut chosen: Option<&Behavior> = None;
    let mut best = 0;
    for (prefix, behavior) in routes.iter() {
        if path.starts_with(prefix.as_str()) && prefix.len() > best {
            chosen = Some(behavior);
            best = prefix.len();
        }
    }
    let behavior = match chosen {
        Some(b) => b,
        None => {
            // No route: a clean `404` is the least-surprising fallback.
            let response = plain(404, "Not Found", b"404\r\n");
            let _ = stream.set_nodelay(true);
            let _ = write_all_or_broken(&mut stream, &response);
            return;
        }
    };

    // A counter-qualified route advances its per-path counter up front.
    let n: Option<u32> = match behavior {
        Behavior::FlakyThenOk { .. } | Behavior::RateLimitedThenOk { .. } => {
            Some(shared.next(&path))
        }
        _ => None,
    };

    // `Slow` applies its delay *before* writing so a client deadline trips.
    if let Behavior::Slow { delay, .. } = behavior {
        thread::sleep(*delay);
    }

    let (status, reason, body) = plan(behavior, n);
    let prefix = build_prefix(status, reason, &body, behavior);
    if matches!(method, "GET" | "HEAD") {
        let mut full = prefix;
        full.extend_from_slice(&body);
        let _ = stream.set_nodelay(true);
        let _ = write_all_or_broken(&mut stream, &full);
    }
}

/// The status/reason/body for a [`Behavior`] at a given counter tick `n`.
fn plan(behavior: &Behavior, n: Option<u32>) -> (u16, &'static str, Vec<u8>) {
    use std::io::Write;
    match behavior {
        Behavior::Text { body, .. } => {
            let mut b = body.clone();
            let _ = b.write_all(b"\n");
            (200, "OK", b)
        }
        Behavior::Big { size } => (200, "OK", vec![b'x'; *size]),
        Behavior::FlakyThenOk {
            ok_body,
            fail_times,
            ..
        } => match n {
            Some(n) if n < *fail_times => (503, "Service Unavailable", b"503\r\n".to_vec()),
            _ => {
                let mut b = ok_body.clone();
                let _ = b.write_all(b"\n");
                (200, "OK", b)
            }
        },
        Behavior::RateLimitedThenOk { ok_body, times, .. } => match n {
            Some(n) if n < *times => (429, "Too Many Requests", b"429\r\n".to_vec()),
            _ => {
                let mut b = ok_body.clone();
                let _ = b.write_all(b"\n");
                (200, "OK", b)
            }
        },
        Behavior::Slow { ok_body, .. } => {
            let mut b = ok_body.clone();
            let _ = b.write_all(b"\n");
            (200, "OK", b)
        }
        Behavior::Redirect { .. } => (308, "Permanent Redirect", Vec::new()),
        Behavior::NotFound => (404, "Not Found", b"404\r\n".to_vec()),
    }
}

/// Build the HTTP/1.1 response prefix (`status line` + `Server` + per-behavior
/// headers + blank line), leaving the body to be appended by the caller.
fn build_prefix(status: u16, reason: &str, body: &[u8], behavior: &Behavior) -> Vec<u8> {
    let mut out =
        format!("HTTP/1.1 {status} {reason}\r\nServer: rust-docs-mcp-test-fixture/0.1\r\n",);
    match behavior {
        Behavior::Text {
            etag,
            last_modified,
            content_type,
            ..
        } => {
            out.push_str(&format!("Content-Length: {}\r\n", body.len()));
            if let Some(e) = etag {
                out.push_str(&format!("ETag: {e}\r\n"));
            }
            if let Some(m) = last_modified {
                out.push_str(&format!("Last-Modified: {m}\r\n"));
            }
            if let Some(c) = content_type {
                out.push_str(&format!("Content-Type: {c}\r\n"));
            }
            out.push_str("Connection: close\r\n\r\n");
        }
        Behavior::Big { .. } => {
            out.push_str(&format!("Content-Length: {}\r\n", body.len()));
            out.push_str("Content-Type: text/plain\r\n");
            out.push_str("Connection: close\r\n\r\n");
        }
        Behavior::RateLimitedThenOk { retry_after, .. } => {
            out.push_str(&format!("Content-Length: {}\r\n", body.len()));
            if let Some(sec) = retry_after {
                out.push_str(&format!("Retry-After: {sec}\r\n"));
            }
            out.push_str("Connection: close\r\n\r\n");
        }
        Behavior::Redirect { location } => {
            out.push_str(&format!("Location: {location}\r\n"));
            out.push_str("Content-Length: 0\r\n");
            out.push_str("Connection: close\r\n\r\n");
        }
        _ => {
            out.push_str(&format!("Content-Length: {}\r\n", body.len()));
            out.push_str("Connection: close\r\n\r\n");
        }
    }
    out.into_bytes()
}

/// A no-route fallback response (`404`, no extra headers).
fn plain(status: u16, reason: &str, body: &[u8]) -> Vec<u8> {
    let prefix = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Server: rust-docs-mcp-test-fixture/0.1\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let mut out = Vec::with_capacity(prefix.len() + body.len());
    out.extend_from_slice(prefix.as_bytes());
    out.extend_from_slice(body);
    out
}

/// Write `bytes` to `stream`, tolerating a half-closed (`Connection: close`) or
/// aborted client so an over-cap transfer that stops reading never traps the
/// handler. Returns `Ok(())` on a clean or cleanly-aborted write.
fn write_all_or_broken(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut offset = 0usize;
    while offset < bytes.len() {
        match stream.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "connection closed by client mid-write",
                ));
            }
            Ok(w) => offset += w,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                // Broken pipe / reset: the client aborted (e.g. an over-cap stop).
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "client aborted the connection",
                ));
            }
        }
    }
    let _ = stream.flush();
    Ok(())
}
