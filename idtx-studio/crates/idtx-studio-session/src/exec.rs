//! `OpenExec` dataflow subsystem (feature `openexec`).
//!
//! The runtime counterpart of idtx-flow's REST datasource nodes. For a stage
//! carrying an IDTX compute graph, this owns one background poller thread per
//! `IDTXRestDatasource` prim. Each poller fetches its endpoint on an interval
//! with a blocking `reqwest` client and sends the JSON response body over an
//! `mpsc` channel — it **never touches the USD stage**. The main-thread
//! [`StudioSession::tick_exec`](crate::StudioSession::tick_exec) drains the
//! channel, feeds the bodies into the C++ exec bridge, and runs the compute +
//! session-layer authoring under the exclusive stage-lifecycle lock.
//!
//! This mirrors the threading of the collab client and the XR gesture worker:
//! background threads exchange plain data over channels; all stage mutation
//! stays on the session thread. When a stage has no compute graph,
//! [`ExecSubsystem::detect_and_build`] returns `None` and no thread is spawned.
//!
//! # Memory safety (bounded queue + capped bodies)
//!
//! Both the delivery channel and each response body are **bounded** so an
//! untrusted stage/server cannot exhaust process memory. The channel is a
//! [`mpsc::sync_channel`] of [`DATASOURCE_QUEUE_CAPACITY`]: a poller that outruns
//! the per-frame drain gets backpressure and *coalesces* (drops the update — a
//! fresh poll follows next interval, and the C++ bridge already keeps only the
//! latest payload per prim), so the queue can never grow without bound. Each
//! fetched body is read through a hard byte cap ([`ExecNetworkPolicy::max_response_bytes`],
//! configured via the project's `[network] max_response_mb`, default 1 MiB) so a
//! hostile or runaway endpoint cannot stream an arbitrarily large body into RAM.
//!
//! # Network security (SSRF gate)
//!
//! A `RestDatasource` endpoint is **stage-authored data**, so a hostile stage
//! could otherwise make the runtime issue HTTP requests to loopback, link-local
//! (incl. the `169.254.169.254` cloud-metadata endpoint), or private-network
//! addresses the moment it opens. Network polling is therefore **deny-by-default**
//! and gated by an [`ExecNetworkPolicy`]: a REST datasource is only polled when
//! the runtime is explicitly configured to allow network access **and** the
//! endpoint host matches an explicit allowlist pattern **and** the endpoint uses
//! `http`/`https` **and** every address is public or covered by an explicit
//! private-host grant. Metadata/link-local addresses remain blocked. Validation
//! runs *before* any poller thread is spawned; the spawned
//! client additionally disables redirects and pins DNS to the validated addresses
//! so a later rebind cannot redirect a connection to an internal host. Mock
//! datasources never touch the network and are unaffected.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use idtx_usd::{ExecDataSource, StageService};

// The SSRF network-egress gate lives in the shared `idtx-net-guard` leaf crate so
// the WASM behavior host (`idtx-behavior`, a lower layer) enforces the identical
// policy for its `net` capability. `ExecNetworkPolicy` is kept as the public name
// this crate has always exported.
use idtx_net_guard::{locked_client, validate_endpoint, EndpointRejection, ValidatedEndpoint};
// `ExecNetworkPolicy` is the crate-level public alias for the shared SSRF gate
// (`idtx_net_guard::NetworkPolicy`), now owned by the `remote` feature which
// `openexec` enables.
use crate::ExecNetworkPolicy;

/// Upper bound on datasource prims a single stage may drive.
///
/// An untrusted stage can author an unbounded number of datasource prims. The
/// polling work is served by a **shared bounded worker pool** (see [`Pool`]), so
/// the thread count is decoupled from the source count — but we still cap the
/// number of source records *examined* so a hostile scene cannot force unbounded
/// URL parsing, DNS resolution, and client construction on the session thread via
/// sources that are ultimately denied or invalid. Sources past this limit are
/// skipped with a diagnostic.
const MAX_DATASOURCES: usize = 64;

/// Hard cap on worker threads in a stage's datasource poll pool.
///
/// The pool runs `min(POOL_WORKERS_CAP, available_parallelism, source_count)`
/// workers plus one scheduler thread — a small fixed ceiling regardless of how
/// many datasource prims the stage authors. REST fetches are blocking (`reqwest`),
/// so a few workers let independent endpoints overlap without one dedicated OS
/// thread per source. One hung host occupies at most one worker for the request
/// timeout, never a thread forever.
const POOL_WORKERS_CAP: usize = 4;

/// Capacity of the bounded datasource→session delivery channel.
///
/// The channel is a [`mpsc::sync_channel`]: once this many responses are queued
/// (because the per-frame drain stalled or the pollers outran it), a poller's
/// `try_send` fails with [`TrySendError::Full`] and the update is **dropped**
/// rather than buffered. Dropping is safe — datasource polling is periodic and
/// latest-wins (the C++ exec bridge coalesces per prim), so a fresh value
/// follows on the next interval. This caps the queue's memory at roughly
/// `DATASOURCE_QUEUE_CAPACITY * max_response_bytes`, independent of how many
/// stage-controlled pollers are producing.
const DATASOURCE_QUEUE_CAPACITY: usize = 64;

/// A datasource payload delivered from a driver thread (REST poller or mock
/// generator) to the session thread.
struct DatasourceResponse {
    prim_path: String,
    body: String,
}

