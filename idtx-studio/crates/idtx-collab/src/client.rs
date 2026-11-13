use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::TcpStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use prost::Message as _;
use tracing::{debug, warn};
use tungstenite::stream::MaybeTlsStream;

use crate::{
    coalesce::{CoalesceQueue, OutboundXform},
    conv::{UsdMat4, usd_from_wire, wire_from_usd},
    error::CollabError,
    proto::{
        BaseMessage, CreatePrimRequest, PrimRefType, PrimType, SeparateTransform,
        SetPrimActiveRequest, TransformUpdate, Vector3, base_message, create_prim_broadcast,
        transform_update,
    },
};

/// The transform payload carried by an inbound remote edit.
///
/// The server prefers a decomposed [`RemoteEditKind::Separate`] (via
/// `UsdGeomXformCommonAPI::GetXformVectorsByAccumulation`) for broadcasts, the
/// join snapshot, and corrections, falling back to [`RemoteEditKind::Matrix`]
/// only when accumulation fails. Both variants must be applied — keeping the
/// decomposed TRS verbatim avoids an Euler round-trip through a rebuilt matrix.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteEditKind {
    /// Ordered end of the join snapshot. Only the stage owner may confirm it.
    SnapshotComplete,
    /// Successful acknowledgement of an already locally authored transform.
    Acknowledged { request_id: u64 },
    /// A state-bearing frame that cannot be applied. It blocks the prefix.
    Invalid { reason: String },
    /// Flat row-major local matrix (already converted from the wire convention).
    Matrix(UsdMat4),
    /// Decomposed transform: rotation is Euler **degrees** in XYZ order.
    Separate {
        translation: [f64; 3],
        rotation_deg: [f64; 3],
        scale: [f64; 3],
    },
    /// A built-in prim the server authored (originator echo, a peer's create, or
    /// a join-snapshot replay). `RemoteEdit::prim_path` is the full server-chosen
    /// path; `type_name` is the USD schema type (`"Cube"`, `"Xform"`, …). The
    /// session authors it into the root layer so the normal hydration flow
    /// renders it. The broadcast carries the attribute values the server actually
    /// authored (has-field semantics: only those applicable to the type are
    /// `Some`); the session authors each present value so the local prim matches
    /// server state exactly rather than relying on local schema fallback defaults
    /// (which can diverge across schema versions or non-default server authoring).
    /// `world`, when present, is the prim's parent-relative **local** transform
    /// the server authored; the session authors it verbatim (the current
    /// contract authors create transforms directly into the layer, with no
    /// world→local conversion). When `None` the prim inherits its parent's
    /// transform (identity local).
    CreatePrim {
        type_name: String,
        size: Option<f64>,
        height: Option<f64>,
        radius: Option<f64>,
        axis: Option<String>,
        width: Option<f64>,
        length: Option<f64>,
        world: Option<RemoteWorldTransform>,
    },
    /// A typeless reference/payload prim the server authored (`PrimType::Ref`).
    /// `RemoteEdit::prim_path` is the full server-chosen path; the session authors
    /// a typeless `def` at that path and then a composition arc — a **payload**
    /// when `as_payload`, else a **reference** — to `reference_path`
    /// (`target_path = None`, composing the asset's `defaultPrim`). `world` is
    /// handled exactly as for [`Self::CreatePrim`].
    CreateReference {
        as_payload: bool,
        reference_path: String,
        world: Option<RemoteWorldTransform>,
    },
    /// A prim active-state change the server authored (our own echo, a peer's
    /// soft delete / undo, or a join-snapshot replay). `RemoteEdit::prim_path` is
    /// the full prim path; `active == false` is a soft delete (the session
    /// authors `active=false` into the root layer, pruning the subtree from the
    /// scene tree and render), `active == true` re-activates it (the session
    /// re-authors `active=true`, and the subtree is re-hydrated and rendered).
    /// Authoring `active` is a topology resync, not a TRANSFORM edit, so the
    /// outbound hook never re-broadcasts it — no echo guard is needed.
    SetActive {
        active: bool,
    },
    /// A file the session's stage uses was reloaded on the server (after a
    /// commit or a replacing upload). The session must re-fetch that one file
    /// (bypassing the cache), reload it into the stage, then ask for a fresh
    /// snapshot — see [`CollabClient::request_snapshot`]. `usd_path` is the
    /// server's canonical uploads-relative path and `usd_uri` its download URI
    /// (`/api/v1/download/<percent-encoded path>`), both as on the `Handshake`,
    /// so the session can match them against the layer it holds.
    LayerReloaded {
        usd_path: String,
        usd_uri: String,
    },
}

/// The **local** transform a `CreatePrimBroadcast` may carry for the new prim,
/// relative to its parent. The updated idtx-core contract authors this
/// transform directly into the layer (no world→local conversion), so the
/// session authors it verbatim as the prim's local `xformOp` (see
/// `StudioSession::apply_remote_create_transform`). Mirrors the two wire
/// shapes; `Matrix` is already converted from the wire convention to a flat
/// row-major USD matrix via [`usd_from_wire`].
///
/// (The name is retained for compatibility; despite "World" the value is the
/// prim's local transform under the current contract.)
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteWorldTransform {
    /// Flat row-major local matrix (already converted from the wire convention).
    Matrix(UsdMat4),
    /// Decomposed local transform: rotation is Euler **degrees** in XYZ order.
    Separate {
        translation: [f64; 3],
        rotation_deg: [f64; 3],
        scale: [f64; 3],
    },
}

/// An edit authored by a remote peer, ready to apply to the local USD stage.
#[derive(Debug, Clone)]
pub struct RemoteEdit {
    /// State version described by this envelope; zero carries no version.
    pub server_seq: u64,
    delivery_id: u64,
    pub prim_path: String,
    pub kind: RemoteEditKind,
}

/// A protocol event surfaced from the background worker for diagnostics /
/// metrics (acks and server errors). Drained by the session via
/// [`CollabClient::drain_events`].
#[derive(Debug, Clone)]
pub enum CollabEvent {
    /// Server acknowledgement of a client `TransformUpdate`. `error` is one of
    /// `stale` / `invalid_base` / `apply_failed` / `queue_full` /
    /// `session_closing` when `ok` is false, otherwise empty.
    Ack { request_id: u64, server_seq: u64, ok: bool, error: String },
    /// Out-of-band server error notification.
    Error { code: String, message: String },
    /// The server rejected a `CreatePrimRequest`. `reason` is the raw server
    /// error code (`invalid_parent` / `apply_failed` / `queue_full` /
    /// `session_closing`). Surfaced to the user by the session.
    CreateRejected {
        parent_path: String,
        type_name: String,
        reason: String,
    },
    /// The server rejected a `SetPrimActiveRequest`. `reason` is the raw server
    /// error code (`invalid_target` / `foreign_layer` / `apply_failed` /
    /// `queue_full` / `session_closing`). `active` is the state that was
    /// requested (`false` = a soft delete, `true` = an undo). Surfaced to the
    /// user by the session.
    SetActiveRejected {
        prim_path: String,
        active: bool,
        reason: String,
    },
}

/// A pending outbound create request, assigned a `request_id` and sent once the
/// session has synced. `prim_type` is the wire enum; `type_name` is kept for a
/// human-readable rejection message. For a reference/payload create
/// (`prim_type == PrimType::Ref`) `ref_type` and `reference_path` carry the arc
/// kind and target; both are `None` for a built-in gprim create.
#[derive(Debug, Clone)]
struct PendingCreate {
    base_seq: u64,
    parent_path: String,
    prim_type: PrimType,
    type_name: String,
    ref_type: Option<PrimRefType>,
    reference_path: Option<String>,
}

/// A pending outbound active-state change, assigned a `request_id` and sent once
/// the session has synced. Mirrors [`PendingCreate`]: kept (not discarded) across
/// an unsynced gap, and recorded per-`request_id` so a failing ack can be
/// reported to the user with the prim it was for.
#[derive(Debug, Clone)]
struct PendingSetActive {
    base_seq: u64,
    prim_path: String,
    active: bool,
}

#[derive(Debug)]
enum DeferredEdit {
    Transform(String, crate::coalesce::AuthoredXform),
    Create(PendingCreate),
    SetActive(PendingSetActive),
}

impl DeferredEdit {
    fn bytes(&self) -> usize {
        512 + match self {
            Self::Transform(path, _) => path.len(),
            Self::Create(edit) => edit.parent_path.len() + edit.type_name.len() + edit.reference_path.as_ref().map_or(0, String::len),
            Self::SetActive(edit) => edit.prim_path.len(),
        }
    }
}

#[derive(Default)]
struct CommitGate {
    pending: Option<PendingCommit>,
    deferred: Vec<DeferredEdit>,
    committed_intent: u64,
    reserved_count: usize,
    reserved_bytes: usize,
    /// Prim paths that already hold an uncommitted transform charge this
    /// commit-cycle. Transforms coalesce last-writer-wins (one pending edit per
    /// prim), so re-sampling a prim already charged here must not add a new
    /// reservation — otherwise a single drag's thousands of samples exhaust the
    /// queue budget while only one transform per prim is ever actually pending.
    /// Rebuilt from the deferred backlog on a successful commit.
    charged_transform_prims: HashSet<String>,
}

struct PendingCommit {
    reply: mpsc::SyncSender<Result<u64, String>>,
    deadline: std::time::Instant,
    writing: Option<CommitWrite>,
    intent: u64,
}

struct CommitWrite {
    receiver: mpsc::Receiver<Result<(), String>>,
    server_seq: u64,
    cutoff: u64,
}

/// Thread-safe handle to the collaboration background worker.
///
/// `send_transform` and `next_inbound` both take `&self` so they can be
/// called through a shared borrow (e.g. `Option<CollabClient>`).
pub struct CollabClient {
    pub session_id: String,
    outbound: Arc<Mutex<CoalesceQueue>>,
    /// Pending outbound prim-create requests, FIFO. Drained by the worker once
    /// synced (unlike transforms, a create issued just before sync is kept and
    /// sent after, not discarded).
    outbound_creates: Arc<Mutex<Vec<PendingCreate>>>,
    /// Pending outbound prim active-state changes (soft delete / undo), FIFO.
    /// Drained by the worker once synced, exactly like `outbound_creates`.
    outbound_set_active: Arc<Mutex<Vec<PendingSetActive>>>,
    inbound: Arc<Mutex<VecDeque<RemoteEdit>>>,
    events: Arc<Mutex<VecDeque<CollabEvent>>>,
    /// Flipped only when the stage owner confirms the ordered snapshot barrier.
    synced: Arc<AtomicBool>,
    applied_seq: Arc<AtomicU64>,
    snapshot_seq: AtomicU64,
    application_failed: AtomicBool,
    resume_live: AtomicBool,
    /// Superseded connection prefixes retained for diagnosis/recovery.
    recovery_inbound: Arc<Mutex<Vec<RemoteEdit>>>,
    journal: Arc<Mutex<crate::journal::Journal>>,
    commit_gate: Arc<Mutex<CommitGate>>,
    intent_revision: AtomicU64,
    stop_tx: mpsc::SyncSender<()>,
    worker: Option<thread::JoinHandle<()>>,
    socket_control: crate::transport::Control,
    token: String,
    shared_auth: bool,
    /// When `false` the session was created by someone else (joined, not owned);
    /// `close()` and `Drop` will disconnect the WebSocket but will NOT send
    /// `DELETE /api/v1/sessions/{id}` so the session stays alive for other peers.
    owns_session: bool,
    /// Full WebSocket URL (`ws://` or `wss://`) stored for reconnect.
    ws_url: String,
}

impl std::fmt::Debug for CollabClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollabClient")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl CollabClient {
    /// Connect to `ws_url` and start the worker receiving the server's handshake
    /// and snapshot. Network errors are surfaced asynchronously through events.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn a new thread.
    pub fn connect(ws_url: &str, session_id: &str, token: &str) -> Result<Self, CollabError> {
        Self::connect_inner(ws_url, session_id, token, false)
    }

    fn connect_inner(ws_url: &str, session_id: &str, token: &str, shared_auth: bool) -> Result<Self, CollabError> {
        if ws_url.len() > 16 * 1024 || session_id.len() > 1024 || token.len() > 16 * 1024 {
            return Err(CollabError::WebSocket("connection parameters exceed limits".into()));
        }
        let outbound: Arc<Mutex<CoalesceQueue>> = Arc::new(Mutex::new(CoalesceQueue::new()));
        let outbound_creates: Arc<Mutex<Vec<PendingCreate>>> = Arc::new(Mutex::new(Vec::new()));
        let outbound_set_active: Arc<Mutex<Vec<PendingSetActive>>> =
            Arc::new(Mutex::new(Vec::new()));
        let inbound: Arc<Mutex<VecDeque<RemoteEdit>>> = Arc::new(Mutex::new(VecDeque::new()));
        let events: Arc<Mutex<VecDeque<CollabEvent>>> = Arc::new(Mutex::new(VecDeque::new()));
        let synced = Arc::new(AtomicBool::new(false));
        let journal = Arc::new(Mutex::new(crate::journal::Journal::default()));
        let commit_gate = Arc::new(Mutex::new(CommitGate::default()));
        let applied_seq = Arc::new(AtomicU64::new(0));
        let (stop_tx, stop_rx) = mpsc::sync_channel::<()>(1);

        let permit = crate::transport::Permit::acquire()?;
        let socket_control = Arc::new(Mutex::new(None));
        let control = socket_control.clone();
        let url = ws_url.to_owned();

        // NB: the client does NOT send a Handshake. Per the reference
        // idtx-flow client, `Handshake` is a server→client message: the client
        // authenticates via the `Authorization: Bearer` upgrade header and the
        // `sid` query parameter already embedded in `ws_url`, then *receives* a
        // Handshake, zero+ snapshot broadcasts, and a terminal SnapshotComplete.
        // The client only ever encodes `TransformUpdate` frames, and only after
        // SnapshotComplete has latched `synced` (see `flush_outbound`).

        let out_clone = Arc::clone(&outbound);
        let create_clone = Arc::clone(&outbound_creates);
        let set_active_clone = Arc::clone(&outbound_set_active);
        let in_clone = Arc::clone(&inbound);
        let ev_clone = Arc::clone(&events);
        let synced_clone = Arc::clone(&synced);
        let journal_clone = Arc::clone(&journal);
        let gate_clone = Arc::clone(&commit_gate);
        let applied_clone = Arc::clone(&applied_seq);
        let commit_origin = http_origin_from_ws(ws_url);
        let worker_token = token.to_owned();
        let sid = session_id.to_owned();
        let worker = thread::Builder::new()
            .name("idtx-collab".into())
            .spawn(move || {
                let _permit = permit;
                let socket = match crate::transport::connect(&url, &worker_token, &control) {
                    Ok(socket) => socket,
                    Err(error) => {
                        push_event(&ev_clone, CollabEvent::Error { code: "connect_failed".into(), message: error.to_string() });
                        return;
                    }
                };
                run_loop(
                    socket,
                    &out_clone,
                    &create_clone,
                    &set_active_clone,
                    &in_clone,
                    &ev_clone,
                    &synced_clone,
                    &journal_clone,
                    &gate_clone,
                    &applied_clone,
                    &commit_origin,
                    &worker_token,
                    shared_auth,
                    &stop_rx,
                    &sid,
                );
            })
            .expect("spawn idtx-collab thread");

        Ok(Self {
            session_id: session_id.to_owned(),
            outbound,
            outbound_creates,
            outbound_set_active,
            inbound,
            events,
            synced,
            applied_seq,
            snapshot_seq: AtomicU64::new(0),
            application_failed: AtomicBool::new(false),
            resume_live: AtomicBool::new(false),
            recovery_inbound: Arc::new(Mutex::new(Vec::new())),
            journal,
            commit_gate,
            intent_revision: AtomicU64::new(0),
            stop_tx,
            worker: Some(worker),
            socket_control,
            token: token.to_owned(),
            shared_auth,
            owns_session: true,
            ws_url: ws_url.to_owned(),
        })
    }

    /// Connect to an *existing* session without taking ownership of it.
    ///    
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn a new thread.
    pub fn join(ws_url: &str, session_id: &str, token: &str) -> Result<Self, CollabError> {
        let mut client = Self::connect(ws_url, session_id, token)?;
        client.owns_session = false;
        Ok(client)
    }

    /// Editor connections follow shared credential rotation and logout. A
    /// rotated token disconnects the old socket; explicit reconnect re-joins
    /// with a fresh snapshot and preserves the recovery journal.
    pub fn connect_shared(ws_url: &str, session_id: &str, owns_session: bool) -> Result<Self, CollabError> {
        let header = idtx_auth::authorization_header_for(&http_origin_from_ws(ws_url))
            .ok_or_else(|| CollabError::WebSocket("login required".into()))?;
        let token = header.strip_prefix("Bearer ").ok_or_else(|| CollabError::WebSocket("bearer login required".into()))?;
        let mut client = Self::connect_inner(ws_url, session_id, token, true)?;
        client.owns_session = owns_session;
        Ok(client)
    }

    /// Queue a local transform edit for outbound sync (coalesced, last-writer-wins).
    ///
    /// Sends a baked local matrix. Prefer [`send_transform_separate`] for prims
    /// whose authored xform is a pure translate/`rotateXYZ`/scale stack: the
    /// matrix path forces the server to decompose, which swaps the Y/Z scale
    /// axes under a ±90° rotation.
    ///
    /// [`send_transform_separate`]: Self::send_transform_separate
    pub fn send_transform(&self, prim_path: String, matrix: UsdMat4) -> bool {
        self.enqueue(DeferredEdit::Transform(prim_path, crate::coalesce::AuthoredXform {
            transform: OutboundXform::Matrix(matrix), base_seq: self.applied_seq(),
        }))
    }

    /// Queue a local transform edit as an exact decomposed TRS (coalesced,
    /// last-writer-wins). Rotation is Euler **degrees**, XYZ order.
    ///
    /// Preferred over [`send_transform`] for pure translate/`rotateXYZ`/scale
    /// prims: the server stores the Euler angles and per-axis scale verbatim, so
    /// no lossy matrix→TRS decomposition is ever forced. Baking such a prim to a
    /// matrix and letting the server decompose it swaps the Y and Z scale axes
    /// under a ±90° rotation (the decompose is ambiguous for a non-uniform scale
    /// combined with an axis-aligned quarter turn), which only surfaces after the
    /// stage is persisted server-side and reopened.
    ///
    /// [`send_transform`]: Self::send_transform
    pub fn send_transform_separate(
        &self,
        prim_path: String,
        translation: [f64; 3],
        rotation_deg: [f64; 3],
        scale: [f64; 3],
    ) -> bool {
        self.enqueue(DeferredEdit::Transform(prim_path, crate::coalesce::AuthoredXform {
            transform: OutboundXform::Separate { translation, rotation_deg, scale }, base_seq: self.applied_seq(),
        }))
    }

    /// Queue a request for the server to create a built-in prim of `usd_type_name`
    /// under `parent_path`. Returns `false` (and queues nothing) when the type is
    /// not a server-creatable built-in (Cube/Sphere/Cone/Cylinder/Capsule/Xform);
    /// the caller surfaces that to the user. On success the prim is authored only
    /// when the server's `CreatePrimBroadcast` arrives (see [`Self::next_inbound`]).
    #[must_use]
    pub fn send_create_prim(&self, parent_path: &str, usd_type_name: &str) -> bool {
        let Some(prim_type) = PrimType::from_usd_type_name(usd_type_name) else {
            return false;
        };
        // Report success only if the request was actually enqueued. A poisoned
        // lock means nothing reaches the server, so returning `true` here would
        // tell the user the create was requested when it was silently dropped.
        self.enqueue(DeferredEdit::Create(PendingCreate {
                    base_seq: self.applied_seq(),
                    parent_path: parent_path.to_owned(),
                    prim_type,
                    type_name: usd_type_name.to_owned(),
                    ref_type: None,
                    reference_path: None,
        }))
    }

    /// Queue a request for the server to create a typeless reference/payload prim
    /// (`PrimType::Ref`) under `parent_path`, composing the layer/prim at
    /// `reference_path` as a **payload** when `as_payload` (else a reference).
    /// Returns `false` only when the request could not be enqueued (poisoned
    /// lock) — a poisoned lock means nothing reaches the server, so reporting
    /// success would tell the user the create was requested when it was silently
    /// dropped. On success the prim is authored only when the server's
    /// `CreatePrimBroadcast` arrives (see [`Self::next_inbound`]), mirroring
    /// [`send_create_prim`]. Never sets a world transform — the prim inherits its
    /// parent's placement unless the server authors one in the broadcast.
    #[must_use]
    pub fn send_create_reference(
        &self,
        parent_path: &str,
        reference_path: &str,
        as_payload: bool,
    ) -> bool {
        let ref_type = if as_payload {
            PrimRefType::Payload
        } else {
            PrimRefType::Reference
        };
        self.enqueue(DeferredEdit::Create(PendingCreate {
                    base_seq: self.applied_seq(),
                    parent_path: parent_path.to_owned(),
                    prim_type: PrimType::Ref,
                    // Human-readable label for a rejection notice; a REF create
                    // has no schema type name.
                    type_name: "Asset".to_owned(),
                    ref_type: Some(ref_type),
                    reference_path: Some(reference_path.to_owned()),
        }))
    }

    /// Queue a request for the server to change the active state of the prim at
    /// `prim_path`. `active == false` is a *soft delete* (the server authors
    /// `active=false`, pruning the prim's subtree); `active == true` undoes it.
    /// Returns `false` only when the request could not be enqueued (poisoned
    /// lock) — a poisoned lock means nothing reaches the server, so reporting
    /// success would tell the user the delete was requested when it was silently
    /// dropped. On success the change is authored locally only when the server's
    /// `SetPrimActiveBroadcast` arrives (see [`Self::next_inbound`]), mirroring the
    /// server-authoritative create path.
    #[must_use]
    pub fn send_set_prim_active(&self, prim_path: &str, active: bool) -> bool {
        self.enqueue(DeferredEdit::SetActive(PendingSetActive {
                    base_seq: self.applied_seq(),
                    prim_path: prim_path.to_owned(),
                    active,
        }))
    }

    fn enqueue(&self, edit: DeferredEdit) -> bool {
        let Ok(mut gate) = self.commit_gate.lock() else { return false };
        let bytes = edit.bytes();
        // A transform for a prim that already holds an uncommitted transform
        // charge this commit-cycle coalesces last-writer-wins onto the existing
        // pending edit (identical byte charge, no new queue slot), so it is
        // charged net-zero: neither re-reserved nor subject to the capacity gate.
        // Creates and active-state changes are distinct FIFO operations and are
        // always charged.
        let already_charged = matches!(&edit, DeferredEdit::Transform(path, _)
            if gate.charged_transform_prims.contains(path));
        if !already_charged
            && (gate.reserved_count >= 4096 || bytes > crate::transport::MAX_FRAME / 2
            || gate.reserved_bytes.saturating_add(bytes) > 8 * 1024 * 1024) {
            push_event(&self.events, CollabEvent::Error { code: "local_queue_full".into(),
                message: "Edit was not queued; retain/export local opinions and commit or reconcile pending intent".into() });
            return false;
        }
        if !already_charged {
            gate.reserved_count += 1;
            gate.reserved_bytes += bytes;
            if let DeferredEdit::Transform(path, _) = &edit { gate.charged_transform_prims.insert(path.clone()); }
        }
        self.intent_revision.fetch_add(1, Ordering::AcqRel);
        if gate.pending.is_some() { gate.deferred.push(edit); return true; }
        enqueue_direct(edit, &self.outbound, &self.outbound_creates, &self.outbound_set_active)
    }

    #[must_use]
    pub fn intent_revision(&self) -> u64 { self.intent_revision.load(Ordering::Acquire) }

    #[must_use]
    pub fn has_uncommitted_intent(&self) -> bool {
        self.commit_gate.lock().map_or(true, |gate| gate.committed_intent < self.intent_revision())
    }

    /// Capture the current outbound prefix. New edits are deferred until its
    /// acknowledgements, stage application and HTTP persistence have completed.
    pub fn begin_commit(&self) -> Result<mpsc::Receiver<Result<u64, String>>, CollabError> {
        if !self.is_synced() { return Err(CollabError::WebSocket("collaboration is not synchronized".into())); }
        let mut gate = self.commit_gate.lock().map_err(|_| CollabError::WebSocket("commit state unavailable".into()))?;
        if gate.pending.is_some() { return Err(CollabError::WebSocket("commit already in progress".into())); }
        let (reply, receiver) = mpsc::sync_channel(1);
        gate.pending = Some(PendingCommit { reply, deadline: std::time::Instant::now() + Duration::from_secs(10), writing: None, intent: self.intent_revision() });
        Ok(receiver)
    }

    /// Inspect the next ordered envelope without removing it. Confirm it only
    /// after successful stage application; a failure leaves the prefix intact.
    #[must_use]
    pub fn next_inbound(&self) -> Option<RemoteEdit> {
        if self.application_failed.load(Ordering::Acquire) { return None; }
        self.inbound.lock().ok()?.front().cloned()
    }

    /// Confirm exactly the current prefix item. Out-of-order or repeated
    /// confirmations do not advance state. Snapshot edits accumulate privately
    /// until the ordered completion barrier is confirmed.
    pub fn acknowledge_inbound(&self, edit: &RemoteEdit) -> bool {
        let Ok(mut queue) = self.inbound.lock() else { return false };
        if self.application_failed.load(Ordering::Acquire)
            || queue.front().is_none_or(|head| head.delivery_id != edit.delivery_id) {
            return false;
        }
        let Some(edit) = queue.pop_front() else { return false };
        self.snapshot_seq.fetch_max(edit.server_seq, Ordering::AcqRel);
        if matches!(edit.kind, RemoteEditKind::SnapshotComplete) {
            self.applied_seq.fetch_max(self.snapshot_seq.load(Ordering::Acquire), Ordering::AcqRel);
            self.synced.store(true, Ordering::Release);
        } else if self.synced.load(Ordering::Acquire) || self.resume_live.load(Ordering::Acquire) {
            let candidate = self.snapshot_seq.load(Ordering::Acquire);
            // Multiple queued frames can describe one version (e.g. a create
            // plus placement). Never publish a known partially applied group.
            if queue.front().is_none_or(|next| next.server_seq > candidate) {
                self.applied_seq.fetch_max(candidate, Ordering::AcqRel);
            }
            if queue.is_empty() && self.resume_live.swap(false, Ordering::AcqRel) {
                self.synced.store(true, Ordering::Release);
            }
        }
        true
    }

    /// Stop at a failed application prefix. S11 supplies reconciliation/retry;
    /// neither receipt nor a later snapshot barrier may hide this failure.
    pub fn fail_inbound(&self) {
        self.application_failed.store(true, Ordering::Release);
        if self.synced.swap(false, Ordering::AcqRel) {
            self.resume_live.store(true, Ordering::Release);
        }
    }

    /// Retry the same prefix after the stage owner repairs a prerequisite.
    /// This does not acknowledge anything or permit outbound sends yet.
    pub fn retry_application(&self) {
        self.application_failed.store(false, Ordering::Release);
    }

    #[must_use]
    pub fn application_failed(&self) -> bool {
        self.application_failed.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn recovery_inbound(&self) -> Vec<RemoteEdit> {
        self.recovery_inbound.lock().map(|queue| queue.clone()).unwrap_or_default()
    }

    /// All sent uncommitted intent and known outcomes, including lost acks.
    #[must_use]
    pub fn operation_journal(&self) -> Vec<crate::journal::Operation> {
        self.journal.lock().map(|journal| journal.operations.values().cloned().collect()).unwrap_or_default()
    }

    /// Current application backlog; used by status/overload diagnostics.
    #[must_use]
    pub fn inbound_count(&self) -> usize {
        self.inbound.lock().map_or(256, |queue| queue.len())
    }

    /// Human-readable recovery evidence, including definitely unsent intent.
    /// Transport credentials are excluded; authored paths are retained verbatim.
    /// This is not an automatic replay file.
    ///
    /// # Panics
    /// Panics if a worker poisoned a shared queue lock.
    pub fn recovery_report(&self) -> String {
        let gate = self.commit_gate.lock().expect("commit gate");
        format!("Session: {}\nApplied revision: {}\nOperations:\n{:#?}\nUnsent transforms:\n{:#?}\nUnsent creates:\n{:#?}\nUnsent active changes:\n{:#?}\nDeferred:\n{:#?}\nArchived inbound:\n{:#?}\nPending inbound:\n{:#?}\n",
            self.session_id, self.applied_seq(), self.operation_journal(),
            self.outbound.lock().expect("outbound"), self.outbound_creates.lock().expect("creates"),
            self.outbound_set_active.lock().expect("active"), gate.deferred, self.recovery_inbound(), self.inbound.lock().expect("inbound"))
    }

    /// Highest server revision successfully applied by the stage owner.
    #[must_use]
    pub fn applied_seq(&self) -> u64 {
        self.applied_seq.load(Ordering::Acquire)
    }

    /// Drain protocol events (acks, server errors) accumulated by the worker.
    #[must_use]
    pub fn drain_events(&self) -> Vec<CollabEvent> {
        match self.events.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// True only after the stage owner confirms the complete snapshot prefix.
    #[must_use]
    pub fn is_synced(&self) -> bool {
        self.is_connected() && self.synced.load(Ordering::Acquire)
    }

    /// Returns `true` while the background I/O thread is still running.
    ///
    /// Once the server closes the WebSocket (e.g. idle timeout) the thread
    /// exits and this returns `false`.  Outbound messages queued after that
    /// point are retained within the admission limits until reconnect.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| !w.is_finished())
    }

    /// Returns `true` when this client created and owns the server-side session
    /// (via [`connect`]), and `false` when it merely joined a session created by
    /// another peer (via [`join`]).
    ///
    /// Distinguishes "Open Remote with Sync" (owner) from "Connect Remote"
    /// (joiner) for connection-status display.
    #[must_use]
    pub fn owns_session(&self) -> bool {
        self.owns_session
    }

    /// # Panics
    ///
    /// The function panics if it is unable to spawn a new collaboration worker
    /// thread.
    ///
    /// Tear down the current (dead or live) background thread and establish a
    /// fresh WebSocket connection to the same session URL.
    ///
    /// The inbound and outbound queues are preserved across reconnects so no
    /// state is lost. Returns admission errors immediately; connection failures
    /// arrive through events. A fresh snapshot confirms completion.
    pub fn reconnect(&mut self) -> Result<(), CollabError> {
        if self.shared_auth {
            self.token = idtx_auth::authorization_header_for(&self.http_origin())
                .and_then(|h| h.strip_prefix("Bearer ").map(str::to_owned))
                .ok_or_else(|| CollabError::WebSocket("login required before reconnect".into()))?;
        }
        if self.worker.as_ref().is_some_and(|worker| !worker.is_finished())
            && self.socket_control.lock().map_or(true, |socket| socket.is_none()) {
            return Err(CollabError::WebSocket("connection attempt still in progress".into()));
        }
        if self.recovery_inbound.lock().map_or(true, |queue| queue.len() + 256 > 512) {
            return Err(CollabError::WebSocket("recovery capacity reached; export evidence before reconnecting".into()));
        }
        let permit = crate::transport::Permit::acquire()?;
        self.synced.store(false, Ordering::Release);
        // Stop and join the old worker if still running.
        let _ = self.stop_tx.try_send(());
        crate::transport::stop(&self.socket_control);
        // Socket shutdown interrupts the old worker. Joining happens on its
        // replacement, so the frame never waits for network teardown.
        let previous = self.worker.take();

        // Archive after the worker has exited below, preventing a late old
        // connection frame from entering the new snapshot.
        self.application_failed.store(false, Ordering::Release);
        self.resume_live.store(false, Ordering::Release);
        self.snapshot_seq.store(self.applied_seq(), Ordering::Release);

        let (stop_tx, stop_rx) = mpsc::sync_channel::<()>(1);
        self.stop_tx = stop_tx;

        // A separate queue isolates the replacement connection; after joining,
        // its worker archives the entire old prefix, including any late frame.
        let old_inbound = self.inbound.clone();
        self.inbound = Arc::new(Mutex::new(VecDeque::new()));
        let recovery = self.recovery_inbound.clone();
        self.socket_control = Arc::new(Mutex::new(None));
        let control = self.socket_control.clone();
        let url = self.ws_url.clone();

        // A reconnect re-gates on a fresh snapshot: the server replays the join
        // snapshot and a new SnapshotComplete, so disarm until it arrives.
        self.synced.store(false, Ordering::Release);

        let out_clone = Arc::clone(&self.outbound);
        let create_clone = Arc::clone(&self.outbound_creates);
        let set_active_clone = Arc::clone(&self.outbound_set_active);
        let in_clone = Arc::clone(&self.inbound);
        let ev_clone = Arc::clone(&self.events);
        let synced_clone = Arc::clone(&self.synced);
        let journal_clone = Arc::clone(&self.journal);
        let gate_clone = Arc::clone(&self.commit_gate);
        let applied_clone = Arc::clone(&self.applied_seq);
        let commit_origin = self.http_origin();
        let worker_token = self.token.clone();
        let shared_auth = self.shared_auth;
        let sid = self.session_id.clone();
        let worker = thread::Builder::new()
            .name("idtx-collab".into())
            .spawn(move || {
                let _permit = permit;
                if let Some(previous) = previous { let _ = previous.join(); }
                if let Ok(mut old) = old_inbound.lock() {
                    recovery.lock().expect("recovery").extend(old.drain(..));
                }
                let socket = match crate::transport::connect(&url, &worker_token, &control) {
                    Ok(socket) => socket,
                    Err(error) => {
                        push_event(&ev_clone, CollabEvent::Error { code: "connect_failed".into(), message: error.to_string() });
                        return;
                    }
                };
                run_loop(
                    socket,
                    &out_clone,
                    &create_clone,
                    &set_active_clone,
                    &in_clone,
                    &ev_clone,
                    &synced_clone,
                    &journal_clone,
                    &gate_clone,
                    &applied_clone,
                    &commit_origin,
                    &worker_token,
                    shared_auth,
                    &stop_rx,
                    &sid,
                );
            })
            .expect("spawn idtx-collab thread");
        self.worker = Some(worker);
        debug!(session_id = self.session_id, "collab socket reconnected");
        Ok(())
    }

    /// Commit the active session's overrides to the server's origin file.
    ///
    /// Calls `POST /api/v1/sessions/{id}/commit`.  Does not destroy the session.
    pub fn commit(&self) -> Result<(), crate::error::CollabError> {
        self.begin_commit()?.recv()
            .map_err(|_| CollabError::WebSocket("commit worker disconnected".into()))?
            .map(|_| ()).map_err(CollabError::WebSocket)
    }

    /// Derive `http(s)://host:port` from the stored `ws(s)://host:port/path?query` URL.
    fn http_origin(&self) -> String {
        http_origin_from_ws(&self.ws_url)
    }

    /// Interrupt the socket and signal the bounded worker to stop, without a join.
    /// The backend will take care of session clean-up if no more clients are connected to it.
    pub fn close(mut self) {
        let _ = self.stop_tx.try_send(());
        crate::transport::stop(&self.socket_control);
        self.worker.take();
    }
}