/// Poller-thread-shared datasource inputs the compute graph may refresh *after*
/// the poller is spawned. Only `authorization` and `json_body` are dynamic: a
/// datasource prim can drive them from a `Compute_*` node (e.g.
/// `authorization.connect = </…/GetAuthToken.outputs:value>` resolving an
/// `${auth:TOKEN}` template), and that connected input is recomputed every
/// `exec_tick`. The session thread writes the freshly-resolved values here after
/// each tick (see [`ExecSubsystem::refresh_inputs`]); the poller snapshots them at
/// fetch time. The endpoint and query are deliberately **not** dynamic — they are
/// validated and DNS-pinned at spawn for SSRF safety, so letting the stage change
/// them after validation would reopen that hole.
#[derive(Debug, Default)]
struct DynamicRestInputs {
    authorization: String,
    json_body: String,
}

/// Validate a REST endpoint (its `endpoint` URL plus an optional separate
/// `query` string) against `policy`, delegating the SSRF checks to the shared
/// [`idtx_net_guard`] gate. The `query` is folded into the URL first (REST
/// datasources author it separately); everything after that — scheme, host
/// allowlist, resolved-address range validation, DNS pin — is the identical
/// policy the behavior `net` capability enforces.
///
/// # Errors
///
/// Returns a classified [`EndpointRejection`] (logged by the caller; no thread
/// spawned) when the URL is malformed or the policy denies the endpoint.
fn validate_rest_endpoint(
    endpoint: &str,
    query: &str,
    policy: &ExecNetworkPolicy,
) -> Result<ValidatedEndpoint, EndpointRejection> {
    // Fold the separately-authored query onto the endpoint URL before validating.
    let mut url = reqwest::Url::parse(endpoint)
        .map_err(|e| EndpointRejection::Policy(format!("invalid endpoint URL: {e}")))?;
    if !query.trim().is_empty() {
        let combined = match url.query() {
            Some(existing) if !existing.is_empty() => format!("{existing}&{}", query.trim()),
            _ => query.trim().to_owned(),
        };
        url.set_query(Some(&combined));
    }
    validate_endpoint(url.as_str(), policy)
}

/// Per-stage `OpenExec` dataflow state: the shared datasource poll pool and the
/// channel it delivers responses on. Present only when the stage carries a
/// compute graph (≥1 registered value key).
#[derive(Debug)]
pub(crate) struct ExecSubsystem {
    /// Shared bounded worker pool that polls every datasource. Declared before
    /// `rx` so it drops first: its `Drop` signals cancel + detaches a reaper and
    /// returns immediately, then `rx` drops so any late `deliver` from a still-
    /// draining worker fails (orphaned) and a freshly rebuilt subsystem — with a
    /// brand-new channel the old workers have no handle to — never ingests a stale
    /// payload. `None` when no pollable source survived validation.
    pool: Option<Pool>,
    rx: Receiver<DatasourceResponse>,
    /// Per-REST-datasource live inputs (keyed by prim path) the compute graph
    /// refreshes each tick via [`Self::refresh_inputs`]. Shared with the pool
    /// worker, which snapshots them at fetch time. Only REST sources with a live
    /// poll entry appear here; mock sources have none.
    dynamic_inputs: HashMap<String, Arc<Mutex<DynamicRestInputs>>>,
    /// Refreshed with the program: whether the compute graph carries a time/host-state
    /// provider (`Compute_Environment`). When `false` the graph is purely
    /// datasource-driven — its computed values change only when a payload arrives
    /// — so the session skips the per-frame compute tick (and the exclusive stage
    /// lock) on frames with no freshly-drained payload. When `true` (idcm clocks /
    /// env / auth tokens) every frame recomputes, preserving the old behaviour.
    has_live_providers: bool,
}