impl Drop for CollabClient {
    fn drop(&mut self) {
        // `close()` calls `self.worker.take()`, so a `None` worker means `close()` already ran.
        if self.worker.is_none() {
            return;
        }
        let _ = self.stop_tx.try_send(());
        crate::transport::stop(&self.socket_control);
        self.worker.take();
    }
}

// ---------------------------------------------------------------------------
// Background thread
// ---------------------------------------------------------------------------

fn http_origin_from_ws(url: &str) -> String {
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("wss://") { ("https", rest) }
        else if let Some(rest) = url.strip_prefix("ws://") { ("http", rest) }
        else { return url.to_owned() };
    format!("{scheme}://{}", rest.split('/').next().unwrap_or(rest))
}

fn enqueue_direct(edit: DeferredEdit, outbound: &Mutex<CoalesceQueue>, creates: &Mutex<Vec<PendingCreate>>, active: &Mutex<Vec<PendingSetActive>>) -> bool {
    match edit {
        DeferredEdit::Transform(path, edit) => outbound.lock().map(|mut q| q.push(path, edit.transform, edit.base_seq)).is_ok(),
        DeferredEdit::Create(edit) => creates.lock().map(|mut q| q.push(edit)).is_ok(),
        DeferredEdit::SetActive(edit) => active.lock().map(|mut q| q.push(edit)).is_ok(),
    }
}

fn finish_commit(gate: &Mutex<CommitGate>, result: Result<u64, String>, outbound: &Mutex<CoalesceQueue>, creates: &Mutex<Vec<PendingCreate>>, active: &Mutex<Vec<PendingSetActive>>) {
    let mut gate = gate.lock().expect("commit gate");
    complete_commit(&mut gate, result, outbound, creates, active);
}

fn complete_commit(gate: &mut CommitGate, result: Result<u64, String>, outbound: &Mutex<CoalesceQueue>, creates: &Mutex<Vec<PendingCreate>>, active: &Mutex<Vec<PendingSetActive>>) {
    if let Some(pending) = gate.pending.take() { let _ = pending.reply.send(result); }
    for edit in gate.deferred.drain(..) { enqueue_direct(edit, outbound, creates, active); }
}

#[allow(clippy::too_many_arguments)]
fn progress_commit(gate: &Mutex<CommitGate>, journal: &Mutex<crate::journal::Journal>, applied: &AtomicU64,
    synced: &AtomicBool, inbound: &Mutex<VecDeque<RemoteEdit>>, outbound: &Mutex<CoalesceQueue>,
    creates: &Mutex<Vec<PendingCreate>>, active: &Mutex<Vec<PendingSetActive>>, origin: &str, token: &str, session_id: &str) {
    use crate::journal::Outcome;
    let mut gate = gate.lock().expect("commit gate");
    let Some(pending) = gate.pending.as_mut() else { return };
    if let Some(write) = &pending.writing {
        let result = match write.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("commit HTTP worker disconnected".into()),
        };
        let seq = write.server_seq;
        if result.is_ok() {
            journal.lock().expect("operation journal").operations.retain(|id, _| *id > write.cutoff);
            gate.committed_intent = pending.intent;
            // Rebuild the reservation from the deferred backlog that becomes the
            // new uncommitted baseline, charging transforms once per distinct
            // prim (last-writer-wins) to match the `enqueue` accounting — several
            // deferred samples of one prim collapse to a single pending edit when
            // drained into the coalescing queue by `complete_commit` below.
            let mut charged = HashSet::new();
            let mut count = 0usize;
            let mut bytes = 0usize;
            for edit in &gate.deferred {
                if let DeferredEdit::Transform(path, _) = edit
                    && !charged.insert(path.clone()) { continue; }
                count += 1;
                bytes += edit.bytes();
            }
            gate.charged_transform_prims = charged;
            gate.reserved_count = count;
            gate.reserved_bytes = bytes;
        }
        complete_commit(&mut gate, result.map(|()| seq), outbound, creates, active);
        return;
    }
    if !synced.load(Ordering::Acquire) || std::time::Instant::now() >= pending.deadline {
        complete_commit(&mut gate, Err("commit synchronization/acknowledgement barrier failed or timed out".into()), outbound, creates, active);
        return;
    }
    let journal = journal.lock().expect("operation journal");
    if journal.operations.values().any(|op| matches!(op.outcome, Outcome::Rejected { .. } | Outcome::Indeterminate)) {
        complete_commit(&mut gate, Err("rejected or indeterminate operations require reconciliation before commit".into()), outbound, creates, active);
        return;
    }
    let seq = applied.load(Ordering::Acquire);
    if !outbound.lock().expect("outbound").is_empty() || !creates.lock().expect("creates").is_empty()
        || !active.lock().expect("active").is_empty() || !inbound.lock().expect("inbound").is_empty()
        || journal.operations.values().any(|op| !matches!(op.outcome, Outcome::Accepted { server_seq } if server_seq != 0 && server_seq <= seq)) {
        return;
    }
    let cutoff = journal.next_id;
    let origin = origin.to_owned();
    let token = token.to_owned();
    let session_id = session_id.to_owned();
    let (reply, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = crate::session::commit_session(&origin, &token, &session_id).map_err(|error| error.to_string());
        let _ = reply.send(result);
    });
    pending.writing = Some(CommitWrite { receiver, server_seq: seq, cutoff });
}

// Shared queues and worker-local protocol state are threaded individually to
// match the existing I/O-thread style; bundling them would only obscure the
// pass-through.
#[allow(clippy::too_many_arguments)]
fn run_loop(
    mut socket: tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    outbound: &Arc<Mutex<CoalesceQueue>>,
    outbound_creates: &Arc<Mutex<Vec<PendingCreate>>>,
    outbound_set_active: &Arc<Mutex<Vec<PendingSetActive>>>,
    inbound: &Arc<Mutex<VecDeque<RemoteEdit>>>,
    events: &Arc<Mutex<VecDeque<CollabEvent>>>,
    synced: &Arc<AtomicBool>,
    journal: &Mutex<crate::journal::Journal>,
    commit_gate: &Mutex<CommitGate>,
    applied_seq: &AtomicU64,
    commit_origin: &str,
    token: &str,
    shared_auth: bool,
    stop_rx: &mpsc::Receiver<()>,
    session_id: &str,
) {
    // Send a WebSocket Ping every 25 s so the server does not close the
    // connection due to an idle timeout before the user starts editing.
    const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(25);
    let mut last_keepalive = std::time::Instant::now();

    // Worker-local received watermark is diagnostic only. Outbound bases are
    // captured from stage-owner application when each operation is queued.
    // `request_id` is a per-connection monotonic counter echoed back on acks.
    let mut base_seq: u64 = 0;
    let mut request_id = journal.lock().expect("operation journal").next_id;
    // request_id -> pending prim-create, so a failing ack can be reported with
    // the parent path and type it was for.
    let mut pending_creates: HashMap<u64, PendingCreate> = HashMap::new();
    // request_id -> pending active-state change, so a failing ack can be reported
    // with the prim path and the state that was requested.
    let mut pending_set_active: HashMap<u64, PendingSetActive> = HashMap::new();
    let mut pending_transforms = HashSet::new();

    loop {
        if stop_rx.try_recv().is_ok() { break; }
        if shared_auth && idtx_auth::authorization_header_for(commit_origin).as_deref() != Some(&format!("Bearer {token}")) {
            push_event(events, CollabEvent::Error { code: "credentials_changed".into(), message: "Credentials changed; reconnect after login to resume from a fresh snapshot".into() });
            break;
        }
        // Stop reading before capacity: TCP backpressure preserves ordered
        // durable state rather than dropping it. One frame produces <=1 edit.
        if inbound.lock().map_or(true, |queue| queue.len() >= 256) {
            progress_commit(commit_gate, journal, applied_seq, synced, inbound,
                outbound, outbound_creates, outbound_set_active, commit_origin, token, session_id);
            if stop_rx.recv_timeout(Duration::from_millis(10)).is_ok() { break; }
            continue;
        }
        if last_keepalive.elapsed() >= KEEPALIVE_INTERVAL {
            let _ = socket.send(tungstenite::Message::Ping(vec![].into()));
            last_keepalive = std::time::Instant::now();
        }

        if !flush_outbound(
            &mut socket,
            outbound,
            synced,
            &mut request_id,
            &mut pending_transforms,
            journal,
            session_id,
        ) { break; }
        if !flush_outbound_creates(
            &mut socket,
            outbound_creates,
            synced,
            &mut request_id,
            &mut pending_creates,
            journal,
            session_id,
        ) { break; }
        if !flush_outbound_set_active(
            &mut socket,
            outbound_set_active,
            synced,
            &mut request_id,
            &mut pending_set_active,
            journal,
            session_id,
        ) { break; }

        progress_commit(commit_gate, journal, applied_seq, synced, inbound,
            outbound, outbound_creates, outbound_set_active, commit_origin, token, session_id);

        if stop_rx.try_recv().is_ok() {
            debug!(session_id, "collab thread stopping");
            let _ = socket.close(None);
            break;
        }

        match socket.read() {
            Ok(msg) => {
                if let tungstenite::Message::Binary(data) = &msg
                    && let Ok(base) = BaseMessage::decode(data.as_ref()) {
                    journal.lock().expect("operation journal").acknowledge(&base);
                }
                handle_msg(
                msg,
                inbound,
                events,
                synced,
                &mut base_seq,
                &mut pending_creates,
                &mut pending_set_active,
                &mut pending_transforms,
                session_id,
                );
            }
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if stop_rx.recv_timeout(Duration::from_millis(10)).is_ok() { break; }
            }
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                debug!(session_id, "collab WebSocket closed by server");
                break;
            }
            Err(e) => {
                warn!(session_id, error = %e, "collab WebSocket error; stopping");
                break;
            }
        }
    }

    // A lost acknowledgement is indeterminate, even for set-active: replaying
    // it could overwrite a newer peer edit. Preserve evidence without resending.
    let indeterminate = {
        let mut journal = journal.lock().expect("operation journal");
        journal.disconnect();
        journal.operations.values().filter(|operation| operation.outcome == crate::journal::Outcome::Indeterminate).count()
    };
    if indeterminate != 0 {
        push_event(events, CollabEvent::Error { code: "indeterminate_operations".into(),
            message: format!("{indeterminate} operations may have reached the server; evidence retained, automatic resend disabled") });
    }
    finish_commit(commit_gate, Err("connection closed before commit confirmation".into()),
        outbound, outbound_creates, outbound_set_active);
    synced.store(false, Ordering::Release);
}