impl ExecSubsystem {
    /// Build the per-stage exec program and, if it has a compute graph, spawn the
    /// REST pollers. Returns `None` when the stage registers zero value keys — the
    /// caller then skips the subsystem entirely (no threads, no per-frame work).
    ///
    /// `policy` gates stage-authored network access: a REST datasource is only
    /// polled when the policy allows it and its validated endpoint passes the
    /// scheme / allowlist / address-range checks (see [`validate_rest_endpoint`]).
    /// Mock datasources never touch the network and ignore the policy.
    pub(crate) fn detect_and_build(
        service: &mut StageService,
        policy: &ExecNetworkPolicy,
    ) -> Option<ExecSubsystem> {
        let value_keys = match service.exec_build() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("exec_build failed: {e}");
                return None;
            }
        };
        if value_keys == 0 {
            return None;
        }

        // Cache whether this graph must recompute every frame (has a live
        // time/host-state provider) vs only when a payload arrives. A read failure
        // defaults to `true` (never gate on uncertainty — correctness over the
        // optimisation).
        let has_live_providers = service.exec_has_live_providers().unwrap_or(true);

        let sources = service.exec_data_sources().unwrap_or_else(|e| {
            tracing::warn!("exec_data_sources failed: {e}");
            Vec::new()
        });

        let (tx, rx) = mpsc::sync_channel(DATASOURCE_QUEUE_CAPACITY);
        let max_bytes = policy.max_response_bytes();
        let mut poll_sources: Vec<PollSource> = Vec::new();
        let mut dynamic_inputs: HashMap<String, Arc<Mutex<DynamicRestInputs>>> = HashMap::new();
        // Cap the number of source records *examined* (see `MAX_DATASOURCES`) so an
        // untrusted scene with many datasource prims cannot force unbounded URL
        // parsing, DNS resolution, and client construction on the session thread.
        for src in sources.into_iter().take(MAX_DATASOURCES) {
            match src.kind.as_str() {
                "rest" => {
                    if src.endpoint.trim().is_empty() {
                        continue; // nothing to poll
                    }
                    // SSRF gate: validate scheme / allowlist / resolved addresses
                    // BEFORE building the client. A rejected datasource is logged
                    // and skipped — the compute graph still runs with whatever the
                    // connected attribute's authored default is.
                    let validated = match validate_rest_endpoint(&src.endpoint, &src.query, policy) {
                        Ok(v) => v,
                        Err(rejection) => {
                            // A policy refusal (allowlist / scheme / SSRF) is logged
                            // distinctly from a host the policy allows but that could
                            // not be resolved (DNS / connectivity), so the log never
                            // blames the allowlist for a network outage.
                            match &rejection {
                                EndpointRejection::Policy(reason) => tracing::warn!(
                                    prim = %src.prim_path,
                                    endpoint = %idtx_net_guard::redact_url(&src.endpoint),
                                    "REST datasource blocked by network policy: {reason}"
                                ),
                                EndpointRejection::Unresolved(reason) => tracing::warn!(
                                    prim = %src.prim_path,
                                    endpoint = %idtx_net_guard::redact_url(&src.endpoint),
                                    "REST datasource allowed by policy but could not be resolved \
                                     (DNS/connectivity, not an allowlist denial): {reason}"
                                ),
                            }
                            continue;
                        }
                    };
                    // Build the client ONCE, locked down to the validated endpoint
                    // (redirects off + DNS pinned) via the shared gate — reused for
                    // every poll by whichever pool worker runs this source.
                    let client = match locked_client(&validated, Duration::from_secs(120)) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(prim = %src.prim_path, "reqwest client build failed: {e}");
                            continue;
                        }
                    };
                    // Live inputs shared with the pool worker: seed from the
                    // statically-authored values so the very first poll (before the
                    // first `exec_tick` resolves any connected input) still uses
                    // whatever was authored. The compute graph overwrites these each
                    // tick via `refresh_inputs`.
                    let dynamic = Arc::new(Mutex::new(DynamicRestInputs {
                        authorization: src.authorization.clone(),
                        json_body: src.json_body.clone(),
                    }));
                    dynamic_inputs.insert(src.prim_path.clone(), Arc::clone(&dynamic));
                    // REST interval floored at 1 s (same policy as before).
                    let interval = Duration::from_secs_f32(src.interval.max(1.0));
                    tracing::info!(
                        prim = %src.prim_path,
                        url = %idtx_net_guard::redact_url(validated.url()),
                        method = %src.method,
                        interval_s = interval.as_secs_f32(),
                        "REST datasource registered"
                    );
                    poll_sources.push(PollSource {
                        prim_path: src.prim_path.clone(),
                        interval,
                        kind: SourceKind::Rest {
                            client,
                            url: validated.url().to_owned(),
                            max_bytes,
                            dynamic,
                        },
                        src,
                        log: Mutex::new(PollLog::default()),
                    });
                }
                "mock" => {
                    // Mock interval floored at 0.05 s (same policy as before).
                    let interval = Duration::from_secs_f32(src.interval.max(0.05));
                    let seed = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0x9E37_79B9_7F4A_7C15)
                        | 1;
                    poll_sources.push(PollSource {
                        prim_path: src.prim_path.clone(),
                        interval,
                        kind: SourceKind::Mock {
                            state: Mutex::new(seed),
                        },
                        src,
                        log: Mutex::new(PollLog::default()),
                    });
                }
                other => tracing::warn!(prim = %src.prim_path, kind = other, "unknown datasource kind"),
            }
        }

        // Build the shared bounded pool over the validated sources. `None` when no
        // source survived (all denied / invalid / empty) — then the subsystem does
        // no polling but still drives the compute graph from authored defaults.
        let pool = Pool::spawn(poll_sources, tx);

        tracing::debug!(
            value_keys,
            sources = pool.as_ref().map_or(0, Pool::source_count),
            workers = pool.as_ref().map_or(0, Pool::worker_count),
            "OpenExec subsystem built"
        );
        Some(ExecSubsystem {
            pool,
            rx,
            dynamic_inputs,
            has_live_providers,
        })
    }

    /// Whether the compute graph must recompute every frame (it carries a live
    /// time/host-state `Compute_Environment` provider). When `false` the session
    /// may skip the compute tick on frames with no freshly-drained payload. See
    /// the field docs on [`ExecSubsystem::has_live_providers`].
    pub(crate) fn has_live_providers(&self) -> bool {
        self.has_live_providers
    }

    /// Program metadata is independent of the retained transport pool. Refresh
    /// after every successful exec_build, failing open to evaluation on a read
    /// error rather than silently freezing a live graph.
    pub(crate) fn refresh_program_metadata(&mut self, service: &StageService) {
        self.has_live_providers = service.exec_has_live_providers().unwrap_or(true);
    }

    /// Refresh each REST poller's live inputs from the values the compute graph
    /// resolved on the most recent [`StageService::exec_tick`]. Call this right
    /// after `exec_tick`: a datasource's connected `authorization` / `jsonBody`
    /// (e.g. an `${auth:TOKEN}` driven by a `Compute_*` node) is recomputed every
    /// tick, and this copies the fresh value into the shared cell the poller reads
    /// at fetch time — so an environment-provider token reaches the request
    /// without ever being authored onto the stage. A datasource whose inputs carry
    /// no connection produces no entries and is left on its authored defaults.
    pub(crate) fn refresh_inputs(&self, service: &StageService) {
        if self.dynamic_inputs.is_empty() {
            return;
        }
        let inputs = match service.exec_data_source_inputs() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("exec_data_source_inputs failed: {e}");
                return;
            }
        };
        for input in inputs {
            let Some(cell) = self.dynamic_inputs.get(&input.prim_path) else {
                continue;
            };
            // A poisoned lock still carries the last good value; recover it rather
            // than panicking the session thread over a background poller's panic.
            let mut d = cell.lock().unwrap_or_else(|e| e.into_inner());
            match input.attribute_name.as_str() {
                "authorization" => d.authorization = input.value,
                "jsonBody" => d.json_body = input.value,
                // endpoint/query stay static (validated + DNS-pinned at spawn);
                // ignore any computed value for them (see `DynamicRestInputs`).
                _ => {}
            }
        }
    }

    /// Drain every REST response currently queued into the C++ exec bridge's
    /// pending-update queue (coalesced per prim). Returns how many were enqueued.
    /// Non-blocking; runs on the session thread. No stage mutation here — the
    /// subsequent `exec_tick` applies the queued payloads under the lock.
    pub(crate) fn drain_into(&self, service: &StageService) -> usize {
        let mut enqueued = 0;
        while let Ok(resp) = self.rx.try_recv() {
            match service.exec_enqueue(&resp.prim_path, &resp.body) {
                Ok(true) => enqueued += 1,
                Ok(false) => tracing::trace!(prim = %resp.prim_path, "exec update dropped"),
                Err(e) => tracing::warn!(prim = %resp.prim_path, "exec_enqueue: {e}"),
            }
        }
        enqueued
    }
}

impl Drop for ExecSubsystem {
    fn drop(&mut self) {
        // Tear the pool down first (its own `Drop` signals cancel + detaches a
        // reaper and returns immediately), then let `rx` drop so any late
        // `deliver` from a still-draining worker is orphaned — see the field docs.
        drop(self.pool.take());
    }
}

#[cfg(all(test, feature = "authoring"))]
mod session_tests;

/// Deliver one response to the session thread over the bounded channel,
/// coalescing under backpressure. Returns `false` only when the receiver is gone
/// (the worker should then stop touching the channel); a **full** queue is not
/// fatal — the update is dropped and a fresh one follows on the next poll
/// interval. This is what keeps the queue bounded regardless of how fast
/// stage-controlled sources produce.
fn deliver(tx: &SyncSender<DatasourceResponse>, resp: DatasourceResponse) -> bool {
    match tx.try_send(resp) {
        Ok(()) => true,
        Err(TrySendError::Full(dropped)) => {
            tracing::trace!(
                prim = %dropped.prim_path,
                "datasource queue full; dropping update (coalesced)"
            );
            true
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

/// One pollable datasource owned by the [`Pool`]: its fixed cadence, the kind-
/// specific fetch state (a reused SSRF-locked client for REST, a PRNG seed for
/// mock), and per-source log state so the reachability breadcrumbs match the old
/// per-thread poller.
struct PollSource {
    /// Datasource prim path (the `outputs:data` target and the delivery key).
    prim_path: String,
    /// Poll cadence (REST floored at 1 s, mock at 0.05 s).
    interval: Duration,
    /// The datasource record (`method`, …) `fetch_once` reads.
    src: ExecDataSource,
    kind: SourceKind,
    /// Reachability-log state, so a healthy source logs "responding" once and a
    /// failing one logs its reason once (not once per poll). Behind a mutex because
    /// any pool worker may service this source.
    log: Mutex<PollLog>,
}

/// Per-source kind-specific poll state.
enum SourceKind {
    /// REST: a `reqwest::blocking` client locked to the validated endpoint
    /// (redirects off + DNS pinned), built once at registration and REUSED across
    /// polls; the validated URL; the response-body cap; and the live inputs the
    /// compute graph refreshes (snapshotted at fetch time).
    Rest {
        client: reqwest::blocking::Client,
        url: String,
        max_bytes: usize,
        dynamic: Arc<Mutex<DynamicRestInputs>>,
    },
    /// Mock: a per-source xorshift PRNG seed, advanced under the lock each poll.
    Mock { state: Mutex<u64> },
}

/// Reachability-log state for a source (mirrors the old per-thread poller's
/// `last_state` / `poll_count`).
#[derive(Default)]
struct PollLog {
    last_state: Option<String>,
    poll_count: u64,
}

/// Ready-job queue shared by the scheduler (producer) and workers (consumers).
struct Jobs {
    /// Source indices ready to poll now, FIFO.
    ready: VecDeque<usize>,
    /// Per-source "already queued or in flight" flag that coalesces a source whose
    /// cadence fired again before its previous poll finished — latest-wins (a fresh
    /// value follows next interval), matching the old drop-on-full semantics.
    pending: Vec<bool>,
}

/// State shared between the pool's scheduler thread and its worker threads.
struct PoolShared {
    /// Immutable after build; interior mutability only inside each source's `log`,
    /// mock seed, and `dynamic` cells.
    sources: Vec<PollSource>,
    /// Bounded coalescing delivery channel to the session thread (see `deliver`).
    tx: SyncSender<DatasourceResponse>,
    /// Set on teardown; observed by the scheduler and workers between operations
    /// (so it cannot interrupt an in-flight `reqwest`, exactly as before).
    cancel: Arc<AtomicBool>,
    jobs: Mutex<Jobs>,
    /// Workers wait here for a ready job (or cancel).
    jobs_cv: Condvar,
    /// Scheduler's min-heap of upcoming `(due_at, source_index)`.
    sched: Mutex<BinaryHeap<Reverse<(Instant, usize)>>>,
    /// Scheduler waits here until the earliest `due_at` (or cancel/notify).
    sched_cv: Condvar,
}

/// Shared bounded worker pool polling every datasource of one stage: a scheduler
/// thread that releases each source at its cadence, and a small fixed set of
/// worker threads that run the (blocking) fetch and deliver the result.
///
/// The thread count is bounded by [`POOL_WORKERS_CAP`] regardless of how many
/// datasource prims the stage authors, idle long-interval sources cost nothing
/// (the scheduler sleeps until the next due time — no 50 ms busy-wake), and one
/// hung host occupies at most one worker for the request timeout rather than a
/// dedicated thread forever.
struct Pool {
    shared: Arc<PoolShared>,
    scheduler: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
    worker_count: usize,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("sources", &self.shared.sources.len())
            .field("workers", &self.worker_count)
            .finish()
    }
}

impl Pool {
    /// Build and start the pool over `sources`. Returns `None` when there is
    /// nothing to poll (empty list, or no worker could be spawned). Every source
    /// starts due immediately — the first poll fires at once, as the old per-thread
    /// poller did — then repeats at its cadence.
    fn spawn(sources: Vec<PollSource>, tx: SyncSender<DatasourceResponse>) -> Option<Pool> {
        if sources.is_empty() {
            return None;
        }
        let n = sources.len();
        let now = Instant::now();
        let mut heap = BinaryHeap::with_capacity(n);
        for idx in 0..n {
            heap.push(Reverse((now, idx)));
        }
        let shared = Arc::new(PoolShared {
            sources,
            tx,
            cancel: Arc::new(AtomicBool::new(false)),
            jobs: Mutex::new(Jobs {
                ready: VecDeque::new(),
                pending: vec![false; n],
            }),
            jobs_cv: Condvar::new(),
            sched: Mutex::new(heap),
            sched_cv: Condvar::new(),
        });

        // Bounded worker count: capped at POOL_WORKERS_CAP and the machine's
        // parallelism, and never more than the source count.
        let worker_count = std::thread::available_parallelism()
            .map_or(POOL_WORKERS_CAP, std::num::NonZeroUsize::get)
            .min(POOL_WORKERS_CAP)
            .min(n)
            .max(1);

        let mut workers = Vec::with_capacity(worker_count);
        for w in 0..worker_count {
            let shared = Arc::clone(&shared);
            match std::thread::Builder::new()
                .name(format!("idtx-exec-poll:{w}"))
                .spawn(move || worker_loop(&shared))
            {
                Ok(h) => workers.push(h),
                Err(e) => tracing::warn!("spawn exec poll worker failed: {e}"),
            }
        }
        if workers.is_empty() {
            // Thread-exhausted process: no worker, so no polling at all.
            return None;
        }

        let sched_shared = Arc::clone(&shared);
        let scheduler = match std::thread::Builder::new()
            .name("idtx-exec-sched".into())
            .spawn(move || scheduler_loop(&sched_shared))
        {
            Ok(h) => h,
            Err(e) => {
                // The scheduler is the only producer of ready jobs; without it the
                // already-started workers would park forever and every datasource
                // would silently stop polling while `spawn` falsely reported a live
                // pool. Treat scheduler creation as mandatory: cancel + wake the
                // workers so they observe the flag and exit, hand their join to a
                // detached reaper (joining here could block on an in-flight request),
                // and report failure.
                tracing::warn!("spawn exec scheduler failed: {e}; tearing down workers");
                shared.cancel.store(true, Ordering::Relaxed);
                shared.jobs_cv.notify_all();
                shared.sched_cv.notify_all();
                let reaper = std::thread::Builder::new()
                    .name("idtx-exec-reaper".into())
                    .spawn(move || {
                        for h in workers {
                            let _ = h.join();
                        }
                    });
                if let Err(e) = reaper {
                    tracing::warn!("exec reaper spawn failed, pool workers left detached: {e}");
                }
                return None;
            }
        };

        Some(Pool {
            shared,
            scheduler: Some(scheduler),
            workers,
            worker_count,
        })
    }

    fn source_count(&self) -> usize {
        self.shared.sources.len()
    }

    fn worker_count(&self) -> usize {
        self.worker_count
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // Signal the scheduler + workers to stop and wake anyone parked. A worker
        // may be inside a blocking `reqwest` request (up to the 120 s timeout);
        // `cancel` is only observed *between* operations, so joining here would
        // block the session thread — this `drop` runs on a topology rebuild / stage
        // close. Hand the join to a detached reaper so `drop` returns immediately.
        // `ExecSubsystem::drop` drops the `Receiver` right after, so a late
        // `deliver` from a still-running worker is orphaned and a rebuilt subsystem
        // (brand-new channel) never ingests a stale payload.
        self.shared.cancel.store(true, Ordering::Relaxed);
        self.shared.jobs_cv.notify_all();
        self.shared.sched_cv.notify_all();
        let mut handles: Vec<JoinHandle<()>> = std::mem::take(&mut self.workers);
        handles.extend(self.scheduler.take());
        if handles.is_empty() {
            return;
        }
        let reaper = std::thread::Builder::new()
            .name("idtx-exec-reaper".into())
            .spawn(move || {
                for h in handles {
                    let _ = h.join();
                }
            });
        if let Err(e) = reaper {
            tracing::warn!("exec reaper spawn failed, pool threads left detached: {e}");
        }
    }
}

/// Scheduler thread: releases each source into the ready queue at its cadence,
/// then sleeps until the next due time (or cancel). There is no fixed poll
/// interval, so a long-interval source genuinely sleeps its whole interval —
/// there is no periodic busy-wake.
fn scheduler_loop(shared: &PoolShared) {
    loop {
        if shared.cancel.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        // Pop every source now due and reschedule each at a FIXED cadence
        // (next = now + interval) so a slow fetch never drifts the schedule.
        let mut due: Vec<usize> = Vec::new();
        {
            let mut heap = shared.sched.lock().unwrap_or_else(|e| e.into_inner());
            while let Some(&Reverse((at, idx))) = heap.peek() {
                if at <= now {
                    heap.pop();
                    due.push(idx);
                    heap.push(Reverse((now + shared.sources[idx].interval, idx)));
                } else {
                    break;
                }
            }
        }
        if !due.is_empty() {
            {
                let mut jobs = shared.jobs.lock().unwrap_or_else(|e| e.into_inner());
                for idx in due {
                    // Coalesce: skip a source already queued or in flight.
                    if !jobs.pending[idx] {
                        jobs.pending[idx] = true;
                        jobs.ready.push_back(idx);
                    }
                }
            }
            shared.jobs_cv.notify_all();
        }
        // Sleep until the earliest upcoming due time (or a cancel/notify wake).
        let heap = shared.sched.lock().unwrap_or_else(|e| e.into_inner());
        let wait = heap
            .peek()
            .map(|Reverse((at, _))| at.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(200))
            .max(Duration::from_millis(1));
        let _ = shared.sched_cv.wait_timeout(heap, wait);
    }
}

/// Worker thread: pulls a ready source index and runs one poll (blocking fetch +
/// deliver), then clears its `pending` flag so the scheduler may release it again.
fn worker_loop(shared: &PoolShared) {
    loop {
        let idx = {
            let mut jobs = shared.jobs.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if shared.cancel.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(idx) = jobs.ready.pop_front() {
                    break idx;
                }
                // Timed wait so a missed `notify` (or cancel set between checks)
                // cannot park a worker forever.
                let (g, _) = shared
                    .jobs_cv
                    .wait_timeout(jobs, Duration::from_millis(200))
                    .unwrap_or_else(|e| e.into_inner());
                jobs = g;
            }
        };

        let alive = poll_one(shared, idx);

        // Release the source for future scheduling.
        {
            let mut jobs = shared.jobs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(p) = jobs.pending.get_mut(idx) {
                *p = false;
            }
        }

        if !alive {
            // Delivery receiver gone (session dropped) — stop the whole pool.
            shared.cancel.store(true, Ordering::Relaxed);
            shared.jobs_cv.notify_all();
            shared.sched_cv.notify_all();
            return;
        }
        if shared.cancel.load(Ordering::Relaxed) {
            return;
        }
    }
}

/// Run one poll of source `idx`: fetch (REST) or generate (mock), log reachability
/// transitions, and deliver any body. Returns `false` only when the delivery
/// receiver is gone (the pool should then stop); a failed/empty poll returns
/// `true` (not fatal — retry next interval).
fn poll_one(shared: &PoolShared, idx: usize) -> bool {
    let source = &shared.sources[idx];
    let body = match &source.kind {
        SourceKind::Rest {
            client,
            url,
            max_bytes,
            dynamic,
        } => {
            // Snapshot the live inputs the compute graph may have refreshed since
            // the last poll (connected authorization / jsonBody). A poisoned lock
            // still holds the last good value — recover it.
            let (authorization, json_body) = {
                let d = dynamic.lock().unwrap_or_else(|e| e.into_inner());
                (d.authorization.clone(), d.json_body.clone())
            };
            let fetched =
                fetch_once(client, &source.src, url, *max_bytes, &authorization, &json_body);
            let state = match &fetched {
                Ok(_) => "responding".to_owned(),
                Err(reason) => format!("not responding: {reason}"),
            };
            // Reachability breadcrumbs (same policy as the old per-thread poller):
            // debug every poll; info on a state transition and every 10 polls.
            {
                let mut log = source.log.lock().unwrap_or_else(|e| e.into_inner());
                log.poll_count += 1;
                let changed = log.last_state.as_deref() != Some(state.as_str());
                tracing::debug!(prim = %source.prim_path, poll = log.poll_count, "REST datasource {state}");
                if changed || log.poll_count % 10 == 0 {
                    tracing::info!(
                        prim = %source.prim_path,
                        url = %idtx_net_guard::redact_url(url),
                        method = %source.src.method,
                        poll = log.poll_count,
                        "REST datasource {state}"
                    );
                    log.last_state = Some(state);
                }
            }
            fetched.ok()
        }
        SourceKind::Mock { state } => {
            // xorshift64* → a float in [0, 1), the shape `MockDatasource_RandomFloat`
            // documents and the idtx-flow mock node produced.
            let r = {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                *s ^= *s >> 12;
                *s ^= *s << 25;
                *s ^= *s >> 27;
                (s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
            };
            Some(format!("{{\"data\":{{\"value\":{r}}}}}"))
        }
    };
    match body {
        // A disconnected receiver (session gone) stops the pool; a full queue just
        // drops this update (see `deliver`).
        Some(body) => deliver(
            &shared.tx,
            DatasourceResponse {
                prim_path: source.prim_path.clone(),
                body,
            },
        ),
        None => true,
    }
}

/// Perform one request; return the body on a 2xx response, else an `Err` holding
/// a short human-readable reason (`HTTP 405`, `request failed: …`, or a rejected
/// body). The caller surfaces that reason in the `info`-level "not responding"
/// breadcrumb, so the failure cause is visible at the default log level (the
/// conductor preview cannot set `RUST_LOG`). The body is read through a hard
/// `max_bytes` cap (see [`read_capped_body`]) so an untrusted server cannot stream
/// an arbitrarily large body into memory.
///
/// `authorization` and `json_body` are the *live* values snapshotted from the
/// poller's shared cell (which the compute graph refreshes each tick), not the
/// statically-authored ones on `src` — so a connected `${auth:TOKEN}` header
/// applies to this request.
fn fetch_once(
    client: &reqwest::blocking::Client,
    src: &ExecDataSource,
    url: &str,
    max_bytes: usize,
    authorization: &str,
    json_body: &str,
) -> Result<String, String> {
    let started = std::time::Instant::now();
    let mut req = if src.method.eq_ignore_ascii_case("POST") {
        // `jsonBody` is authored and documented as JSON; without the content type
        // many REST APIs reject or misparse the body (they default to
        // `application/x-www-form-urlencoded`).
        client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(json_body.to_owned())
    } else {
        client.get(url)
    };
    let credential = idtx_auth::authorization_from_input(authorization, url);
    if let Some(header) = &credential {
        req = req.header(reqwest::header::AUTHORIZATION, header);
    }
    // Unlike the blocking-client timeout, the per-request timeout also bounds
    // the complete streamed response when a peer sends a slow trickle of bytes.
    let response = req.timeout(Duration::from_secs(120)).send();
    let response = match response {
        Ok(resp) if resp.status().as_u16() == 401 && !src.method.eq_ignore_ascii_case("POST")
            && authorization.starts_with("auth:")
            && idtx_auth::refresh_after_unauthorized(url, credential.as_deref(), Duration::from_secs(120).saturating_sub(started.elapsed())) => {
                let remaining = Duration::from_secs(120).saturating_sub(started.elapsed());
                if remaining.is_zero() { return Err("request deadline exceeded".into()); }
                let mut retry = client.get(url).timeout(remaining);
                if let Some(header) = idtx_auth::authorization_from_input(authorization, url) { retry = retry.header(reqwest::header::AUTHORIZATION, header); }
                retry.send()
            }
        other => other,
    };
    match response {
        Ok(resp) if resp.status().is_success() => read_capped_body(resp, src, max_bytes)
            .ok_or_else(|| "response body rejected (too large or not UTF-8)".to_owned()),
        Ok(resp) => Err(format!("HTTP {}", resp.status())),
        // `reqwest::Error`'s `Display` can embed the full request URL (including
        // any secret query parameters), and this string is promoted into the
        // info-level poller-state log — bypassing the otherwise mandatory
        // `redact_url` path. Strip the URL from the error before formatting it.
        Err(e) => Err(format!("request failed: {}", e.without_url())),
    }
}

/// Read a success response body, enforcing a `max_bytes` cap. Rejects (logs and
/// returns `None`) when the advertised `Content-Length` already exceeds the cap,
/// **and** independently when the streamed body does — the read is capped at
/// `max_bytes + 1`, so an absent or lying `Content-Length` cannot evade it and no
/// more than one byte past the cap is ever buffered. The extra byte distinguishes
/// a body that is exactly at the cap (accepted) from one that is over (rejected).
fn read_capped_body(
    resp: reqwest::blocking::Response,
    src: &ExecDataSource,
    max_bytes: usize,
) -> Option<String> {
    if let Some(len) = resp.content_length() {
        if len > max_bytes as u64 {
            tracing::warn!(
                prim = %src.prim_path,
                content_length = len,
                max_bytes,
                "REST response exceeds size limit; dropping"
            );
            return None;
        }
    }
    let mut buf = Vec::new();
    let mut limited = resp.take(max_bytes as u64 + 1);
    if let Err(e) = limited.read_to_end(&mut buf) {
        tracing::warn!(prim = %src.prim_path, error = ?e.kind(), "REST body read failed");
        return None;
    }
    if buf.len() > max_bytes {
        tracing::warn!(
            prim = %src.prim_path,
            max_bytes,
            "REST response exceeds size limit; dropping"
        );
        return None;
    }
    match String::from_utf8(buf) {
        Ok(body) => Some(body),
        Err(e) => {
            tracing::warn!(prim = %src.prim_path, "REST body not valid UTF-8: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_references_are_resolved_for_each_request_origin_and_logout() {
        fn server(count: usize) -> (String, std::thread::JoinHandle<Vec<Option<String>>>) {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let url = format!("http://{}", server.server_addr().to_ip().unwrap());
            let worker = std::thread::spawn(move || {
                let mut headers = Vec::new();
                for _ in 0..count {
                    let request = server.recv_timeout(Duration::from_secs(3)).unwrap().expect("request");
                    headers.push(request.headers().iter().find(|h| h.field.equiv("Authorization"))
                        .map(|h| h.value.as_str().to_owned()));
                    request.respond(tiny_http::Response::from_string("{}")).unwrap();
                }
                headers
            });
            (url, worker)
        }
        let (authorized, a) = server(4);
        let (other, b) = server(1);
        let src = ExecDataSource { prim_path: "/Source".into(), kind: "rest".into(),
            endpoint: authorized.clone(), query: String::new(), method: "GET".into(),
            authorization: "auth:TOKEN".into(), json_body: String::new(), interval: 1.0 };
        // The transport is local test plumbing; fetch_once is the actual REST
        // send path. Network-policy validation is independently tested upstream.
        let client = reqwest::blocking::Client::builder().no_proxy()
            .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(2)).build().unwrap();
        idtx_auth::clear();
        idtx_auth::set_bearer_token_for_origins(std::slice::from_ref(&authorized), "token").unwrap();
        fetch_once(&client, &src, &authorized, 1024, "auth:TOKEN", "").unwrap();
        fetch_once(&client, &src, &other, 1024, "auth:TOKEN", "").unwrap();
        idtx_auth::set_user_credentials_for_origins(std::slice::from_ref(&authorized), "user", "pass").unwrap();
        fetch_once(&client, &src, &authorized, 1024, "auth:USERCRED", "").unwrap();
        fetch_once(&client, &src, &authorized, 1024, "auth:TOKEN", "").unwrap();
        idtx_auth::clear();
        fetch_once(&client, &src, &authorized, 1024, "auth:USERCRED", "").unwrap();
        assert_eq!(a.join().unwrap(), [Some("Bearer token".into()), Some("Basic dXNlcjpwYXNz".into()), None, None]);
        assert_eq!(b.join().unwrap(), [None]);
    }

    // Pure IP-range + host-pattern checks live in `idtx-net-guard`'s own tests;
    // here we cover the REST wrapper (`validate_rest_endpoint`) over the shared
    // gate, including the datasource-specific query-combine.

    #[test]
    fn denied_policy_rejects_everything() {
        let denied = ExecNetworkPolicy::denied();
        assert!(denied.is_denied());
        assert!(validate_rest_endpoint("https://api.example.com/x", "", &denied).is_err());

        // allow_network on but no hosts listed is still denied (explicit list is
        // mandatory).
        let empty = ExecNetworkPolicy::new(true, vec![]);
        assert!(empty.is_denied());
        assert!(validate_rest_endpoint("https://api.example.com/x", "", &empty).is_err());
    }

    #[test]
    fn rejects_non_http_schemes() {
        let policy = ExecNetworkPolicy::new(true, vec!["example.com".into()]);
        for url in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "gopher://example.com",
        ] {
            assert!(
                validate_rest_endpoint(url, "", &policy).is_err(),
                "{url} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_host_not_in_allowlist() {
        let policy = ExecNetworkPolicy::new(true, vec!["api.example.com".into()]);
        // Public but unlisted host (1.1.1.1 literal) is rejected before any DNS.
        assert!(validate_rest_endpoint("https://1.1.1.1/x", "", &policy).is_err());
    }

    #[test]
    fn rejects_allowlisted_host_that_resolves_to_loopback() {
        // `localhost` is a hostname that resolves to a blocked address; even if a
        // (foolish) allowlist names it, the address-range check rejects it.
        let policy = ExecNetworkPolicy::new(true, vec!["localhost".into()]);
        assert!(validate_rest_endpoint("http://localhost/x", "", &policy).is_err());
    }

    #[test]
    fn rejects_private_ip_literal_even_when_allowlisted() {
        let policy = ExecNetworkPolicy::new(true, vec!["169.254.169.254".into()]);
        assert!(validate_rest_endpoint("http://169.254.169.254/latest/meta-data", "", &policy).is_err());
    }

    #[test]
    fn max_response_bytes_defaults_and_resolves() {
        // Unconfigured → default 1 MiB (never "unlimited").
        assert_eq!(
            ExecNetworkPolicy::new(true, vec![]).max_response_bytes(),
            idtx_net_guard::DEFAULT_MAX_RESPONSE_BYTES
        );
        // A configured MB value resolves to that many MiB.
        assert_eq!(
            ExecNetworkPolicy::new(true, vec!["x".into()])
                .with_max_response_mb(5)
                .max_response_bytes(),
            5 * 1024 * 1024
        );
    }

    // ── Bounded poll pool ────────────────────────────────────────
    // Mock sources never touch the network, so these exercise the shared pool's
    // scheduling + delivery + teardown without any HTTP.

    fn mock_source(path: &str, interval_s: f32) -> PollSource {
        PollSource {
            prim_path: path.to_owned(),
            interval: Duration::from_secs_f32(interval_s.max(0.05)),
            src: ExecDataSource {
                prim_path: path.to_owned(),
                kind: "mock".to_owned(),
                endpoint: String::new(),
                query: String::new(),
                method: "GET".to_owned(),
                authorization: String::new(),
                json_body: String::new(),
                interval: interval_s,
            },
            kind: SourceKind::Mock {
                state: Mutex::new(0x1234_5678_9abc_def1),
            },
            log: Mutex::new(PollLog::default()),
        }
    }

    #[test]
    fn pool_is_none_without_sources() {
        let (tx, _rx) = mpsc::sync_channel(DATASOURCE_QUEUE_CAPACITY);
        assert!(Pool::spawn(Vec::new(), tx).is_none());
    }

    #[test]
    fn pool_delivers_and_bounds_worker_count() {
        let (tx, rx) = mpsc::sync_channel(DATASOURCE_QUEUE_CAPACITY);
        let sources = vec![mock_source("/A", 0.05), mock_source("/B", 0.05)];
        let n = sources.len();
        let pool = Pool::spawn(sources, tx).expect("pool spawns for non-empty sources");

        // Thread count is bounded regardless of source count — the whole point of
        // the pool (was one OS thread per source).
        assert!(pool.worker_count() >= 1);
        assert!(pool.worker_count() <= POOL_WORKERS_CAP);
        assert!(pool.worker_count() <= n);
        assert_eq!(pool.source_count(), n);

        // The first poll fires immediately, so a mock payload arrives promptly.
        let resp = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a mock payload is delivered");
        assert!(resp.body.contains("\"value\""), "unexpected body: {}", resp.body);
        assert!(resp.prim_path == "/A" || resp.prim_path == "/B");

        // Teardown (Drop) must return promptly (detached reaper) and not hang.
        drop(pool);
        drop(rx);
    }

    #[test]
    fn pool_worker_count_capped_below_many_sources() {
        let (tx, _rx) = mpsc::sync_channel(DATASOURCE_QUEUE_CAPACITY);
        // Far more sources than workers — the pool must NOT spawn one thread each.
        let sources: Vec<PollSource> = (0..32)
            .map(|i| mock_source(&format!("/m{i}"), 1.0))
            .collect();
        let pool = Pool::spawn(sources, tx).expect("pool spawns");
        assert!(
            pool.worker_count() <= POOL_WORKERS_CAP,
            "worker count {} exceeded cap {}",
            pool.worker_count(),
            POOL_WORKERS_CAP
        );
        assert_eq!(pool.source_count(), 32);
    }
}