fn flush_outbound(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    outbound: &Mutex<CoalesceQueue>,
    synced: &AtomicBool,
    request_id: &mut u64,
    pending_transforms: &mut HashSet<u64>,
    journal: &Mutex<crate::journal::Journal>,
    session_id: &str,
) -> bool {
    if !synced.load(Ordering::Acquire) { return true; }
    let edits = match outbound.lock() {
        Ok(mut q) => q.flush(),
        Err(_) => return false,
    };
    if edits.is_empty() {
        return true;
    }
    let ts = now_millis();
    let mut edits = edits.into_iter();
    while let Some((prim_path, xform)) = edits.next() {
        *request_id += 1;
        let msg = make_xform_update(session_id, prim_path, xform.transform, ts, *request_id, xform.base_seq);
        journal.lock().expect("operation journal").record(&msg);
        if let Err(e) = socket.send(tungstenite::Message::Binary(msg.encode_to_vec().into()))
        {
            warn!(session_id, error = %e, "collab send error");
            if let Ok(mut queue) = outbound.lock() { queue.restore_unsent(edits); }
            synced.store(false, Ordering::Release);
            return false;
        }
        pending_transforms.insert(*request_id);
    }
    true
}

/// Send any queued prim-create requests. Unlike transforms, creates are NOT
/// drained until the session has synced — a create issued moments before
/// `SnapshotComplete` must still reach the server, not be discarded. Each sent
/// request is recorded in `pending_creates` so a failing ack can be reported.
fn flush_outbound_creates(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    outbound_creates: &Mutex<Vec<PendingCreate>>,
    synced: &AtomicBool,
    request_id: &mut u64,
    pending_creates: &mut HashMap<u64, PendingCreate>,
    journal: &Mutex<crate::journal::Journal>,
    session_id: &str,
) -> bool {
    // Protocol rule: do not author/send before SnapshotComplete. Leave creates
    // queued (do not drain) until synced so none are lost.
    if !synced.load(Ordering::Acquire) {
        return true;
    }
    let mut creates = match outbound_creates.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return false,
    };
    // Journal each attempted send before I/O. A failed write may have delivered
    // bytes, so only the never-attempted remainder is safe to requeue.
    while !creates.is_empty() {
        *request_id += 1;
        let (parent_path, prim_type, ref_type, reference_path) = {
            let create = &creates[0];
            (
                create.parent_path.clone(),
                create.prim_type,
                create.ref_type,
                create.reference_path.clone(),
            )
        };
        let msg = BaseMessage {
            session_id: Some(session_id.to_owned()),
            payload: Some(base_message::Payload::CreatePrim(CreatePrimRequest {
                session_id: session_id.to_owned(),
                parent_path,
                r#type: prim_type as i32,
                ref_type: ref_type.map(|r| r as i32),
                reference_path,
                // idtx-studio never requests a placement: the server (or the
                // prim's parent) decides the new prim's transform.
                world_transform: None,
            })),
            request_id: *request_id,
            server_seq: creates[0].base_seq,
        };
        let buf = msg.encode_to_vec();
        let create = creates.remove(0);
        journal.lock().expect("operation journal").record(&msg);
        if let Err(e) = socket.send(tungstenite::Message::Binary(buf.into())) {
            warn!(session_id, error = %e, "collab create send error");
            // `creates` is exactly the unsent remainder (sent items were popped
            // off the front). Re-queue it so a reconnect resends in order.
            requeue_unsent_creates(outbound_creates, creates);
            synced.store(false, Ordering::Release);
            return false;
        }
        // Accepted by the socket — now pending an ack. Record it (so a failing
        // ack can be reported) and drop it from the batch.
        pending_creates.insert(*request_id, create);
    }
    true
}

/// Re-queue un-sent prim-create requests at the FRONT of the shared outbound
/// queue, preserving their order ahead of anything queued meanwhile. Called when
/// a socket send fails mid-batch so the reconnected worker resends them —
/// otherwise the batch, already emptied by `mem::take` and held only in the
/// worker-local `pending_creates` (discarded on reconnect), would be lost.
fn requeue_unsent_creates(outbound_creates: &Mutex<Vec<PendingCreate>>, mut unsent: Vec<PendingCreate>) {
    if unsent.is_empty() {
        return;
    }
    if let Ok(mut q) = outbound_creates.lock() {
        unsent.append(&mut *q);
        *q = unsent;
    }
}

/// Send any queued prim active-state changes (soft delete / undo). Mirrors
/// [`flush_outbound_creates`] exactly: nothing is drained until the session has
/// synced (so a change issued moments before `SnapshotComplete` still reaches
/// the server), each sent request is recorded in `pending_set_active` so a
/// failing ack can be reported, and the unsent remainder is re-queued at the
/// front on a socket send failure so a reconnect resends it in order.
fn flush_outbound_set_active(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    outbound_set_active: &Mutex<Vec<PendingSetActive>>,
    synced: &AtomicBool,
    request_id: &mut u64,
    pending_set_active: &mut HashMap<u64, PendingSetActive>,
    journal: &Mutex<crate::journal::Journal>,
    session_id: &str,
) -> bool {
    // Protocol rule: do not author/send before SnapshotComplete. Leave changes
    // queued (do not drain) until synced so none are lost.
    if !synced.load(Ordering::Acquire) {
        return true;
    }
    let mut changes = match outbound_set_active.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return false,
    };
    while !changes.is_empty() {
        *request_id += 1;
        let (prim_path, active) = {
            let change = &changes[0];
            (change.prim_path.clone(), change.active)
        };
        let msg = BaseMessage {
            session_id: Some(session_id.to_owned()),
            payload: Some(base_message::Payload::SetPrimActive(SetPrimActiveRequest {
                session_id: session_id.to_owned(),
                prim_path,
                active,
            })),
            request_id: *request_id,
            server_seq: changes[0].base_seq,
        };
        let buf = msg.encode_to_vec();
        let change = changes.remove(0);
        journal.lock().expect("operation journal").record(&msg);
        if let Err(e) = socket.send(tungstenite::Message::Binary(buf.into())) {
            warn!(session_id, error = %e, "collab set-active send error");
            // `changes` is exactly the unsent remainder (sent items were popped
            // off the front). Re-queue it so a reconnect resends in order.
            requeue_unsent_set_active(outbound_set_active, changes);
            synced.store(false, Ordering::Release);
            return false;
        }
        // Accepted by the socket — now pending an ack. Record it (so a failing
        // ack can be reported) and drop it from the batch.
        pending_set_active.insert(*request_id, change);
    }
    true
}

/// Re-queue un-sent active-state changes at the FRONT of the shared outbound
/// queue, preserving their order ahead of anything queued meanwhile. The
/// set-active analogue of [`requeue_unsent_creates`].
fn requeue_unsent_set_active(
    outbound_set_active: &Mutex<Vec<PendingSetActive>>,
    mut unsent: Vec<PendingSetActive>,
) {
    if unsent.is_empty() {
        return;
    }
    if let Ok(mut q) = outbound_set_active.lock() {
        unsent.append(&mut *q);
        *q = unsent;
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_msg(
    msg: tungstenite::Message,
    inbound: &Mutex<VecDeque<RemoteEdit>>,
    events: &Mutex<VecDeque<CollabEvent>>,
    _synced: &AtomicBool,
    base_seq: &mut u64,
    pending_creates: &mut HashMap<u64, PendingCreate>,
    pending_set_active: &mut HashMap<u64, PendingSetActive>,
    pending_transforms: &mut HashSet<u64>,
    session_id: &str,
) {
    let tungstenite::Message::Binary(data) = msg else {
        return;
    };
    let Ok(base) = BaseMessage::decode(data.as_ref()) else {
        invalid_envelope(inbound, 0, "malformed protobuf frame");
        return;
    };
    let request_id = base.request_id;
    // Diagnostic receipt watermark only; it is never an outbound authored base.
    let server_seq = base.server_seq;
    let adopt_seq = |base_seq: &mut u64| {
        if server_seq > *base_seq {
            *base_seq = server_seq;
        }
    };

    match base.payload {
        Some(base_message::Payload::XformBroadcast(bc)) => {
            adopt_seq(base_seq);
            let Some(update) = bc.update else {
                invalid_envelope(inbound, server_seq, "transform broadcast has no update");
                return;
            };
            let prim_path = update.prim_path;
            let kind = match update.transform {
                Some(transform_update::Transform::Matrix(m)) => {
                    RemoteEditKind::Matrix(usd_from_wire(&m))
                }
                Some(transform_update::Transform::Seperate(s)) => {
                    let v = |opt: Option<crate::proto::Vector3>, dflt: [f64; 3]| {
                        opt.map_or(dflt, |p| [p.x, p.y, p.z])
                    };
                    RemoteEditKind::Separate {
                        translation: v(s.translation, [0.0; 3]),
                        rotation_deg: v(s.rotation, [0.0; 3]),
                        scale: v(s.scale, [1.0; 3]),
                    }
                }
                None => {
                    invalid_envelope(inbound, server_seq, "transform update has no transform");
                    return;
                }
            };
            if let Ok(mut q) = inbound.lock() {
                q.push_back(envelope(server_seq, prim_path, kind));
            }
        }
        Some(base_message::Payload::CreatePrimBroadcast(bc)) => {
            // A prim the server authored — the originator's own echo, a peer's
            // create, or a join-snapshot replay (which arrives ancestors-first,
            // before SnapshotComplete). Queue it on the SAME ordered inbound
            // stream as transforms so a transform positioning this prim can never
            // be applied before the prim is authored.
            adopt_seq(base_seq);
            let Ok(ptype) = PrimType::try_from(bc.r#type) else {
                debug!(session_id, r#type = bc.r#type, "ignoring unknown prim type");
                invalid_envelope(inbound, server_seq, "unknown created prim type");
                return;
            };
            let prim_path = join_prim_path(&bc.parent_path, &bc.prim_name);
            // The optional world-space placement (converted from the wire
            // convention for the matrix variant). Shared by every prim kind.
            let world = world_from_broadcast(bc.world_transform);
            let kind = if ptype == PrimType::Ref {
                // A typeless reference/payload prim. Without a reference path
                // there is nothing to compose, so drop it defensively.
                let Some(reference_path) = bc.reference_path else {
                    debug!(session_id, %prim_path, "ignoring REF create with no reference_path");
                    invalid_envelope(inbound, server_seq, "reference create has no reference path");
                    return;
                };
                // Default (absent) ref_type is REFERENCE (proto3 default 0).
                let as_payload = bc.ref_type == Some(PrimRefType::Payload as i32);
                RemoteEditKind::CreateReference {
                    as_payload,
                    reference_path,
                    world,
                }
            } else {
                let Some(type_name) = ptype.usd_type_name() else {
                    // PRIM_TYPE_UNSPECIFIED — nothing to author.
                    invalid_envelope(inbound, server_seq, "unspecified created prim type");
                    return;
                };
                RemoteEditKind::CreatePrim {
                    type_name: type_name.to_owned(),
                    size: bc.size,
                    height: bc.height,
                    radius: bc.radius,
                    axis: bc.axis,
                    width: bc.width,
                    length: bc.length,
                    world,
                }
            };
            if let Ok(mut q) = inbound.lock() {
                q.push_back(envelope(server_seq, prim_path, kind));
            }
        }
        Some(base_message::Payload::SetPrimActiveBroadcast(bc)) => {
            // A prim active-state change the server authored — the originator's
            // own echo, a peer's soft delete / undo, or a join-snapshot replay.
            // Queue it on the SAME ordered inbound stream as transforms and
            // creates so it never races a transform or create for the same prim.
            adopt_seq(base_seq);
            if let Ok(mut q) = inbound.lock() {
                q.push_back(envelope(server_seq, bc.prim_path, RemoteEditKind::SetActive { active: bc.active }));
            }
        }
        Some(base_message::Payload::Ack(ack)) => {
            // Record receipt. Only a matched successful transform ack becomes
            // an application barrier (its local edit was already authored).
            adopt_seq(base_seq);
            // Is this the ack for an outbound prim-create? If so, a failure must
            // be surfaced with the parent/type it was for; a success needs no
            // action (the prim is authored from the CreatePrimBroadcast).
            if let Some(pending) = pending_creates.remove(&request_id) {
                if !ack.ok {
                    warn!(
                        session_id,
                        parent = %pending.parent_path,
                        type_name = %pending.type_name,
                        error = %ack.error,
                        "collab prim create rejected"
                    );
                    push_event(
                        events,
                        CollabEvent::CreateRejected {
                            parent_path: pending.parent_path,
                            type_name: pending.type_name,
                            reason: ack.error,
                        },
                    );
                }
                return;
            }
            // Or the ack for an outbound active-state change (soft delete / undo)?
            // A failure must be surfaced with the prim and the state it was for; a
            // success needs no action (the change is authored from the
            // SetPrimActiveBroadcast).
            if let Some(pending) = pending_set_active.remove(&request_id) {
                if !ack.ok {
                    warn!(
                        session_id,
                        prim_path = %pending.prim_path,
                        active = pending.active,
                        error = %ack.error,
                        "collab prim set-active rejected"
                    );
                    push_event(
                        events,
                        CollabEvent::SetActiveRejected {
                            prim_path: pending.prim_path,
                            active: pending.active,
                            reason: ack.error,
                        },
                    );
                }
                return;
            }
            if !ack.ok {
                warn!(session_id, error = %ack.error, "collab update rejected");
            }
            if pending_transforms.remove(&request_id) && ack.ok
                && let Ok(mut q) = inbound.lock() {
                q.push_back(envelope(server_seq, String::new(), RemoteEditKind::Acknowledged { request_id }));
            }
            push_event(
                events,
                CollabEvent::Ack {
                    request_id,
                    server_seq,
                    ok: ack.ok,
                    error: ack.error,
                },
            );
        }
        Some(base_message::Payload::SnapshotComplete(_)) => {
            adopt_seq(base_seq);
            if let Ok(mut q) = inbound.lock() {
                q.push_back(envelope(server_seq, String::new(), RemoteEditKind::SnapshotComplete));
            }
            debug!(session_id, base_seq = *base_seq, request_id, "collab snapshot complete");
        }
        Some(base_message::Payload::LayerReloaded(lr)) => {
            // A file the session's stage uses was reloaded on the server. Queue
            // it on the SAME ordered inbound stream so the session processes it
            // in command order with the surrounding frames; the session then
            // re-fetches that file, reloads it into the stage, and re-gates on a
            // fresh snapshot. No `server_seq` ordering is claimed for the reload
            // frame itself (it carries the post-reload version for context only).
            adopt_seq(base_seq);
            debug!(session_id, usd_path = %lr.usd_path, usd_uri = %lr.usd_uri, "collab layer reloaded");
            if let Ok(mut q) = inbound.lock() {
                q.push_back(envelope(
                    server_seq,
                    String::new(),
                    RemoteEditKind::LayerReloaded { usd_path: lr.usd_path, usd_uri: lr.usd_uri },
                ));
            }
        }
        Some(base_message::Payload::Error(err)) => {
            warn!(session_id, code = %err.code, message = %err.message, "collab server error");
            push_event(
                events,
                CollabEvent::Error {
                    code: err.code,
                    message: err.message,
                },
            );
        }
        Some(base_message::Payload::Handshake(h)) => {
            // Informational: the shell has already downloaded and loaded the
            // stage over REST before connecting, so `usd_uri` is not needed here.
            debug!(session_id, usd_path = %h.usd_path, "collab handshake received");
        }
        Some(
            base_message::Payload::XformUpdate(_)
            | base_message::Payload::CreatePrim(_)
            | base_message::Payload::SetPrimActive(_)
            | base_message::Payload::SnapshotRequest(_),
        ) => {
            // Client→server only; a server never sends these. Ignore defensively.
            debug!(session_id, "ignoring unexpected inbound client→server message");
        }
        None => {}
    }
}

fn envelope(server_seq: u64, prim_path: String, kind: RemoteEditKind) -> RemoteEdit {
    static NEXT_DELIVERY: AtomicU64 = AtomicU64::new(1);
    RemoteEdit { server_seq, prim_path, kind, delivery_id: NEXT_DELIVERY.fetch_add(1, Ordering::Relaxed) }
}

fn invalid_envelope(inbound: &Mutex<VecDeque<RemoteEdit>>, seq: u64, reason: &str) {
    if let Ok(mut queue) = inbound.lock() {
        queue.push_back(envelope(seq, String::new(), RemoteEditKind::Invalid { reason: reason.into() }));
    }
}

/// Join a parent prim path and a server-assigned leaf name into an absolute
/// path, collapsing the root `/` so it never doubles (`/` + `Cube` → `/Cube`,
/// `/World` + `Cube` → `/World/Cube`).
fn join_prim_path(parent_path: &str, prim_name: &str) -> String {
    let parent = parent_path.trim_end_matches('/');
    format!("{parent}/{prim_name}")
}

/// Convert a `CreatePrimBroadcast` world-transform oneof into a
/// [`RemoteWorldTransform`]. The matrix variant is converted from the wire
/// convention to a flat row-major USD matrix via [`usd_from_wire`]; the
/// decomposed variant is kept verbatim (rotation is Euler degrees, XYZ order),
/// matching how `XformBroadcast` handles its transform oneof. `None` passes
/// through unchanged (the prim inherits its parent's transform).
fn world_from_broadcast(
    wt: Option<create_prim_broadcast::WorldTransform>,
) -> Option<RemoteWorldTransform> {
    match wt? {
        create_prim_broadcast::WorldTransform::Matrix(m) => {
            Some(RemoteWorldTransform::Matrix(usd_from_wire(&m)))
        }
        create_prim_broadcast::WorldTransform::Seperate(s) => {
            let v = |opt: Option<crate::proto::Vector3>, dflt: [f64; 3]| {
                opt.map_or(dflt, |p| [p.x, p.y, p.z])
            };
            Some(RemoteWorldTransform::Separate {
                translation: v(s.translation, [0.0; 3]),
                rotation_deg: v(s.rotation, [0.0; 3]),
                scale: v(s.scale, [1.0; 3]),
            })
        }
    }
}

fn push_event(events: &Mutex<VecDeque<CollabEvent>>, ev: CollabEvent) {
    if let Ok(mut q) = events.lock() {
        // Bound the queue so a session that never drains cannot grow unbounded.
        const MAX_EVENTS: usize = 256;
        if q.len() >= MAX_EVENTS {
            q.pop_front();
        }
        q.push_back(ev);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Builds a client→server `Handshake` envelope.
///
/// Retained only for the proto round-trip test: at runtime the client never
/// sends a Handshake (it is a server→client message — see `connect`).
#[cfg(test)]
fn make_handshake(session_id: &str) -> BaseMessage {
    BaseMessage {
        session_id: Some(session_id.to_owned()),
        payload: Some(base_message::Payload::Handshake(crate::proto::Handshake {
            session_id: session_id.to_owned(),
            session_layer: String::new(),
            usd_path: String::new(),
            usd_uri: String::new(),
        })),
        request_id: 0,
        server_seq: 0,
    }
}

fn make_xform_update(
    session_id: &str,
    prim_path: String,
    xform: OutboundXform,
    timestamp: i64,
    request_id: u64,
    base_seq: u64,
) -> BaseMessage {
    let transform = match xform {
        // A pure TRS prim: send the exact authored Euler degrees + per-axis scale
        // so the server stores them verbatim (no matrix decompose → no Y/Z scale
        // swap under a ±90° rotation).
        OutboundXform::Separate {
            translation,
            rotation_deg,
            scale,
        } => transform_update::Transform::Seperate(SeparateTransform {
            translation: Some(Vector3 {
                x: translation[0],
                y: translation[1],
                z: translation[2],
            }),
            rotation: Some(Vector3 {
                x: rotation_deg[0],
                y: rotation_deg[1],
                z: rotation_deg[2],
            }),
            scale: Some(Vector3 {
                x: scale[0],
                y: scale[1],
                z: scale[2],
            }),
        }),
        // A matrix-op or otherwise non-TRS prim: fall back to the baked matrix.
        OutboundXform::Matrix(matrix) => {
            transform_update::Transform::Matrix(wire_from_usd(matrix))
        }
    };
    BaseMessage {
        session_id: Some(session_id.to_owned()),
        payload: Some(base_message::Payload::XformUpdate(TransformUpdate {
            session_id: session_id.to_owned(),
            usd_file: String::new(),
            prim_path,
            transform: Some(transform),
            timestamp,
        })),
        request_id,
        // The base of this update: the highest server_seq applied so far.
        server_seq: base_seq,
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub(crate) fn set_read_timeout(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) -> Result<(), CollabError> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(timeout)?,
        MaybeTlsStream::Rustls(s) => s.get_ref().set_read_timeout(timeout)?,
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Codec fixtures do not have sent transforms. Wire acknowledgement ordering
    // with real pending requests is exercised in tests/revision_contract.rs.
    #[allow(clippy::too_many_arguments)]
    fn handle_msg(
        msg: tungstenite::Message,
        inbound: &Mutex<VecDeque<RemoteEdit>>,
        events: &Mutex<VecDeque<CollabEvent>>,
        synced: &AtomicBool,
        received: &mut u64,
        creates: &mut HashMap<u64, PendingCreate>,
        active: &mut HashMap<u64, PendingSetActive>,
        session_id: &str,
    ) {
        super::handle_msg(msg, inbound, events, synced, received, creates, active, &mut HashSet::new(), session_id);
    }
    use super::*;
    use crate::proto::{
        CreatePrimBroadcast, PrimRefType, PrimType, SnapshotComplete, TransformBroadcast, Vector3,
        create_prim_broadcast,
    };

    fn encode(msg: &BaseMessage) -> tungstenite::Message {
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        tungstenite::Message::Binary(buf.into())
    }

    fn broadcast_frame(
        payload: transform_update::Transform,
        server_seq: u64,
    ) -> tungstenite::Message {
        encode(&BaseMessage {
            session_id: Some("s1".into()),
            payload: Some(base_message::Payload::XformBroadcast(TransformBroadcast {
                client_id: String::new(),
                update: Some(TransformUpdate {
                    session_id: "s1".into(),
                    usd_file: String::new(),
                    prim_path: "/World/Prim".into(),
                    transform: Some(payload),
                    timestamp: 1,
                }),
            })),
            request_id: 0,
            server_seq,
        })
    }

    #[test]
    fn handshake_carries_session_id() {
        let msg = make_handshake("test-session-42");
        assert_eq!(msg.session_id, Some("test-session-42".into()));
        let Some(base_message::Payload::Handshake(h)) = msg.payload else {
            panic!("expected Handshake payload");
        };
        assert_eq!(h.session_id, "test-session-42");
    }

    #[test]
    fn xform_update_encodes_and_decodes() {
        let matrix = crate::conv::UsdMat4::default();
        let msg = make_xform_update(
            "s1",
            "/World/Prim".into(),
            OutboundXform::Matrix(matrix),
            999,
            3,
            11,
        );
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        assert_eq!(decoded.request_id, 3);
        assert_eq!(
            decoded.server_seq, 11,
            "base_seq must be stamped on the envelope"
        );
        let Some(base_message::Payload::XformUpdate(u)) = decoded.payload else {
            panic!("expected XformUpdate");
        };
        assert_eq!(u.prim_path, "/World/Prim");
        assert_eq!(u.timestamp, 999);
        assert!(matches!(
            u.transform,
            Some(transform_update::Transform::Matrix(_))
        ));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn separate_xform_update_preserves_exact_trs_under_quarter_turn() {
        // Regression for the Y/Z scale-axis swap: a prim scaled on X and Y and
        // rotated +90° about X. A baked matrix would force the server to
        // decompose and could move the Y scale onto Z. The Separate path must
        // transmit the exact authored Euler + per-axis scale, untouched by the
        // rotation.
        let msg = make_xform_update(
            "s1",
            "/World/Plane".into(),
            OutboundXform::Separate {
                translation: [1.0, 2.0, 3.0],
                rotation_deg: [90.0, 0.0, 0.0],
                scale: [2.0, 3.0, 1.0],
            },
            7,
            4,
            12,
        );
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();
        let decoded = BaseMessage::decode(buf.as_slice()).unwrap();
        let Some(base_message::Payload::XformUpdate(u)) = decoded.payload else {
            panic!("expected XformUpdate");
        };
        let Some(transform_update::Transform::Seperate(s)) = u.transform else {
            panic!("expected a SeparateTransform on the wire, not a baked matrix");
        };
        assert_eq!(s.translation, Some(Vector3 { x: 1.0, y: 2.0, z: 3.0 }));
        assert_eq!(s.rotation, Some(Vector3 { x: 90.0, y: 0.0, z: 0.0 }));
        assert_eq!(
            s.scale,
            Some(Vector3 { x: 2.0, y: 3.0, z: 1.0 }),
            "Y scale (3.0) must stay on Y and not migrate to Z"
        );
    }

    #[test]
    fn inbound_matrix_broadcast_decodes_to_matrix_edit() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(false);
        let mut base_seq = 0;
        let frame = broadcast_frame(
            transform_update::Transform::Matrix(wire_from_usd([
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ])),
            5,
        );
        let mut pending_creates = std::collections::HashMap::new();
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut pending_creates, &mut std::collections::HashMap::new(), "s1");
        assert_eq!(base_seq, 5, "broadcast server_seq must advance base");
        let edit = inbound.lock().unwrap().pop_front().unwrap();
        assert_eq!(edit.prim_path, "/World/Prim");
        assert!(matches!(edit.kind, RemoteEditKind::Matrix(_)));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn inbound_separate_broadcast_decodes_to_separate_edit() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(false);
        let mut base_seq = 0;
        let frame = broadcast_frame(
            transform_update::Transform::Seperate(crate::proto::SeparateTransform {
                translation: Some(Vector3 {
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                }),
                rotation: Some(Vector3 {
                    x: 0.0,
                    y: 90.0,
                    z: 0.0,
                }),
                scale: Some(Vector3 {
                    x: 2.0,
                    y: 2.0,
                    z: 2.0,
                }),
            }),
            9,
        );
        let mut pending_creates = std::collections::HashMap::new();
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut pending_creates, &mut std::collections::HashMap::new(), "s1");
        let edit = inbound.lock().unwrap().pop_front().unwrap();
        let RemoteEditKind::Separate {
            translation,
            rotation_deg,
            scale,
        } = edit.kind
        else {
            panic!("expected Separate edit; the server prefers SeparateTransform");
        };
        assert_eq!(translation, [1.0, 2.0, 3.0]);
        assert_eq!(rotation_deg, [0.0, 90.0, 0.0]);
        assert_eq!(scale, [2.0, 2.0, 2.0]);
    }

    #[test]
    fn snapshot_complete_queues_an_application_barrier() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(false);
        let mut base_seq = 0;
        let frame = encode(&BaseMessage {
            session_id: Some("s1".into()),
            payload: Some(base_message::Payload::SnapshotComplete(SnapshotComplete {})),
            request_id: 0,
            server_seq: 42,
        });
        let mut pending_creates = std::collections::HashMap::new();
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut pending_creates, &mut std::collections::HashMap::new(), "s1");
        assert!(!synced.load(Ordering::Acquire));
        assert!(matches!(inbound.lock().unwrap().front().unwrap().kind, RemoteEditKind::SnapshotComplete));
        assert_eq!(base_seq, 42);
    }

    #[test]
    fn ack_adopts_server_seq_and_records_event() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(true);
        let mut base_seq = 3;
        let frame = encode(&BaseMessage {
            session_id: None,
            payload: Some(base_message::Payload::Ack(crate::proto::Ack {
                ref_id: String::new(),
                ok: false,
                error: "stale".into(),
            })),
            request_id: 7,
            server_seq: 10,
        });
        let mut pending_creates = std::collections::HashMap::new();
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut pending_creates, &mut std::collections::HashMap::new(), "s1");
        assert_eq!(
            base_seq, 10,
            "ack advances only the diagnostic received watermark"
        );
        let ev = events.lock().unwrap().pop_front().unwrap();
        assert!(matches!(ev, CollabEvent::Ack { ok: false, .. }));
    }

    #[test]
    fn inbound_create_prim_broadcast_decodes_to_create_edit() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(false);
        let mut base_seq = 0;
        let mut pending_creates = std::collections::HashMap::new();
        let frame = encode(&BaseMessage {
            session_id: Some("s1".into()),
            payload: Some(base_message::Payload::CreatePrimBroadcast(CreatePrimBroadcast {
                parent_path: "/World".into(),
                prim_name: "Cube_1".into(),
                r#type: PrimType::Cube as i32,
                size: Some(2.0),
                height: None,
                radius: None,
                axis: None,
                width: None,
                length: None,
                ref_type: None,
                reference_path: None,
                world_transform: None,
            })),
            request_id: 0,
            server_seq: 6,
        });
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut pending_creates, &mut std::collections::HashMap::new(), "s1");
        assert_eq!(base_seq, 6, "create broadcast server_seq must advance base");
        let edit = inbound.lock().unwrap().pop_front().unwrap();
        assert_eq!(edit.prim_path, "/World/Cube_1");
        assert_eq!(
            edit.kind,
            RemoteEditKind::CreatePrim {
                type_name: "Cube".into(),
                size: Some(2.0),
                height: None,
                radius: None,
                axis: None,
                width: None,
                length: None,
                world: None,
            },
            "the server-authored attribute values must be carried, not dropped"
        );
    }

    #[test]
    fn create_broadcast_under_root_has_single_slash() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(false);
        let mut base_seq = 0;
        let mut pending_creates = std::collections::HashMap::new();
        let frame = encode(&BaseMessage {
            session_id: Some("s1".into()),
            payload: Some(base_message::Payload::CreatePrimBroadcast(CreatePrimBroadcast {
                parent_path: "/".into(),
                prim_name: "Xform".into(),
                r#type: PrimType::Xform as i32,
                size: None,
                height: None,
                radius: None,
                axis: None,
                width: None,
                length: None,
                ref_type: None,
                reference_path: None,
                world_transform: None,
            })),
            request_id: 0,
            server_seq: 1,
        });
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut pending_creates, &mut std::collections::HashMap::new(), "s1");
        let edit = inbound.lock().unwrap().pop_front().unwrap();
        assert_eq!(edit.prim_path, "/Xform", "root parent must not double the slash");
    }

    #[test]
    fn inbound_ref_broadcast_decodes_to_create_reference_edit() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(false);
        let mut base_seq = 0;
        let mut pending_creates = std::collections::HashMap::new();
        let frame = encode(&BaseMessage {
            session_id: Some("s1".into()),
            payload: Some(base_message::Payload::CreatePrimBroadcast(CreatePrimBroadcast {
                parent_path: "/World".into(),
                prim_name: "car".into(),
                r#type: PrimType::Ref as i32,
                size: None,
                height: None,
                radius: None,
                axis: None,
                width: None,
                length: None,
                ref_type: Some(PrimRefType::Payload as i32),
                reference_path: Some("../assets/car.usda".into()),
                world_transform: None,
            })),
            request_id: 0,
            server_seq: 8,
        });
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut pending_creates, &mut std::collections::HashMap::new(), "s1");
        let edit = inbound.lock().unwrap().pop_front().unwrap();
        assert_eq!(edit.prim_path, "/World/car");
        assert_eq!(
            edit.kind,
            RemoteEditKind::CreateReference {
                as_payload: true,
                reference_path: "../assets/car.usda".into(),
                world: None,
            },
            "a PAYLOAD REF broadcast must decode to a payload CreateReference"
        );
    }

    #[test]
    fn inbound_ref_broadcast_absent_ref_type_defaults_to_reference() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(false);
        let mut base_seq = 0;
        let frame = encode(&BaseMessage {
            session_id: Some("s1".into()),
            payload: Some(base_message::Payload::CreatePrimBroadcast(CreatePrimBroadcast {
                parent_path: "/World".into(),
                prim_name: "car".into(),
                r#type: PrimType::Ref as i32,
                size: None,
                height: None,
                radius: None,
                axis: None,
                width: None,
                length: None,
                // Absent ref_type → proto3 default REFERENCE.
                ref_type: None,
                reference_path: Some("car.usda".into()),
                world_transform: None,
            })),
            request_id: 0,
            server_seq: 1,
        });
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut std::collections::HashMap::new(), &mut std::collections::HashMap::new(), "s1");
        let edit = inbound.lock().unwrap().pop_front().unwrap();
        assert!(matches!(
            edit.kind,
            RemoteEditKind::CreateReference { as_payload: false, .. }
        ));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn inbound_create_broadcast_carries_world_matrix_transform() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(false);
        let mut base_seq = 0;
        let frame = encode(&BaseMessage {
            session_id: Some("s1".into()),
            payload: Some(base_message::Payload::CreatePrimBroadcast(CreatePrimBroadcast {
                parent_path: "/World".into(),
                prim_name: "Cube_1".into(),
                r#type: PrimType::Cube as i32,
                size: Some(1.0),
                height: None,
                radius: None,
                axis: None,
                width: None,
                length: None,
                ref_type: None,
                reference_path: None,
                world_transform: Some(create_prim_broadcast::WorldTransform::Matrix(
                    wire_from_usd([
                        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 5.0, 6.0, 7.0,
                        1.0,
                    ]),
                )),
            })),
            request_id: 0,
            server_seq: 3,
        });
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut std::collections::HashMap::new(), &mut std::collections::HashMap::new(), "s1");
        let edit = inbound.lock().unwrap().pop_front().unwrap();
        let RemoteEditKind::CreatePrim { world, .. } = edit.kind else {
            panic!("expected CreatePrim");
        };
        let Some(RemoteWorldTransform::Matrix(m)) = world else {
            panic!("expected a world matrix transform");
        };
        // usd_from_wire(wire_from_usd(x)) round-trips: translation row preserved.
        assert_eq!([m[12], m[13], m[14]], [5.0, 6.0, 7.0]);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn world_from_broadcast_separate_keeps_trs_verbatim() {
        let wt = Some(create_prim_broadcast::WorldTransform::Seperate(
            crate::proto::SeparateTransform {
                translation: Some(Vector3 { x: 1.0, y: 2.0, z: 3.0 }),
                rotation: Some(Vector3 { x: 0.0, y: 90.0, z: 0.0 }),
                scale: Some(Vector3 { x: 2.0, y: 2.0, z: 2.0 }),
            },
        ));
        let Some(RemoteWorldTransform::Separate { translation, rotation_deg, scale }) =
            world_from_broadcast(wt)
        else {
            panic!("expected a separate world transform");
        };
        assert_eq!(translation, [1.0, 2.0, 3.0]);
        assert_eq!(rotation_deg, [0.0, 90.0, 0.0]);
        assert_eq!(scale, [2.0, 2.0, 2.0]);
        assert_eq!(world_from_broadcast(None), None);
    }

    #[test]
    fn create_ack_failure_emits_create_rejected_not_plain_ack() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(true);
        let mut base_seq = 3;
        let mut pending_creates = std::collections::HashMap::new();
        pending_creates.insert(
            7,
            PendingCreate {
                base_seq: 0,
                parent_path: "/World/Cube".into(),
                prim_type: PrimType::Sphere,
                type_name: "Sphere".into(),
                ref_type: None,
                reference_path: None,
            },
        );
        let frame = encode(&BaseMessage {
            session_id: None,
            payload: Some(base_message::Payload::Ack(crate::proto::Ack {
                ref_id: String::new(),
                ok: false,
                error: "invalid_parent".into(),
            })),
            request_id: 7,
            server_seq: 3,
        });
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut pending_creates, &mut std::collections::HashMap::new(), "s1");
        assert!(
            pending_creates.is_empty(),
            "the create ack must be consumed from the pending map"
        );
        let ev = events.lock().unwrap().pop_front().unwrap();
        let CollabEvent::CreateRejected {
            parent_path,
            type_name,
            reason,
        } = ev
        else {
            panic!("expected CreateRejected, not a plain Ack");
        };
        assert_eq!(parent_path, "/World/Cube");
        assert_eq!(type_name, "Sphere");
        assert_eq!(reason, "invalid_parent");
    }

    #[test]
    fn create_ack_success_is_silent() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(true);
        let mut base_seq = 3;
        let mut pending_creates = std::collections::HashMap::new();
        pending_creates.insert(
            9,
            PendingCreate {
                base_seq: 0,
                parent_path: "/World".into(),
                prim_type: PrimType::Cube,
                type_name: "Cube".into(),
                ref_type: None,
                reference_path: None,
            },
        );
        let frame = encode(&BaseMessage {
            session_id: None,
            payload: Some(base_message::Payload::Ack(crate::proto::Ack {
                ref_id: String::new(),
                ok: true,
                error: String::new(),
            })),
            request_id: 9,
            server_seq: 12,
        });
        handle_msg(frame, &inbound, &events, &synced, &mut base_seq, &mut pending_creates, &mut std::collections::HashMap::new(), "s1");
        assert!(pending_creates.is_empty(), "successful create ack is consumed");
        assert!(
            events.lock().unwrap().is_empty(),
            "a successful create ack emits no user event (prim comes from the broadcast)"
        );
        assert_eq!(base_seq, 12, "the create ack still advances base");
    }

    #[test]
    fn requeue_unsent_creates_prepends_in_order() {
        // A send failure mid-batch must put the unsent remainder back AHEAD of
        // anything queued meanwhile, in its original order, so a reconnect
        // resends it before newer creates (and loses nothing).
        let shared = Mutex::new(vec![PendingCreate {
            base_seq: 0,
            parent_path: "/".into(),
            prim_type: PrimType::Xform,
            type_name: "Xform".into(),
            ref_type: None,
            reference_path: None,
        }]);
        let unsent = vec![
            PendingCreate {
                base_seq: 0,
                parent_path: "/A".into(),
                prim_type: PrimType::Cube,
                type_name: "Cube".into(),
                ref_type: None,
                reference_path: None,
            },
            PendingCreate {
                base_seq: 0,
                parent_path: "/B".into(),
                prim_type: PrimType::Sphere,
                type_name: "Sphere".into(),
                ref_type: None,
                reference_path: None,
            },
        ];
        requeue_unsent_creates(&shared, unsent);
        let q = shared.lock().unwrap();
        let paths: Vec<&str> = q.iter().map(|c| c.parent_path.as_str()).collect();
        assert_eq!(
            paths,
            ["/A", "/B", "/"],
            "unsent remainder must sit ahead of newly-queued creates, in order"
        );
    }

    #[test]
    fn requeue_unsent_creates_empty_is_noop() {
        let shared = Mutex::new(vec![PendingCreate {
            base_seq: 0,
            parent_path: "/".into(),
            prim_type: PrimType::Xform,
            type_name: "Xform".into(),
            ref_type: None,
            reference_path: None,
        }]);
        requeue_unsent_creates(&shared, Vec::new());
        assert_eq!(shared.lock().unwrap().len(), 1, "no spurious entries added");
    }

    #[test]
    fn inbound_set_prim_active_broadcast_decodes_to_set_active_edit() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(false);
        let mut base_seq = 0;
        let frame = encode(&BaseMessage {
            session_id: Some("s1".into()),
            payload: Some(base_message::Payload::SetPrimActiveBroadcast(
                crate::proto::SetPrimActiveBroadcast {
                    prim_path: "/World/Cube".into(),
                    active: false,
                },
            )),
            request_id: 0,
            server_seq: 14,
        });
        handle_msg(
            frame,
            &inbound,
            &events,
            &synced,
            &mut base_seq,
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
            "s1",
        );
        assert_eq!(base_seq, 14, "set-active broadcast server_seq must advance base");
        let edit = inbound.lock().unwrap().pop_front().unwrap();
        assert_eq!(edit.prim_path, "/World/Cube");
        assert_eq!(
            edit.kind,
            RemoteEditKind::SetActive { active: false },
            "a soft-delete broadcast carries active=false"
        );
    }

    #[test]
    fn set_active_ack_failure_emits_set_active_rejected() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(true);
        let mut base_seq = 3;
        let mut pending_set_active = std::collections::HashMap::new();
        pending_set_active.insert(
            5,
            PendingSetActive {
                base_seq: 0,
                prim_path: "/World/Ref/Child".into(),
                active: false,
            },
        );
        let frame = encode(&BaseMessage {
            session_id: None,
            payload: Some(base_message::Payload::Ack(crate::proto::Ack {
                ref_id: String::new(),
                ok: false,
                error: "foreign_layer".into(),
            })),
            request_id: 5,
            server_seq: 3,
        });
        handle_msg(
            frame,
            &inbound,
            &events,
            &synced,
            &mut base_seq,
            &mut std::collections::HashMap::new(),
            &mut pending_set_active,
            "s1",
        );
        assert!(
            pending_set_active.is_empty(),
            "the set-active ack must be consumed from the pending map"
        );
        let ev = events.lock().unwrap().pop_front().unwrap();
        let CollabEvent::SetActiveRejected {
            prim_path,
            active,
            reason,
        } = ev
        else {
            panic!("expected SetActiveRejected, not a plain Ack");
        };
        assert_eq!(prim_path, "/World/Ref/Child");
        assert!(!active);
        assert_eq!(reason, "foreign_layer");
    }

    #[test]
    fn set_active_ack_success_is_silent() {
        let inbound = Mutex::new(VecDeque::new());
        let events = Mutex::new(VecDeque::new());
        let synced = AtomicBool::new(true);
        let mut base_seq = 3;
        let mut pending_set_active = std::collections::HashMap::new();
        pending_set_active.insert(
            6,
            PendingSetActive {
                base_seq: 0,
                prim_path: "/World/Cube".into(),
                active: false,
            },
        );
        let frame = encode(&BaseMessage {
            session_id: None,
            payload: Some(base_message::Payload::Ack(crate::proto::Ack {
                ref_id: String::new(),
                ok: true,
                error: String::new(),
            })),
            request_id: 6,
            server_seq: 15,
        });
        handle_msg(
            frame,
            &inbound,
            &events,
            &synced,
            &mut base_seq,
            &mut std::collections::HashMap::new(),
            &mut pending_set_active,
            "s1",
        );
        assert!(
            pending_set_active.is_empty(),
            "successful set-active ack is consumed"
        );
        assert!(
            events.lock().unwrap().is_empty(),
            "a successful set-active ack emits no user event (change comes from the broadcast)"
        );
        assert_eq!(base_seq, 15, "the set-active ack still advances base");
    }

    #[test]
    fn requeue_unsent_set_active_prepends_in_order() {
        let shared = Mutex::new(vec![PendingSetActive {
            base_seq: 0,
            prim_path: "/C".into(),
            active: true,
        }]);
        let unsent = vec![
            PendingSetActive {
                base_seq: 0,
                prim_path: "/A".into(),
                active: false,
            },
            PendingSetActive {
                base_seq: 0,
                prim_path: "/B".into(),
                active: false,
            },
        ];
        requeue_unsent_set_active(&shared, unsent);
        let q = shared.lock().unwrap();
        let paths: Vec<&str> = q.iter().map(|c| c.prim_path.as_str()).collect();
        assert_eq!(
            paths,
            ["/A", "/B", "/C"],
            "unsent remainder must sit ahead of newly-queued changes, in order"
        );
    }

    #[test]
    fn now_millis_is_in_reasonable_range() {
        let ts = now_millis();
        // 2000-01-01 in millis
        assert!(ts > 946_684_800_000, "timestamp looks too old: {ts}");
        // 2100-01-01 in millis
        assert!(
            ts < 4_102_444_800_000_i64,
            "timestamp looks too far ahead: {ts}"
        );
    }
}
